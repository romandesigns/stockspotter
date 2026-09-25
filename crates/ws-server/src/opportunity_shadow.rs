//! Opportunity Intelligence shadow persistence — an **independent consumer** of
//! the already-broadcast `ScanEvent` stream.
//!
//! Deliberately a sibling of `measurement.rs`, not an extension of it:
//!
//! * It subscribes to the same `broadcast` channel and receives the same events
//!   *after* they have been dispatched. It cannot reorder, suppress, delay or
//!   mutate anything a client or the auto-trader sees.
//! * Nothing it produces is read by a detector, by client ordering, or by
//!   `auto_trader`. There is no path from this module back into production --
//!   the only writer is an append-only research file.
//! * It is bounded in the same three places the measurement collector had to be
//!   (open state, queue depth, write path) and it counts its own drops. The
//!   `PendingCapacityReached` incident is the precedent: an unbounded or
//!   silently-saturating research subsystem is worse than none.
//!
//! Writes happen on a dedicated thread behind a bounded queue, and the
//! market-facing side only ever offers without blocking, so disk latency can
//! never reach dispatch.
//!
//! # The September-16 capacity repair
//!
//! The queue used to be 64 records deep and the writer reopened the target file
//! for every line. `rank` emits the *entire* open cohort synchronously -- 3,280
//! records on average during the September-16 regular session -- so the queue
//! held about 2% of one emission. 2,276,531 of 2,558,786 snapshots were
//! discarded, an 11.0% capture rate, and the loss was visible only as
//! power-of-two log lines.
//!
//! Both halves are now derived from that measurement: see `QUEUE_RECORDS` here
//! and `OiConfig::max_open_opportunities` for the engine bound that was
//! truncating the cohort before the writer ever saw it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use backtest_metrics::opportunity::{
    CohortTruncation, OiConfig, OpportunityIntelligence, OpportunityScoreSnapshot,
};
use backtest_metrics::opportunity_outcome::ClosureNotice;
use chrono::{DateTime, Utc};
use market_data::ScanEvent;
use tracing::{info, warn};

use crate::research_writer::{Bounds, Naming, ResearchWriter, WriterHealth};

/// Queue depth, in records.
///
/// **One whole ranking window at the engine's capacity.** `rank` emits one
/// snapshot per open opportunity in a single synchronous loop, so the only
/// depth that cannot be overrun by construction is one that holds a full
/// cohort. `OiConfig::max_open_opportunities` is 16,375, so 16,384 covers it
/// with the queue never being the binding constraint -- which is the property
/// the previous 64 lacked by a factor of 256.
const QUEUE_RECORDS: usize = 16_384;

/// Queue bound in bytes.
///
/// Measured: the preserved September-16 capture is 1,359,083,113 bytes over
/// 352,640 records, a mean of 3,854 B. 96 MiB therefore holds a full 16,384-
/// record window at 6 KB each, comfortably above the observed mean, and caps
/// the writer's worst-case footprint at a figure that can be stated rather
/// than hoped for. A record bound alone would bound an unknown quantity.
const QUEUE_BYTES: u64 = 96 * 1024 * 1024;

const STEM: &str = "opportunity-intelligence";

/// Counters describing shadow-capture completeness. Non-zero values are
/// findings, not noise.
///
/// Now a thin alias over the shared writer's accounting: the previous struct
/// counted drops and writes but not *attempts*, so a capture rate could only
/// be computed by reconstructing the denominator from the engine's cohort
/// sizes afterwards. That reconstruction is what the September-16 gate had to
/// do, and it is not a property an instrument should require.
pub type ShadowHealth = WriterHealth;

/// Append-only NDJSON writer for shadow scoring decisions.
pub struct ShadowRecorder {
    writer: ResearchWriter,
}

impl ShadowRecorder {
    /// Starts the writer, or returns `None` when the directory is unusable --
    /// research capture must degrade to *off*, never take the service down.
    pub fn start(dir: PathBuf) -> Option<Self> {
        Self::start_bounded(dir, Bounds { records: QUEUE_RECORDS, bytes: QUEUE_BYTES }, None)
    }

