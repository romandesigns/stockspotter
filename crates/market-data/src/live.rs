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
use consolidation_breakout::{
    ConsolidationBreakoutConfig, ConsolidationBreakoutEvent, ConsolidationBreakoutMonitor, ConsolidationThresholds,
};
use fast_funnel::{explain, FilterThresholds};
use halt_detector::{AlertLevel, HaltWarningConfig, HaltWarningMonitor};
use ignition_detector::{IgnitionMonitor, MonitorConfig, MonitorEvent, StatusTransition};
use serde::Serialize;
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::config::AlpacaConfig;
use crate::events::{ConsolidationEventKind, ConsolidationStrategy, HaltAlertLevel, IgnitionEventKind, ScanEvent};
use crate::movers::SharedTodayMovers;
use crate::qualify::{qualify_shortlist, SymbolQualification};
use crate::rest::{fetch_daily_seeds, DailySeed};
use crate::session::SessionTracker;
use crate::universe::{scan_shortlist, FloatCache, QualifiedSymbol, ScanOutcome};
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

/// Keep a quiet stock subscribed while it crosses the gap between quiet-watch
/// and funnel thresholds. The extra 150 transition slots bound subscription use.
const QUIET_TRANSITION_GRACE: Duration = Duration::from_secs(300);
const QUIET_TOTAL_CAP: usize = 300;

fn remember_confirmed(watch:&mut HashMap<String,std::time::Instant>,symbol:&str,now:std::time::Instant) {
    watch.insert(symbol.to_string(),now);
    if watch.len()>150 {
        let oldest=watch.iter().min_by(|a,b|a.1.cmp(b.1).then_with(||a.0.cmp(b.0))).map(|(s,_)|s.clone());
        if let Some(oldest)=oldest {watch.remove(&oldest);}
    }
}

fn quiet_watch_with_grace(selected: &[String], last_selected: &mut HashMap<String, std::time::Instant>, now: std::time::Instant) -> HashSet<String> {
    for symbol in selected { last_selected.insert(symbol.clone(), now); }
    last_selected.retain(|_, at| now.duration_since(*at) < QUIET_TRANSITION_GRACE);
    let selected: HashSet<_> = selected.iter().collect();
    let mut ranked: Vec<_> = last_selected.iter().map(|(s,at)|(s.clone(),*at)).collect();
    ranked.sort_by(|a,b| selected.contains(&b.0).cmp(&selected.contains(&a.0))
        .then_with(||b.1.cmp(&a.1)).then_with(||a.0.cmp(&b.0)));
    ranked.truncate(QUIET_TOTAL_CAP);
    let wanted: HashSet<String> = ranked.into_iter().map(|(s,_)|s).collect();
    last_selected.retain(|s,_| wanted.contains(s));
    wanted
}

/// Env flag turning on literal universe-wide ignition detection —
/// `IGNITION_UNIVERSE_MODE=1`. Off by default.
///
/// **What it changes.** Normally ignition coverage comes from three
/// bounded tiers (funnel qualifiers, movers leaderboard, quiet watch).
/// With this on, the scanner additionally subscribes the ENTIRE market's
/// trade tape (`AlpacaStream::subscribe_all_trades`) and runs an
/// `IgnitionMonitor` for any symbol that trades, which is the
/// architecture doc's literal requirement: ignition watches everything,
/// "since explosive moves can happen on stocks with no prior setup".
///
/// **Why it's opt-in rather than the default.** This is a genuine
/// resource decision, not a threshold to flip. The full US equity trade
/// tape runs to tens of millions of prints a day, and a monitor per
/// active symbol costs real memory (bounded here by
/// `UNIVERSE_MAX_MONITORS` and a reduced per-monitor history). On a
/// small VPS that is a different sizing conversation than the default
/// tiers, so it's a deliberate switch someone throws after deciding the
/// box can take it — not something that silently changes the resource
/// profile of an existing deployment on upgrade.
const UNIVERSE_IGNITION_ENV: &str = "IGNITION_UNIVERSE_MODE";

/// Hard cap on universe-tier ignition monitors held at once. Reached
/// only if that many DISTINCT symbols trade inside the eviction window;
/// the real US equity universe is ~11k names but only a fraction print
/// in any given stretch. Least-recently-traded monitors are evicted
/// first (see `evict_idle_universe_monitors`), which is the right
/// eviction order here: a symbol that hasn't printed in minutes is
/// definitionally not igniting.
const UNIVERSE_MAX_MONITORS: usize = 6_000;

/// How long a universe-tier symbol can go without a trade before its
/// monitor is eligible for eviction. Comfortably longer than the
/// detector's own baseline window (20s), so eviction can never discard
/// history a live detection was about to use.
const UNIVERSE_MONITOR_IDLE_SECS: f64 = 300.0;

/// How often to sweep for idle universe monitors. Sweeping on every
/// trade would be O(n) per print across the whole tape; once a minute
/// is ample for a 5-minute idle threshold.
const UNIVERSE_EVICTION_INTERVAL: Duration = Duration::from_secs(60);

/// Minimum trade count retained in the universe tier. The complete 21-second
/// detection interval is retained at higher rates, so memory scales with tape activity.
const UNIVERSE_MONITOR_MAX_TRADES: usize = 120;

/// `MonitorConfig` for universe-tier symbols: the shipped detector
/// thresholds unchanged, with only the memory bounds reduced. The
/// detection logic is deliberately identical to every other tier — a
/// symbol must not become more or less likely to fire based on which
/// coverage tier happened to pick it up.
fn universe_monitor_config() -> MonitorConfig {
    MonitorConfig {
        max_trades: UNIVERSE_MONITOR_MAX_TRADES,
        max_quotes: 0, // universe tier streams no quotes; see subscribe_all_trades
        ..MonitorConfig::default()
    }
}

/// Drops universe-tier monitors whose last trade is older than
/// `UNIVERSE_MONITOR_IDLE_SECS`, then, if still over `max_monitors`,
/// drops the least-recently-traded until back under the cap.
///
/// Pure and separately testable, same split as `diff_watchlist` and
/// `not_covered_by_other_source` — the eviction policy is the part with
/// real decisions in it, and it shouldn't need a live tape to exercise.
fn evict_idle_universe_monitors(
    monitors: &mut HashMap<String, IgnitionMonitor>,
    last_trade_secs: &mut HashMap<String, f64>,
    now_secs: f64,
    idle_secs: f64,
    max_monitors: usize,
) -> usize {
    let before = monitors.len();

    last_trade_secs.retain(|symbol, last| {
        let keep = now_secs - *last <= idle_secs;
        if !keep {
            monitors.remove(symbol);
        }
        keep
    });

    if monitors.len() > max_monitors {
        let mut by_age: Vec<(String, f64)> = last_trade_secs.iter().map(|(s, t)| (s.clone(), *t)).collect();
        // Oldest first, symbol as a deterministic tiebreak.
        by_age.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.0.cmp(&b.0)));
        let excess = monitors.len() - max_monitors;
        for (symbol, _) in by_age.into_iter().take(excess) {
            monitors.remove(&symbol);
            last_trade_secs.remove(&symbol);
        }
    }

    before - monitors.len()
}

