//! The live per-symbol scan loop — fast funnel + momentum scorer +
//! ignition detector + halt-detector + consolidation-breakout, all fed
//! from one Alpaca WS connection. Extracted from `bin/scan.rs` (which
//! now just calls this) so `ws-server` can reuse the identical loop and
//! additionally broadcast every event to connected clients, rather than
//! only logging it.
//!
//! **Two loops, not one** — this is the actual answer to "how does the
//! app stay aware of the whole market, not just a fixed list": a
//! WebSocket only streams symbols it's told to subscribe to, so
//! `scan_shortlist` (the wide, cheap Stage 1/2 REST scan across the
//! *entire* tradable universe — measured ~3s for ~13,378 symbols) runs
//! on its own schedule in the background (`spawn_periodic_rescan`), and
//! every time it produces a fresh shortlist this loop diffs it against
//! what's currently tracked: newly-qualifying symbols get seeded and
//! subscribed, symbols that stopped qualifying get dropped and
//! unsubscribed, mid-stream (`AlpacaStream::subscribe`/`unsubscribe`) —
//! no reconnect, and no loss of accumulated per-symbol state (rolling
//! windows, halt reference prices, ignition history) for symbols that
//! are still qualifying. `ws-server` no longer needs a hardcoded
//! watchlist at all; `initial_symbols` below is just an optional
//! fast-start seed, not the source of truth.
//!
//! Every `ScanEvent` this emits goes out on `events` *and* through
//! `tracing`, in that order, at the same points — `bin/scan.rs`'s
//! already-verified log output is unchanged by this refactor.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use consolidation_breakout::{ConsolidationBreakoutConfig, ConsolidationBreakoutEvent, ConsolidationBreakoutMonitor};
use fast_funnel::{explain, FilterThresholds};
use halt_detector::{AlertLevel, HaltWarningConfig, HaltWarningMonitor};
use ignition_detector::{IgnitionMonitor, MonitorConfig, MonitorEvent, StatusTransition};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::config::AlpacaConfig;
use crate::events::{ConsolidationEventKind, HaltAlertLevel, IgnitionEventKind, ScanEvent};
use crate::rest::{fetch_daily_seeds, DailySeed};
use crate::session::SessionTracker;
use crate::universe::{scan_shortlist, QualifiedSymbol};
use crate::ws::AlpacaStream;
use crate::AlpacaMessage;

/// A true "connection seems dead" safety net, not the primary reconnect
/// trigger it used to be. Long silent stretches are now expected and
/// normal (a quiet overnight period with real symbols subscribed, or the
/// first few seconds before the first universe scan completes) — tearing
/// down and rebuilding all tracked state every 20s during those stretches
/// (the old behavior) fights against the whole point of dynamic
/// tracking, which is to *not* lose accumulated per-symbol state.
const IDLE_TIMEOUT: Duration = Duration::from_secs(600);
const DAILY_LOOKBACK: u32 = 20;
// 20-period MA needs 21 candles minimum; keep a little headroom above that.
const MOMENTUM_WINDOW: usize = 30;
/// How often the wide universe scan re-runs. Measured ~3s per full pass
/// across ~13,378 symbols (2026-08-31) — this interval is chosen for
/// freshness, not because the scan itself is slow.
///
/// The real constraint is FMP float lookups (one call per Stage-2
/// survivor, sequential): confirmed FMP Starter plan = 300 calls/min.
/// Observed survivor counts tonight were 15-25/scan; at 15s that's
/// ~60-100 calls/min — still 3-5x under the 300/min ceiling (would take
/// ~75 survivors in one scan, 3x anything observed, to hit the cap).
/// Alpaca's own snapshot-endpoint rate limit isn't independently
/// verified the same way, but `scan_shortlist` errors already degrade
/// gracefully (a skipped cycle, logged, not a crash — see the rescan
/// branch in `run_live_scan`), so there's low downside to being
/// aggressive here.
const UNIVERSE_RESCAN_INTERVAL: Duration = Duration::from_secs(15);

fn to_secs(t: DateTime<Utc>) -> f64 {
    t.timestamp() as f64 + t.timestamp_subsec_nanos() as f64 / 1_000_000_000.0
}