    /// `gate`, when supplied, is waited on before the writer drains anything.
    ///
    /// It exists so the queue bound is *exercisable* rather than merely
    /// asserted: with a live writer thread a 16,384-deep channel never fills in
    /// a test, and a drop counter no test can reach is indistinguishable from a
    /// drop counter that does not work. That is the measurement lesson applied
    /// to the measurement code itself.
    #[cfg(test)]
    pub fn start_inner(
        dir: PathBuf,
        depth: usize,
        gate: Option<Arc<std::sync::Barrier>>,
    ) -> Option<Self> {
        // Byte bound lifted out of the way so `depth` is unambiguously what
        // binds: a test that means to exercise the record bound must not
        // accidentally be exercising the byte bound.
        Self::start_bounded(dir, Bounds { records: depth, bytes: u64::MAX / 2 }, gate)
    }

    fn start_bounded(
        dir: PathBuf,
        bounds: Bounds,
        gate: Option<Arc<std::sync::Barrier>>,
    ) -> Option<Self> {
        let naming = Naming { dir, stem: STEM.to_string() };
        ResearchWriter::start_inner(naming, bounds, gate).map(|writer| Self { writer })
    }

    pub fn health(&self) -> &Arc<ShadowHealth> {
        self.writer.health()
    }

    /// Non-blocking. A full queue drops and counts; it never waits, so disk
    /// latency cannot reach market dispatch.
    pub fn record(&self, snapshot: &OpportunityScoreSnapshot) {
        self.writer.record(snapshot, snapshot.timestamp.date_naive());
    }

    /// Persists one capacity-eviction marker.
    ///
    /// Section 4: an eviction must be discoverable from the artifact, naming
    /// the opportunity, the capacity and the population that forced it. The
    /// September-16 session could establish that capacity had bound only by
    /// noticing cohort sizes pinned at exactly 3,750 in the surviving records.
    pub fn capacity_eviction(&self, eviction: &backtest_metrics::opportunity::CapacityEviction) {
        self.writer.marker(
            "opportunity_capacity_reached",
            serde_json::to_value(eviction).ok(),
        );
    }

    /// Bounded drain, for shutdown only.
    pub fn flush(&self, timeout: std::time::Duration) {
        self.writer.flush(timeout);
    }

    /// Persists one opportunity close as an `opportunity_closed` marker (D4).
    ///
    /// # Why it is persisted at all
    ///
    /// Until D4 no close reached any artifact, so a historical session's
    /// dispositions are unrecoverable -- nothing short of the raw event
    /// stream, which is not kept, can say when an opportunity closed or why.
    /// With these records `oi_outcome_replay --closures` reproduces every
    /// row's disposition offline, through the same collector rule the live
    /// path uses.
    ///
    /// # Why a marker and not a data record
    ///
    /// The data file is read line-by-line as `OpportunityScoreSnapshot` by
    /// the alpha dataset reader and the integrity check, and a line of any
    /// other shape counts as *malformed* -- which is blocking. The marker file
    /// already carries self-describing events of exactly this kind
    /// (`opportunity_capacity_reached`).
    ///
    /// # Bounds, and what loss looks like
    ///
    /// One marker per close: ~0.85/s over a regular session (~20k/day,
    /// ~250 B each), through the same bounded, non-blocking queue as every
    /// other record. They cannot crowd a ranking window out of that queue:
    /// within one step, closes plus snapshots are at most the open set before
    /// the step plus the one opportunity it may open (a closed opportunity is
    /// never ranked), i.e. <= 16,376 against a 16,384-record queue sized for
    /// exactly one window. Marker loss is deliberately *not* counted as data loss
    /// by the writer, so a replay must reconcile before trusting
    /// dispositions: the count of `opportunity_closed` markers must equal
    /// `opportunitiesClosed` in the capture's `capture_finished` marker (or
    /// `opportunityEngine.closedByReason` on the live health route). Any
    /// shortfall means those dispositions are unknown for the affected
    /// opportunities, not `still_open`.
    pub fn opportunity_closed(&self, notice: &ClosureNotice) {
        self.writer.marker("opportunity_closed", serde_json::to_value(notice).ok());
    }

