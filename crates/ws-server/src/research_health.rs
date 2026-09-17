//! One read-only answer to "did this session lose scientific evidence".
//!
//! # Why one surface, and why it must be live
//!
//! On September 16 every counter needed to disqualify the session already
//! existed. None of them could be read. `capture_health()` was unrouted,
//! `/health` returned the string `ok`, and the drop counters were reachable
//! only through shutdown logging — so the only way to learn that the instrument
//! had discarded 89% of the session was to stop the process that was still
//! writing it, which section 0 of the evaluation brief correctly forbade.
//!
//! The failure was therefore established six hours after the fact, by grepping
//! power-of-two log lines and reconstructing the denominator from cohort sizes
//! in the surviving records. That is not an instrument; that is an
//! archaeological dig.
//!
//! This module fixes the reachability, not the counting: it holds shared
//! handles onto accounting that each subsystem already owns, and renders them
//! as one [`CompletenessReport`]. Nothing here computes anything, so there is
//! no second source of truth to drift.
//!
//! # Access
//!
//! Exposed at `GET /research/completeness` on the HTTP listener, behind the
//! same `access::protect` middleware as every other route — which fails closed,
//! so with no `STOCKSPOTTER_API_TOKEN` configured nothing is authorized at all.
//! It carries no secrets: counters, queue depths, file names and byte totals.

use std::sync::{Arc, OnceLock};

use backtest_metrics::completeness::{CompletenessReport, DiscoveryCapture, EngineCapture};

use crate::measurement::CollectorHealth;
use crate::opportunity_shadow::EngineHealth;
use crate::research_retention::{RetentionHealth, RetentionSnapshot};
use crate::research_writer::WriterHealth;

/// Shared handles onto each capture's own accounting.
///
/// `OnceLock` rather than a lock: each field is set exactly once, while the
/// process is starting, by the code that creates the subsystem. A capture that
/// is switched off leaves its field empty, and "was not running" stays
/// distinguishable from "was running and recorded nothing" — a distinction the
/// completeness checker depends on to answer INDETERMINATE rather than VALID.
#[derive(Default)]
pub struct ResearchHealth {
    opportunity_intelligence: OnceLock<Arc<WriterHealth>>,
    measurement: OnceLock<Arc<WriterHealth>>,
    measurement_engine: OnceLock<Arc<CollectorHealth>>,
    engine: OnceLock<Arc<EngineHealth>>,
    retention: OnceLock<Arc<RetentionHealth>>,
    oi_config_fingerprint: OnceLock<String>,
}

impl ResearchHealth {
    pub fn set_opportunity_intelligence(&self, health: Arc<WriterHealth>) {
        let _ = self.opportunity_intelligence.set(health);
    }
    pub fn set_measurement(&self, health: Arc<WriterHealth>) {
        let _ = self.measurement.set(health);
    }
    pub fn set_measurement_engine(&self, health: Arc<CollectorHealth>) {
        let _ = self.measurement_engine.set(health);
    }
    pub fn set_engine(&self, health: Arc<EngineHealth>) {
        let _ = self.engine.set(health);
    }
    pub fn set_retention(&self, health: Arc<RetentionHealth>) {
        let _ = self.retention.set(health);
    }
    pub fn set_oi_config_fingerprint(&self, fingerprint: String) {
        let _ = self.oi_config_fingerprint.set(fingerprint);
    }

    /// Retention accounting, or `None` when retention is not running.
    pub fn retention(&self) -> Option<RetentionSnapshot> {
        self.retention.get().map(|h| h.snapshot())
    }

    /// The files the writers are currently appending to.
    ///
    /// Retention consults this rather than inferring from filenames: a writer
    /// that is behind, or replaying, may legitimately still be appending to an
    /// older date, and a deletion decided from the name alone would race it.
    pub fn current_capture_files(&self) -> Vec<String> {
        [self.opportunity_intelligence.get(), self.measurement.get()]
            .into_iter()
            .flatten()
            .filter_map(|h| h.current_file.lock().ok().map(|f| f.clone()))
            .filter(|f| !f.is_empty())
            .collect()
    }

    /// The measurement collector's pending-set accounting, for the settlement
    /// half of the completeness verdict.
    pub fn measurement_engine(&self) -> Option<&Arc<CollectorHealth>> {
        self.measurement_engine.get()
    }

    /// The whole surface, as one document.
    pub fn report(&self) -> CompletenessReport {
        CompletenessReport {
            generated_at: chrono::Utc::now(),
            // Stamped at build time by `ops/vps/deploy.sh`, so a report can
            // never be attributed to the wrong build. Absent in a local `cargo
            // run`, which the checker treats as missing provenance rather than
            // as a match.
            //
            // Deliberately not read from `ops/vps/.deployed-commit`: that is a
            // mutable runtime file describing what the deploy script last
            // recorded, not what this binary was built from, and the cases
            // where the two disagree are exactly the ones an honest stamp
            // exists to catch. See `crate::provenance`.
            commit: crate::provenance::build_commit().map(str::to_string),
            oi_config_fingerprint: self.oi_config_fingerprint.get().cloned(),
            opportunity_intelligence: self.opportunity_intelligence.get().map(|h| h.snapshot()),
            measurement: self.measurement.get().map(|h| h.snapshot()),
            discovery: Some(discovery_capture()),
            opportunity_engine: self.engine.get().map(|h| engine_capture(h)),
        }
    }
}

fn engine_capture(health: &EngineHealth) -> EngineCapture {
    let s = health.snapshot();
    EngineCapture {
        open: s.open,
        peak: s.peak,
        capacity: s.capacity,
        capacity_evictions: s.capacity_evictions,
        eviction_markers_dropped: s.eviction_markers_dropped,
        opportunities_opened: s.opportunities_opened,
        opportunities_closed: s.opportunities_closed,
        cohort_truncations: s.cohort_truncations,
        scores_emitted: s.scores_emitted,
    }
}

/// Discovery's accounting lives behind a process-global recorder in
/// `market-data`, so it is read rather than held.
fn discovery_capture() -> DiscoveryCapture {
    let h = market_data::discovery_audit::health();
    DiscoveryCapture {
        attempted: h.attempted,
        written: h.written,
        queue_lost: h.queue_lost,
        write_errors: h.write_errors,
        sampled_out: h.sampled_out,
        budget_dropped: h.budget_dropped,
        lost_records_total: h.lost_records_total,
        queue_depth: h.queue_depth,
        queue_peak: h.queue_peak,
        queue_capacity: h.queue_capacity,
        queued_bytes: h.queued_bytes,
        queued_bytes_peak: h.queued_bytes_peak,
        queue_capacity_bytes: h.queue_capacity_bytes,
        bytes_written: h.bytes_written,
        batches_written: h.batches_written,
        last_write: h.last_write,
        current_file: h.current_file,
        current_file_bytes: h.current_file_bytes,
        degraded: h.degraded,
    }
}
