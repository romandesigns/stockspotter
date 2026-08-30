//! Runnable proof that live Alpaca data flows through the *same*
//! `fast_funnel` functions the (future) replay engine will use — the
//! architecture doc's "one code path, live or replay" requirement, made
//! real for the first time rather than just stated.
//!
//! Watches a small fixed symbol list (the same real small-caps used by the
//! Super Chart prototype), seeds each from Alpaca's daily-bars REST
//! endpoint, then streams realtime bars and logs each one's fast-funnel
//! verdict. Exits cleanly after an idle period with no bars — expected
//! outside market hours — rather than hanging forever.
//!
//! Run with: `cargo run -p market-data --bin scan` (from the repo root, so
//! `.env` is found).

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use fast_funnel::{explain, FilterThresholds};
use ignition_detector::{IgnitionMonitor, MonitorConfig, MonitorEvent, StatusTransition};
use market_data::{fetch_daily_seeds, AlpacaConfig, AlpacaMessage, AlpacaStream, SessionTracker};
use momentum_scorer::{Candle, MomentumWeights, RollingWindow, DEFAULT_QUALIFY_THRESHOLD};
use tracing::{info, warn};

/// Alpaca timestamps in, plain epoch seconds out — ignition-detector's
/// pure functions only ever compare relative elapsed time, so an absolute
/// epoch is fine and keeps the conversion trivial. f64 retains
/// sub-microsecond precision at today's epoch magnitude.
fn to_secs(t: DateTime<Utc>) -> f64 {
    t.timestamp() as f64 + t.timestamp_subsec_nanos() as f64 / 1_000_000_000.0
}