    /// Persists one D6 `ranking_cohort_truncated` marker. Unreachable under
    /// the shipped configuration; see `CohortTruncation`.
    pub fn cohort_truncation(&self, truncation: &CohortTruncation) {
        self.writer.marker("ranking_cohort_truncated", serde_json::to_value(truncation).ok());
    }

    pub fn marker(&self, kind: &str, data: Option<serde_json::Value>) {
        self.writer.marker(kind, data);
    }
}

/// What one observation produced: the ranking snapshots (the outcome
/// collector's anchors) and the closes it caused (D4).
///
/// Returned together so a caller cannot take one and silently drop the other
/// -- dropping the closes is exactly how every production outcome row came to
/// say `still_open` (`let _closed = ...` here, before D4). The live loop must
/// hand `closures` to the outcome collector **before** it settles the step;
/// `OutcomeDriver::advance` does both in that order.
#[derive(Debug, Default)]
#[must_use = "a step's closures must reach the outcome collector before it settles (D4)"]
pub struct ShadowStep {
    pub snapshots: Vec<OpportunityScoreSnapshot>,
    pub closures: Vec<ClosureNotice>,
}

// ---------------------------------------------------------------------------
// Engine health, readable while the session is still running
// ---------------------------------------------------------------------------

/// The engine's capacity accounting, published as atomics so the research
/// health surface can read it without touching the driver.
///
/// `OiHealth` lives inside the engine, inside a `tokio` task that owns it for
/// the life of the process. On September 16 that meant `capacity_evictions`
/// was knowable only at shutdown -- which is exactly when it is too late to
/// matter. Section 4: "No restart should be required to learn this."
#[derive(Debug, Default)]
pub struct EngineHealth {
    pub open: AtomicUsize,
    pub peak: AtomicUsize,
    pub capacity: AtomicUsize,
    pub capacity_evictions: AtomicU64,
    pub eviction_markers_dropped: AtomicU64,
    pub opportunities_opened: AtomicU64,
    pub opportunities_closed: AtomicU64,
    pub cohort_truncations: AtomicU64,
    pub scores_emitted: AtomicU64,
    // --- D6 ranking cohort ---------------------------------------------------
    pub rank_cohort_capacity: AtomicUsize,
    pub early_cohort_truncations: AtomicU64,
    pub continuation_cohort_truncations: AtomicU64,
    pub truncation_markers_dropped: AtomicU64,
    pub early_cohort_last: AtomicUsize,
    pub continuation_cohort_last: AtomicUsize,
    pub early_cohort_peak: AtomicUsize,
    pub continuation_cohort_peak: AtomicUsize,
    pub ranking_windows: AtomicU64,
    /// Wall-clock cost of the most recent `rank()` that produced a window, and
    /// the worst seen, in microseconds. Operational evidence for the D6 cost
    /// claim (+0-1 ms/window at the production peak); measured here in the
    /// driver rather than in the engine so the engine's health stays a pure
    /// function of its input and replay-comparable.
    pub last_rank_micros: AtomicU64,
    pub peak_rank_micros: AtomicU64,
    // --- D4 closes, by the engine's reason ----------------------------------
    pub closed_inactivity: AtomicU64,
    pub closed_session_boundary: AtomicU64,
    pub closed_capacity_reached: AtomicU64,
    pub closed_capture_ended: AtomicU64,
    /// D5 `move-v1` close reasons.
    pub closed_setup_inactivity: AtomicU64,
    pub closed_invalidated: AtomicU64,
    /// D5: opens refused by the duplicate-identity guard. A qualification
    /// gate (preregistration section 7).
    pub duplicate_identity_refused: AtomicU64,
    /// `OiVersions::lifecycle` of the engine this publishes for. Fixed at
    /// construction, so a `OnceLock` rather than an atomic.
    pub lifecycle: std::sync::OnceLock<String>,
    /// UTC `sessionDate` of the most recently opened opportunity, as days
    /// since 0001-01-01 (`NaiveDate::num_days_from_ce`); 0 = none yet. This is
    /// the date the engine is *assigning* to identities right now, which is
    /// what a D5 reviewer needs to see around the UTC rollover.
    pub engine_session_date: AtomicI32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineHealthSnapshot {
    pub open: usize,
    pub peak: usize,
    pub capacity: usize,
    pub capacity_evictions: u64,
    pub eviction_markers_dropped: u64,
    pub opportunities_opened: u64,
    pub opportunities_closed: u64,
    pub cohort_truncations: u64,
    pub scores_emitted: u64,
    pub rank_cohort_capacity: usize,
    pub early_cohort_truncations: u64,
    pub continuation_cohort_truncations: u64,
    pub truncation_markers_dropped: u64,
    pub early_cohort_last: usize,
    pub continuation_cohort_last: usize,
    pub early_cohort_peak: usize,
    pub continuation_cohort_peak: usize,
    pub ranking_windows: u64,
    pub last_rank_micros: u64,
    pub peak_rank_micros: u64,
    pub closed_by_reason: backtest_metrics::opportunity::ClosedByReason,
    pub engine_session_date: Option<chrono::NaiveDate>,
    #[serde(default)]
    pub duplicate_identity_refused: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<String>,
}

impl EngineHealth {
    pub fn snapshot(&self) -> EngineHealthSnapshot {
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let u = |a: &AtomicUsize| a.load(Ordering::Relaxed);
        EngineHealthSnapshot {
            open: u(&self.open),
            peak: u(&self.peak),
            capacity: u(&self.capacity),
            capacity_evictions: g(&self.capacity_evictions),
            eviction_markers_dropped: g(&self.eviction_markers_dropped),
            opportunities_opened: g(&self.opportunities_opened),
            opportunities_closed: g(&self.opportunities_closed),
            cohort_truncations: g(&self.cohort_truncations),
            scores_emitted: g(&self.scores_emitted),
            rank_cohort_capacity: u(&self.rank_cohort_capacity),
            early_cohort_truncations: g(&self.early_cohort_truncations),
            continuation_cohort_truncations: g(&self.continuation_cohort_truncations),
            truncation_markers_dropped: g(&self.truncation_markers_dropped),
            early_cohort_last: u(&self.early_cohort_last),
            continuation_cohort_last: u(&self.continuation_cohort_last),
            early_cohort_peak: u(&self.early_cohort_peak),
            continuation_cohort_peak: u(&self.continuation_cohort_peak),
            ranking_windows: g(&self.ranking_windows),
            last_rank_micros: g(&self.last_rank_micros),
            peak_rank_micros: g(&self.peak_rank_micros),
            closed_by_reason: backtest_metrics::opportunity::ClosedByReason {
                inactivity: g(&self.closed_inactivity),
                session_boundary: g(&self.closed_session_boundary),
                capacity_reached: g(&self.closed_capacity_reached),
                capture_ended: g(&self.closed_capture_ended),
                setup_inactivity: g(&self.closed_setup_inactivity),
                invalidated: g(&self.closed_invalidated),
            },
            duplicate_identity_refused: g(&self.duplicate_identity_refused),
            lifecycle: self.lifecycle.get().cloned(),
            engine_session_date: match self.engine_session_date.load(Ordering::Relaxed) {
                0 => None,
                days => chrono::NaiveDate::from_num_days_from_ce_opt(days),
            },
        }
    }
}

/// Drives `OpportunityIntelligence` from a live event stream and persists the
/// ranking snapshots it produces.
///
/// The engine itself is shared with offline replay (`replay_stream` below), so
/// live and replay cannot drift apart: there is exactly one implementation of
/// the research logic.
pub struct ShadowDriver {
    engine: OpportunityIntelligence,
    recorder: Option<ShadowRecorder>,
    engine_health: Arc<EngineHealth>,
}

impl ShadowDriver {
    pub fn new(config: OiConfig, recorder: Option<ShadowRecorder>) -> Self {
        let engine_health = Arc::new(EngineHealth::default());
        engine_health
            .capacity
            .store(config.max_open_opportunities(), Ordering::Relaxed);
        engine_health.rank_cohort_capacity.store(config.max_rank_cohort, Ordering::Relaxed);
        let _ = engine_health.lifecycle.set(config.lifecycle.version().to_string());
        Self { engine: OpportunityIntelligence::new(config), recorder, engine_health }
    }

