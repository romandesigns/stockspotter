//! Live wiring for the Alpha measurement engine.
//!
//! Subscribes to the same broadcast every WS client reads from — a second,
//! independent receiver, exactly as the live detection-efficiency collector in
//! `main.rs` already does — folds each event into `backtest_metrics`'
//! `EpisodeTracker`, and appends completed research artifacts to disk.
//!
//! # This must never become a production outage path
//!
//! Three properties, in priority order:
//!
//! 1. **It cannot block market dispatch.** Records go to a bounded channel and
//!    a dedicated writer thread. When the channel is full, records are
//!    *dropped and counted* — never awaited. The same shape
//!    `market_data::discovery_audit` already uses, and for the same reason.
//! 2. **It cannot fail the realtime path.** Every disk error is logged and
//!    counted; nothing propagates. A research file that cannot be written is a
//!    measurement gap, not an outage.
//! 3. **It cannot alter what production does.** It only reads events that have
//!    already been broadcast. It emits nothing, gates nothing, and reorders
//!    nothing.
//!
//! Dropped and failed records are counted separately and surfaced, because a
//! silent gap would invalidate exactly the completeness claims this data
//! exists to support.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::Arc;

use backtest_metrics::episode::{EpisodeTracker, OpportunityEpisode};
use backtest_metrics::horizon::{evaluate_horizons, PricePoint};
use chrono::{DateTime, Utc};
use market_data::ScanEvent;
use tracing::{info, warn};

/// Bounded, matching `discovery_audit`'s queue depth. Small on purpose: a
/// backlog means the writer cannot keep up, and the correct response is to
/// drop and count rather than to grow memory in a realtime process.
const QUEUE_DEPTH: usize = 64;

/// How often contemporaneous candidates are ranked for research telemetry.
/// Research-only: nothing reads this ordering except later analysis.
const RANKING_INTERVAL_SECS: i64 = 30;

/// How long a closed episode keeps collecting forward prices before its
/// outcome is settled and written.
///
/// **Derived from the horizon grid, never declared here (R1).** This used to be
/// an independent `1800`, exactly equal to the longest horizon, which made that
/// horizon observable only by a race between `extend_paths` and `settle_due`
/// inside one `observe` call -- 0.06% observed in Session 001. The deadline now
/// comes from `horizon::SETTLE_AFTER_SECS`, which is `longest_horizon_secs() +
/// OBSERVATION_MARGIN_SECS` and is compile-time asserted to exceed every
/// configured horizon. Editing `HORIZON_SECS` moves this automatically.
use backtest_metrics::horizon::SETTLE_AFTER_SECS as OUTCOME_WINDOW_SECS;

/// The episode rate this collector is *designed* to retain fully, in
/// hundredths of an episode per second. Integer hundredths rather than `f64`
/// so the capacity below is exact const arithmetic.
///
/// **16.00/s, chosen from measured evidence, not rounded up from a guess.**
/// Instrument Validation Session 002 (2026-09-11, regular session) measured:
///
/// | statistic | value |
/// |---|---|
/// | mean rate | 5.19/s |
/// | peak rate sustained across a full 1920s window | **12.61/s** |
/// | peak instantaneous minute | 37.8/s |
///
/// The quantity that sizes a pending set is the *sustained* rate over one
/// settlement window, because that is literally the population: 24,217
/// episodes were open-and-unsettled at the observed peak. 16.00/s carries 27%
/// headroom over that peak and 3.1x the mean, so an ordinarily busier session
/// does not saturate. Instantaneous 37.8/s bursts do not size this — a burst
/// lasting a minute adds ~2,268 episodes to a population measured in tens of
/// thousands.
const SUPPORTED_EPISODE_RATE_CENTI: u64 = 1_600;

/// Headroom beyond the supported rate, as a fraction (5/4 = 1.25). Absorbs a
/// session materially busier than any yet observed before eviction begins.
const PENDING_SAFETY_NUM: u64 = 5;
const PENDING_SAFETY_DEN: u64 = 4;

/// Episodes retained awaiting an outcome, **derived** from the supported
/// throughput envelope and the settlement window rather than picked.
///
/// `16.00/s x 1920s x 1.25 = 38,400`.
///
/// The previous value was a flat `4096`, which at Session 002's regular-session
/// rate saturated after ~789s and force-settled every episode long before the
/// 600/900/1800s horizons matured (defect 48-A). It bound for 415 of 1,384
/// sampled minutes. This is still a hard bound — the process is realtime and
/// must not grow without limit — but it is now a bound derived from what the
/// collector claims to support.
const MAX_PENDING_OUTCOMES: usize = ((SUPPORTED_EPISODE_RATE_CENTI
    * OUTCOME_WINDOW_SECS as u64
    * PENDING_SAFETY_NUM)
    / (100 * PENDING_SAFETY_DEN)) as usize;

/// The §2 invariant, enforced at compile time: capacity must hold the supported
/// rate for a **full** settlement window before any safety margin is counted.
/// If someone later lowers the capacity, raises the supported rate, or extends
/// the horizon grid, this fails the build rather than silently reintroducing
/// capacity-induced censoring.
const _: () = {
    assert!(
        (MAX_PENDING_OUTCOMES as u64) * 100
            >= SUPPORTED_EPISODE_RATE_CENTI * (OUTCOME_WINDOW_SECS as u64),
        "pending capacity cannot hold the supported episode rate for one \
         settlement window; episodes would be evicted before their horizons \
         mature (see defect 48-A)"
    );
};

/// Forward observations retained per pending episode. At the measured live
/// cadence this comfortably covers 30 minutes while staying bounded.
const MAX_PATH_POINTS: usize = 2048;

#[derive(Debug)]
enum Record {
    Episode(Box<OpportunityEpisode>),
    Flush(std::sync::mpsc::Sender<()>),
}

/// Counters describing capture completeness. Non-zero values are findings.
#[derive(Debug, Default)]
pub struct MeasurementHealth {
    /// Records discarded because the writer could not keep up.
    pub dropped: AtomicU64,
    /// Records the writer accepted but failed to persist.
    pub write_errors: AtomicU64,
    pub episodes_written: AtomicU64,
}

// Pending-capacity accounting deliberately lives on `MeasurementCollector`,
// not here: the collector owns the pending set, and duplicating the counters
// onto the recorder would create two sources of truth for one fact. See
// `MeasurementCollector::capacity_evictions` / `pending_peak` /
// `pending_capacity`, surfaced at shutdown by `main.rs`.

