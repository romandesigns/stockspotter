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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use backtest_metrics::opportunity::{
    OiConfig, OpportunityIntelligence, OpportunityScoreSnapshot,
};
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

    pub fn marker(&self, kind: &str, data: Option<serde_json::Value>) {
        self.writer.marker(kind, data);
    }
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
}

impl EngineHealth {
    pub fn snapshot(&self) -> EngineHealthSnapshot {
        EngineHealthSnapshot {
            open: self.open.load(Ordering::Relaxed),
            peak: self.peak.load(Ordering::Relaxed),
            capacity: self.capacity.load(Ordering::Relaxed),
            capacity_evictions: self.capacity_evictions.load(Ordering::Relaxed),
            eviction_markers_dropped: self.eviction_markers_dropped.load(Ordering::Relaxed),
            opportunities_opened: self.opportunities_opened.load(Ordering::Relaxed),
            opportunities_closed: self.opportunities_closed.load(Ordering::Relaxed),
            cohort_truncations: self.cohort_truncations.load(Ordering::Relaxed),
            scores_emitted: self.scores_emitted.load(Ordering::Relaxed),
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

    /// Folds one already-broadcast event in. Returns the snapshots produced, so
    /// a caller (or a test) can inspect them without reading the file.
    pub fn observe(
        &mut self,
        event: &ScanEvent,
        received_at: DateTime<Utc>,
    ) -> Vec<OpportunityScoreSnapshot> {
        // Closed opportunities are not persisted here: the shadow log records
        // *scoring decisions*, and outcomes are joined later by the existing
        // measurement system rather than duplicated into a second schema.
        //
        // Capacity evictions are the one exception, and they are not an
        // outcome: they are the instrument reporting that it discarded
        // evidence. Persisted as markers, not as data.
        let _closed = self.engine.observe(event, received_at);
        let snapshots = self.engine.rank(received_at).unwrap_or_default();
        if let Some(recorder) = &self.recorder {
            for eviction in self.engine.take_capacity_evictions() {
                recorder.capacity_eviction(&eviction);
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
        snapshots
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
    }

    pub fn finish(&mut self, at: DateTime<Utc>) {
        let _ = self.engine.finish(at);
        self.publish_engine_health();
        let h = self.engine.health().clone();
        let evictions = self.engine.take_capacity_evictions();
        if let Some(recorder) = &self.recorder {
            for eviction in &evictions {
                recorder.capacity_eviction(eviction);
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
    }
}

#[cfg(test)]
#[path = "opportunity_shadow_tests.rs"]
mod tests;