    /// Shared handle onto the engine's capacity accounting, for the research
    /// health surface.
    pub fn engine_health(&self) -> &Arc<EngineHealth> {
        &self.engine_health
    }

    #[cfg(test)]
    pub fn engine(&self) -> &OpportunityIntelligence {
        &self.engine
    }

    /// Capture completeness, or `None` when capture is off. Exposed so a
    /// caller can distinguish "no records" from "records lost", which is the
    /// distinction the measurement milestone had to add retroactively.
    ///
    /// Test-only: production reads the same counters through the research
    /// health surface, which holds the `Arc` directly rather than reaching
    /// through the driver.
    #[cfg(test)]
    pub fn capture_health(&self) -> Option<&Arc<ShadowHealth>> {
        self.recorder.as_ref().map(|r| r.health())
    }

    /// Folds one already-broadcast event in. Returns the snapshots produced
    /// and the closes the event caused, so a caller (or a test) can inspect
    /// them without reading the file.
    ///
    /// Every close is observed at `received_at`: it is the receipt instant of
    /// the event whose processing produced it, which is the first moment this
    /// process could know of it -- including an inactivity expiry triggered by
    /// an unrelated symbol's event.
    pub fn observe(&mut self, event: &ScanEvent, received_at: DateTime<Utc>) -> ShadowStep {
        // Before D4 this was `let _closed = ...`: the engine's closes were
        // thrown away here, so the outcome collector never learned of one and
        // every row said `still_open`. They are now returned to the caller and
        // persisted as `opportunity_closed` markers -- the scoring log itself
        // still records only scoring decisions.
        let closed = self.engine.observe(event, received_at);
        let closures: Vec<ClosureNotice> =
            closed.iter().filter_map(|op| ClosureNotice::from_closed(op, received_at)).collect();
        let started = std::time::Instant::now();
        let ranked = self.engine.rank(received_at);
        if ranked.is_some() {
            let micros = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
            self.engine_health.last_rank_micros.store(micros, Ordering::Relaxed);
            self.engine_health.peak_rank_micros.fetch_max(micros, Ordering::Relaxed);
        }
        let snapshots = ranked.unwrap_or_default();
        let truncations = self.engine.take_cohort_truncations();
        if let Some(recorder) = &self.recorder {
            // Capacity evictions and cohort truncations are not outcomes: they
            // are the instrument reporting that it discarded evidence. Closes
            // are lifecycle facts the outcome replay needs. All three are
            // markers, never data records.
            for eviction in self.engine.take_capacity_evictions() {
                recorder.capacity_eviction(&eviction);
            }
            for truncation in &truncations {
                recorder.cohort_truncation(truncation);
            }
            for notice in &closures {
                recorder.opportunity_closed(notice);
            }
            for snapshot in &snapshots {
                recorder.record(snapshot);
            }
        } else {
            // Keep the buffer bounded even with capture off, so a disabled
            // recorder cannot turn the engine into the thing that grows.
            let _ = self.engine.take_capacity_evictions();
        }
        self.publish_engine_health();
        ShadowStep { snapshots, closures }
    }