const WATCH_SYMBOLS: &[&str] = &["SWVL", "WCT", "BCAB", "VISN", "WETO"];
const IDLE_TIMEOUT: Duration = Duration::from_secs(20);
const DAILY_LOOKBACK: u32 = 20;
// 20-period MA needs 21 candles minimum; keep a little headroom above that.
const MOMENTUM_WINDOW: usize = 30;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .init();
    dotenvy::dotenv().ok();

    let cfg = AlpacaConfig::from_env()?;
    let thresholds = FilterThresholds::default();
    let symbols: Vec<String> = WATCH_SYMBOLS.iter().map(|s| s.to_string()).collect();

    info!(?symbols, "seeding prior close / avg daily volume from alpaca rest");
    let seeds = fetch_daily_seeds(&cfg, &symbols, DAILY_LOOKBACK).await?;

    let momentum_weights = MomentumWeights::default();
    let mut trackers: HashMap<String, SessionTracker> = HashMap::new();
    let mut momentum_windows: HashMap<String, RollingWindow> = HashMap::new();
    let mut ignition_monitors: HashMap<String, IgnitionMonitor> = HashMap::new();
    for symbol in &symbols {
        match seeds.get(symbol) {
            Some(seed) => {
                info!(
                    symbol,
                    prior_close = seed.prior_close,
                    avg_daily_volume = seed.avg_daily_volume,
                    "seeded"
                );
                // float_shares: None — Alpaca has no float endpoint; see
                // SessionTracker's doc comment. Stage 1 fails closed on
                // this until a float source is wired in.
                trackers.insert(
                    symbol.clone(),
                    SessionTracker::new(symbol.clone(), seed.prior_close, seed.avg_daily_volume, None),
                );
                momentum_windows.insert(symbol.clone(), RollingWindow::new(MOMENTUM_WINDOW));
                ignition_monitors.insert(symbol.clone(), IgnitionMonitor::new(MonitorConfig::default()));
            }
            None => warn!(symbol, "no seed data; bars for this symbol will be skipped"),
        }
    }

    info!(ws = %cfg.market_ws, "connecting to alpaca realtime stream");
    let mut stream = AlpacaStream::connect(&cfg, &symbols).await?;
    info!(idle_timeout = ?IDLE_TIMEOUT, "connected + subscribed, waiting for bars");

    let mut bars_seen = 0u32;
    let mut trades_seen = 0u32;
    let mut quotes_seen = 0u32;
    loop {
        let batch = match tokio::time::timeout(IDLE_TIMEOUT, stream.next_batch()).await {
            Ok(Ok(Some(batch))) => batch,
            Ok(Ok(None)) => {
                info!("alpaca closed the stream");
                break;
            }
            Ok(Err(e)) => {
                warn!(error = %e, "stream error");
                break;
            }
            Err(_) => {
                info!(
                    bars_seen,
                    "idle timeout with no new bars — expected outside market hours, exiting cleanly"
                );
                break;
            }
        };

        for msg in batch {
            match msg {
                AlpacaMessage::Bar(bar) => {
                    bars_seen += 1;
                    let Some(tracker) = trackers.get_mut(&bar.symbol) else {
                        continue;
                    };
                    let snapshot = tracker.on_bar(&bar);
                    let verdict = explain(&snapshot, &thresholds);
                    info!(
                        symbol = %bar.symbol,
                        price = snapshot.price,
                        gap_pct = format!("{:.2}", snapshot.gap_pct),
                        session_volume = snapshot.session_volume,
                        rel_vol_ok = verdict.rel_vol_ok,
                        gap_ok = verdict.gap_ok,
                        float_ok = verdict.float_ok,
                        passed = verdict.passed(),
                        "bar processed through fast funnel"
                    );

                    if let Some(window) = momentum_windows.get_mut(&bar.symbol) {
                        window.push(Candle {
                            open: bar.open,
                            high: bar.high,
                            low: bar.low,
                            close: bar.close,
                            volume: bar.volume,
                        });
                        let momentum = momentum_scorer::score(window.as_slice(), &momentum_weights);
                        info!(
                            symbol = %bar.symbol,
                            candles_buffered = window.len(),
                            volume_confirmation = format!("{:.2}", momentum.volume_confirmation),
                            structure = format!("{:.2}", momentum.structure),
                            ma_slope = format!("{:.2}", momentum.ma_slope),
                            wick_rejection = format!("{:.2}", momentum.wick_rejection),
                            overall = format!("{:.2}", momentum.overall),
                            qualifies = momentum.qualifies(DEFAULT_QUALIFY_THRESHOLD),
                            "bar processed through momentum scorer"
                        );
                    }
                }
                AlpacaMessage::Trade(trade) => {
                    trades_seen += 1;
                    let Some(monitor) = ignition_monitors.get_mut(&trade.symbol) else {
                        continue;
                    };
                    let event = monitor.on_trade(ignition_detector::Trade {
                        timestamp_secs: to_secs(trade.timestamp),
                        price: trade.price,
                        size: trade.size,
                    });
                    match event {
                        MonitorEvent::None => {}
                        MonitorEvent::CandidateOpened(signals) => {
                            info!(
                                symbol = %trade.symbol,
                                price = trade.price,
                                trade_frequency_ratio = ?signals.trade_frequency_ratio,
                                spread_ratio = ?signals.spread_ratio,
                                ask_absorbed = ?signals.ask_absorbed,
                                "ignition candidate opened, awaiting follow-through"
                            );
                        }
                        MonitorEvent::FollowThroughResolved(result) => {
                            info!(
                                symbol = %trade.symbol,
                                held_above_breakout = result.held_above_breakout,
                                dips_bought = result.dips_bought,
                                confirmed = result.confirmed,
                                "ignition follow-through resolved"
                            );
                        }
                    }
                }
                AlpacaMessage::Status(status) => {
                    if let Some(monitor) = ignition_monitors.get_mut(&status.symbol) {
                        match monitor.on_status(&status.status_code) {
                            StatusTransition::Unchanged => {}
                            StatusTransition::Halted => {
                                info!(symbol = %status.symbol, status_code = %status.status_code, "trading halted");
                            }
                            StatusTransition::Resumed => {
                                info!(symbol = %status.symbol, "halt lifted, awaiting first post-halt trade");
                            }
                        }
                    }
                }
                AlpacaMessage::Quote(quote) => {
                    quotes_seen += 1;
                    if let Some(monitor) = ignition_monitors.get_mut(&quote.symbol) {
                        monitor.on_quote(ignition_detector::Quote {
                            timestamp_secs: to_secs(quote.timestamp),
                            bid_price: quote.bid_price,
                            bid_size: quote.bid_size,
                            ask_price: quote.ask_price,
                            ask_size: quote.ask_size,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    info!(bars_seen, trades_seen, quotes_seen, "scan run finished");
    Ok(())
}
