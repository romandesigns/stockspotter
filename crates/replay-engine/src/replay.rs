//! The actual replay engine — architecture doc section 7. Takes
//! historical data for one symbol/date-range and runs it through the
//! *exact same* `fast_funnel`/`momentum_scorer`/`ignition_detector` code
//! the live path (`market_data::bin::scan`) uses, just fed by
//! `historical.rs`'s REST fetches instead of `ws.rs`'s live stream. This
//! is the doc's own stated architecture, not an approximation of it — no
//! separate "backtest version" of any detection logic exists anywhere in
//! this codebase; `replay_symbol` below calls the identical functions and
//! types `bin/scan.rs` calls.
//!
//! Not implemented: halt-lift resumption during replay — Alpaca doesn't
//! expose a historical trading-status/LULD REST endpoint this crate could
//! find, so `IgnitionMonitor::on_status` never gets called here. Live
//! ignition detection still has all four signals; replay currently has
//! three. Documented gap, same pattern as the other honestly-scoped gaps
//! (float, etc.) elsewhere in this codebase.

use anyhow::Result;
use chrono::{DateTime, Utc};
use fast_funnel::{explain, FilterThresholds, FunnelExplanation};
use ignition_detector::{IgnitionMonitor, MonitorConfig, MonitorEvent};
use market_data::{fetch_daily_seeds, AlpacaConfig, SessionTracker};
use momentum_scorer::{Candle, MomentumScore, MomentumWeights, RollingWindow};
use serde::Serialize;
use tracing::warn;

use crate::historical::{fetch_historical_bars, fetch_historical_quotes, fetch_historical_trades};

const MOMENTUM_WINDOW: usize = 30;

#[derive(Debug, Clone, Serialize)]
pub struct BarEvent {
    pub timestamp: DateTime<Utc>,
    pub price: f64,
    pub gap_pct: f64,
    pub session_volume: u64,
    pub funnel: FunnelExplanation,
    pub momentum: MomentumScore,
}

#[derive(Debug, Clone, Serialize)]
pub struct IgnitionEvent {
    pub timestamp: DateTime<Utc>,
    pub price: f64,
    pub kind: IgnitionEventKind,
}