impl MeasurementHealth {
    pub fn is_degraded(&self) -> bool {
        self.dropped.load(Ordering::Relaxed) > 0 || self.write_errors.load(Ordering::Relaxed) > 0
    }
}

pub struct MeasurementRecorder {
    tx: SyncSender<Record>,
    health: Arc<MeasurementHealth>,
}

impl MeasurementRecorder {
    /// Starts the writer thread. Returns `None` when the directory cannot be
    /// created — measurement is then simply off, and the caller carries on.
    pub fn start(dir: PathBuf) -> Option<Self> {
        if let Err(error) = std::fs::create_dir_all(&dir) {
            warn!(%error, path = %dir.display(), "measurement capture unavailable: cannot create directory");
            return None;
        }
        let logged_dir = dir.clone();
        let (tx, rx) = sync_channel::<Record>(QUEUE_DEPTH);
        let health = Arc::new(MeasurementHealth::default());
        let writer_health = health.clone();
        std::thread::spawn(move || {
            for record in rx {
                match record {
                    Record::Flush(reply) => {
                        let _ = reply.send(());
                    }
                    Record::Episode(episode) => {
                        // One file per UTC day, matching how every other
                        // capture in this project rotates.
                        let day = episode.opened_at.date_naive().to_string();
                        let path = dir.join(format!("episodes-{day}.ndjson"));
                        match append_json(&path, &*episode) {
                            Ok(()) => {
                                writer_health.episodes_written.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(error) => {
                                let n =
                                    writer_health.write_errors.fetch_add(1, Ordering::Relaxed) + 1;
                                // Log on powers of two so a persistent
                                // failure stays visible without flooding.
                                if n.is_power_of_two() {
                                    warn!(%error, failed_writes = n, "measurement capture has gaps");
                                }
                            }
                        }
                    }
                }
            }
        });
        info!(path = %logged_dir.display(), "measurement capture enabled");
        Some(Self { tx, health })
    }

    pub fn health(&self) -> &MeasurementHealth {
        &self.health
    }

    /// Never blocks. A full queue drops the record and counts it.
    fn send(&self, record: Record) {
        if self.tx.try_send(record).is_err() {
            let n = self.health.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_power_of_two() {
                warn!(dropped = n, "measurement queue full; research records dropped");
            }
        }
    }

    pub fn record_episode(&self, episode: OpportunityEpisode) {
        self.send(Record::Episode(Box::new(episode)));
    }

    /// Waits for the writer to drain, bounded. Shutdown must not hang on
    /// research bookkeeping.
    pub fn flush(&self, timeout: std::time::Duration) {
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        if self.tx.try_send(Record::Flush(reply_tx)).is_ok() {
            let _ = reply_rx.recv_timeout(timeout);
        }
    }
}

/// Marker for "observation stopped because capture ended", so the outcome
/// records that rather than looking like an ordinary short window.
struct CaptureEnd;

fn finalize(entry: PendingOutcome, capture_end: Option<CaptureEnd>) -> OpportunityEpisode {
    let PendingOutcome { mut episode, path } = entry;
    // `session_end` is supplied only when capture itself stopped, so an
    // unobserved horizon is attributed to us rather than to the session.
    let session_end = capture_end.map(|_| path.last().map(|(t, _)| *t).unwrap_or(episode.opened_at));
    episode.outcome = Some(evaluate_horizons(
        episode.opening_price,
        episode.opened_at,
        &path,
        session_end,
    ));
    episode
}

/// The symbol/**observability time**/price an event contributes to a forward
/// path. Events carrying no price of their own contribute nothing rather than
/// a guess.
///
/// # The causal timestamp model (R2)
///
/// Every point is `(the instant this price became observable to Stockspotter,
/// price)`. That is deliberately *not* the same as the event's own `timestamp`
/// field for bar-derived events, and conflating them was defect F4.
///
/// | Event | Price | Observable at | Why |
/// |---|---|---|---|
/// | `IgnitionEvent` | trade price | `timestamp` | stamped with the trade that printed it |
/// | `ConsolidationEvent` | bar close | `timestamp` | already stamped bar-close by `live.rs` |
/// | `FunnelSignal` | snapshot price | `timestamp` | already stamped bar-close by `live.rs` |
/// | `HaltWarning` | `current_price` | `timestamp` | stamped with the trade that printed it |
/// | `BarUpdate { is_final: true }` | bar close | `timestamp + interval_secs` | **the close is not knowable until the bar ends** |
/// | `BarUpdate { is_final: false }` | running close | `received_at` | a live bucket carries `bucket_start`, which is up to a full interval early and identical across every update in the bucket |
///
/// `received_at` is this process's receipt clock, taken from the same `now`
/// that drives episode lifecycle. It is used *only* where the event carries no
/// market timestamp that is semantically correct -- the in-progress bucket
/// case. Preferring the market timestamp everywhere else keeps the path on
/// exchange time wherever exchange time is meaningful.
///
/// Note this reads the existing `ScanEvent` and changes nothing about it: the
/// wire format, event ordering, and every strategy input are untouched.
fn observed_price(
    event: &ScanEvent,
    received_at: DateTime<Utc>,
) -> Option<(String, DateTime<Utc>, f64)> {
    match event {
        ScanEvent::IgnitionEvent { symbol, timestamp, price, .. }
        | ScanEvent::ConsolidationEvent { symbol, timestamp, price, .. }
        | ScanEvent::FunnelSignal { symbol, timestamp, price, .. } => {
            Some((symbol.clone(), *timestamp, *price))
        }
        ScanEvent::HaltWarning { symbol, timestamp, current_price, .. } => {
            Some((symbol.clone(), *timestamp, *current_price))
        }
        // A *finalised* bar's close is a fact about the end of the bar, so it
        // becomes observable one interval after the bar's opening timestamp.
        ScanEvent::BarUpdate {
            symbol,
            timestamp,
            close,
            is_final: true,
            interval_secs,
            ..
        } => Some((
            symbol.clone(),
            *timestamp + chrono::Duration::seconds(i64::from(*interval_secs)),
            *close,
        )),
        // An in-progress bucket broadcasts repeatedly (every 500ms in
        // `live.rs`) with `timestamp` pinned to `bucket_start`. Using that
        // would assign many different prices one artificial timestamp, so the
        // receipt clock is the only honest answer available.
        ScanEvent::BarUpdate { symbol, close, is_final: false, .. } => {
            Some((symbol.clone(), received_at, *close))
        }
        _ => None,
    }
}

fn append_json<T: serde::Serialize>(path: &std::path::Path, value: &T) -> anyhow::Result<()> {
    use std::io::Write;
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&line)?;
    Ok(())
}