/// Runs until Alpaca closes the stream, a stream error occurs, or
/// `IDLE_TIMEOUT` passes with no new messages at all (a real dead-
/// connection safety net now, not a normal exit path) — same exit
/// conditions `bin/scan.rs` always had, just a much longer fuse. A
/// dropped `events` receiver (e.g. `bin/scan.rs`'s own demo run, which
/// doesn't keep one) isn't an error — `broadcast::Sender::send` just
/// reports nobody was listening for that particular message and this
/// keeps going.
pub async fn run_live_scan(
    cfg: &AlpacaConfig,
    initial_symbols: &[String],
    events: broadcast::Sender<ScanEvent>,
) -> Result<()> {
    let thresholds = FilterThresholds::default();

    let momentum_weights = momentum_scorer::MomentumWeights::default();
    let mut trackers: HashMap<String, SessionTracker> = HashMap::new();
    let mut momentum_windows: HashMap<String, momentum_scorer::RollingWindow> = HashMap::new();
    let mut ignition_monitors: HashMap<String, IgnitionMonitor> = HashMap::new();
    let mut halt_monitors: HashMap<String, HaltWarningMonitor> = HashMap::new();
    let mut consolidation_monitors: HashMap<String, ConsolidationBreakoutMonitor> = HashMap::new();

    if !initial_symbols.is_empty() {
        info!(symbols = ?initial_symbols, "seeding initial fast-start symbols");
        let seeds = fetch_daily_seeds(cfg, initial_symbols, DAILY_LOOKBACK).await?;
        for symbol in initial_symbols {
            match seeds.get(symbol) {
                Some(seed) => {
                    info!(symbol, prior_close = seed.prior_close, avg_daily_volume = seed.avg_daily_volume, "seeded");
                    track_symbol(
                        symbol,
                        seed,
                        None, // fast-start seed path has no scan result to draw float from — fails closed, see track_symbol's doc comment
                        &mut trackers,
                        &mut momentum_windows,
                        &mut ignition_monitors,
                        &mut halt_monitors,
                        &mut consolidation_monitors,
                    );
                }
                None => warn!(symbol, "no seed data; bars for this symbol will be skipped"),
            }
        }
    }

    info!(ws = %cfg.market_ws, "connecting to alpaca realtime stream");
    let mut stream = AlpacaStream::connect(cfg, initial_symbols).await?;
    info!(idle_timeout = ?IDLE_TIMEOUT, rescan_interval = ?UNIVERSE_RESCAN_INTERVAL, "connected, waiting for bars — universe rescan running in the background");

    let (rescan_tx, mut rescan_rx) = mpsc::channel::<Result<Vec<QualifiedSymbol>>>(1);
    let rescan_handle = spawn_periodic_rescan(cfg.clone(), rescan_tx);

    let mut bars_seen = 0u32;
    let mut trades_seen = 0u32;
    let mut quotes_seen = 0u32;

    loop {
        tokio::select! {
            batch_result = tokio::time::timeout(IDLE_TIMEOUT, stream.next_batch()) => {
                let batch = match batch_result {
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
                        info!(bars_seen, tracked = trackers.len(), "idle timeout with no messages at all — connection likely dead, reconnecting");
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

            rescan = rescan_rx.recv() => {
                match rescan {
                    Some(Ok(new_shortlist)) => {
                        let new_set: HashSet<&str> = new_shortlist.iter().map(|q| q.symbol.as_str()).collect();
                        let dropped: Vec<String> = trackers.keys().filter(|s| !new_set.contains(s.as_str())).cloned().collect();
                        let added: Vec<QualifiedSymbol> =
                            new_shortlist.into_iter().filter(|q| !trackers.contains_key(q.symbol.as_str())).collect();

                        if !dropped.is_empty() {
                            info!(?dropped, "universe rescan: no longer qualifies, dropping");
                            for symbol in &dropped {
                                untrack_symbol(symbol, &mut trackers, &mut momentum_windows, &mut ignition_monitors, &mut halt_monitors, &mut consolidation_monitors);
                            }
                            if let Err(e) = stream.unsubscribe(&dropped).await {
                                warn!(error = %e, "failed to unsubscribe dropped symbols");
                            }
                        }

                        if !added.is_empty() {
                            let added_symbols: Vec<String> = added.iter().map(|q| q.symbol.clone()).collect();
                            info!(added = ?added_symbols, "universe rescan: newly qualifying, adding");
                            match fetch_daily_seeds(cfg, &added_symbols, DAILY_LOOKBACK).await {
                                Ok(seeds) => {
                                    let mut actually_added = Vec::new();
                                    for q in &added {
                                        match seeds.get(&q.symbol) {
                                            Some(seed) => {
                                                track_symbol(
                                                    &q.symbol,
                                                    seed,
                                                    q.float_shares,
                                                    &mut trackers,
                                                    &mut momentum_windows,
                                                    &mut ignition_monitors,
                                                    &mut halt_monitors,
                                                    &mut consolidation_monitors,
                                                );
                                                actually_added.push(q.symbol.clone());
                                            }
                                            None => warn!(symbol = %q.symbol, "no seed data for newly-promoted symbol; will retry next scan"),
                                        }
                                    }
                                    if let Err(e) = stream.subscribe(&actually_added).await {
                                        warn!(error = %e, "failed to subscribe newly promoted symbols");
                                    }
                                }
                                Err(e) => warn!(error = %e, "failed to fetch seed data for newly promoted symbols; will retry next scan"),
                            }
                        }

                        if !dropped.is_empty() || !added.is_empty() {
                            info!(now_tracking = trackers.len(), "universe rescan applied");
                        }
                    }
                    Some(Err(e)) => warn!(error = %e, "universe rescan failed; keeping current watchlist"),
                    None => warn!("universe rescan task ended unexpectedly"),
                }
            }
        }
    }

    rescan_handle.abort();
    info!(bars_seen, trades_seen, quotes_seen, tracked = trackers.len(), "scan run finished");
    Ok(())
}

/// Spawns the background task that re-runs the full universe scan every
/// `UNIVERSE_RESCAN_INTERVAL` and sends each result back over `tx` — kept
/// as a separate task (not inline in the main select loop) specifically
/// so the few seconds the scan itself takes doesn't block live message
/// processing; the main loop only pauses briefly to apply the diff once
/// a result actually arrives. `tokio::time::interval`'s first tick fires
/// immediately, so the real, funnel-driven watchlist populates within
/// seconds of startup rather than waiting a full interval.
fn spawn_periodic_rescan(cfg: AlpacaConfig, tx: mpsc::Sender<Result<Vec<QualifiedSymbol>>>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let thresholds = FilterThresholds::default();
        let mut ticker = tokio::time::interval(UNIVERSE_RESCAN_INTERVAL);
        loop {
            ticker.tick().await;
            let result = scan_shortlist(&cfg, &thresholds).await;
            if tx.send(result).await.is_err() {
                break; // run_live_scan has exited; stop rescanning
            }
        }
    })
}