/// **2026-09-03: momentum/ignition/consolidation-breakout(+micropullback)
/// now get the SAME dual-source treatment halt-warning has had since the
/// FAMI case above** — real evidence forced this: YQ (a genuine, huge
/// mover, real halt-adjacent behavior) flapped in/out of Stage 1/2
/// funnel qualification every ~15-45s live, and every re-add reset its
/// momentum window to `candles_buffered=1` — the real detectors never
/// got a fair multi-bar window to evaluate it, so the ONLY thing that
/// ever caught it was halt-warning's own already-decoupled coverage.
/// `halt_watch_ticker`'s existing add/drop diff against the movers
/// leaderboard now also creates/tears down momentum_windows/
/// ignition_monitors/consolidation_monitors/micropullback_monitors for
/// `needs_new`/`needs_removal` (`track_symbol_for_movers`, mirroring
/// `track_symbol`'s funnel path minus the SessionTracker/FunnelSignal
/// piece specifically, since a movers-only symbol hasn't cleared Stage
/// 1/2 and shouldn't broadcast FunnelSignal claiming it has).
/// `untrack_symbol`'s own guard (only tear down what the OTHER source
/// doesn't also want) is extended the same way halt_monitors/
/// halt_levels already works. The `AlpacaMessage::Bar` handler is
/// restructured so momentum/consolidation/micropullback no longer sit
/// *inside* the funnel's own `trackers.get_mut(...)` gate — they used to
/// be structurally unreachable for a movers-only symbol regardless of
/// whether a monitor existed for it, since the whole match arm returned
/// early before ever reaching them.
///
/// **Doesn't touch what was already validated**: a funnel-tracked
/// symbol's FunnelSignal/momentum/ignition/consolidation behavior is
/// byte-for-byte unchanged (same SessionTracker calls, same event
/// content) — the restructuring only changes what runs for a symbol
/// `trackers` DOESN'T have, which previously ran nothing at all.
///
/// Micropullback's own config — see [[stockspotter-open-tasks]]'s
/// tune_broad finding (2026-09-03): `min_consolidation_candles: 1`
/// genuinely unlocks a real, distinct pattern (37 surges -> 27 confirmed
/// -> 15 real entries vs. the validated default's 36/20/11), not just a
/// looser version of the same one — kept as a SEPARATE parallel monitor
/// per symbol rather than replacing the default, since the extra
/// signals it unlocks scored a lower hit rate on the same real data
/// (too small a sample, 11 vs 15, to trust that difference either way —
/// same caution this project already learned once on momentum's
/// original threshold).
pub fn micropullback_config() -> ConsolidationBreakoutConfig {
    ConsolidationBreakoutConfig {
        consolidation: ConsolidationThresholds { min_consolidation_candles: 1, ..ConsolidationThresholds::default() },
        ..ConsolidationBreakoutConfig::default()
    }
}

/// How often a still-forming candle's live update actually gets
/// broadcast, independent of how often trades arrive — a liquid symbol
/// can trade many times a second, and broadcasting every single one would
/// flood the channel and every client's chart re-render for no visible
/// benefit at that resolution. 500ms keeps the candle visibly "growing"
/// in real time without that flood.
const LIVE_BAR_BROADCAST_INTERVAL: Duration = Duration::from_millis(500);

/// Real sub-minute (2026-09-03) live-candle granularity — Roman wanted
/// to "observe the price action at a granular label from 30 seconds and
/// up". Confirmed live against Alpaca's own REST API before building
/// this: `timeframe=30Sec` is rejected outright
/// (`{"message":"invalid timeframe: 30Sec"}`) — `1Min` is the true
/// floor for HISTORICAL bars, no sub-minute backfill exists or ever
/// will via that endpoint. This bucket is therefore built the same way
/// the 1-minute `LiveBar` below is (raw trade ticks, no new Alpaca
/// subscription needed — trades already stream per-tick, independent of
/// the 1-minute bar cadence) but is **permanently live-only**: Alpaca's
/// official `Bar` message is never sub-minute, so unlike the 60s
/// `LiveBar` (authoritatively corrected every time the real minute
/// closes), this estimate never gets a correction. That's an accepted,
/// honestly-surfaced tradeoff (clients label a 30s view "live, no
/// history" rather than pretending it has the same footing as 1m/5m/
/// 15m), not a bug to eventually fix.
const SUB_MINUTE_BUCKET_SECS: i64 = 30;

/// Running OHLCV for one symbol's CURRENT, still-forming bucket — built
/// from raw trade ticks between Alpaca's own once-per-minute `Bar`
/// messages (see the Trade handler in `run_live_scan`). Originally only
/// ever a 1-minute bucket; now also reused verbatim for the 30s
/// sub-minute path above (`sub_minute_bars`, same struct, same
/// broadcast throttle, different `floor_to_interval` width) — the only
/// real behavioral difference between the two is that the 1-minute one
/// gets an authoritative correction from Alpaca's own official `Bar`
/// and the 30s one never does (see SUB_MINUTE_BUCKET_SECS's own doc
/// comment). This is a best-effort live preview: Alpaca's official
/// `Bar` for the same minute, once it actually closes, is still sent
/// separately and authoritatively corrects/replaces whatever this
/// produced (clients merge `ScanEvent::BarUpdate` by its own
/// `timestamp`, so the later, official message simply overwrites the
/// live estimate) — this struct never needs to be "right", just close
/// enough to look continuous.
struct LiveBar {
    bucket_start: DateTime<Utc>,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: u64,
    last_broadcast: Instant,
}

struct AbortOnDrop(tokio::task::AbortHandle);
impl Drop for AbortOnDrop {
    fn drop(&mut self) { self.0.abort(); }
}

fn managed_position_symbols() -> Vec<String> {
    let path = std::env::var("AUTO_TRADER_JOURNAL_PATH").unwrap_or_else(|_| "data/auto_trader_journal.jsonl".into());
    let Ok(content) = std::fs::read_to_string(path) else { return Vec::new(); };
    let mut symbols = HashSet::new();
    for line in content.lines() {
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else { continue; };
        let Some(symbol) = entry["symbol"].as_str() else { continue; };
        match entry["type"].as_str() {
            Some("entered") => { symbols.insert(symbol.to_string()); }
            Some("exited") => { symbols.remove(symbol); }
            _ => {}
        }
    }
    symbols.into_iter().collect()
}