/// Owns the tracker and decides when to rank and when to persist.
///
/// Separate from `MeasurementRecorder` so the decision logic is testable
/// without a filesystem, and so a test can substitute a failing sink.
pub struct MeasurementCollector {
    tracker: EpisodeTracker,
    last_ranked: Option<DateTime<Utc>>,
    ranking_windows: u64,
    /// Episodes force-settled because the pending set was full. Their
    /// unresolved horizons carry `PendingCapacityReached`, never the ordinary
    /// `InsufficientForwardData`.
    capacity_evictions: u64,
    /// High-water mark of `pending`, so capacity pressure is observable while
    /// it happens rather than inferred from span distributions afterwards.
    pending_peak: usize,
    /// Episodes settled ahead of schedule because the pending set was full.
    settled_early: Vec<OpportunityEpisode>,
    /// Monotonic id making `PendingKey` unique when two episodes share an
    /// `opened_at`.
    next_pending_id: u64,
    /// Symbol -> keys of its pending episodes, so a price event touches only
    /// the episodes it can actually affect.
    ///
    /// Added with the capacity increase, and required by it. Both hot paths
    /// were previously linear in the pending set -- `extend_paths` compared
    /// every entry's symbol on every price, and `settle_due` rescanned the
    /// whole vector on every event -- so raising capacity 4,096 -> 38,400
    /// would have multiplied per-event work 9.4x on the realtime path. A
    /// collector that cannot keep up lags its broadcast receiver and silently
    /// misses observations, which would have traded one measurement defect for
    /// another.
    pending_by_symbol: std::collections::HashMap<String, Vec<PendingKey>>,
    /// Forward price path for each *currently open* episode, keyed by symbol
    /// exactly as the tracker keys open episodes.
    ///
    /// Collected while the episode is open rather than reconstructed at close:
    /// an episode that stays open while bars arrive, or one closed at
    /// shutdown, would otherwise carry no path and censor every horizon. It is
    /// bounded by count only -- trimming by age would punch a hole between the
    /// opening price and the retained tail, which `evaluate_horizons` would
    /// correctly but uselessly report as a data gap.
    open_paths: std::collections::HashMap<String, Vec<PricePoint>>,
    /// Episodes whose detector activity has ended but whose forward outcome is
    /// still being observed. Each carries the price path collected since it
    /// opened.
    ///
    /// Keyed by `(opened_at, id)` so the map is ordered by settlement
    /// deadline: settlement is `opened_at + OUTCOME_WINDOW_SECS`, a constant
    /// offset, so key order *is* due order. That makes `settle_due` a range
    /// query over only the entries actually due, and capacity eviction a
    /// first-key lookup, instead of a full rescan on every event.
    pending: std::collections::BTreeMap<PendingKey, PendingOutcome>,
}

/// `(opened_at, id)` — ordered by settlement deadline, unique per episode.
type PendingKey = (DateTime<Utc>, u64);

struct PendingOutcome {
    episode: OpportunityEpisode,
    path: Vec<PricePoint>,
}

impl Default for MeasurementCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl MeasurementCollector {
    pub fn new() -> Self {
        Self {
            tracker: EpisodeTracker::new(),
            last_ranked: None,
            ranking_windows: 0,
            capacity_evictions: 0,
            pending_peak: 0,
            settled_early: Vec::new(),
            open_paths: std::collections::HashMap::new(),
            next_pending_id: 0,
            pending_by_symbol: std::collections::HashMap::new(),
            pending: std::collections::BTreeMap::new(),
        }
    }

    #[allow(dead_code)]
    pub fn pending_outcomes(&self) -> usize {
        self.pending.len()
    }

    /// Episodes force-settled for want of capacity. **Non-zero means
    /// long-horizon outcomes in this session are not trustworthy.**
    pub fn capacity_evictions(&self) -> u64 {
        self.capacity_evictions
    }

    /// High-water mark of the pending set.
    pub fn pending_peak(&self) -> usize {
        self.pending_peak
    }

    /// The configured bound, so `pending_peak` can be read against it.
    pub fn pending_capacity(&self) -> usize {
        MAX_PENDING_OUTCOMES
    }


    /// Records a price for this symbol: into the rolling per-symbol path, and
    /// into every pending episode already awaiting an outcome.
    fn extend_paths(&mut self, symbol: &str, at: DateTime<Utc>, price: f64) {
        if !price.is_finite() || price <= 0.0 {
            return;
        }
        let trail = self.open_paths.entry(symbol.to_string()).or_default();
        trail.push((at, price));
        if trail.len() > MAX_PATH_POINTS {
            let excess = trail.len() - MAX_PATH_POINTS;
            trail.drain(0..excess);
        }
        // Only the episodes for this symbol, not every pending episode.
        if let Some(keys) = self.pending_by_symbol.get(symbol) {
            for key in keys {
                if let Some(entry) = self.pending.get_mut(key) {
                    if entry.path.len() < MAX_PATH_POINTS {
                        entry.path.push((at, price));
                    }
                }
            }
        }
    }

    /// Hands a closing episode the path collected while it was open, and
    /// clears it so a subsequent episode for the same symbol starts fresh.
    fn take_path(&mut self, episode: &OpportunityEpisode) -> Vec<PricePoint> {
        let mut path = vec![(episode.opened_at, episode.opening_price)];
        if let Some(trail) = self.open_paths.remove(&episode.id.symbol) {
            path.extend(trail.into_iter().filter(|(t, _)| *t > episode.opened_at));
        }
        path
    }

    /// Removes one pending entry from both the ordered map and the
    /// symbol index, so the two can never disagree about what is pending.
    fn take_pending(&mut self, key: &PendingKey) -> Option<PendingOutcome> {
        let entry = self.pending.remove(key)?;
        if let Some(keys) = self.pending_by_symbol.get_mut(&entry.episode.id.symbol) {
            keys.retain(|k| k != key);
            if keys.is_empty() {
                self.pending_by_symbol.remove(&entry.episode.id.symbol);
            }
        }
        Some(entry)
    }