#[derive(Debug, Clone, Serialize)]
pub enum IgnitionEventKind {
    CandidateOpened,
    FollowThroughConfirmed,
    FollowThroughRejected,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplayResult {
    pub symbol: String,
    pub bar_events: Vec<BarEvent>,
    pub ignition_events: Vec<IgnitionEvent>,
}

/// Replays one symbol over `[start, end)` (RFC3339 strings, passed
/// straight to Alpaca) through the live detection pipeline. `prior_close`/
/// `avg_daily_volume` are seeded the same way the live path does
/// (`fetch_daily_seeds`, looking back from `start`) — replay needs its
/// own prior-session context exactly like a live session would have had
/// one already running.
pub async fn replay_symbol(
    cfg: &AlpacaConfig,
    symbol: &str,
    start: &str,
    end: &str,
) -> Result<ReplayResult> {
    let symbols = vec![symbol.to_string()];
    let seeds = fetch_daily_seeds(cfg, &symbols, 20).await?;
    let seed = seeds.get(symbol);
    let (prior_close, avg_daily_volume) = match seed {
        Some(s) => (s.prior_close, s.avg_daily_volume),
        None => {
            warn!(symbol, "no daily seed available; defaulting prior_close/avg_daily_volume to 0");
            (0.0, 0)
        }
    };

    // float_shares: None, same reasoning as the live path — this replay
    // engine doesn't re-fetch float per historical run; a caller wanting
    // Stage 1 to actually pass historically would need to pipe a real
    // float value in separately (e.g. from FMP), not something this
    // function does on its own.
    let mut tracker = SessionTracker::new(symbol.to_string(), prior_close, avg_daily_volume, None);
    let mut momentum_window = RollingWindow::new(MOMENTUM_WINDOW);
    let momentum_weights = MomentumWeights::default();
    let thresholds = FilterThresholds::default();

    let bars = fetch_historical_bars(cfg, symbol, start, end, "1Min").await?;
    let mut bar_events = Vec::with_capacity(bars.len());
    for bar in &bars {
        let snapshot = tracker.on_bar(bar);
        let funnel = explain(&snapshot, &thresholds);

        momentum_window.push(Candle {
            open: bar.open,
            high: bar.high,
            low: bar.low,
            close: bar.close,
            volume: bar.volume,
        });
        let momentum = momentum_scorer::score(momentum_window.as_slice(), &momentum_weights);

        bar_events.push(BarEvent {
            timestamp: bar.timestamp,
            price: snapshot.price,
            gap_pct: snapshot.gap_pct,
            session_volume: snapshot.session_volume,
            funnel,
            momentum,
        });
    }

    let trades = fetch_historical_trades(cfg, symbol, start, end).await?;
    let quotes = fetch_historical_quotes(cfg, symbol, start, end).await?;

    let ticks = merge_ticks(trades, quotes);

    let mut monitor = IgnitionMonitor::new(MonitorConfig::default());
    let mut ignition_events = Vec::new();
    for tick in &ticks {
        match tick {
            Tick::Quote(q) => {
                monitor.on_quote(ignition_detector::Quote {
                    timestamp_secs: to_secs(q.timestamp),
                    bid_price: q.bid_price,
                    bid_size: q.bid_size,
                    ask_price: q.ask_price,
                    ask_size: q.ask_size,
                });
            }
            Tick::Trade(t) => {
                let event = monitor.on_trade(ignition_detector::Trade {
                    timestamp_secs: to_secs(t.timestamp),
                    price: t.price,
                    size: t.size,
                });
                let kind = match event {
                    MonitorEvent::None => continue,
                    MonitorEvent::CandidateOpened(_) => IgnitionEventKind::CandidateOpened,
                    MonitorEvent::FollowThroughResolved(result) if result.confirmed => {
                        IgnitionEventKind::FollowThroughConfirmed
                    }
                    MonitorEvent::FollowThroughResolved(_) => IgnitionEventKind::FollowThroughRejected,
                };
                ignition_events.push(IgnitionEvent {
                    timestamp: t.timestamp,
                    price: t.price,
                    kind,
                });
            }
        }
    }

    Ok(ReplayResult {
        symbol: symbol.to_string(),
        bar_events,
        ignition_events,
    })
}

/// Trades and quotes come back from Alpaca as two separate historical
/// lists — this is just what lets `replay_symbol` merge and sort them
/// into one chronological stream before feeding `IgnitionMonitor`.
enum Tick {
    Trade(market_data::Trade),
    Quote(market_data::Quote),
}

impl Tick {
    fn timestamp(&self) -> DateTime<Utc> {
        match self {
            Tick::Trade(t) => t.timestamp,
            Tick::Quote(q) => q.timestamp,
        }
    }
}

fn to_secs(t: DateTime<Utc>) -> f64 {
    t.timestamp() as f64 + t.timestamp_subsec_nanos() as f64 / 1_000_000_000.0
}

/// Trades and quotes come back from Alpaca as two separate historical
/// lists — merges them into one chronological stream. Feeding
/// `IgnitionMonitor` out of order would let quotes it hasn't "seen" yet
/// influence a trade's detection, or vice versa, which live streaming
/// never does (everything arrives in true wall-clock order there).
fn merge_ticks(trades: Vec<market_data::Trade>, quotes: Vec<market_data::Quote>) -> Vec<Tick> {
    let mut ticks: Vec<Tick> = Vec::with_capacity(trades.len() + quotes.len());
    ticks.extend(trades.into_iter().map(Tick::Trade));
    ticks.extend(quotes.into_iter().map(Tick::Quote));
    ticks.sort_by_key(Tick::timestamp);
    ticks
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn trade_at(secs: i64) -> market_data::Trade {
        market_data::Trade {
            symbol: "TEST".to_string(),
            price: 1.0,
            size: 100,
            timestamp: Utc.timestamp_opt(secs, 0).unwrap(),
        }
    }

    fn quote_at(secs: i64) -> market_data::Quote {
        market_data::Quote {
            symbol: "TEST".to_string(),
            bid_price: 0.99,
            bid_size: 100,
            ask_price: 1.01,
            ask_size: 100,
            timestamp: Utc.timestamp_opt(secs, 0).unwrap(),
        }
    }

    #[test]
    fn merge_ticks_produces_chronological_order() {
        let trades = vec![trade_at(30), trade_at(10)];
        let quotes = vec![quote_at(20), quote_at(0)];

        let merged = merge_ticks(trades, quotes);
        let timestamps: Vec<i64> = merged.iter().map(|t| t.timestamp().timestamp()).collect();

        assert_eq!(timestamps, vec![0, 10, 20, 30]);
        assert!(matches!(merged[0], Tick::Quote(_)));
        assert!(matches!(merged[1], Tick::Trade(_)));
        assert!(matches!(merged[2], Tick::Quote(_)));
        assert!(matches!(merged[3], Tick::Trade(_)));
    }

    #[test]
    fn merge_ticks_handles_empty_inputs() {
        assert!(merge_ticks(Vec::new(), Vec::new()).is_empty());
    }
}