/// Floors a real timestamp down to the start of its `interval_secs`-wide
/// bucket. For `interval_secs = 60` this is the same bucket boundary
/// Alpaca's own bar `t` field represents, so a live update and the
/// eventual official bar for the same minute land on the identical
/// `timestamp` and merge into one chart candle client-side rather than
/// appearing as two. Generalized 2026-09-03 (was `floor_to_minute`,
/// hardcoded to `% 60`) to also serve `SUB_MINUTE_BUCKET_SECS` — same
/// math, just parameterized; the 60s call site's behavior is unchanged
/// byte-for-byte (see the regression test locking this in).
fn floor_to_interval(t: DateTime<Utc>, interval_secs: i64) -> DateTime<Utc> {
    let secs = t.timestamp();
    let floored = secs - secs.rem_euclid(interval_secs);
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
    // Parallel, faster-triggering monitor per symbol -- same pattern as
    // consolidation_monitors above, tuned via micropullback_config() to
    // catch a genuine single-candle micropullback the default 2-candle
    // minimum structurally can't (see that function's own doc comment).
    let mut micropullback_monitors: HashMap<String, ConsolidationBreakoutMonitor> = HashMap::new();
    // Running OHLCV for each symbol's CURRENT, still-forming minute, built
    // from raw trade ticks -- see the Trade handler below and LiveBar's own
    // doc comment for why this exists (confirmed live: without it, a
    // chart's current candle just snaps into existence once a minute
    // instead of growing continuously, a real felt lag against a platform
    // like Robinhood's, not a cosmetic nitpick).
    let mut live_bars: HashMap<String, LiveBar> = HashMap::new();
    // Real sub-minute (30s) live candle -- same struct/mechanism as
    // live_bars above, see SUB_MINUTE_BUCKET_SECS's own doc comment for
    // why this is a second, independent map rather than folded into the
    // 1-minute one (permanently live-only, never gets an authoritative
    // correction the way 1-minute bars do).
    let mut sub_minute_bars: HashMap<String, LiveBar> = HashMap::new();
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
    // Third coverage source (2026-09-06). Ignition-only, and deliberately
    // populated with the INVERSE profile of the other two: quiet,
    // low-priced, non-gapping stocks. See QuietWatchConfig's own doc
    // comment for why the ignition detector was structurally blind to
    // the flat-base pattern without this.
    let mut quiet_tracked: HashSet<String> = HashSet::new();
    let mut quiet_last_selected = HashMap::new();
    let mut confirmed_watch: HashMap<String, std::time::Instant> = HashMap::new();
    // Fourth tier, opt-in: literal universe-wide ignition coverage. See
    // UNIVERSE_IGNITION_ENV's own doc comment for what it costs and why
    // it isn't the default.
    let universe_mode = std::env::var(UNIVERSE_IGNITION_ENV).is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    let mut universe_monitors: HashMap<String, IgnitionMonitor> = HashMap::new();
    let mut universe_last_trade: HashMap<String, f64> = HashMap::new();
    let mut universe_eviction_ticker = tokio::time::interval(UNIVERSE_EVICTION_INTERVAL);
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
                        &mut micropullback_monitors,
                    );
                }
                None => warn!(symbol, "no seed data; bars for this symbol will be skipped"),
            }
        }
    }

    for (symbol, tracker) in trackers.iter_mut() {
        for bar in crate::rest::fetch_session_bars(cfg, symbol).await? { tracker.on_bar(&bar); }
    }
    let scan_date = Utc::now().with_timezone(&chrono_tz::America::New_York).date_naive();
    info!(ws = %cfg.market_ws, "connecting to alpaca realtime stream");
    let mut stream = AlpacaStream::connect(cfg, initial_symbols).await?;
    if universe_mode {
        match stream.subscribe_all_trades().await {
            Ok(()) => info!(
                max_monitors = UNIVERSE_MAX_MONITORS,
                "IGNITION_UNIVERSE_MODE on: ignition now watches the entire market's trade tape, not just the tracked tiers"
            ),
            Err(e) => warn!(error = %e, "full-market subscribe failed; falling back to the bounded coverage tiers"),
        }
    }

    info!(idle_timeout = ?IDLE_TIMEOUT, rescan_interval = ?UNIVERSE_RESCAN_INTERVAL, "connected, waiting for bars — universe rescan running in the background");

    let (rescan_tx, mut rescan_rx) = mpsc::channel::<Result<ScanOutcome>>(1);
    let rescan_handle = spawn_periodic_rescan(cfg.clone(), rescan_tx);
    let _rescan_guard = AbortOnDrop(rescan_handle.abort_handle());

    let qualify_url =
        std::env::var("QUALIFY_SERVICE_URL").unwrap_or_else(|_| DEFAULT_QUALIFY_SERVICE_URL.to_string());
    // Bounded, generous — catalyst batches are small (one per rescan's
    // newly-added symbols, rarely more than a handful) and infrequent.
    let (catalyst_tx, mut catalyst_rx) = mpsc::channel::<Vec<SymbolQualification>>(8);

    let mut bars_seen = 0u32;
    let mut trades_seen = 0u32;
    let mut quotes_seen = 0u32;
    let mut halt_last_sent: HashMap<String, std::time::Instant> = HashMap::new();

    // First tick fires immediately (same tokio::time::interval behavior
    // spawn_periodic_rescan already relies on) -- so halt coverage for
    // whatever's already leading Top Gainers/Highly Trading at startup
    // populates within seconds, not after a full minute's wait.
    let mut halt_watch_ticker = tokio::time::interval(HALT_WATCH_REFRESH_INTERVAL);

    let (mover_seed_tx, mut mover_seed_rx) = mpsc::channel::<Result<HashMap<String, DailySeed>>>(1);
    let mut mover_seed_cache = HashMap::new();
    let mut mover_seed_inflight = false;
    let mut audit_tick = tokio::time::interval(Duration::from_secs(15));
    let mut audit_receipts = crate::discovery_audit::Receipts::default();
    if crate::discovery_audit::enabled() {
        crate::discovery_audit::emit("stream_started", serde_json::json!({
            "universe_mode":universe_mode,"feed":cfg.feed}));
    }
    loop {
        tokio::select! {
            _ = audit_tick.tick(), if crate::discovery_audit::enabled() => {
                crate::discovery_audit::emit("coverage", serde_json::json!({
                    "funnel":trackers.keys().collect::<Vec<_>>(),
                    "mover":mover_tracked,"quiet":quiet_tracked,
                    "confirmed":confirmed_watch.keys().collect::<Vec<_>>(),
                    "full_ignition":ignition_monitors.keys().collect::<Vec<_>>(),
                    "universe_ignition":universe_monitors.keys().collect::<Vec<_>>(),
                    "momentum":momentum_windows.keys().collect::<Vec<_>>(),
                    "receipts":audit_receipts.take()}));
            }
            Some(result) = mover_seed_rx.recv() => {
                mover_seed_inflight = false;
                match result {
                    Ok(seeds) => { mover_seed_cache.extend(seeds); halt_watch_ticker.reset_immediately(); },
                    Err(e) => warn!(error = %e, "background mover seeding failed; will retry"),
                }
            }
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
                        AlpacaMessage::Luld { symbol, lower, upper, timestamp } => {
                            if let Some(monitor) = halt_monitors.get_mut(&symbol) { monitor.on_luld(lower, upper, timestamp); }
                        }
                        AlpacaMessage::UpdatedBar(bar) => {
                            if let Some(tracker) = trackers.get_mut(&bar.symbol) { tracker.on_bar(&bar); }
                            let _ = events.send(ScanEvent::BarUpdate { symbol:bar.symbol,timestamp:bar.timestamp,
                                open:bar.open,high:bar.high,low:bar.low,close:bar.close,volume:bar.volume,interval_secs:60,is_final:true });
                        }
                        AlpacaMessage::Bar(bar) => {
                            if bar.timestamp.with_timezone(&chrono_tz::America::New_York).date_naive() > scan_date {
                                anyhow::bail!("new session: reconnecting to rebuild daily seeds and all detector state");
                            }
                            bars_seen += 1;

                            // Funnel-specific: needs the real SessionTracker,
                            // which only ever exists for funnel-qualified
                            // symbols (see track_symbol vs.
                            // track_symbol_for_movers). Scoped to just this
                            // block now, not the whole match arm -- it USED
                            // to gate everything below too (momentum/
                            // consolidation/micropullback), which silently
                            // made them unreachable for a movers-only
                            // symbol regardless of whether a monitor existed
                            // for it. A funnel-tracked symbol's behavior
                            // here is byte-for-byte unchanged: same
                            // SessionTracker call, same event content, just
                            // no longer the gate for everything else too.
                            if let Some(tracker) = trackers.get_mut(&bar.symbol) {
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
                                    timestamp: bar.timestamp + chrono::Duration::minutes(1),
                                    price: snapshot.price,
                                    gap_pct: snapshot.gap_pct,
                                    session_volume: snapshot.session_volume,
                                    price_ok: verdict.price_ok,
                                    float_ok: verdict.float_ok,
                                    rel_vol_ok: verdict.rel_vol_ok,
                                    gap_ok: verdict.gap_ok,
                                    passed: verdict.passed(),
                                });
                            }

                            // Raw OHLCV, straight from Alpaca's bar -- see
                            // ScanEvent::BarUpdate's doc comment on why
                            // this is separate from FunnelSignal above.
                            // Keyed off momentum_windows rather than
                            // `trackers` specifically (2026-09-03): that map
                            // always exists for BOTH funnel-tracked and
                            // movers-only symbols (track_symbol/
                            // track_symbol_for_movers both create it), so
                            // it's the right single "are we tracking this
                            // symbol's bars at all" signal -- a movers-only
                            // symbol's chart can now show real bars too,
                            // not just its halt-proximity reading.
                            if momentum_windows.contains_key(&bar.symbol) {
                                let _ = events.send(ScanEvent::BarUpdate {
                                    symbol: bar.symbol.clone(),
                                    timestamp: bar.timestamp,
                                    open: bar.open,
                                    high: bar.high,
                                    low: bar.low,
                                    close: bar.close,
                                    volume: bar.volume,
                                    // Alpaca's own bar is always exactly
                                    // 1-minute -- no sub-minute official bar
                                    // exists (SUB_MINUTE_BUCKET_SECS's own
                                    // doc comment).
                                    is_final: true, interval_secs: 60,
                                });
                            }

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
                                    timestamp: bar.timestamp + chrono::Duration::minutes(1),
                                    volume_confirmation: momentum.volume_confirmation,
                                    structure: momentum.structure,
                                    ma_slope: momentum.ma_slope,
                                    wick_rejection: momentum.wick_rejection,
                                    overall: momentum.overall,
                                    qualifies,
                                });
                            }

                            // Two parallel monitors per symbol, same
                            // candle, different sensitivity -- see
                            // micropullback_config()'s own doc comment.
                            // Factored into one closure so the two blocks
                            // can't silently drift from each other; only
                            // `strategy` (and which map) differs.
                            let run_consolidation = |monitor: &mut ConsolidationBreakoutMonitor, strategy: ConsolidationStrategy| {
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
                                    info!(symbol = %bar.symbol, ?kind, ?strategy, price = bar.close, "consolidation-breakout event");
                                    let _ = events.send(ScanEvent::ConsolidationEvent {
                                        symbol: bar.symbol.clone(),
                                        timestamp: bar.timestamp + chrono::Duration::minutes(1),
                                        price: bar.close,
                                        kind,
                                        strategy,
                                    });
                                }
                            };
                            if let Some(monitor) = consolidation_monitors.get_mut(&bar.symbol) {
                                run_consolidation(monitor, ConsolidationStrategy::ConsolidationBreakout);
                            }
                            if let Some(monitor) = micropullback_monitors.get_mut(&bar.symbol) {
                                run_consolidation(monitor, ConsolidationStrategy::Micropullback);
                            }
                        }
                        AlpacaMessage::Trade(trade) => {
                            trades_seen += 1;
                            audit_receipts.trade(&trade, ignition_monitors.contains_key(&trade.symbol) || universe_mode);

                            // Live-updates the current candle from this
                            // trade tick -- see LiveBar's own doc comment.
                            // Gated on `trackers` (the same symbol universe
                            // ScanEvent::BarUpdate's official broadcast
                            // already uses below) rather than
                            // ignition_monitors specifically, since this
                            // should apply to every tracked symbol
                            // regardless of which other monitors it has.
                            // Factored into a closure (2026-09-03) so the
                            // 1-minute and 30-second sub-minute buckets
                            // (SUB_MINUTE_BUCKET_SECS's own doc comment)
                            // can't silently drift from each other -- same
                            // real reasoning as the run_consolidation
                            // closure above for the two consolidation
                            // strategies.
                            let update_live_bar = |bars: &mut HashMap<String, LiveBar>, interval_secs: i64| {
                                let bucket_start = floor_to_interval(trade.timestamp, interval_secs);
                                let state = bars.entry(trade.symbol.clone()).or_insert_with(|| LiveBar {
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
                                    // A new bucket started -- for the 60s
                                    // map, Alpaca's own official Bar for the
                                    // just-finished minute arrives separately
                                    // (handled above) and is authoritative;
                                    // this just starts tracking the new one
                                    // live. The 30s map never gets that
                                    // correction (SUB_MINUTE_BUCKET_SECS's
                                    // own doc comment).
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
                                        interval_secs: interval_secs as u32,
                                        is_final: false,
                                    });
                                }
                            };
                            if trackers.contains_key(&trade.symbol) {
                                update_live_bar(&mut live_bars, 60);
                                update_live_bar(&mut sub_minute_bars, SUB_MINUTE_BUCKET_SECS);
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
                                let should_send = previous != Some(level) || halt_last_sent.get(&trade.symbol).is_none_or(|at| at.elapsed() >= Duration::from_millis(500));
                                if should_send {
                                halt_last_sent.insert(trade.symbol.clone(), std::time::Instant::now());
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
                                    luld_in_effect: reading.luld_in_effect,
                                    estimated_bands: reading.estimated_bands,
                                });
                                }
                            }

                            // Tiered lookup: a symbol covered by funnel/
                            // movers/quiet-watch uses its existing full
                            // monitor (which also has quotes streaming).
                            // Otherwise, in universe mode, it gets a
                            // trades-only monitor created on first print
                            // -- this is what makes coverage literally
                            // universe-wide rather than shortlist-wide.
                            let monitor = match ignition_monitors.get_mut(&trade.symbol) {
                                Some(m) => m,
                                None if universe_mode => {
                                    universe_last_trade.insert(trade.symbol.clone(), to_secs(trade.timestamp));
                                    universe_monitors
                                        .entry(trade.symbol.clone())
                                        .or_insert_with(|| IgnitionMonitor::new(universe_monitor_config()))
                                }
                                None => continue,
                            };
                            let event = monitor.on_trade(ignition_detector::Trade {
                                timestamp_secs: to_secs(trade.timestamp),
                                price: trade.price,
                                size: trade.size,
                            });
                            match event {
                                MonitorEvent::None => {}
                                MonitorEvent::CandidateOpened(signals) => {
                                    if crate::discovery_audit::enabled() {
                                        crate::discovery_audit::emit("ignition", serde_json::json!({
                                            "symbol":trade.symbol,"market_at":trade.timestamp,
                                            "price":trade.price,"stage":"candidate"}));
                                    }
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
                                    if crate::discovery_audit::enabled() {
                                        crate::discovery_audit::emit("ignition", serde_json::json!({
                                            "symbol":trade.symbol,"market_at":trade.timestamp,"price":trade.price,
                                            "stage":if result.confirmed {"confirmed"} else {"rejected"}}));
                                    }
                                    if result.confirmed {
                                        remember_confirmed(&mut confirmed_watch,&trade.symbol,std::time::Instant::now());
                                        halt_watch_ticker.reset_immediately();
                                    }
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
                            // Same tiered lookup as trades. Halt-lift is
                            // one of the signals most worth having
                            // market-wide rather than shortlist-wide --
                            // a halted stock resuming is a textbook
                            // ignition setup and there's no reason to
                            // only notice it on names already on a list.
                            let tiered = ignition_monitors
                                .get_mut(&status.symbol)
                                .or_else(|| universe_monitors.get_mut(&status.symbol));
                            if let Some(monitor) = tiered {
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
                    Some(Ok(ScanOutcome { qualified: new_shortlist, float_status, quiet_watch, daily_seeds, session_bars })) => {
                        // Broadcast every scan, healthy or not, so the UI
                        // always knows whether an empty funnel panel means
                        // "quiet market" or "can't answer" -- see
                        // ScanEvent::FunnelHealth's own doc comment.
                        let _ = events.send(ScanEvent::FunnelHealth {
                            timestamp: Utc::now(),
                            float_budget_remaining: float_status.remaining,
                            float_budget: float_status.budget,
                            starved_candidates: float_status.starved_candidates,
                            api_key_missing: float_status.api_key_missing,
                        });

                        // --- quiet watch (flat-base ignition coverage) ---
                        //
                        // Reconciled against the same 15s scan, but kept
                        // as its own block rather than folded into the
                        // funnel diff below: this is a different tier
                        // with a different (inverse) selection rule, and
                        // blending them would make "why is this symbol
                        // subscribed" unanswerable.
                        //
                        // No seed fetch and no daily-bar call -- an
                        // IgnitionMonitor needs nothing but its config,
                        // so this whole tier costs zero extra API
                        // requests on top of the snapshots the scan
                        // already pulled.
                        let wanted_quiet = quiet_watch_with_grace(&quiet_watch, &mut quiet_last_selected, std::time::Instant::now());
                        let quiet_dropped: Vec<String> =
                            quiet_tracked.iter().filter(|s| !wanted_quiet.contains(s.as_str())).cloned().collect();
                        let quiet_added: Vec<String> =
                            wanted_quiet.iter().filter(|s| !quiet_tracked.contains(s.as_str())).cloned().collect();
                        if !quiet_added.is_empty() || !quiet_dropped.is_empty() {
                            info!(added=?quiet_added,dropped=?quiet_dropped,selected=quiet_watch.len(),
                                covered=wanted_quiet.len(),"quiet candidate coverage transition (five-minute grace, bounded capacity)");
                        }

                        if !quiet_dropped.is_empty() {
                            for symbol in &quiet_dropped {
                                quiet_tracked.remove(symbol);
                            }
                            // Departure occurs only after the grace window or
                            // bounded-cap eviction; another tier may still own it.
                            let needs_removal = not_covered_by_other_source(&quiet_dropped, |s| {
                                trackers.contains_key(s) || mover_tracked.contains(s)
                            });
                            if !needs_removal.is_empty() {
                                for symbol in &needs_removal {
                                    ignition_monitors.remove(symbol);
                                }
                                if let Err(e) = stream.unsubscribe(&needs_removal).await {
                                    warn!(error = %e, "failed to unsubscribe quiet-watch symbols");
                                }
                            }
                        }

                        if !quiet_added.is_empty() {
                            for symbol in &quiet_added {
                                quiet_tracked.insert(symbol.clone());
                            }
                            let needs_new = not_covered_by_other_source(&quiet_added, |s| {
                                trackers.contains_key(s) || mover_tracked.contains(s)
                            });
                            if !needs_new.is_empty() {
                                for symbol in &needs_new {
                                    ignition_monitors
                                        .insert(symbol.clone(), IgnitionMonitor::new(MonitorConfig::default()));
                                }
                                info!(
                                    count = needs_new.len(),
                                    "quiet watch: added ignition-only coverage for quiet low-priced symbols (flat-base candidates)"
                                );
                                if let Err(e) = stream.subscribe(&needs_new).await {
                                    warn!(error = %e, "failed to subscribe quiet-watch symbols");
                                }
                            }
                        }
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
                                untrack_symbol(symbol, &mut trackers, &mut momentum_windows, &mut ignition_monitors, &mut halt_monitors, &mut consolidation_monitors, &mut micropullback_monitors, &mut halt_levels, &mut live_bars, &mut sub_minute_bars, &mover_tracked, &quiet_tracked);
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
                            let needs_unsubscribe = not_covered_by_other_source(&dropped, |s| mover_tracked.contains(s) || quiet_tracked.contains(s));
                            if !needs_unsubscribe.is_empty() {
                                if let Err(e) = stream.unsubscribe(&needs_unsubscribe).await {
                                    warn!(error = %e, "failed to unsubscribe dropped symbols");
                                }
                            }
                        }

                        if !added.is_empty() {
                            let added_symbols: Vec<String> = added.iter().map(|q| q.symbol.clone()).collect();
                            info!(added = ?added_symbols, "universe rescan: newly qualifying, adding");
                            match Ok::<_, anyhow::Error>(daily_seeds) {
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
                                                    &mut micropullback_monitors,
                                                );
                                                if let Some(tracker) = trackers.get_mut(&q.symbol) {
                                                    for bar in session_bars.get(&q.symbol).into_iter().flatten() { tracker.on_bar(bar); }
                                                }
                                                if let Some(window) = momentum_windows.get_mut(&q.symbol) {
                                                    if window.len() == 0 {
                                                        for bar in session_bars.get(&q.symbol).into_iter().flatten() {
                                                            window.push(momentum_scorer::Candle { open:bar.open, high:bar.high, low:bar.low, close:bar.close, volume:bar.volume });
                                                            let candle = consolidation_breakout::Candle { open:bar.open, high:bar.high, low:bar.low, close:bar.close, volume:bar.volume };
                                                            if let Some(monitor) = consolidation_monitors.get_mut(&q.symbol) { monitor.on_candle(candle); }
                                                            if let Some(monitor) = micropullback_monitors.get_mut(&q.symbol) { monitor.on_candle(candle); }
                                                        }
                                                    }
                                                }
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
                                    let needs_subscribe = not_covered_by_other_source(&actually_added, |s| mover_tracked.contains(s) || quiet_tracked.contains(s));
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
            _ = universe_eviction_ticker.tick(), if universe_mode => {
                let evicted = evict_idle_universe_monitors(
                    &mut universe_monitors,
                    &mut universe_last_trade,
                    to_secs(Utc::now()),
                    UNIVERSE_MONITOR_IDLE_SECS,
                    UNIVERSE_MAX_MONITORS,
                );
                if evicted > 0 {
                    debug!(evicted, live = universe_monitors.len(), "universe ignition: evicted idle monitors");
                }
            }

            _ = halt_watch_ticker.tick() => {
                confirmed_watch.retain(|_,at| at.elapsed() < Duration::from_secs(20 * 60));
                let mut wanted: Vec<QualifiedSymbol> = {
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

                wanted.extend(confirmed_watch.keys().cloned().map(|symbol| QualifiedSymbol { symbol, float_shares: None }));
                wanted.extend(managed_position_symbols().into_iter().map(|symbol| QualifiedSymbol { symbol, float_shares: None }));
                let currently_mover_tracked: Vec<String> = mover_tracked.iter().cloned().collect();
                let dropped = diff_watchlist(&currently_mover_tracked, &wanted, &mut mover_misses, MAX_CONSECUTIVE_WATCHLIST_MISSES);
                let wanted_set: HashSet<String> = wanted.iter().map(|q| q.symbol.clone()).collect();
                let missing: Vec<String> = wanted_set.iter().filter(|s| !trackers.contains_key(s.as_str()) && !mover_seed_cache.contains_key(s.as_str())).cloned().collect();
                if !missing.is_empty() && !mover_seed_inflight {
                    mover_seed_inflight = true;
                    let cfg = cfg.clone(); let tx = mover_seed_tx.clone();
                    tokio::spawn(async move { let result = fetch_daily_seeds(&cfg, &missing, DAILY_LOOKBACK).await; let _ = tx.send(result).await; });
                }
                let added: Vec<String> = wanted_set.iter().filter(|s| !mover_tracked.contains(s.as_str())
                    && (trackers.contains_key(s.as_str()) || mover_seed_cache.contains_key(s.as_str()))).cloned().collect();

                if !dropped.is_empty() {
                    for symbol in &dropped {
                        mover_tracked.remove(symbol);
                    }
                    // Only actually tear down coverage / unsubscribe for a
                    // symbol the funnel isn't ALSO tracking -- see
                    // not_covered_by_other_source's doc comment. The
                    // funnel takes precedence: if it still wants this
                    // symbol, it already owns full coverage via
                    // track_symbol, untouched here. 2026-09-03: widened
                    // from halt-only to momentum/ignition/consolidation/
                    // micropullback too (see HALT_WATCH_REFRESH_INTERVAL's
                    // own doc comment) -- same rule, more maps.
                    let needs_removal = not_covered_by_other_source(&dropped, |s| trackers.contains_key(s));
                    if !needs_removal.is_empty() {
                        info!(dropped = ?needs_removal, "movers leaderboard: no longer a top mover, dropping coverage");
                        for symbol in &needs_removal {
                            momentum_windows.remove(symbol);
                            consolidation_monitors.remove(symbol);
                            micropullback_monitors.remove(symbol);
                            halt_monitors.remove(symbol);
                            halt_levels.remove(symbol);
                            // Same rule as untrack_symbol's: the quiet
                            // watch is ignition-only, so it keeps this
                            // one monitor alive and nothing else.
                            if !quiet_tracked.contains(symbol) {
                                ignition_monitors.remove(symbol);
                            }
                        }
                        let needs_unsubscribe = not_covered_by_other_source(&needs_removal, |s| quiet_tracked.contains(s));
                        if !needs_unsubscribe.is_empty() {
                            if let Err(e) = stream.unsubscribe(&needs_unsubscribe).await {
                                warn!(error = %e, "failed to unsubscribe movers-leaderboard watch symbols");
                            }
                        }
                    }
                }

                if !added.is_empty() {
                    for symbol in &added {
                        mover_tracked.insert(symbol.clone());
                    }
                    // Only symbols not already funnel-tracked need NEW
                    // monitors + subscription -- the funnel already gives
                    // full coverage to anything it tracks. 2026-09-03:
                    // track_symbol_for_movers replaces the old
                    // halt-monitor-only insert -- same real fix
                    // HALT_WATCH_REFRESH_INTERVAL's own doc comment
                    // describes (the YQ finding: momentum/ignition never
                    // got a fair window on a symbol that only ever
                    // flickered through funnel qualification).
                    let needs_new = not_covered_by_other_source(&added, |s| trackers.contains_key(s));
                    if !needs_new.is_empty() {
                        match Ok::<_, anyhow::Error>(mover_seed_cache.clone()) {
                            Ok(seeds) => {
                                let mut actually_added = Vec::new();
                                for symbol in &needs_new {
                                    match seeds.get(symbol) {
                                        Some(seed) => {
                                            // Preserve the confirmed universe monitor's cooldown/history
                                            // when upgrading it from trades-only to full coverage.
                                            if !ignition_monitors.contains_key(symbol) {
                                                if let Some(mut monitor)=universe_monitors.remove(symbol) {
                                                    monitor.enable_full_history();
                                                    ignition_monitors.insert(symbol.clone(),monitor);
                                                    universe_last_trade.remove(symbol);
                                                }
                                            }
                                            track_symbol_for_movers(
                                                symbol,
                                                seed,
                                                &mut momentum_windows,
                                                &mut ignition_monitors,
                                                &mut halt_monitors,
                                                &mut consolidation_monitors,
                                                &mut micropullback_monitors,
                                            );
                                            actually_added.push(symbol.clone());
                                        }
                                        None => warn!(symbol, "no seed data for movers-leaderboard watch; will retry next cycle"),
                                    }
                                }
                                if !actually_added.is_empty() {
                                    info!(added = ?actually_added, "movers leaderboard: added momentum/ignition/consolidation/halt coverage (not funnel-qualified)");
                                    let needs_subscribe = not_covered_by_other_source(&actually_added, |s| quiet_tracked.contains(s));
                                    if !needs_subscribe.is_empty() {
                                        if let Err(e) = stream.subscribe(&needs_subscribe).await {
                                            warn!(error = %e, "failed to subscribe movers-leaderboard watch symbols");
                                        }
                                    }
                                }
                            }
                            Err(e) => warn!(error = %e, "failed to fetch seed data for movers-leaderboard watch; will retry next cycle"),
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
fn spawn_periodic_rescan(cfg: AlpacaConfig, tx: mpsc::Sender<Result<ScanOutcome>>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let thresholds = FilterThresholds::default();
        let mut ticker = tokio::time::interval(UNIVERSE_RESCAN_INTERVAL);
        // Created once, threaded through every tick -- see FloatCache's
        // own doc comment (universe.rs). It carries both the per-day
        // resolved-float map and the daily FMP request budget, so it has
        // to outlive a single scan to be worth anything. Same "state
        // created once outside the loop, passed &mut each cycle" pattern
        // as every other per-symbol map in this file.
        let mut float_cache = FloatCache::from_env();
        loop {
            ticker.tick().await;
            let mut result = scan_shortlist(&cfg, &thresholds, &mut float_cache).await;
            if let Ok(outcome) = &mut result {
                // Network work happens in the rescan worker, never the trade dispatch loop.
                for q in &outcome.qualified {
                    match crate::rest::fetch_session_bars(&cfg, &q.symbol).await {
                        Ok(bars) => { outcome.session_bars.insert(q.symbol.clone(), bars); }
                        Err(e) => warn!(symbol = %q.symbol, error = %e, "session backfill unavailable; deferring promotion"),
                    }
                }
                outcome.qualified.retain(|q| outcome.session_bars.contains_key(&q.symbol));
            }
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
    micropullback_monitors: &mut HashMap<String, ConsolidationBreakoutMonitor>,
) {
    trackers.insert(
        symbol.to_string(),
        SessionTracker::new(symbol.to_string(), seed.prior_close, seed.avg_daily_volume, float_shares),
    );
    momentum_windows.entry(symbol.to_string()).or_insert_with(|| momentum_scorer::RollingWindow::new(MOMENTUM_WINDOW));
    ignition_monitors.entry(symbol.to_string()).or_insert_with(|| IgnitionMonitor::new(MonitorConfig::default()));
    halt_monitors.insert(
        symbol.to_string(),
        HaltWarningMonitor::new(HaltWarningConfig::default(), seed.avg_daily_volume),
    );
    consolidation_monitors.entry(symbol.to_string()).or_insert_with(|| ConsolidationBreakoutMonitor::new(ConsolidationBreakoutConfig::default()));
    micropullback_monitors.entry(symbol.to_string()).or_insert_with(|| ConsolidationBreakoutMonitor::new(micropullback_config()));
}

/// Movers-leaderboard-only counterpart to `track_symbol` -- everything
/// EXCEPT the funnel/`SessionTracker` (see `HALT_WATCH_REFRESH_INTERVAL`'s
/// own doc comment on the whole 2026-09-03 change this is part of). A
/// symbol reaching this path explicitly hasn't cleared Stage 1/2, so it
/// deliberately does NOT get a `trackers` entry -- that would make it
/// start emitting `FunnelSignal` events claiming a qualification it
/// never earned. `float_shares` isn't a parameter here for the same
/// reason `fast_funnel` never runs for these symbols: nothing on this
/// path needs it.
fn track_symbol_for_movers(
    symbol: &str,
    seed: &DailySeed,
    momentum_windows: &mut HashMap<String, momentum_scorer::RollingWindow>,
    ignition_monitors: &mut HashMap<String, IgnitionMonitor>,
    halt_monitors: &mut HashMap<String, HaltWarningMonitor>,
    consolidation_monitors: &mut HashMap<String, ConsolidationBreakoutMonitor>,
    micropullback_monitors: &mut HashMap<String, ConsolidationBreakoutMonitor>,
) {
    momentum_windows.entry(symbol.to_string()).or_insert_with(|| momentum_scorer::RollingWindow::new(MOMENTUM_WINDOW));
    ignition_monitors.entry(symbol.to_string()).or_insert_with(|| IgnitionMonitor::new(MonitorConfig::default()));
    halt_monitors.insert(
        symbol.to_string(),
        HaltWarningMonitor::new(HaltWarningConfig::default(), seed.avg_daily_volume),
    );
    consolidation_monitors.entry(symbol.to_string()).or_insert_with(|| ConsolidationBreakoutMonitor::new(ConsolidationBreakoutConfig::default()));
    micropullback_monitors.entry(symbol.to_string()).or_insert_with(|| ConsolidationBreakoutMonitor::new(micropullback_config()));
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_quiet_stock_keeps_coverage_while_crossing_into_a_run() {
        let now=std::time::Instant::now();let mut selected_at=HashMap::new();
        let mut snapshots=HashMap::from([("RUNNER".into(),fast_funnel::TickerSnapshot{symbol:"RUNNER".into(),
            price:1.5,float_shares:Some(1_000_000),avg_daily_volume:100_000,session_volume:50_000,gap_pct:2.})]);
        let selected=crate::universe::select_quiet_watch(&snapshots,&crate::universe::QuietWatchConfig::default());
        let first=quiet_watch_with_grace(&selected,&mut selected_at,now);
        assert!(first.contains("RUNNER"));
        // It has left quiet thresholds but has not reached the funnel or leaderboard.
        let runner=snapshots.get_mut("RUNNER").unwrap();runner.gap_pct=6.;runner.session_volume=120_000;
        assert!(!fast_funnel::explain(runner,&FilterThresholds::default()).passed());
        let selected=crate::universe::select_quiet_watch(&snapshots,&crate::universe::QuietWatchConfig::default());
        assert!(selected.is_empty());
        let transition=quiet_watch_with_grace(&selected,&mut selected_at,now+Duration::from_secs(30));
        assert!(transition.contains("RUNNER"));
        assert!(quiet_watch_with_grace(&[],&mut selected_at,now+QUIET_TRANSITION_GRACE).is_empty());
    }
    #[test]
    fn quiet_transition_capacity_is_bounded_and_new_selections_take_priority() {
        let now=std::time::Instant::now();let mut selected_at=HashMap::new();
        for generation in 0..4 {
            let names:Vec<String>=(0..150).map(|i|format!("G{generation}-{i}")).collect();
            let wanted=quiet_watch_with_grace(&names,&mut selected_at,now+Duration::from_secs(generation));
            assert!(wanted.len()<=QUIET_TOTAL_CAP);assert!(names.iter().all(|s|wanted.contains(s)));
        }
    }
    #[test]
    fn confirmed_candidate_promotion_is_bounded() {
        let now=std::time::Instant::now();let mut watch=HashMap::new();
        for i in 0..151 {remember_confirmed(&mut watch,&format!("S{i}"),now+Duration::from_secs(i));}
        assert_eq!(watch.len(),150);assert!(!watch.contains_key("S0"));assert!(watch.contains_key("S150"));
    }

    use super::*;

    fn qualified(symbol: &str) -> QualifiedSymbol {
        QualifiedSymbol { symbol: symbol.to_string(), float_shares: None }
    }

    fn tracked(symbols: &[&str]) -> Vec<String> {
        symbols.iter().map(|s| s.to_string()).collect()
    }

    // --- floor_to_interval (2026-09-03, generalized from floor_to_minute) ---

    #[test]
    fn floor_to_interval_at_60s_matches_the_old_floor_to_minute_behavior() {
        use chrono::TimeZone;
        // 14:32:47 UTC -- real arbitrary timestamp, not aligned to any
        // round boundary, so this actually exercises the flooring math.
        let t = Utc.with_ymd_and_hms(2026, 9, 3, 14, 32, 47).unwrap();
        let floored = floor_to_interval(t, 60);
        assert_eq!(floored, Utc.with_ymd_and_hms(2026, 9, 3, 14, 32, 0).unwrap());
    }

    #[test]
    fn floor_to_interval_at_30s_buckets_correctly() {
        use chrono::TimeZone;
        // :47 falls in the second half of its minute -- the 30s bucket
        // it belongs to starts at :30, not :00 or :60.
        let t = Utc.with_ymd_and_hms(2026, 9, 3, 14, 32, 47).unwrap();
        let floored = floor_to_interval(t, 30);
        assert_eq!(floored, Utc.with_ymd_and_hms(2026, 9, 3, 14, 32, 30).unwrap());
    }

    #[test]
    fn floor_to_interval_at_30s_handles_the_first_half_of_the_minute_too() {
        use chrono::TimeZone;
        let t = Utc.with_ymd_and_hms(2026, 9, 3, 14, 32, 12).unwrap();
        let floored = floor_to_interval(t, 30);
        assert_eq!(floored, Utc.with_ymd_and_hms(2026, 9, 3, 14, 32, 0).unwrap());
    }

    #[test]
    fn floor_to_interval_on_an_exact_boundary_is_a_no_op() {
        use chrono::TimeZone;
        let t = Utc.with_ymd_and_hms(2026, 9, 3, 14, 32, 0).unwrap();
        assert_eq!(floor_to_interval(t, 60), t);
        assert_eq!(floor_to_interval(t, 30), t);
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

    fn seed() -> DailySeed {
        DailySeed { prior_close: 3.00, avg_daily_volume: 500_000 }
    }

    #[test]
    fn track_symbol_for_movers_populates_everything_except_trackers() {
        // The whole point of this path (see its own doc comment): a
        // movers-only symbol gets real momentum/ignition/consolidation/
        // micropullback/halt coverage, but never a SessionTracker --
        // that would make it start emitting FunnelSignal claiming a
        // qualification it never earned.
        let mut momentum_windows = HashMap::new();
        let mut ignition_monitors = HashMap::new();
        let mut halt_monitors = HashMap::new();
        let mut consolidation_monitors = HashMap::new();
        let mut micropullback_monitors = HashMap::new();

        track_symbol_for_movers(
            "YQ",
            &seed(),
            &mut momentum_windows,
            &mut ignition_monitors,
            &mut halt_monitors,
            &mut consolidation_monitors,
            &mut micropullback_monitors,
        );

        assert!(momentum_windows.contains_key("YQ"));
        assert!(ignition_monitors.contains_key("YQ"));
        assert!(halt_monitors.contains_key("YQ"));
        assert!(consolidation_monitors.contains_key("YQ"));
        assert!(micropullback_monitors.contains_key("YQ"), "micropullback monitor should exist alongside the standard one");
    }

    #[test]
    fn idle_universe_monitors_are_evicted_and_active_ones_kept() {
        // The bound that makes universe-wide coverage affordable: a
        // symbol that hasn't printed in longer than the idle window is
        // definitionally not igniting, so its rolling buffer is dead
        // weight.
        let mut monitors = HashMap::new();
        let mut last = HashMap::new();
        for (sym, t) in [("ACTIVE", 990.0), ("STALE", 100.0), ("ALSOSTALE", 50.0)] {
            monitors.insert(sym.to_string(), IgnitionMonitor::new(universe_monitor_config()));
            last.insert(sym.to_string(), t);
        }

        let evicted = evict_idle_universe_monitors(&mut monitors, &mut last, 1000.0, 300.0, 10_000);

        assert_eq!(evicted, 2);
        assert!(monitors.contains_key("ACTIVE"), "a symbol printing 10s ago must survive");
        assert!(!monitors.contains_key("STALE"));
        assert!(!monitors.contains_key("ALSOSTALE"));
        assert!(!last.contains_key("STALE"), "the timestamp map must not leak entries either");
    }

    #[test]
    fn the_cap_evicts_least_recently_traded_first() {
        // Over the cap even after idle pruning: whatever traded longest
        // ago goes first, which is the least-likely-to-ignite ordering.
        let mut monitors = HashMap::new();
        let mut last = HashMap::new();
        for (sym, t) in [("NEWEST", 1000.0), ("MIDDLE", 999.0), ("OLDEST", 998.0)] {
            monitors.insert(sym.to_string(), IgnitionMonitor::new(universe_monitor_config()));
            last.insert(sym.to_string(), t);
        }

        // Nothing is idle (all within 300s), so only the cap can bite.
        let evicted = evict_idle_universe_monitors(&mut monitors, &mut last, 1000.0, 300.0, 2);

        assert_eq!(evicted, 1);
        assert!(!monitors.contains_key("OLDEST"));
        assert!(monitors.contains_key("NEWEST"));
        assert!(monitors.contains_key("MIDDLE"));
    }

    #[test]
    fn eviction_is_a_no_op_when_everything_is_active_and_under_cap() {
        let mut monitors = HashMap::new();
        let mut last = HashMap::new();
        monitors.insert("A".to_string(), IgnitionMonitor::new(universe_monitor_config()));
        last.insert("A".to_string(), 1000.0);

        assert_eq!(evict_idle_universe_monitors(&mut monitors, &mut last, 1000.0, 300.0, 10), 0);
        assert_eq!(monitors.len(), 1);
    }

    #[test]
    fn universe_tier_keeps_the_same_detection_thresholds_as_every_other_tier() {
        // Only the memory bounds may differ. If universe-tier symbols
        // used different thresholds, whether a stock fired would depend
        // on which tier happened to pick it up -- which would make every
        // backtest number untrustworthy, since replay has no tiers.
        let universe = universe_monitor_config();
        let default = MonitorConfig::default();
        assert_eq!(universe.thresholds, default.thresholds);
        assert_eq!(universe.follow_through, default.follow_through);
        assert_eq!(universe.flat_base, default.flat_base);
        assert_eq!(universe.confirmation_trade_count, default.confirmation_trade_count);
        assert_eq!(universe.alert_cooldown_secs, default.alert_cooldown_secs);
        // The one intended difference.
        assert!(universe.max_trades < default.max_trades);
    }

    #[test]
    fn a_quiet_watch_symbol_keeps_its_ignition_monitor_across_a_funnel_drop() {
        // The quiet watch is ignition-only, so a funnel drop must strip
        // everything else but leave that one monitor -- and its
        // accumulated tick history -- intact. This is the case that
        // matters most: a stock falling OUT of funnel qualification back
        // into quiet is a flat base forming, which is exactly when
        // wiping its ignition state would be worst.
        let mut trackers = HashMap::new();
        trackers.insert("QUIET".to_string(), SessionTracker::new("QUIET".to_string(), 1.0, 1_000_000, None));
        let mut momentum_windows = HashMap::new();
        momentum_windows.insert("QUIET".to_string(), momentum_scorer::RollingWindow::new(MOMENTUM_WINDOW));
        let mut ignition_monitors = HashMap::new();
        ignition_monitors.insert("QUIET".to_string(), IgnitionMonitor::new(MonitorConfig::default()));
        let mut halt_monitors = HashMap::new();
        halt_monitors.insert("QUIET".to_string(), HaltWarningMonitor::new(HaltWarningConfig::default(), 1_000_000));
        let mut consolidation_monitors = HashMap::new();
        consolidation_monitors.insert("QUIET".to_string(), ConsolidationBreakoutMonitor::new(ConsolidationBreakoutConfig::default()));
        let mut micropullback_monitors = HashMap::new();
        micropullback_monitors.insert("QUIET".to_string(), ConsolidationBreakoutMonitor::new(micropullback_config()));
        let mut halt_levels = HashMap::new();
        let mut live_bars = HashMap::new();
        let mut sub_minute_bars = HashMap::new();

        let quiet_tracked: HashSet<String> = ["QUIET".to_string()].into_iter().collect();
        untrack_symbol(
            "QUIET",
            &mut trackers,
            &mut momentum_windows,
            &mut ignition_monitors,
            &mut halt_monitors,
            &mut consolidation_monitors,
            &mut micropullback_monitors,
            &mut halt_levels,
            &mut live_bars,
            &mut sub_minute_bars,
            &HashSet::new(),
            &quiet_tracked,
        );

        assert!(!trackers.contains_key("QUIET"), "funnel qualification always goes away");
        assert!(
            ignition_monitors.contains_key("QUIET"),
            "the quiet watch still wants ignition coverage for this symbol"
        );
        // Everything the quiet watch has no claim on is gone.
        assert!(!momentum_windows.contains_key("QUIET"));
        assert!(!halt_monitors.contains_key("QUIET"));
        assert!(!consolidation_monitors.contains_key("QUIET"));
        assert!(!micropullback_monitors.contains_key("QUIET"));
    }

    #[test]
    fn untrack_symbol_preserves_everything_but_trackers_when_still_mover_tracked() {
        // Real scenario this guards: a symbol flaps out of funnel
        // qualification (the YQ finding) but the movers leaderboard
        // still wants it -- momentum_windows/ignition_monitors/
        // consolidation_monitors/micropullback_monitors/halt_monitors/
        // halt_levels must all survive so accumulated state (rolling
        // windows, ignition history) isn't wiped and rebuilt from
        // scratch on the very next re-add.
        let mut trackers = HashMap::new();
        trackers.insert("YQ".to_string(), SessionTracker::new("YQ".to_string(), 3.00, 500_000, None));
        let mut momentum_windows = HashMap::new();
        momentum_windows.insert("YQ".to_string(), momentum_scorer::RollingWindow::new(MOMENTUM_WINDOW));
        let mut ignition_monitors = HashMap::new();
        ignition_monitors.insert("YQ".to_string(), IgnitionMonitor::new(MonitorConfig::default()));
        let mut halt_monitors = HashMap::new();
        halt_monitors.insert("YQ".to_string(), HaltWarningMonitor::new(HaltWarningConfig::default(), 500_000));
        let mut consolidation_monitors = HashMap::new();
        consolidation_monitors.insert("YQ".to_string(), ConsolidationBreakoutMonitor::new(ConsolidationBreakoutConfig::default()));
        let mut micropullback_monitors = HashMap::new();
        micropullback_monitors.insert("YQ".to_string(), ConsolidationBreakoutMonitor::new(micropullback_config()));
        let mut halt_levels = HashMap::new();
        halt_levels.insert("YQ".to_string(), HaltAlertLevel::Amber);
        let mut live_bars = HashMap::new();
        live_bars.insert(
            "YQ".to_string(),
            LiveBar { bucket_start: Utc::now(), open: 3.0, high: 3.0, low: 3.0, close: 3.0, volume: 0, last_broadcast: Instant::now() },
        );
        let mut sub_minute_bars = HashMap::new();
        sub_minute_bars.insert(
            "YQ".to_string(),
            LiveBar { bucket_start: Utc::now(), open: 3.0, high: 3.0, low: 3.0, close: 3.0, volume: 0, last_broadcast: Instant::now() },
        );
        let mover_tracked: HashSet<String> = ["YQ".to_string()].into_iter().collect();

        untrack_symbol(
            "YQ",
            &mut trackers,
            &mut momentum_windows,
            &mut ignition_monitors,
            &mut halt_monitors,
            &mut consolidation_monitors,
            &mut micropullback_monitors,
            &mut halt_levels,
            &mut live_bars,
            &mut sub_minute_bars,
            &mover_tracked,
            &HashSet::new(),
        );

        // Funnel-specific state always goes away on a funnel drop.
        assert!(!trackers.contains_key("YQ"));
        assert!(!live_bars.contains_key("YQ"));
        assert!(!sub_minute_bars.contains_key("YQ"));
        // Everything else survives because mover_tracked still wants it.
        assert!(momentum_windows.contains_key("YQ"));
        assert!(ignition_monitors.contains_key("YQ"));
        assert!(consolidation_monitors.contains_key("YQ"));
        assert!(micropullback_monitors.contains_key("YQ"));
        assert!(halt_monitors.contains_key("YQ"));
        assert!(halt_levels.contains_key("YQ"));
    }

    #[test]
    fn untrack_symbol_clears_everything_when_no_longer_mover_tracked_either() {
        let mut trackers = HashMap::new();
        trackers.insert("SWVL".to_string(), SessionTracker::new("SWVL".to_string(), 3.00, 500_000, None));
        let mut momentum_windows = HashMap::new();
        momentum_windows.insert("SWVL".to_string(), momentum_scorer::RollingWindow::new(MOMENTUM_WINDOW));
        let mut ignition_monitors = HashMap::new();
        ignition_monitors.insert("SWVL".to_string(), IgnitionMonitor::new(MonitorConfig::default()));
        let mut halt_monitors = HashMap::new();
        halt_monitors.insert("SWVL".to_string(), HaltWarningMonitor::new(HaltWarningConfig::default(), 500_000));
        let mut consolidation_monitors = HashMap::new();
        consolidation_monitors.insert("SWVL".to_string(), ConsolidationBreakoutMonitor::new(ConsolidationBreakoutConfig::default()));
        let mut micropullback_monitors = HashMap::new();
        micropullback_monitors.insert("SWVL".to_string(), ConsolidationBreakoutMonitor::new(micropullback_config()));
        let mut halt_levels = HashMap::new();
        halt_levels.insert("SWVL".to_string(), HaltAlertLevel::Calm);
        let mut live_bars = HashMap::new();
        let mut sub_minute_bars = HashMap::new();
        let mover_tracked: HashSet<String> = HashSet::new();

        untrack_symbol(
            "SWVL",
            &mut trackers,
            &mut momentum_windows,
            &mut ignition_monitors,
            &mut halt_monitors,
            &mut consolidation_monitors,
            &mut micropullback_monitors,
            &mut halt_levels,
            &mut live_bars,
            &mut sub_minute_bars,
            &mover_tracked,
            &HashSet::new(),
        );

        assert!(!trackers.contains_key("SWVL"));
        assert!(!momentum_windows.contains_key("SWVL"));
        assert!(!ignition_monitors.contains_key("SWVL"));
        assert!(!consolidation_monitors.contains_key("SWVL"));
        assert!(!micropullback_monitors.contains_key("SWVL"));
        assert!(!halt_monitors.contains_key("SWVL"));
        assert!(!halt_levels.contains_key("SWVL"));
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
    micropullback_monitors: &mut HashMap<String, ConsolidationBreakoutMonitor>,
    halt_levels: &mut HashMap<String, HaltAlertLevel>,
    live_bars: &mut HashMap<String, LiveBar>,
    sub_minute_bars: &mut HashMap<String, LiveBar>,
    mover_tracked: &HashSet<String>,
    quiet_tracked: &HashSet<String>,
) {
    // Funnel qualification itself (trackers/FunnelSignal) always goes
    // away on a funnel drop, regardless of movers-side status -- a
    // symbol that stopped clearing Stage 1/2 shouldn't keep emitting
    // FunnelSignal events pretending it still does. Chart bars (both the
    // 1-minute live_bars and the 2026-09-03 sub-minute sub_minute_bars)
    // go with it too (unaffected by this change -- still funnel-only,
    // see HALT_WATCH_REFRESH_INTERVAL's own doc comment on that
    // deliberate, still-open scope boundary -- the Trade handler above
    // only ever populates either map for `trackers`-gated symbols, so
    // both are cleaned up the same unconditional way).
    trackers.remove(symbol);
    live_bars.remove(symbol);
    sub_minute_bars.remove(symbol);
    // Everything else -- momentum/ignition/consolidation/micropullback/
    // halt -- stays alive if the movers-leaderboard side still wants
    // this symbol (2026-09-03: extended from halt-only to all of them,
    // see HALT_WATCH_REFRESH_INTERVAL's own doc comment). Preserves
    // real accumulated state (rolling windows, ignition history,
    // consolidation tracking) across a funnel drop instead of wiping it,
    // the exact class of gap the YQ finding surfaced.
    if !mover_tracked.contains(symbol) {
        momentum_windows.remove(symbol);
        consolidation_monitors.remove(symbol);
        micropullback_monitors.remove(symbol);
        halt_monitors.remove(symbol);
        halt_levels.remove(symbol);
        // Ignition alone survives a funnel drop when the quiet watch
        // still wants this symbol -- that tier is ignition-only by
        // design (see QuietWatchConfig's doc comment), so it has a claim
        // on this one monitor and none of the others. A stock falling
        // out of funnel qualification back into quiet is precisely the
        // flat-base setup forming, so wiping its ignition history at
        // that exact moment would be the worst possible time.
        if !quiet_tracked.contains(symbol) {
            ignition_monitors.remove(symbol);
        }
    }
}