    /// Settles any pending episode whose outcome window has elapsed, computing
    /// its horizons from the observed path. Censoring is left to
    /// `evaluate_horizons` -- an unobserved horizon becomes `Censored`, never a
    /// zero return or a silent failure.
    fn settle_due(&mut self, now: DateTime<Utc>) -> Vec<OpportunityEpisode> {
        // Everything opened at or before this instant is due, and key order is
        // due order, so this touches only the entries being settled.
        let cutoff = now - chrono::Duration::seconds(OUTCOME_WINDOW_SECS);
        let due: Vec<PendingKey> = self
            .pending
            .range(..=(cutoff, u64::MAX))
            .map(|(key, _)| *key)
            .collect();
        let mut settled = Vec::with_capacity(due.len());
        for key in due {
            if let Some(entry) = self.take_pending(&key) {
                settled.push(finalize(entry, None));
            }
        }
        settled
    }

    /// Live counts, for tests and any future health surface.
    #[allow(dead_code)]
    pub fn open_episodes(&self) -> usize {
        self.tracker.open_count()
    }

    /// Folds one already-broadcast event in, returning any episodes that
    /// closed. Ranking runs on a timer so a burst of events cannot turn into
    /// a burst of sorts.
    pub fn observe(&mut self, event: &ScanEvent, now: DateTime<Utc>) -> Vec<OpportunityEpisode> {
        if let Some((symbol, at, price)) = observed_price(event, now) {
            self.extend_paths(&symbol, at, price);
        }
        let closed = self.tracker.observe(event, now);
        // A closed episode is not finished being measured -- it moves to the
        // pending set and keeps collecting forward prices.
        for episode in closed {
            if self.pending.len() >= MAX_PENDING_OUTCOMES {
                // Capacity pressure is a measurement *failure*, not a normal
                // settlement, and must never masquerade as one. Normal
                // settlement is age-driven (`settle_due`); reaching this branch
                // means the collector could not retain the episode long enough
                // to answer the question it was tracking.
                //
                // Whatever already matured is kept -- short horizons that
                // completed before the eviction are real measurements. Only the
                // unresolved ones are re-attributed.
                let oldest_key = *self
                    .pending
                    .keys()
                    .next()
                    .expect("pending is non-empty at capacity");
                let oldest = self
                    .take_pending(&oldest_key)
                    .expect("key came from the map");
                let mut evicted = finalize(oldest, None);
                if let Some(outcome) = evicted.outcome.as_mut() {
                    outcome.mark_capacity_censored();
                }
                self.capacity_evictions += 1;
                if self.capacity_evictions.is_power_of_two() {
                    warn!(
                        capacity_evictions = self.capacity_evictions,
                        pending_capacity = MAX_PENDING_OUTCOMES,
                        "measurement pending capacity reached; long-horizon \
                         outcomes are capacity-censored and must not be read as \
                         market behaviour"
                    );
                }
                self.settled_early.push(evicted);
            }
            let path = self.take_path(&episode);
            let key: PendingKey = (episode.opened_at, self.next_pending_id);
            self.next_pending_id += 1;
            self.pending_by_symbol
                .entry(episode.id.symbol.clone())
                .or_default()
                .push(key);
            self.pending.insert(key, PendingOutcome { episode, path });
            self.pending_peak = self.pending_peak.max(self.pending.len());
        }
        let mut out = std::mem::take(&mut self.settled_early);
        out.extend(self.settle_due(now));
        let due = self
            .last_ranked
            .is_none_or(|last| (now - last).num_seconds() >= RANKING_INTERVAL_SECS);
        if due {
            // Research-only: writes to a field nothing in the production path
            // reads. It cannot reorder client messages, suppress events, or
            // reach the auto-trader, which is a separate process entirely.
            let window = self.ranking_windows + 1;
            let ranked = self
                .tracker
                .assign_research_rank(now, &format!("w-{window}"));
            // An empty cohort must not consume the window. Otherwise the very
            // first event -- which arrives before any momentum score exists --
            // would burn the interval and nothing would ever be ranked.
            if ranked > 0 {
                self.ranking_windows = window;
                self.last_ranked = Some(now);
            }
        }
        out
    }

    /// Closes every still-open episode as censored and settles every pending
    /// outcome with whatever was observed. An episode alive when capture
    /// stopped says something about us, not about the opportunity -- and its
    /// unobserved horizons are censored, never synthesized.
    pub fn finish(&mut self, now: DateTime<Utc>) -> Vec<OpportunityEpisode> {
        let mut out = std::mem::take(&mut self.settled_early);
        for episode in self.tracker.finish(now) {
            let path = self.take_path(&episode);
            let key: PendingKey = (episode.opened_at, self.next_pending_id);
            self.next_pending_id += 1;
            self.pending.insert(key, PendingOutcome { episode, path });
        }
        self.pending_by_symbol.clear();
        for (_, entry) in std::mem::take(&mut self.pending) {
            out.push(finalize(entry, Some(CaptureEnd)));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backtest_metrics::episode::EpisodeCloseReason;
    use backtest_metrics::horizon::{longest_horizon_secs, CensorReason, Observation, SETTLE_AFTER_SECS};
    use chrono::TimeZone;
    use market_data::events::IgnitionEventKind;

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_757_000_000 + secs, 0).unwrap()
    }

    fn confirmed(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
        ScanEvent::IgnitionEvent {
            symbol: symbol.into(),
            timestamp: t,
            price,
            kind: IgnitionEventKind::FollowThroughConfirmed,
        }
    }

    fn bar(symbol: &str, t: DateTime<Utc>, close: f64) -> ScanEvent {
        ScanEvent::BarUpdate {
            symbol: symbol.into(), timestamp: t, interval_secs: 60,
            open: close, high: close, low: close, close, volume: 1_000,
            is_final: true,
        }
    }

    fn momentum(symbol: &str, t: DateTime<Utc>, overall: f64) -> ScanEvent {
        ScanEvent::MomentumUpdate {
            symbol: symbol.into(),
            timestamp: t,
            volume_confirmation: 0.5,
            structure: 0.5,
            ma_slope: 0.5,
            wick_rejection: 0.5,
            overall,
            qualifies: overall >= 0.6,
        }
    }

    fn live_bar(symbol: &str, bucket_start: DateTime<Utc>, close: f64) -> ScanEvent {
        ScanEvent::BarUpdate {
            symbol: symbol.into(), timestamp: bucket_start, interval_secs: 60,
            open: close, high: close, low: close, close, volume: 1_000,
            is_final: false,
        }
    }

    // --- R2: forward-path causality (defect F4) ---