    /// Republishes the engine's counters into the shared atomics.
    ///
    /// Cheap enough to run on every observation -- nine relaxed stores against
    /// a path that already does feature-cache work and, on a ranking window,
    /// scores the whole cohort.
    fn publish_engine_health(&self) {
        let h = self.engine.health();
        let s = &self.engine_health;
        s.open.store(h.open_opportunities, Ordering::Relaxed);
        s.peak.store(h.peak_open_opportunities, Ordering::Relaxed);
        s.capacity.store(h.opportunity_capacity, Ordering::Relaxed);
        s.capacity_evictions.store(h.capacity_evictions, Ordering::Relaxed);
        s.eviction_markers_dropped
            .store(self.engine.eviction_markers_dropped(), Ordering::Relaxed);
        s.opportunities_opened.store(h.opportunities_opened, Ordering::Relaxed);
        s.opportunities_closed.store(h.opportunities_closed, Ordering::Relaxed);
        s.cohort_truncations.store(h.cohort_truncations, Ordering::Relaxed);
        s.scores_emitted.store(h.scores_emitted, Ordering::Relaxed);
        s.rank_cohort_capacity.store(h.rank_cohort_capacity, Ordering::Relaxed);
        s.early_cohort_truncations.store(h.early_cohort_truncations, Ordering::Relaxed);
        s.continuation_cohort_truncations
            .store(h.continuation_cohort_truncations, Ordering::Relaxed);
        s.truncation_markers_dropped.store(h.truncation_markers_dropped, Ordering::Relaxed);
        s.early_cohort_last.store(h.early_cohort_last, Ordering::Relaxed);
        s.continuation_cohort_last.store(h.continuation_cohort_last, Ordering::Relaxed);
        s.early_cohort_peak.store(h.early_cohort_peak, Ordering::Relaxed);
        s.continuation_cohort_peak.store(h.continuation_cohort_peak, Ordering::Relaxed);
        s.ranking_windows.store(h.ranking_windows, Ordering::Relaxed);
        let c = h.closed_by_reason;
        s.closed_inactivity.store(c.inactivity, Ordering::Relaxed);
        s.closed_session_boundary.store(c.session_boundary, Ordering::Relaxed);
        s.closed_capacity_reached.store(c.capacity_reached, Ordering::Relaxed);
        s.closed_capture_ended.store(c.capture_ended, Ordering::Relaxed);
        s.closed_setup_inactivity.store(c.setup_inactivity, Ordering::Relaxed);
        s.closed_invalidated.store(c.invalidated, Ordering::Relaxed);
        s.duplicate_identity_refused.store(h.duplicate_identity_refused, Ordering::Relaxed);
        if let Some(date) = self.engine.current_session_date() {
            use chrono::Datelike;
            s.engine_session_date.store(date.num_days_from_ce(), Ordering::Relaxed);
        }
    }

