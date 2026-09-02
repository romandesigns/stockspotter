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
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};
use consolidation_breakout::{ConsolidationBreakoutConfig, ConsolidationBreakoutEvent, ConsolidationBreakoutMonitor};
use fast_funnel::{explain, FilterThresholds};
use halt_detector::{AlertLevel, HaltWarningConfig, HaltWarningMonitor};
use ignition_detector::{IgnitionMonitor, MonitorConfig, MonitorEvent, StatusTransition};
use serde::Serialize;
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::config::AlpacaConfig;
use crate::events::{ConsolidationEventKind, HaltAlertLevel, IgnitionEventKind, ScanEvent};
use crate::movers::SharedTodayMovers;
use crate::qualify::{qualify_shortlist, SymbolQualification};
use crate::rest::{fetch_daily_seeds, DailySeed};
use crate::session::SessionTracker;
use crate::universe::{scan_shortlist, QualifiedSymbol};
use crate::ws::AlpacaStream;
use crate::AlpacaMessage;

/// One symbol's latest catalyst lookup -- same fields
/// `ScanEvent::CatalystUpdate` broadcasts, cached here too (see
/// `run_live_scan`'s own doc comment on why a newly-connecting client
/// needs this, not just the live broadcast).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalystRecord {
    pub symbol: String,
    pub timestamp: DateTime<Utc>,
    pub catalyst_tags: Vec<String>,
    pub headline_count: u32,
    pub most_recent_headline: Option<String>,
}

pub type SharedCatalysts = Arc<RwLock<HashMap<String, CatalystRecord>>>;

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
/// Where the Python qualitative layer (`python/app/main.py`) is expected
/// to be running — overridable via env var since where this runs is a
/// deployment decision, not something to hardcode past local dev.
const DEFAULT_QUALIFY_SERVICE_URL: &str = "http://localhost:8000";
/// How many *consecutive* rescans a tracked symbol can fail to
/// re-qualify for before it's actually dropped — found live 2026-08-31
/// (regular-hours open): AUID/MOVE/MOBX flapped in and out of the
/// watchlist every 12-30s, sitting right at the Stage 2 rel-vol/gap
/// boundary. Every drop wiped that symbol's real accumulated state
/// (ignition history, halt reference price, momentum window) and every
/// re-add wasted a real FMP float call + Python catalyst call for a
/// symbol just looked up minutes earlier — directly undermining the
/// self-driving watchlist's own point (preserving state for symbols
/// that are "still qualifying"). Same fix as
/// `consolidation_breakout::ConsolidationThresholds::max_consecutive_invalid`:
/// tolerate a couple of misses before giving up, don't reset on the
/// first one.
const MAX_CONSECUTIVE_WATCHLIST_MISSES: usize = 2;

/// How often the Top Gainers/Highly Trading leaderboard (`movers.rs`) is
/// re-read to decide which non-funnel-qualified symbols should still get
/// halt-risk monitoring — see this module's own doc comment addition on
/// why halt coverage has a second, independent trigger now, separate
/// from Stage 1/2 qualification (a stock like a real +200% mover with a
/// float just over the funnel's 20M ceiling gets zero halt coverage
/// otherwise, despite being exactly the kind of stock most likely to
/// threaten a real LULD halt). Matches `today_movers`'s own real refresh
/// cadence (`market_data::movers::MOVERS_RESCAN_INTERVAL`) — reading it
/// more often than the underlying data actually changes would just be
/// re-processing the same stale snapshot.
const HALT_WATCH_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// How often a still-forming candle's live update actually gets
/// broadcast, independent of how often trades arrive — a liquid symbol
/// can trade many times a second, and broadcasting every single one would
/// flood the channel and every client's chart re-render for no visible
/// benefit at that resolution. 500ms keeps the candle visibly "growing"
/// in real time without that flood.
const LIVE_BAR_BROADCAST_INTERVAL: Duration = Duration::from_millis(500);

/// Running OHLCV for one symbol's CURRENT, still-forming minute — built
/// from raw trade ticks between Alpaca's own once-per-minute `Bar`
/// messages (see the Trade handler in `run_live_scan`). This is a
/// best-effort live preview: Alpaca's own official `Bar` for the same
/// minute, once it actually closes, is still sent separately and
/// authoritatively corrects/replaces whatever this produced (clients
/// merge `ScanEvent::BarUpdate` by its own `timestamp`, so the later,
/// official message simply overwrites the live estimate) — this struct
/// never needs to be "right", just close enough to look continuous.
struct LiveBar {
    bucket_start: DateTime<Utc>,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: u64,
    last_broadcast: Instant,
}

