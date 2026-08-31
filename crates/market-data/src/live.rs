//! The live per-symbol scan loop — fast funnel + momentum scorer +
//! ignition detector, all fed from one Alpaca WS connection. Extracted
//! from `bin/scan.rs` (which now just calls this) so `ws-server` can
//! reuse the identical loop and additionally broadcast every event to
//! connected clients, rather than only logging it.
//!
//! Every `ScanEvent` this emits goes out on `events` *and* through
//! `tracing`, in that order, at the same points — `bin/scan.rs`'s
//! already-verified log output is unchanged by this refactor.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use consolidation_breakout::{ConsolidationBreakoutConfig, ConsolidationBreakoutEvent, ConsolidationBreakoutMonitor};
use fast_funnel::{explain, FilterThresholds};
use halt_detector::{AlertLevel, HaltWarningConfig, HaltWarningMonitor};
use ignition_detector::{IgnitionMonitor, MonitorConfig, MonitorEvent, StatusTransition};
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::config::AlpacaConfig;
use crate::events::{ConsolidationEventKind, HaltAlertLevel, IgnitionEventKind, ScanEvent};
use crate::rest::fetch_daily_seeds;
use crate::session::SessionTracker;
use crate::ws::AlpacaStream;
use crate::AlpacaMessage;

const IDLE_TIMEOUT: Duration = Duration::from_secs(20);
const DAILY_LOOKBACK: u32 = 20;
// 20-period MA needs 21 candles minimum; keep a little headroom above that.
const MOMENTUM_WINDOW: usize = 30;

fn to_secs(t: DateTime<Utc>) -> f64 {
    t.timestamp() as f64 + t.timestamp_subsec_nanos() as f64 / 1_000_000_000.0
}

