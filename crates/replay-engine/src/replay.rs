//! The actual replay engine — architecture doc section 7. Takes
//! historical data for one symbol/date-range and runs it through the
//! *exact same* `fast_funnel`/`momentum_scorer`/`ignition_detector` code
//! the live path (`market_data::bin::scan`) uses, just fed by
//! `historical.rs`'s REST fetches instead of `ws.rs`'s live stream. This
//! is the doc's own stated architecture, not an approximation of it — no
//! separate "backtest version" of any detection logic exists anywhere in
//! this codebase.
//!
//! Split into two stages on purpose: `fetch_replay_data` (network, slow,
//! config-independent) and `run_replay` (pure, fast, config-dependent).
//! Threshold tuning means running the *same* historical window through
//! many different configs — fusing fetch+run into one function (as an
//! earlier version of this file did) would mean re-hitting Alpaca once
//! per config tried, which is both slow and needless: the historical data
//! never changes, only what the detectors do with it does.
//!
//! Not implemented: halt-lift resumption during replay — Alpaca doesn't
//! expose a historical trading-status/LULD REST endpoint this crate could
//! find, so `IgnitionMonitor::on_status` never gets called here. Live
//! ignition detection still has all four signals; replay currently has
//! three. Documented gap, same pattern as other honestly-scoped gaps
//! elsewhere in this codebase. (Float used to be one too — replay's
//! `SessionTracker` was hardcoded to `None` — but `fetch_replay_data`
//! now does a real FMP lookup, since that one was actually fixable.)

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fast_funnel::{explain, FilterThresholds, FunnelExplanation};
use ignition_detector::{IgnitionMonitor, MonitorConfig, MonitorEvent};
use market_data::{fetch_daily_seeds_as_of, AlpacaConfig, SessionTracker};
use momentum_scorer::{Candle, MomentumScore, MomentumWeights, RollingWindow};
use serde::Serialize;
use tracing::warn;

use crate::historical::{fetch_historical_bars, fetch_historical_quotes, fetch_historical_trades};

const MOMENTUM_WINDOW: usize = 30;

/// Everything fetched from Alpaca for one symbol/date-range — the
/// expensive, config-independent half of a replay. Fetch this once, then
/// call `run_replay` as many times as needed with different configs.
#[derive(Debug, Clone)]
pub struct ReplayData {
    pub symbol: String,
    pub prior_close: f64,
    pub avg_daily_volume: u64,
    /// `None` if `AlpacaConfig::fmp_api_key` wasn't set, or the lookup
    /// failed/had no data for this symbol — same fail-closed handling as
    /// everywhere else float appears in this codebase. Fetched once here
    /// (not per `run_replay` call) since it's config-independent, same
    /// as everything else in this struct.
    pub float_shares: Option<u64>,
    pub bars: Vec<market_data::Bar>,
    pub trades: Vec<market_data::Trade>,
    pub quotes: Vec<market_data::Quote>,
}

/// Every threshold/weight `run_replay` needs — bundled so a sweep can
/// construct many variants and pass each straight through, and so this
/// list only has to be extended in one place if another config knob
/// becomes worth tuning later.
#[derive(Debug, Clone)]
pub struct ReplayConfig {
    pub funnel_thresholds: FilterThresholds,
    pub momentum_weights: MomentumWeights,
    pub monitor_config: MonitorConfig,
}

impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            funnel_thresholds: FilterThresholds::default(),
            momentum_weights: MomentumWeights::default(),
            monitor_config: MonitorConfig::default(),
        }
    }
}

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