/// Creates and inserts every per-symbol tracker/monitor this loop needs —
/// shared by the initial seeding pass and the rescan-driven promotion
/// path so there's exactly one place that defines "what does it mean to
/// start tracking a symbol."
///
/// `float_shares` comes from the caller: the rescan path passes through
/// the value `scan_shortlist`'s own Stage 1 already paid an FMP call to
/// confirm (fixed 2026-08-31 — this used to always pass `None` here,
/// re-defaulting Stage 1 closed for every live bar of an already-
/// qualified symbol, which meant the Gap & Go panel's `float_ok` would
/// show `false` forever for every live-tracked symbol despite having
/// passed a real float check moments earlier). The initial fast-start
/// seeding path has no scan result to draw from, so it still passes
/// `None` — fails closed the same way Stage 1 does everywhere else float
/// is unknown, correct for that path specifically.
#[allow(clippy::too_many_arguments)]
fn track_symbol(
    symbol: &str,
    seed: &DailySeed,
    float_shares: Option<u64>,
    trackers: &mut HashMap<String, SessionTracker>,
    momentum_windows: &mut HashMap<String, momentum_scorer::RollingWindow>,
    ignition_monitors: &mut HashMap<String, IgnitionMonitor>,
    halt_monitors: &mut HashMap<String, HaltWarningMonitor>,
    consolidation_monitors: &mut HashMap<String, ConsolidationBreakoutMonitor>,
) {
    trackers.insert(
        symbol.to_string(),
        SessionTracker::new(symbol.to_string(), seed.prior_close, seed.avg_daily_volume, float_shares),
    );
    momentum_windows.insert(symbol.to_string(), momentum_scorer::RollingWindow::new(MOMENTUM_WINDOW));
    ignition_monitors.insert(symbol.to_string(), IgnitionMonitor::new(MonitorConfig::default()));
    halt_monitors.insert(
        symbol.to_string(),
        HaltWarningMonitor::new(HaltWarningConfig::default(), seed.avg_daily_volume),
    );
    consolidation_monitors.insert(symbol.to_string(), ConsolidationBreakoutMonitor::new(ConsolidationBreakoutConfig::default()));
}

fn untrack_symbol(
    symbol: &str,
    trackers: &mut HashMap<String, SessionTracker>,
    momentum_windows: &mut HashMap<String, momentum_scorer::RollingWindow>,
    ignition_monitors: &mut HashMap<String, IgnitionMonitor>,
    halt_monitors: &mut HashMap<String, HaltWarningMonitor>,
    consolidation_monitors: &mut HashMap<String, ConsolidationBreakoutMonitor>,
) {
    trackers.remove(symbol);
    momentum_windows.remove(symbol);
    ignition_monitors.remove(symbol);
    halt_monitors.remove(symbol);
    consolidation_monitors.remove(symbol);
}