/// Runs until Alpaca closes the stream, a stream error occurs, or
/// `IDLE_TIMEOUT` passes with no new messages (expected outside market
/// hours) — same exit conditions `bin/scan.rs` always had. A dropped
/// `events` receiver (e.g. `bin/scan.rs`'s own demo run, which doesn't
/// keep one) isn't an error — `broadcast::Sender::send` just reports
/// nobody was listening for that particular message and this keeps going.
pub async fn run_live_scan(
    cfg: &AlpacaConfig,
    symbols: &[String],
    events: broadcast::Sender<ScanEvent>,
) -> Result<()> {
    let thresholds = FilterThresholds::default();

    info!(?symbols, "seeding prior close / avg daily volume from alpaca rest");
    let seeds = fetch_daily_seeds(cfg, symbols, DAILY_LOOKBACK).await?;

    let momentum_weights = momentum_scorer::MomentumWeights::default();
    let mut trackers: HashMap<String, SessionTracker> = HashMap::new();
    let mut momentum_windows: HashMap<String, momentum_scorer::RollingWindow> = HashMap::new();
    let mut ignition_monitors: HashMap<String, IgnitionMonitor> = HashMap::new();
    // Both independent of every tracker/monitor above, per each crate's
    // own isolation guarantee — halt_detector only needs avg_daily_volume
    // (already fetched for the funnel's seed anyway), consolidation
    // watches raw candles it derives its own surge detection from.
    let mut halt_monitors: HashMap<String, HaltWarningMonitor> = HashMap::new();
    let mut consolidation_monitors: HashMap<String, ConsolidationBreakoutMonitor> = HashMap::new();
    for symbol in symbols {
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
                momentum_windows.insert(symbol.clone(), momentum_scorer::RollingWindow::new(MOMENTUM_WINDOW));
                ignition_monitors.insert(symbol.clone(), IgnitionMonitor::new(MonitorConfig::default()));
                halt_monitors.insert(
                    symbol.clone(),
                    HaltWarningMonitor::new(HaltWarningConfig::default(), seed.avg_daily_volume),
                );
                consolidation_monitors.insert(
                    symbol.clone(),
                    ConsolidationBreakoutMonitor::new(ConsolidationBreakoutConfig::default()),
                );
            }
            None => warn!(symbol, "no seed data; bars for this symbol will be skipped"),
        }
    }

    info!(ws = %cfg.market_ws, "connecting to alpaca realtime stream");
    let mut stream = AlpacaStream::connect(cfg, symbols).await?;
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
                    let _ = events.send(ScanEvent::FunnelSignal {
                        symbol: bar.symbol.clone(),
                        timestamp: bar.timestamp,
                        price: snapshot.price,
                        gap_pct: snapshot.gap_pct,
                        session_volume: snapshot.session_volume,
                        price_ok: verdict.price_ok,
                        float_ok: verdict.float_ok,
                        rel_vol_ok: verdict.rel_vol_ok,
                        gap_ok: verdict.gap_ok,
                        passed: verdict.passed(),
                    });

                    if let Some(window) = momentum_windows.get_mut(&bar.symbol) {
                        window.push(momentum_scorer::Candle {
                            open: bar.open,
                            high: bar.high,
                            low: bar.low,
                            close: bar.close,
                            volume: bar.volume,
                        });
                        let momentum = momentum_scorer::score(window.as_slice(), &momentum_weights);
                        let qualifies = momentum.qualifies(momentum_scorer::DEFAULT_QUALIFY_THRESHOLD);
                        info!(
                            symbol = %bar.symbol,
                            candles_buffered = window.len(),
                            volume_confirmation = format!("{:.2}", momentum.volume_confirmation),
                            structure = format!("{:.2}", momentum.structure),
                            ma_slope = format!("{:.2}", momentum.ma_slope),
                            wick_rejection = format!("{:.2}", momentum.wick_rejection),
                            overall = format!("{:.2}", momentum.overall),
                            qualifies,
                            "bar processed through momentum scorer"
                        );
                        let _ = events.send(ScanEvent::MomentumUpdate {
                            symbol: bar.symbol.clone(),
                            timestamp: bar.timestamp,
                            volume_confirmation: momentum.volume_confirmation,
                            structure: momentum.structure,
                            ma_slope: momentum.ma_slope,
                            wick_rejection: momentum.wick_rejection,
                            overall: momentum.overall,
                            qualifies,
                        });
                    }

                    if let Some(monitor) = consolidation_monitors.get_mut(&bar.symbol) {
                        let candle = consolidation_breakout::Candle {
                            open: bar.open,
                            high: bar.high,
                            low: bar.low,
                            close: bar.close,
                            volume: bar.volume,
                        };
                        let kind = match monitor.on_candle(candle) {
                            ConsolidationBreakoutEvent::None => None,
                            ConsolidationBreakoutEvent::SurgeDetected { .. } => Some(ConsolidationEventKind::SurgeDetected),
                            ConsolidationBreakoutEvent::ConsolidationConfirmed { .. } => {
                                Some(ConsolidationEventKind::ConsolidationConfirmed)
                            }
                            ConsolidationBreakoutEvent::EntryTriggered { .. } => Some(ConsolidationEventKind::EntryTriggered),
                        };
                        if let Some(kind) = kind {
                            info!(symbol = %bar.symbol, ?kind, price = bar.close, "consolidation-breakout event");
                            let _ = events.send(ScanEvent::ConsolidationEvent {
                                symbol: bar.symbol.clone(),
                                timestamp: bar.timestamp,
                                price: bar.close,
                                kind,
                            });
                        }
                    }
                }
                AlpacaMessage::Trade(trade) => {
                    trades_seen += 1;

                    if let Some(monitor) = halt_monitors.get_mut(&trade.symbol) {
                        let reading = monitor.on_trade(
                            halt_detector::Trade {
                                timestamp_secs: to_secs(trade.timestamp),
                                price: trade.price,
                                size: trade.size,
                            },
                            trade.timestamp,
                        );
                        let level = match reading.level {
                            AlertLevel::Calm => HaltAlertLevel::Calm,
                            AlertLevel::Amber => HaltAlertLevel::Amber,
                            AlertLevel::Red => HaltAlertLevel::Red,
                        };
                        let _ = events.send(ScanEvent::HaltWarning {
                            symbol: trade.symbol.clone(),
                            timestamp: trade.timestamp,
                            reference_price: reading.reference_price,
                            current_price: reading.current_price,
                            band_width_dollars: reading.band_width_dollars,
                            band_doubled: reading.band_doubled,
                            proximity_ratio: reading.proximity_ratio,
                            relative_volume: reading.relative_volume,
                            level,
                        });
                    }

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
                            let _ = events.send(ScanEvent::IgnitionEvent {
                                symbol: trade.symbol.clone(),
                                timestamp: trade.timestamp,
                                price: trade.price,
                                kind: IgnitionEventKind::CandidateOpened,
                            });
                        }
                        MonitorEvent::FollowThroughResolved(result) => {
                            info!(
                                symbol = %trade.symbol,
                                held_above_breakout = result.held_above_breakout,
                                dips_bought = result.dips_bought,
                                confirmed = result.confirmed,
                                "ignition follow-through resolved"
                            );
                            let kind = if result.confirmed {
                                IgnitionEventKind::FollowThroughConfirmed
                            } else {
                                IgnitionEventKind::FollowThroughRejected
                            };
                            let _ = events.send(ScanEvent::IgnitionEvent {
                                symbol: trade.symbol.clone(),
                                timestamp: trade.timestamp,
                                price: trade.price,
                                kind,
                            });
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