    /// Closes everything still open as `CaptureEnded` and returns those closes,
    /// observed at `at`.
    ///
    /// The caller must hand them to the outcome collector **before** that
    /// collector's own `finish`, so the anchors of opportunities still open at
    /// shutdown carry `capture_ended` rather than `still_open` (D4.4-6).
    pub fn finish(&mut self, at: DateTime<Utc>) -> Vec<ClosureNotice> {
        let closures: Vec<ClosureNotice> = self
            .engine
            .finish(at)
            .iter()
            .filter_map(|op| ClosureNotice::from_closed(op, at))
            .collect();
        self.publish_engine_health();
        let h = self.engine.health().clone();
        let evictions = self.engine.take_capacity_evictions();
        let truncations = self.engine.take_cohort_truncations();
        if let Some(recorder) = &self.recorder {
            for eviction in &evictions {
                recorder.capacity_eviction(eviction);
            }
            for truncation in &truncations {
                recorder.cohort_truncation(truncation);
            }
            for notice in &closures {
                recorder.opportunity_closed(notice);
            }
            recorder.marker(
                "capture_finished",
                serde_json::to_value(&h).ok(),
            );
            recorder.flush(std::time::Duration::from_secs(5));
            let health = recorder.health();
            if health.is_degraded() {
                warn!(
                    attempted = health.attempted.load(Ordering::Relaxed),
                    written = health.written.load(Ordering::Relaxed),
                    dropped = health.dropped.load(Ordering::Relaxed),
                    write_errors = health.write_errors.load(Ordering::Relaxed),
                    "opportunity-intelligence capture finished with gaps"
                );
            }
            // Always reported, pass or fail -- saturation must be establishable
            // without inferring it from the data afterwards.
            info!(
                peak_open = h.peak_open_opportunities,
                capacity = h.opportunity_capacity,
                capacity_evictions = h.capacity_evictions,
                cohort_truncations = h.cohort_truncations,
                opportunities_opened = h.opportunities_opened,
                scores_emitted = h.scores_emitted,
                attempted = health.attempted.load(Ordering::Relaxed),
                written = health.written.load(Ordering::Relaxed),
                queue_peak = health.queue_peak.load(Ordering::Relaxed),
                "opportunity-intelligence shadow summary"
            );
        }
        closures
    }
}

#[cfg(test)]
#[path = "opportunity_shadow_tests.rs"]
mod tests;