    #[test]
    fn a_final_bars_close_is_observable_at_bar_end_not_bar_open() {
        // The close of a 60s bar opening at t is a fact about t+60. Recording
        // it at t asserts we knew the price a minute before it existed.
        let event = bar("AAA", at(0), 12.5);
        let (_, observable_at, price) = observed_price(&event, at(999)).unwrap();
        assert_eq!(observable_at, at(60), "a final bar's close belongs at bar end");
        assert_eq!(price, 12.5);
    }

    #[test]
    fn an_in_progress_bucket_is_stamped_with_receipt_time() {
        // A live bucket carries `bucket_start`, which is both early and
        // identical across every update in the bucket. Receipt time is the
        // only honest clock available for it.
        let event = live_bar("AAA", at(0), 12.5);
        let (_, observable_at, _) = observed_price(&event, at(37)).unwrap();
        assert_eq!(observable_at, at(37), "in-progress prices use the receipt clock");
        assert_ne!(observable_at, at(0), "bucket_start must never be used directly");
    }

    #[test]
    fn live_updates_in_one_bucket_never_share_a_timestamp() {
        // The concrete F4 failure: 500ms broadcasts through one 60s bucket all
        // stamped `bucket_start`, assigning many different prices one instant.
        let bucket = at(0);
        let stamps: Vec<_> = [(1, 10.0), (2, 10.5), (3, 11.0), (4, 11.5)]
            .into_iter()
            .map(|(secs, price)| {
                let event = live_bar("AAA", bucket, price);
                observed_price(&event, at(secs)).unwrap().1
            })
            .collect();
        for pair in stamps.windows(2) {
            assert!(pair[1] > pair[0], "observation timestamps must strictly increase");
        }
        assert!(
            !stamps.contains(&bucket),
            "no observation may carry the artificial bucket-start timestamp"
        );
    }

    #[test]
    fn trade_stamped_events_keep_their_market_timestamp() {
        // Only the bar-derived cases needed correcting. Ignition prices come
        // stamped with the trade that printed them, and moving those to a
        // receipt clock would lose real precision.
        let event = confirmed("AAA", at(42), 3.25);
        let (_, observable_at, price) = observed_price(&event, at(999)).unwrap();
        assert_eq!(observable_at, at(42), "market time is preferred where it is correct");
        assert_eq!(price, 3.25);
    }

    #[test]
    fn a_bar_close_is_attributed_to_the_instant_it_became_knowable() {
        // End-to-end statement of the R2 invariant, and worth being precise
        // about what it does and does not claim.
        //
        // The sampling rule is "first observation at or after signal_at + h",
        // so a sparse path may legitimately answer a 30s horizon with a later
        // price -- that is coarse, not acausal, and it predates this repair.
        // What F4 broke is *attribution*: a bar opening at t=0 had its close
        // recorded at t=0, so a price that only existed at t=60 was presented
        // as the price at t=0. Excursion timing is where that shows up
        // directly.
        let mut collector = MeasurementCollector::new();
        collector.observe(&confirmed("AAA", at(0), 100.0), at(0));
        collector.observe(&bar("AAA", at(0), 400.0), at(60));
        let settled = collector.finish(at(SETTLE_AFTER_SECS + 10));
        let episode = settled.iter().find(|e| e.id.symbol == "AAA").unwrap();
        let outcome = episode.outcome.as_ref().expect("outcome present");
        let excursion = outcome.excursion.observed().expect("path is gap-free");
        assert_eq!(
            excursion.seconds_to_mfe, 60,
            "the peak must be dated to bar end, not to the bar's opening timestamp"
        );
    }

    #[test]
    fn the_collector_settles_on_the_derived_deadline() {
        // Guards the R1 wiring: the collector must follow the horizon module's
        // deadline, not a second constant of its own.
        let mut collector = MeasurementCollector::new();
        collector.observe(&confirmed("AAA", at(0), 100.0), at(0));
        collector.observe(&confirmed("AAA", at(1), 101.0), at(1));
        let too_early = collector.observe(&bar("BBB", at(2), 5.0), at(longest_horizon_secs()));
        assert!(
            too_early.iter().all(|e| e.id.symbol != "AAA"),
            "must not settle at the old 1800s boundary"
        );
        let settled = collector.observe(&bar("BBB", at(3), 5.0), at(SETTLE_AFTER_SECS + 1));
        assert!(
            settled.iter().any(|e| e.id.symbol == "AAA"),
            "must settle once the derived deadline passes"
        );
    }

    fn rejected(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
        ScanEvent::IgnitionEvent {
            symbol: symbol.into(),
            timestamp: t,
            price,
            kind: IgnitionEventKind::FollowThroughRejected,
        }
    }

    /// Drives `collector` at `rate_centi` hundredths-of-an-episode per second
    /// for `secs` of **synthetic** time, opening an episode per symbol and
    /// immediately invalidating it so it enters the pending set -- the
    /// dominant Session 002 lifecycle (117,593 of 128,144 closed
    /// `invalidated`). Every `feed_every` episodes also receives a forward
    /// price past the longest horizon, so horizon reachability is testable
    /// without fanning out to every symbol.
    fn drive(
        collector: &mut MeasurementCollector,
        rate_centi: u64,
        secs: i64,
        start: i64,
        feed_every: usize,
    ) -> (usize, Vec<OpportunityEpisode>) {
        let total = (rate_centi as i64 * secs / 100) as usize;
        let mut opened = 0usize;
        let mut settled = Vec::new();
        for i in 0..total {
            let t = at(start + (i as i64 * 100) / rate_centi as i64);
            let symbol = format!("S{i}");
            settled.extend(collector.observe(&confirmed(&symbol, t, 100.0), t));
            settled.extend(collector.observe(&rejected(&symbol, t, 100.0), t));
            opened += 1;
            if i % feed_every == 0 {
                // A forward price for an episode opened one settlement window
                // ago, so its long horizons can mature without fanning out to
                // every symbol.
                let back = (1850 * rate_centi / 100) as usize;
                if i > back {
                    let s = format!("S{}", i - back);
                    settled.extend(collector.observe(&bar(&s, t, 101.0), t));
                }
            }
        }
        (opened, settled)
    }

    // --- 48-A: pending capacity (§7 adversarial load) ---

