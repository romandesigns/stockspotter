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
use crate::opportunity_outcomes::OutcomeEngineHealth;
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
    /// Opportunity-native outcome capture: the writer, and the collector's
    /// own accounting. Both empty when the capture is off, so "was not
    /// running" stays distinguishable from "ran and recorded nothing".
    opportunity_outcomes: OnceLock<Arc<WriterHealth>>,
    opportunity_outcome_engine: OnceLock<Arc<OutcomeEngineHealth>>,
    retention: OnceLock<Arc<RetentionHealth>>,
    oi_config_fingerprint: OnceLock<String>,
    /// Every version the running OI engine and outcome capture stamp on their
    /// records. Set once at start-up, like the fingerprint.
    versions: OnceLock<ReportVersions>,
}

/// The versions `/research/completeness` reports, so preflight can check each
/// pinned `expected*` value of the qualification contract rather than only the
/// fingerprint (D4 moved `outcomeMeasurementVersion`; D3/D7 move the feature
/// schemas).
#[derive(Debug, Clone)]
pub struct ReportVersions {
    /// The engine's own version set, carried whole so a field added to it
    /// later is reported without touching this module.
    pub oi: backtest_metrics::opportunity::OiVersions,
    pub outcome_measurement_version: String,
    pub episode_schema: u32,
    pub signal_context_schema: u32,
}

impl ReportVersions {
    /// What this build stamps, for `config`.
    pub fn for_config(config: &backtest_metrics::opportunity::OiConfig) -> Self {
        Self {
            oi: config.versions(),
            outcome_measurement_version:
                backtest_metrics::opportunity_outcome::OPPORTUNITY_OUTCOME_VERSION.to_string(),
            episode_schema: backtest_metrics::episode::EPISODE_SCHEMA_VERSION,
            signal_context_schema: backtest_metrics::context::SIGNAL_CONTEXT_SCHEMA_VERSION,
        }
    }
}

/// Shape version of the completeness document; see
/// `CompletenessReport::report_schema_version`.
pub const REPORT_SCHEMA_VERSION: u32 = 1;

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
    pub fn set_opportunity_outcomes(&self, health: Arc<WriterHealth>) {
        let _ = self.opportunity_outcomes.set(health);
    }
    pub fn set_opportunity_outcome_engine(&self, health: Arc<OutcomeEngineHealth>) {
        let _ = self.opportunity_outcome_engine.set(health);
    }

    /// The outcome collector's accounting, or `None` when the capture is off.
    pub fn opportunity_outcome_engine(&self) -> Option<&Arc<OutcomeEngineHealth>> {
        self.opportunity_outcome_engine.get()
    }
    /// The outcome writer's accounting, or `None` when the capture is off.
    pub fn opportunity_outcomes(&self) -> Option<&Arc<WriterHealth>> {
        self.opportunity_outcomes.get()
    }
    pub fn set_retention(&self, health: Arc<RetentionHealth>) {
        let _ = self.retention.set(health);
    }
    pub fn set_oi_config_fingerprint(&self, fingerprint: String) {
        let _ = self.oi_config_fingerprint.set(fingerprint);
    }
    pub fn set_versions(&self, versions: ReportVersions) {
        let _ = self.versions.set(versions);
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
        [
            self.opportunity_intelligence.get(),
            self.measurement.get(),
            self.opportunity_outcomes.get(),
        ]
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
        let versions = self.versions.get();
        CompletenessReport {
            report_schema_version: REPORT_SCHEMA_VERSION,
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
            oi_versions: versions.map(|v| v.oi.clone()),
            outcome_measurement_version: versions.map(|v| v.outcome_measurement_version.clone()),
            episode_schema: versions.map(|v| v.episode_schema),
            signal_context_schema: versions.map(|v| v.signal_context_schema),
            opportunity_intelligence: self.opportunity_intelligence.get().map(|h| h.snapshot()),
            measurement: self.measurement.get().map(|h| h.snapshot()),
            discovery: Some(discovery_capture()),
            opportunity_engine: self.engine.get().map(|h| engine_capture(h)),
            opportunity_outcomes: self.opportunity_outcomes.get().map(|h| h.snapshot()),
            opportunity_outcome_engine: self
                .opportunity_outcome_engine
                .get()
                .map(|h| h.snapshot()),
        }
    }
}

/// Fixed-size scalars and one small fixed enum only -- no per-symbol or
/// per-window collection may be added here, because this document is read
/// repeatedly while the session runs.
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
        rank_cohort_capacity: s.rank_cohort_capacity,
        early_cohort_truncations: s.early_cohort_truncations,
        continuation_cohort_truncations: s.continuation_cohort_truncations,
        truncation_markers_dropped: s.truncation_markers_dropped,
        early_cohort_last: s.early_cohort_last,
        continuation_cohort_last: s.continuation_cohort_last,
        early_cohort_peak: s.early_cohort_peak,
        continuation_cohort_peak: s.continuation_cohort_peak,
        ranking_windows: s.ranking_windows,
        last_rank_micros: s.last_rank_micros,
        peak_rank_micros: s.peak_rank_micros,
        closed_by_reason: s.closed_by_reason,
        engine_session_date: s.engine_session_date,
        // TODO(D3/D7a merge): `market_data::trading_session::market_day(now)`.
        market_day_id: None,
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