/// Floors a real timestamp down to the start of its minute — the same
/// bucket boundary Alpaca's own bar `t` field represents, so a live
/// update and the eventual official bar for the same minute land on the
/// identical `timestamp` and merge into one chart candle client-side
/// rather than appearing as two.
fn floor_to_minute(t: DateTime<Utc>) -> DateTime<Utc> {
    let secs = t.timestamp();
    let floored = secs - secs.rem_euclid(60);
    DateTime::from_timestamp(floored, 0).unwrap_or(t)
}

/// Pure diff between what's currently tracked and a fresh rescan result,
/// applying the miss-tolerance rule above. Split out from the rescan
/// branch below purely so it's unit-testable without spinning up the
/// whole async loop — same shape as
/// `consolidation_breakout::step_consolidation`. `misses` is mutated in
/// place (cleared on reappearance, incremented on a miss, cleared again
/// once a symbol is actually dropped) and the caller owns
/// unsubscribing/untracking whatever comes back in the returned list.
fn diff_watchlist(
    currently_tracked: &[String],
    new_shortlist: &[QualifiedSymbol],
    misses: &mut HashMap<String, usize>,
    max_misses: usize,
) -> Vec<String> {
    let new_set: HashSet<&str> = new_shortlist.iter().map(|q| q.symbol.as_str()).collect();
    let mut dropped: Vec<String> = Vec::new();
    for symbol in currently_tracked {
        if new_set.contains(symbol.as_str()) {
            misses.remove(symbol);
            continue;
        }
        let strikes = misses.get(symbol).copied().unwrap_or(0) + 1;
        if strikes > max_misses {
            misses.remove(symbol);
            dropped.push(symbol.clone());
        } else {
            misses.insert(symbol.clone(), strikes);
        }
    }
    dropped
}