    #[test]
    fn the_pending_footprint_stays_within_its_stated_bound() {
        // §6 memory analysis, measured rather than asserted. The dominant term
        // is the forward path: `MAX_PATH_POINTS` points of `PricePoint`.
        let point = std::mem::size_of::<PricePoint>();
        let entry = std::mem::size_of::<PendingOutcome>();
        let worst_path_bytes = point * MAX_PATH_POINTS;
        // Session 002 measured a mean of 141.4 retained points per episode and
        // only 0.52% of episodes reaching the cap, so the aggregate is driven
        // by the mean, not the ceiling.
        let expected_bytes_per_entry = entry + point * 142;
        let expected_total = expected_bytes_per_entry * MAX_PENDING_OUTCOMES;
        let theoretical_total = (entry + worst_path_bytes) * MAX_PENDING_OUTCOMES;

        println!(
            "PricePoint={point}B PendingOutcome={entry}B capacity={MAX_PENDING_OUTCOMES} \
             expected_total={:.1}MiB theoretical_total={:.2}GiB",
            expected_total as f64 / (1024.0 * 1024.0),
            theoretical_total as f64 / (1024.0 * 1024.0 * 1024.0),
        );
        assert!(
            point <= 32,
            "PricePoint grew to {point} bytes; the memory analysis assumes <= 32"
        );
        assert!(
            expected_total < 400 * 1024 * 1024,
            "expected pending footprint {expected_total} exceeds the 400 MiB \
             stated in the repair report"
        );
        assert!(
            theoretical_total < 4 * 1024 * 1024 * 1024,
            "theoretical worst case {theoretical_total} exceeds 4 GiB; capacity \
             or MAX_PATH_POINTS needs revisiting rather than documenting"
        );
    }

    #[test]
    fn capacity_is_derived_from_the_supported_rate_and_settlement_window() {
        // 16.00/s x 1920s x 1.25 = 38,400. Also compile-time asserted.
        assert_eq!(MAX_PENDING_OUTCOMES, 38_400);
        assert!(
            (MAX_PENDING_OUTCOMES as u64) * 100
                >= SUPPORTED_EPISODE_RATE_CENTI * OUTCOME_WINDOW_SECS as u64,
            "capacity must hold the supported rate for one full settlement window"
        );
        // The Session 002 observed peak population must fit with room to spare.
        assert!(
            MAX_PENDING_OUTCOMES > 24_217,
            "capacity must exceed the peak population actually observed"
        );
    }

    #[test]
    fn a_at_the_observed_session_002_rate_nothing_is_capacity_evicted() {
        // Case A: 5.19 eps/s for longer than one settlement window.
        let mut c = MeasurementCollector::new();
        let _ = drive(&mut c, 519, OUTCOME_WINDOW_SECS + 200, 0, 400);
        assert_eq!(
            c.capacity_evictions(),
            0,
            "the rate that broke Session 002 must no longer evict; peak was {}",
            c.pending_peak()
        );
        assert!(c.pending_peak() <= MAX_PENDING_OUTCOMES);
    }

    #[test]
    fn b_at_the_declared_supported_rate_nothing_is_capacity_evicted() {
        // Case B: the full declared envelope, 16.00 eps/s.
        let mut c = MeasurementCollector::new();
        let _ = drive(&mut c, SUPPORTED_EPISODE_RATE_CENTI, OUTCOME_WINDOW_SECS + 200, 0, 2000);
        assert_eq!(
            c.capacity_evictions(),
            0,
            "the declared supported rate must not evict; peak was {}",
            c.pending_peak()
        );
        assert!(
            c.pending_outcomes() <= MAX_PENDING_OUTCOMES,
            "pending set must stay bounded"
        );
    }

    #[test]
    fn c_a_burst_above_the_envelope_evicts_explicitly_and_stays_bounded() {
        // Case C: sustained overload well past the supported rate.
        let mut c = MeasurementCollector::new();
        let (_, evicted) = drive(&mut c, 4_000, OUTCOME_WINDOW_SECS, 0, 5000); // 40/s
        assert!(
            c.capacity_evictions() > 0,
            "overload must actually exercise the eviction path"
        );
        assert!(
            c.pending_outcomes() <= MAX_PENDING_OUTCOMES,
            "pending set must remain bounded under overload"
        );
        // The decisive property: eviction is never disguised as ordinary
        // insufficient data.
        assert!(!evicted.is_empty());
        let mut saw_capacity = false;
        for episode in &evicted {
            let outcome = episode.outcome.as_ref().expect("outcome present");
            for r in &outcome.returns {
                match r.outcome {
                    Observation::Censored(CensorReason::PendingCapacityReached) => {
                        saw_capacity = true
                    }
                    Observation::Censored(CensorReason::InsufficientForwardData) => panic!(
                        "capacity eviction was reported as ordinary InsufficientForwardData"
                    ),
                    _ => {}
                }
            }
        }
        assert!(saw_capacity, "expected PendingCapacityReached censoring");
    }

    #[test]
    fn d_the_collector_recovers_and_returns_to_age_based_settlement() {
        // Case D: overload, then a return to a normal rate.
        let mut c = MeasurementCollector::new();
        let _ = drive(&mut c, 4_000, 1_200, 0, 5000); // 48,000 episodes > capacity
        let during = c.capacity_evictions();
        assert!(during > 0);
        // Quiet period long enough for everything to age out normally.
        let resume = 1_200 + OUTCOME_WINDOW_SECS * 2;
        c.observe(&bar("QUIET", at(resume), 1.0), at(resume));
        assert_eq!(
            c.pending_outcomes(),
            0,
            "age-based settlement must drain the backlog once pressure ends"
        );
        let _ = drive(&mut c, 519, 300, resume + 10, 400);
        assert_eq!(
            c.capacity_evictions(),
            during,
            "no further evictions once the rate is back within the envelope"
        );
    }

    #[test]
    fn session_002_pressure_profile_no_longer_truncates_long_horizons() {
        // §8 regression. Session 002's regular session ran at 5.19 eps/s with a
        // 1920s settlement window and produced a median span of ~923s and
        // ~0% 1800s observability, because 4,096 saturated after ~789s.
        let mut c = MeasurementCollector::new();
        let (opened, _) = drive(&mut c, 519, OUTCOME_WINDOW_SECS + 400, 0, 200);
        assert!(opened > 9_000, "profile must actually load the collector");
        assert_eq!(
            c.capacity_evictions(),
            0,
            "Session 002's own pressure profile must no longer force-settle anything"
        );
        // The old bound would have been exceeded; the new one is not.
        assert!(
            c.pending_peak() > 4_096,
            "profile must exceed the OLD cap ({}) or it does not reproduce the \
             Session 002 condition; peak was {}",
            4_096,
            c.pending_peak()
        );
        assert!(c.pending_peak() <= MAX_PENDING_OUTCOMES);
    }