/// Fetches everything needed to replay one symbol over `[start, end)`
/// (RFC3339 strings, passed straight to Alpaca) — bars, trades, quotes,
/// and the prior-session seed. Does not run any detection logic itself.
///
/// The seed is anchored to `start` via `fetch_daily_seeds_as_of`, not
/// `fetch_daily_seeds`'s real-time default — anchoring to "now" here was
/// a genuine lookahead-bias bug (see that function's doc comment): a
/// replay of a past date computed today could silently pull in trailing-
/// average data from after that date, which a real trader at the time
/// could never have known, and the same historical window replayed on
/// different days would produce different, non-reproducible results.
pub async fn fetch_replay_data(
    cfg: &AlpacaConfig,
    symbol: &str,
    start: &str,
    end: &str,
) -> Result<ReplayData> {
    let symbols = vec![symbol.to_string()];
    let start_time: DateTime<Utc> = start
        .parse()
        .with_context(|| format!("parsing replay start '{start}' as RFC3339"))?;
    let seeds = fetch_daily_seeds_as_of(cfg, &symbols, 20, start_time).await?;
    let seed = seeds.get(symbol);
    let (prior_close, avg_daily_volume) = match seed {
        Some(s) => (s.prior_close, s.avg_daily_volume),
        None => {
            warn!(symbol, "no daily seed available; defaulting prior_close/avg_daily_volume to 0");
            (0.0, 0)
        }
    };

    let bars = fetch_historical_bars(cfg, symbol, start, end, "1Min").await?;
    let trades = fetch_historical_trades(cfg, symbol, start, end).await?;
    let quotes = fetch_historical_quotes(cfg, symbol, start, end).await?;

    let float_shares = match &cfg.fmp_api_key {
        Some(key) => match market_data::fetch_float_shares(key, symbol).await {
            Ok(f) => f,
            Err(e) => {
                warn!(symbol, error = %e, "float lookup failed for replay; Stage 1 will fail closed on it");
                None
            }
        },
        None => {
            warn!(symbol, "FMP_API_KEY not set; replay's Stage 1 will fail closed on unknown float");
            None
        }
    };

    Ok(ReplayData {
        symbol: symbol.to_string(),
        prior_close,
        avg_daily_volume,
        float_shares,
        bars,
        trades,
        quotes,
    })
}

/// Runs already-fetched data through the detection pipeline with the
/// given config. Pure and synchronous — no network, safe to call
/// repeatedly with different configs against the same `data`.
///
/// Uses `data.float_shares` (fetched once by `fetch_replay_data`, not
/// re-fetched per call) — Stage 1 fails closed exactly as it does live if
/// that's `None`.
pub fn run_replay(data: &ReplayData, config: &ReplayConfig) -> ReplayResult {
    let mut tracker = SessionTracker::new(
        data.symbol.clone(),
        data.prior_close,
        data.avg_daily_volume,
        data.float_shares,
    );
    let mut momentum_window = RollingWindow::new(MOMENTUM_WINDOW);

    let mut bar_events = Vec::with_capacity(data.bars.len());
    for bar in &data.bars {
        let snapshot = tracker.on_bar(bar);
        let funnel = explain(&snapshot, &config.funnel_thresholds);

        momentum_window.push(Candle {
            open: bar.open,
            high: bar.high,
            low: bar.low,
            close: bar.close,
            volume: bar.volume,
        });
        let momentum = momentum_scorer::score(momentum_window.as_slice(), &config.momentum_weights);

        bar_events.push(BarEvent {
            timestamp: bar.timestamp,
            price: snapshot.price,
            gap_pct: snapshot.gap_pct,
            session_volume: snapshot.session_volume,
            funnel,
            momentum,
        });
    }

    let ticks = merge_ticks(data.trades.clone(), data.quotes.clone());

    let mut monitor = IgnitionMonitor::new(config.monitor_config);
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

    ReplayResult {
        symbol: data.symbol.clone(),
        bar_events,
        ignition_events,
    }
}

/// Convenience wrapper for the common one-shot case (`bin/replay.rs`):
/// fetch and run with the default config in one call.
pub async fn replay_symbol(
    cfg: &AlpacaConfig,
    symbol: &str,
    start: &str,
    end: &str,
) -> Result<ReplayResult> {
    let data = fetch_replay_data(cfg, symbol, start, end).await?;
    Ok(run_replay(&data, &ReplayConfig::default()))
}

/// Trades and quotes come back from Alpaca as two separate historical
/// lists — this is just what lets `run_replay` merge and sort them into
/// one chronological stream before feeding `IgnitionMonitor`.
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

    #[test]
    fn run_replay_is_deterministic_across_repeated_calls_on_the_same_data() {
        // The whole point of splitting fetch/run: calling run_replay
        // twice on identical ReplayData with identical config must
        // produce identical results — no hidden state leaking between
        // calls (e.g. a shared RNG, a static, anything like that).
        let data = ReplayData {
            symbol: "TEST".to_string(),
            prior_close: 1.0,
            avg_daily_volume: 100_000,
            float_shares: Some(5_000_000),
            bars: vec![market_data::Bar {
                symbol: "TEST".to_string(),
                open: 1.0,
                high: 1.05,
                low: 0.99,
                close: 1.02,
                volume: 5000,
                timestamp: Utc.timestamp_opt(0, 0).unwrap(),
            }],
            trades: vec![trade_at(0)],
            quotes: vec![quote_at(0)],
        };
        let config = ReplayConfig::default();

        let first = run_replay(&data, &config);
        let second = run_replay(&data, &config);

        assert_eq!(first.bar_events.len(), second.bar_events.len());
        assert_eq!(first.bar_events[0].price, second.bar_events[0].price);
        assert_eq!(first.ignition_events.len(), second.ignition_events.len());
    }
}