/// Halt-risk monitoring now has TWO independent reasons a symbol can
/// need it: full funnel qualification (`trackers`), or just being a top
/// mover (`mover_tracked`, see `HALT_WATCH_REFRESH_INTERVAL`'s own doc
/// comment). Filters `symbols` (either a drop-list or an add-list from
/// ONE of those two sources) down to just the ones the OTHER source
/// doesn't already account for — a dropped symbol only actually loses
/// halt coverage when neither source wants it anymore, and an added
/// symbol only actually needs a fresh `HaltWarningMonitor` + subscribe
/// when neither source already covers it. Pure and shared by all four
/// call sites (the funnel's own drop/add handling, and the new
/// movers-tick branch's drop/add handling) so this one rule can't drift
/// between them.
fn not_covered_by_other_source(symbols: &[String], other_source_has: impl Fn(&str) -> bool) -> Vec<String> {
    symbols.iter().filter(|s| !other_source_has(s.as_str())).cloned().collect()
}

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
///
/// `catalysts` is written alongside every `ScanEvent::CatalystUpdate`
/// broadcast (see the catalyst_rx branch below) -- confirmed live
/// 2026-09-01: a client that connects *after* a symbol's one-time
/// catalyst lookup already fired (catalyst lookups run once per
/// promotion, not repeatedly like funnel/momentum/halt) received an
/// honestly-empty Catalysts panel forever for that symbol, even though
/// real catalyst data existed server-side the whole time. This cache is
/// what a fresh client backfills from (ws-server's `GET /catalysts/today`)
/// before relying on the live broadcast for anything promoted afterward.
pub async fn run_live_scan(
    cfg: &AlpacaConfig,
    initial_symbols: &[String],
    events: broadcast::Sender<ScanEvent>,
    catalysts: SharedCatalysts,
    movers: SharedTodayMovers,
) -> Result<()> {
    let thresholds = FilterThresholds::default();

    let momentum_weights = momentum_scorer::MomentumWeights::default();
    let mut trackers: HashMap<String, SessionTracker> = HashMap::new();
    let mut momentum_windows: HashMap<String, momentum_scorer::RollingWindow> = HashMap::new();
    let mut ignition_monitors: HashMap<String, IgnitionMonitor> = HashMap::new();
    let mut halt_monitors: HashMap<String, HaltWarningMonitor> = HashMap::new();
    let mut consolidation_monitors: HashMap<String, ConsolidationBreakoutMonitor> = HashMap::new();
    // Running OHLCV for each symbol's CURRENT, still-forming minute, built
    // from raw trade ticks -- see the Trade handler below and LiveBar's own
    // doc comment for why this exists (confirmed live: without it, a
    // chart's current candle just snaps into existence once a minute
    // instead of growing continuously, a real felt lag against a platform
    // like Robinhood's, not a cosmetic nitpick).
    let mut live_bars: HashMap<String, LiveBar> = HashMap::new();
    // Last logged level per symbol — see the Trade handler below: without
    // this, a real approach to a halt threshold logs on *every trade*
    // (confirmed live 2026-08-31: a single genuine AEHL escalation
    // produced hundreds of near-identical lines within seconds — the
    // reading itself is correct, logging it unconditionally isn't).
    let mut halt_levels: HashMap<String, HaltAlertLevel> = HashMap::new();
    // Consecutive-miss counter per tracked symbol — see
    // MAX_CONSECUTIVE_WATCHLIST_MISSES's doc comment. Absence from this
    // map means zero consecutive misses (either never missed, or just
    // reappeared and had its count cleared).
    let mut watchlist_misses: HashMap<String, usize> = HashMap::new();
    // The second, independent source of halt coverage -- symbols on the
    // Top Gainers/Highly Trading leaderboard, regardless of whether they
    // ever clear Stage 1/2 (see HALT_WATCH_REFRESH_INTERVAL's own doc
    // comment). Deliberately NOT a subset of `trackers` -- a symbol can
    // be in `mover_tracked`, `trackers`, or both, and this file's own
    // `not_covered_by_other_source` helper is what keeps their halt
    // coverage correct regardless of which combination applies.
    let mut mover_tracked: HashSet<String> = HashSet::new();
    let mut mover_misses: HashMap<String, usize> = HashMap::new();

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

    let qualify_url =
        std::env::var("QUALIFY_SERVICE_URL").unwrap_or_else(|_| DEFAULT_QUALIFY_SERVICE_URL.to_string());
    // Bounded, generous — catalyst batches are small (one per rescan's
    // newly-added symbols, rarely more than a handful) and infrequent.
    let (catalyst_tx, mut catalyst_rx) = mpsc::channel::<Vec<SymbolQualification>>(8);

    let mut bars_seen = 0u32;
    let mut trades_seen = 0u32;
    let mut quotes_seen = 0u32;

    // First tick fires immediately (same tokio::time::interval behavior
    // spawn_periodic_rescan already relies on) -- so halt coverage for
    // whatever's already leading Top Gainers/Highly Trading at startup
    // populates within seconds, not after a full minute's wait.
    let mut halt_watch_ticker = tokio::time::interval(HALT_WATCH_REFRESH_INTERVAL);

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
                            // Raw OHLCV, straight from Alpaca's bar -- see
                            // ScanEvent::BarUpdate's doc comment on why
                            // this is separate from FunnelSignal above.
                            let _ = events.send(ScanEvent::BarUpdate {
                                symbol: bar.symbol.clone(),
                                timestamp: bar.timestamp,
                                open: bar.open,
                                high: bar.high,
                                low: bar.low,
                                close: bar.close,
                                volume: bar.volume,
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

                            // Live-updates the current candle from this
                            // trade tick -- see LiveBar's own doc comment.
                            // Gated on `trackers` (the same symbol universe
                            // ScanEvent::BarUpdate's official broadcast
                            // already uses below) rather than
                            // ignition_monitors specifically, since this
                            // should apply to every tracked symbol
                            // regardless of which other monitors it has.
                            if trackers.contains_key(&trade.symbol) {
                                let bucket_start = floor_to_minute(trade.timestamp);
                                let state = live_bars.entry(trade.symbol.clone()).or_insert_with(|| LiveBar {
                                    bucket_start,
                                    open: trade.price,
                                    high: trade.price,
                                    low: trade.price,
                                    close: trade.price,
                                    volume: 0,
                                    // Backdated so the very first trade of a
                                    // newly-tracked symbol broadcasts
                                    // immediately instead of waiting out a
                                    // full throttle interval first.
                                    last_broadcast: Instant::now() - LIVE_BAR_BROADCAST_INTERVAL,
                                });
                                if state.bucket_start != bucket_start {
                                    // A new minute started -- Alpaca's own
                                    // official Bar for the just-finished
                                    // minute arrives separately (handled
                                    // above) and is authoritative; this
                                    // just starts tracking the new one live.
                                    *state = LiveBar {
                                        bucket_start,
                                        open: trade.price,
                                        high: trade.price,
                                        low: trade.price,
                                        close: trade.price,
                                        volume: 0,
                                        last_broadcast: state.last_broadcast,
                                    };
                                }
                                state.high = state.high.max(trade.price);
                                state.low = state.low.min(trade.price);
                                state.close = trade.price;
                                state.volume += trade.size;

                                if state.last_broadcast.elapsed() >= LIVE_BAR_BROADCAST_INTERVAL {
                                    state.last_broadcast = Instant::now();
                                    let _ = events.send(ScanEvent::BarUpdate {
                                        symbol: trade.symbol.clone(),
                                        timestamp: state.bucket_start,
                                        open: state.open,
                                        high: state.high,
                                        low: state.low,
                                        close: state.close,
                                        volume: state.volume,
                                    });
                                }
                            }

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
                                // Edge-triggered on the *level itself
                                // changing* — not "is this trade
                                // Amber/Red", which still fires on every
                                // single trade for as long as a stock
                                // hovers near its band (confirmed live:
                                // hundreds of lines/second during a real
                                // AEHL approach). A real level change
                                // (escalating OR de-escalating) is
                                // exactly the "something happened" moment
                                // worth a line.
                                let previous = halt_levels.insert(trade.symbol.clone(), level);
                                if previous != Some(level) {
                                    info!(
                                        symbol = %trade.symbol,
                                        ?level,
                                        current_price = reading.current_price,
                                        reference_price = reading.reference_price,
                                        proximity_ratio = format!("{:.2}", reading.proximity_ratio),
                                        relative_volume = ?reading.relative_volume,
                                        "halt-warning level changed"
                                    );
                                }
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
                        // Missing-this-scan doesn't mean drop-this-scan —
                        // tolerate a few consecutive misses first (see
                        // MAX_CONSECUTIVE_WATCHLIST_MISSES's doc comment).
                        // A symbol that's genuinely gone is still dropped
                        // within a few cycles (45s at the current 15s
                        // interval); one that was just flickering at its
                        // qualification boundary keeps its accumulated
                        // state through the flicker instead of losing it
                        // every 12-30s.
                        let currently_tracked: Vec<String> = trackers.keys().cloned().collect();
                        let dropped = diff_watchlist(&currently_tracked, &new_shortlist, &mut watchlist_misses, MAX_CONSECUTIVE_WATCHLIST_MISSES);
                        let added: Vec<QualifiedSymbol> =
                            new_shortlist.into_iter().filter(|q| !trackers.contains_key(q.symbol.as_str())).collect();

                        if !dropped.is_empty() {
                            info!(?dropped, "universe rescan: no longer qualifies after tolerance exceeded, dropping");
                            for symbol in &dropped {
                                untrack_symbol(symbol, &mut trackers, &mut momentum_windows, &mut ignition_monitors, &mut halt_monitors, &mut consolidation_monitors, &mut halt_levels, &mut live_bars, &mover_tracked);
                            }
                            // Keeps the Catalysts cache scoped to symbols
                            // actually still on the watchlist -- without
                            // this a dropped symbol's stale catalyst tags
                            // would linger in a newly-connecting client's
                            // backfill forever (nothing else ever clears
                            // this map). Unconditional on mover_tracked --
                            // catalysts are a funnel-only concept, a
                            // symbol only kept alive by the movers side
                            // never had one to begin with.
                            {
                                let mut c = catalysts.write().await;
                                for symbol in &dropped {
                                    c.remove(symbol);
                                }
                            }
                            // Only actually unsubscribe symbols the movers
                            // leaderboard doesn't still want -- see
                            // not_covered_by_other_source's doc comment.
                            let needs_unsubscribe = not_covered_by_other_source(&dropped, |s| mover_tracked.contains(s));
                            if !needs_unsubscribe.is_empty() {
                                if let Err(e) = stream.unsubscribe(&needs_unsubscribe).await {
                                    warn!(error = %e, "failed to unsubscribe dropped symbols");
                                }
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
                                    // Only actually subscribe symbols not
                                    // already subscribed via the movers
                                    // side -- see
                                    // not_covered_by_other_source's doc
                                    // comment. Catalyst lookup still runs
                                    // for every actually_added symbol
                                    // regardless (funnel-only concept,
                                    // unrelated to WS subscription state).
                                    let needs_subscribe = not_covered_by_other_source(&actually_added, |s| mover_tracked.contains(s));
                                    if !needs_subscribe.is_empty() {
                                        if let Err(e) = stream.subscribe(&needs_subscribe).await {
                                            warn!(error = %e, "failed to subscribe newly promoted symbols");
                                        }
                                    }
                                    if !actually_added.is_empty() {
                                        spawn_catalyst_lookup(qualify_url.clone(), actually_added, catalyst_tx.clone());
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

            Some(results) = catalyst_rx.recv() => {
                for q in results {
                    if let Some(err) = &q.error {
                        warn!(symbol = %q.symbol, error = %err, "catalyst lookup failed for this symbol");
                        continue;
                    }
                    info!(
                        symbol = %q.symbol,
                        catalyst_tags = ?q.catalyst_tags,
                        headline_count = q.headline_count,
                        "catalyst tags"
                    );
                    let record = CatalystRecord {
                        symbol: q.symbol.clone(),
                        timestamp: Utc::now(),
                        catalyst_tags: q.catalyst_tags,
                        headline_count: q.headline_count,
                        most_recent_headline: q.most_recent_headline,
                    };
                    catalysts.write().await.insert(record.symbol.clone(), record.clone());
                    let _ = events.send(ScanEvent::CatalystUpdate {
                        symbol: record.symbol,
                        timestamp: record.timestamp,
                        catalyst_tags: record.catalyst_tags,
                        headline_count: record.headline_count,
                        most_recent_headline: record.most_recent_headline,
                    });
                }
            }

            // Second, independent source of halt coverage -- see
            // HALT_WATCH_REFRESH_INTERVAL's own doc comment. Reads
            // whatever movers.rs's own background scan most recently
            // computed rather than running a second universe scan here.
            _ = halt_watch_ticker.tick() => {
                let wanted: Vec<QualifiedSymbol> = {
                    let today = movers.read().await;
                    today
                        .gainers
                        .iter()
                        .chain(today.most_active.iter())
                        .map(|m| m.symbol.clone())
                        .collect::<HashSet<String>>()
                        .into_iter()
                        .map(|symbol| QualifiedSymbol { symbol, float_shares: None })
                        .collect()
                };

                let currently_mover_tracked: Vec<String> = mover_tracked.iter().cloned().collect();
                let dropped = diff_watchlist(&currently_mover_tracked, &wanted, &mut mover_misses, MAX_CONSECUTIVE_WATCHLIST_MISSES);
                let wanted_set: HashSet<String> = wanted.iter().map(|q| q.symbol.clone()).collect();
                let added: Vec<String> = wanted_set.iter().filter(|s| !mover_tracked.contains(s.as_str())).cloned().collect();

                if !dropped.is_empty() {
                    for symbol in &dropped {
                        mover_tracked.remove(symbol);
                    }
                    // Only actually tear down halt coverage / unsubscribe
                    // for a symbol the funnel isn't ALSO tracking -- see
                    // not_covered_by_other_source's doc comment. The
                    // funnel takes precedence: if it still wants this
                    // symbol, it already owns full coverage (including
                    // halt) via track_symbol, untouched here.
                    let needs_removal = not_covered_by_other_source(&dropped, |s| trackers.contains_key(s));
                    if !needs_removal.is_empty() {
                        info!(dropped = ?needs_removal, "movers leaderboard: no longer a top mover, dropping halt-only coverage");
                        for symbol in &needs_removal {
                            halt_monitors.remove(symbol);
                            halt_levels.remove(symbol);
                        }
                        if let Err(e) = stream.unsubscribe(&needs_removal).await {
                            warn!(error = %e, "failed to unsubscribe movers-leaderboard halt-watch symbols");
                        }
                    }
                }

                if !added.is_empty() {
                    for symbol in &added {
                        mover_tracked.insert(symbol.clone());
                    }
                    // Only symbols not already funnel-tracked need a NEW
                    // halt monitor + subscription -- the funnel already
                    // gives full coverage (including halt) to anything
                    // it tracks.
                    let needs_new = not_covered_by_other_source(&added, |s| trackers.contains_key(s));
                    if !needs_new.is_empty() {
                        match fetch_daily_seeds(cfg, &needs_new, DAILY_LOOKBACK).await {
                            Ok(seeds) => {
                                let mut actually_added = Vec::new();
                                for symbol in &needs_new {
                                    match seeds.get(symbol) {
                                        Some(seed) => {
                                            halt_monitors.insert(
                                                symbol.clone(),
                                                HaltWarningMonitor::new(HaltWarningConfig::default(), seed.avg_daily_volume),
                                            );
                                            actually_added.push(symbol.clone());
                                        }
                                        None => warn!(symbol, "no seed data for movers-leaderboard halt watch; will retry next cycle"),
                                    }
                                }
                                if !actually_added.is_empty() {
                                    info!(added = ?actually_added, "movers leaderboard: added halt-only coverage (not funnel-qualified)");
                                    if let Err(e) = stream.subscribe(&actually_added).await {
                                        warn!(error = %e, "failed to subscribe movers-leaderboard halt-watch symbols");
                                    }
                                }
                            }
                            Err(e) => warn!(error = %e, "failed to fetch seed data for movers-leaderboard halt watch; will retry next cycle"),
                        }
                    }
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

/// Fire-and-forget: looks up news catalyst tags for newly-promoted
/// symbols via the Python qualitative layer and sends the result back
/// over `tx`. A one-shot task, not a loop like `spawn_periodic_rescan` —
/// catalysts are fetched once per symbol at promotion time, not on a
/// schedule, since they don't change tick-by-tick the way price does.
/// Spawned rather than awaited inline specifically so an unreachable or
/// slow qualitative-layer service can never stall live tick processing —
/// a failure here degrades to "no catalyst tags for this symbol", never
/// a hang in the main loop.
fn spawn_catalyst_lookup(qualify_url: String, symbols: Vec<String>, tx: mpsc::Sender<Vec<SymbolQualification>>) {
    tokio::spawn(async move {
        match qualify_shortlist(&qualify_url, &symbols).await {
            Ok(results) => {
                let _ = tx.send(results).await;
            }
            Err(e) => warn!(error = %e, ?symbols, "catalyst lookup unreachable — is the qualitative layer running?"),
        }
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    fn qualified(symbol: &str) -> QualifiedSymbol {
        QualifiedSymbol { symbol: symbol.to_string(), float_shares: None }
    }

    fn tracked(symbols: &[&str]) -> Vec<String> {
        symbols.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_symbol_present_in_the_new_shortlist_is_never_dropped() {
        let mut misses = HashMap::new();
        let dropped = diff_watchlist(&tracked(&["AAPL"]), &[qualified("AAPL")], &mut misses, 2);
        assert!(dropped.is_empty());
        assert!(!misses.contains_key("AAPL"));
    }

    #[test]
    fn a_single_miss_is_tolerated_not_dropped() {
        let mut misses = HashMap::new();
        let dropped = diff_watchlist(&tracked(&["AAPL"]), &[], &mut misses, 2);
        assert!(dropped.is_empty(), "one miss should be tolerated within a limit of 2");
        assert_eq!(misses.get("AAPL"), Some(&1));
    }

    #[test]
    fn reappearing_before_the_limit_clears_the_miss_count() {
        let mut misses = HashMap::new();
        diff_watchlist(&tracked(&["AAPL"]), &[], &mut misses, 2); // miss 1
        let dropped = diff_watchlist(&tracked(&["AAPL"]), &[qualified("AAPL")], &mut misses, 2); // reappears
        assert!(dropped.is_empty());
        assert!(!misses.contains_key("AAPL"), "reappearing should reset the strike count, not just decrement it");
    }

    #[test]
    fn a_symbol_is_dropped_only_after_exceeding_the_consecutive_miss_limit() {
        // Real bug found live 2026-08-31: AUID/MOVE/MOBX flapped in/out of
        // the watchlist every rescan cycle sitting right at the
        // qualification boundary. With a limit of 2, 3 consecutive misses
        // must exceed tolerance and actually drop the symbol.
        let mut misses = HashMap::new();
        assert!(diff_watchlist(&tracked(&["AAPL"]), &[], &mut misses, 2).is_empty()); // miss 1
        assert!(diff_watchlist(&tracked(&["AAPL"]), &[], &mut misses, 2).is_empty()); // miss 2
        let dropped = diff_watchlist(&tracked(&["AAPL"]), &[], &mut misses, 2); // miss 3 - exceeds
        assert_eq!(dropped, vec!["AAPL".to_string()]);
        assert!(!misses.contains_key("AAPL"), "should be cleared out of the miss map once actually dropped");
    }

    #[test]
    fn a_dropped_symbol_is_no_longer_tracked_so_it_cant_be_dropped_again_next_cycle() {
        // Guards against double-counting: once diff_watchlist reports a
        // symbol dropped, the caller removes it from `trackers`, so it
        // won't appear in `currently_tracked` on the next call at all.
        let mut misses = HashMap::new();
        diff_watchlist(&tracked(&["AAPL"]), &[], &mut misses, 2);
        diff_watchlist(&tracked(&["AAPL"]), &[], &mut misses, 2);
        diff_watchlist(&tracked(&["AAPL"]), &[], &mut misses, 2);
        // AAPL no longer in currently_tracked, as the caller would do after a drop.
        let dropped = diff_watchlist(&tracked(&[]), &[], &mut misses, 2);
        assert!(dropped.is_empty());
    }

    #[test]
    fn unrelated_symbols_track_their_own_independent_miss_counts() {
        let mut misses = HashMap::new();
        // AAPL misses twice, MSFT stays present throughout.
        diff_watchlist(&tracked(&["AAPL", "MSFT"]), &[qualified("MSFT")], &mut misses, 2);
        diff_watchlist(&tracked(&["AAPL", "MSFT"]), &[qualified("MSFT")], &mut misses, 2);
        assert_eq!(misses.get("AAPL"), Some(&2));
        assert!(!misses.contains_key("MSFT"));

        let dropped = diff_watchlist(&tracked(&["AAPL", "MSFT"]), &[qualified("MSFT")], &mut misses, 2);
        assert_eq!(dropped, vec!["AAPL".to_string()]);
        assert!(!misses.contains_key("MSFT"));
    }

    #[test]
    fn not_covered_by_other_source_keeps_only_symbols_the_other_side_doesnt_have() {
        // Real scenario this guards: FAMI is dropped from the movers
        // leaderboard, but the funnel is ALSO tracking it -- it must NOT
        // lose halt coverage or get unsubscribed.
        let dropped = tracked(&["FAMI", "GELS"]);
        let funnel_has: HashSet<&str> = ["FAMI"].into_iter().collect();
        let needs_removal = not_covered_by_other_source(&dropped, |s| funnel_has.contains(s));
        assert_eq!(needs_removal, vec!["GELS".to_string()], "FAMI is still funnel-tracked, GELS isn't -- only GELS should actually lose coverage");
    }

    #[test]
    fn not_covered_by_other_source_returns_everything_when_the_other_side_has_none() {
        let symbols = tracked(&["A", "B", "C"]);
        let result = not_covered_by_other_source(&symbols, |_| false);
        assert_eq!(result, symbols);
    }

    #[test]
    fn not_covered_by_other_source_returns_nothing_when_the_other_side_has_all() {
        let symbols = tracked(&["A", "B", "C"]);
        let result = not_covered_by_other_source(&symbols, |_| true);
        assert!(result.is_empty());
    }
}

#[allow(clippy::too_many_arguments)]
fn untrack_symbol(
    symbol: &str,
    trackers: &mut HashMap<String, SessionTracker>,
    momentum_windows: &mut HashMap<String, momentum_scorer::RollingWindow>,
    ignition_monitors: &mut HashMap<String, IgnitionMonitor>,
    halt_monitors: &mut HashMap<String, HaltWarningMonitor>,
    consolidation_monitors: &mut HashMap<String, ConsolidationBreakoutMonitor>,
    halt_levels: &mut HashMap<String, HaltAlertLevel>,
    live_bars: &mut HashMap<String, LiveBar>,
    mover_tracked: &HashSet<String>,
) {
    trackers.remove(symbol);
    momentum_windows.remove(symbol);
    ignition_monitors.remove(symbol);
    consolidation_monitors.remove(symbol);
    live_bars.remove(symbol);
    // Halt coverage stays alive if the movers-leaderboard side still
    // wants this symbol -- see not_covered_by_other_source's doc comment.
    // Funnel/momentum/ignition/consolidation/chart-bars are all
    // gap-and-go-strategy-specific and correctly go away regardless
    // (mover_tracked never grants those, only halt).
    if !mover_tracked.contains(symbol) {
        halt_monitors.remove(symbol);
        halt_levels.remove(symbol);
    }
}