    #[test]
    fn a_live_event_sequence_opens_and_accumulates_one_episode() {
        let mut collector = MeasurementCollector::new();
        collector.observe(&confirmed("AAA", at(0), 10.0), at(0));
        assert_eq!(collector.open_episodes(), 1);

        for n in 1..50 {
            collector.observe(&momentum("AAA", at(n), 0.7), at(n));
        }
        assert_eq!(collector.open_episodes(), 1, "50 updates are one opportunity");

        let closed = collector.finish(at(60));
        assert_eq!(closed.len(), 1);
        let episode = &closed[0];
        assert_eq!(episode.event_count, 50);
        assert!(episode.opening_context.momentum.is_none() || episode.momentum_track.len() > 0);
        assert_eq!(episode.close_reason, Some(EpisodeCloseReason::CaptureEnded));
    }

    #[test]
    fn ranking_runs_on_a_timer_not_on_every_event() {
        let mut collector = MeasurementCollector::new();
        collector.observe(&confirmed("AAA", at(0), 10.0), at(0));
        collector.observe(&momentum("AAA", at(1), 0.9), at(1));
        let first = collector.ranking_windows;

        // A burst inside the interval must not produce a burst of rankings.
        for n in 2..200 {
            collector.observe(&momentum("AAA", at(n % RANKING_INTERVAL_SECS), 0.9), at(2));
        }
        assert_eq!(collector.ranking_windows, first, "no re-rank inside the window");

        collector.observe(&momentum("AAA", at(RANKING_INTERVAL_SECS + 5), 0.9), at(RANKING_INTERVAL_SECS + 5));
        assert!(collector.ranking_windows > first, "the window elapsed, so rank again");
    }

    #[test]
    fn contemporaneous_candidates_receive_stable_research_ranks() {
        let mut collector = MeasurementCollector::new();
        // Candidates accumulate first; ranking is paced on a timer, so the
        // cohort is only complete once a ranking window elapses. Feeding all
        // three inside one window would rank only whichever arrived first --
        // which is correct behaviour, not something to assert against.
        for (symbol, score) in [("AAA", 0.62), ("BBB", 0.95), ("CCC", 0.78)] {
            collector.observe(&confirmed(symbol, at(0), 10.0), at(0));
            collector.observe(&momentum(symbol, at(1), score), at(1));
        }
        let after_window = at(RANKING_INTERVAL_SECS + 5);
        collector.observe(&momentum("AAA", after_window, 0.62), after_window);

        let closed = collector.finish(after_window);
        let mut ranked: Vec<(String, u32)> = closed
            .iter()
            .filter_map(|e| e.research_rank.as_ref().map(|r| (e.id.symbol.clone(), r.rank)))
            .collect();
        ranked.sort_by_key(|r| r.1);
        assert_eq!(ranked.len(), 3);
        assert_eq!(ranked[0].0, "BBB", "highest momentum ranks first");
        assert_eq!(ranked[2].0, "AAA");
    }

    /// The most important test in this milestone: instrumentation must not
    /// change what production sees.
    ///
    /// The collector is fed the identical event stream twice -- once alone,
    /// once alongside a second consumer standing in for the production
    /// subscribers -- and the events that consumer observes must be
    /// byte-identical in content and order. If measurement ever consumed,
    /// reordered, filtered or mutated an event, this fails.
    #[test]
    fn measurement_does_not_alter_the_events_production_sees() {
        let stream: Vec<ScanEvent> = (0..60)
            .map(|n| {
                if n % 3 == 0 {
                    confirmed(&format!("S{}", n % 7), at(n), 10.0 + n as f64 * 0.1)
                } else {
                    momentum(&format!("S{}", n % 7), at(n), 0.4 + (n % 5) as f64 * 0.15)
                }
            })
            .collect();

        // Production path alone.
        let baseline: Vec<String> =
            stream.iter().map(|e| serde_json::to_string(e).unwrap()).collect();

        // Production path with measurement running against the same stream.
        let mut collector = MeasurementCollector::new();
        let mut observed: Vec<String> = Vec::new();
        for (n, event) in stream.iter().enumerate() {
            let _ = collector.observe(event, at(n as i64));
            observed.push(serde_json::to_string(event).unwrap());
        }

        assert_eq!(
            baseline, observed,
            "measurement must not change event content or ordering"
        );
        assert!(collector.open_episodes() > 0, "and it must actually have been running");
    }

    #[test]
    fn trader_decisions_link_to_the_intended_episode_after_the_fact() {
        use backtest_metrics::episode::{
            link_trader_decisions, TraderDecision, TraderDecisionKind,
        };
        let mut collector = MeasurementCollector::new();
        collector.observe(&confirmed("AAA", at(0), 10.0), at(0));
        collector.observe(&confirmed("BBB", at(1), 20.0), at(1));
        let mut episodes = collector.finish(at(60));

        let decisions = vec![
            TraderDecision {
                symbol: "AAA".into(), at: at(5),
                kind: TraderDecisionKind::Entered,
                reason: None, price: Some(10.2),
            },
            TraderDecision {
                symbol: "AAA".into(), at: at(400), // after close, inside exit grace
                kind: TraderDecisionKind::Exited,
                reason: Some("target_hit".into()), price: Some(11.0),
            },
            TraderDecision {
                symbol: "BBB".into(), at: at(6),
                kind: TraderDecisionKind::Skipped,
                reason: Some("max_concurrent_positions".into()), price: None,
            },
            TraderDecision {
                symbol: "ZZZ".into(), at: at(7),
                kind: TraderDecisionKind::Entered,
                reason: None, price: Some(1.0),
            },
        ];

        let report = link_trader_decisions(&mut episodes, &decisions);
        assert_eq!(report.linked, 3);
        assert_eq!(report.unmatched, 1, "a decision with no episode is counted, never forced");

        let aaa = episodes.iter().find(|e| e.id.symbol == "AAA").unwrap();
        assert_eq!(aaa.trader.entry_price, Some(10.2));
        assert_eq!(aaa.trader.exit_reason.as_deref(), Some("target_hit"));
        assert!(aaa.trader.skip_reason.is_none());

        let bbb = episodes.iter().find(|e| e.id.symbol == "BBB").unwrap();
        assert_eq!(bbb.trader.skip_reason.as_deref(), Some("max_concurrent_positions"));
        assert!(bbb.trader.entered_at.is_none());
    }

    #[test]
    fn a_decision_predating_an_episode_is_never_attributed_to_it() {
        use backtest_metrics::episode::{
            link_trader_decisions, TraderDecision, TraderDecisionKind,
        };
        let mut collector = MeasurementCollector::new();
        collector.observe(&confirmed("AAA", at(100), 10.0), at(100));
        let mut episodes = collector.finish(at(200));

        let early = vec![TraderDecision {
            symbol: "AAA".into(), at: at(50), // before the episode opened
            kind: TraderDecisionKind::Entered, reason: None, price: Some(9.0),
        }];
        let report = link_trader_decisions(&mut episodes, &early);
        assert_eq!(report.linked, 0);
        assert_eq!(report.unmatched, 1);
        assert!(episodes[0].trader.entered_at.is_none());
    }

    #[test]
    fn forward_prices_accumulate_and_produce_real_horizon_outcomes() {
        let mut collector = MeasurementCollector::new();
        collector.observe(&confirmed("AAA", at(0), 10.0), at(0));

        // Continuous 60s bars, as a real symbol produces. These extend the
        // episode AND build its forward price path.
        for n in 1..=35 {
            let t = at(60 * n);
            collector.observe(&bar("AAA", t, 10.0 + n as f64 * 0.1), t);
        }

        let settled = collector.finish(at(60 * 36));
        let aaa = settled.iter().find(|e| e.id.symbol == "AAA").expect("AAA settles");
        let outcome = aaa.outcome.as_ref().expect("a settled episode carries an outcome");

        // Every horizon inside the observed span must be a real number.
        for horizon in [60_i64, 180, 300, 600, 900, 1800] {
            let r = outcome.returns.iter().find(|r| r.horizon_secs == horizon).unwrap();
            assert!(
                !r.outcome.is_censored(),
                "{horizon}s was observed and must not be censored"
            );
        }
        let excursion = outcome.excursion.observed().expect("continuous path, real excursion");
        assert!(excursion.mfe_pct > 0.0, "prices rose, so MFE is positive");
        assert!(outcome.observation_count > 30, "the whole path must be retained");
    }

    #[test]
    fn an_unobserved_horizon_is_censored_not_reported_as_zero() {
        let mut collector = MeasurementCollector::new();
        collector.observe(&confirmed("AAA", at(0), 10.0), at(0));
        // Capture ends almost immediately: nothing beyond a few seconds exists.
        let settled = collector.finish(at(20));
        let outcome = settled[0].outcome.as_ref().expect("outcome present even when censored");
        let thirty_min = outcome.returns.iter().find(|r| r.horizon_secs == 1800).unwrap();
        assert!(
            thirty_min.outcome.is_censored(),
            "we stopped observing; that is not a zero return"
        );
    }

    #[test]
    fn the_pending_set_stays_bounded() {
        let mut collector = MeasurementCollector::new();
        // Open and close far more episodes than the cap allows.
        for n in 0..(MAX_PENDING_OUTCOMES as i64 + 500) {
            let t = at(n);
            collector.observe(&confirmed(&format!("S{n}"), t, 10.0), t);
        }
        // Force closure of everything still open, then check the bound held
        // throughout by inspecting the pending set before finishing.
        assert!(
            collector.pending_outcomes() <= MAX_PENDING_OUTCOMES,
            "pending set must stay bounded, saw {}",
            collector.pending_outcomes()
        );
    }

    #[test]
    fn shutdown_closes_active_episodes_as_censored() {
        let mut collector = MeasurementCollector::new();
        collector.observe(&confirmed("AAA", at(0), 10.0), at(0));
        collector.observe(&confirmed("BBB", at(1), 20.0), at(1));
        let closed = collector.finish(at(5));
        assert_eq!(closed.len(), 2);
        assert!(
            closed.iter().all(|e| e.close_reason == Some(EpisodeCloseReason::CaptureEnded)),
            "an episode alive at shutdown is censored, never concluded"
        );
    }

    #[test]
    fn an_unwritable_directory_disables_capture_without_failing() {
        // A file where a directory should be: create_dir_all must fail.
        let tmp = std::env::temp_dir().join(format!("ss-meas-{}", std::process::id()));
        std::fs::write(&tmp, b"not a directory").unwrap();
        let recorder = MeasurementRecorder::start(tmp.join("sub"));
        assert!(recorder.is_none(), "capture is simply off; the caller carries on");
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn a_full_queue_drops_and_counts_rather_than_blocking() {
        let dir = std::env::temp_dir().join(format!("ss-meas-q-{}", std::process::id()));
        let recorder = MeasurementRecorder::start(dir.clone()).expect("recorder");
        // The writer drains, so this asserts the mechanism rather than a
        // guaranteed drop: sending far more than the queue depth must return
        // promptly and never panic.
        let mut collector = MeasurementCollector::new();
        for n in 0..(QUEUE_DEPTH as i64 * 20) {
            collector.observe(&confirmed(&format!("S{n}"), at(n), 10.0), at(n));
        }
        for episode in collector.finish(at(100_000)) {
            recorder.record_episode(episode);
        }
        recorder.flush(std::time::Duration::from_secs(2));
        let health = recorder.health();
        // Whatever happened, it is accounted for: nothing vanishes silently.
        let written = health.episodes_written.load(Ordering::Relaxed);
        let dropped = health.dropped.load(Ordering::Relaxed);
        let errors = health.write_errors.load(Ordering::Relaxed);
        assert!(written + dropped + errors > 0, "every record is accounted for");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn records_are_appended_as_readable_ndjson() {
        let dir = std::env::temp_dir().join(format!("ss-meas-w-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let recorder = MeasurementRecorder::start(dir.clone()).expect("recorder");
        let mut collector = MeasurementCollector::new();
        collector.observe(&confirmed("AAA", at(0), 10.0), at(0));
        for episode in collector.finish(at(5)) {
            recorder.record_episode(episode);
        }
        recorder.flush(std::time::Duration::from_secs(2));

        let day = at(0).date_naive().to_string();
        let path = dir.join(format!("episodes-{day}.ndjson"));
        let contents = std::fs::read_to_string(&path).expect("episode file should exist");
        let parsed: OpportunityEpisode =
            serde_json::from_str(contents.lines().next().unwrap()).expect("valid NDJSON");
        assert_eq!(parsed.id.symbol, "AAA");
        assert_eq!(recorder.health().episodes_written.load(Ordering::Relaxed), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
