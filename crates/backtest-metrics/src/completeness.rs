//! Whether a captured session is analysis-grade — decided mechanically.
//!
//! # Why this exists
//!
//! The September-16 instrument-validation session was declared complete on the
//! strength of a log query, and the log query was wrong. The level filter used
//! `\bERROR`, but `tracing` right-pads shorter level names: `INFO` and `WARN`
//! carry a leading space inside the ANSI escape while `ERROR` abuts the `m` of
//! `\x1b[31m`. `m` is a word character, so no word boundary existed and the
//! pattern silently matched nothing. Ten ERROR lines — a third, independent
//! capture failure — were reported as zero.
//!
//! The lesson is not "write better regexes". It is that **completeness must be
//! decidable from counters, not from prose**, and that the decision must be a
//! function rather than a judgement. This module is that function.
//!
//! # The three-valued result
//!
//! A boolean would be wrong. "Nothing proves this session lost data" and "this
//! session is proven not to have lost data" are different claims, and only the
//! second supports a prospective result. So:
//!
//! * [`Verdict::Invalid`] — a blocking condition is positively established.
//! * [`Verdict::Indeterminate`] — the evidence needed to *prove* validity is
//!   absent. Not a pass.
//! * [`Verdict::Valid`] — every blocking invariant is positively established.
//!
//! `Valid` is never inferred from the absence of a warning.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// The health surface (section 10)
// ---------------------------------------------------------------------------

/// One buffered research writer's capture accounting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WriterCapture {
    /// Records offered. The denominator for everything else, and the field
    /// whose absence made the September-16 capture rate an inference.
    pub attempted: u64,
    pub written: u64,
    pub dropped: u64,
    pub write_errors: u64,
    /// Contiguous loss bursts. A span of 900 drops is one span.
    pub loss_spans: u64,
    pub queue_depth: u64,
    pub queue_peak: u64,
    pub queue_capacity: u64,
    pub queued_bytes: u64,
    pub queued_bytes_peak: u64,
    pub queue_capacity_bytes: u64,
    pub bytes_written: u64,
    pub batches_written: u64,
    pub last_write: Option<DateTime<Utc>>,
    pub current_file: String,
    pub current_file_bytes: u64,
    pub degraded: bool,
}

impl WriterCapture {
    /// Records offered that were persisted, or `None` when nothing was offered
    /// (which is a different statement from "nothing survived").
    pub fn capture_rate(&self) -> Option<f64> {
        if self.attempted == 0 {
            None
        } else {
            Some(self.written as f64 / self.attempted as f64)
        }
    }
}

/// Discovery's accounting. Shaped differently because discovery's writer has
/// responsibilities the other two do not: a daily byte budget, deliberate
/// downsampling under pressure, and segment rotation. Those must stay
/// distinguishable from queue loss, because one is a policy and the other is a
/// defect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryCapture {
    pub attempted: u64,
    pub written: u64,
    /// Records lost because the queue was full. A defect.
    pub queue_lost: u64,
    /// Records the writer accepted but could not persist. A defect.
    pub write_errors: u64,
    /// Records deliberately downsampled under budget pressure. A policy, and
    /// self-describing in-band — never counted as queue loss.
    pub sampled_out: u64,
    /// Records refused because the daily byte budget was exhausted. A policy.
    pub budget_dropped: u64,
    /// The aggregate `lost_records` counter carried in-band on every record.
    /// Retained unchanged so existing readers keep working.
    pub lost_records_total: u64,
    pub queue_depth: u64,
    pub queue_peak: u64,
    pub queue_capacity: u64,
    pub queued_bytes: u64,
    pub queued_bytes_peak: u64,
    pub queue_capacity_bytes: u64,
    pub bytes_written: u64,
    pub batches_written: u64,
    pub last_write: Option<DateTime<Utc>>,
    pub current_file: String,
    pub current_file_bytes: u64,
    pub degraded: bool,
}

/// The opportunity engine's capacity accounting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EngineCapture {
    pub open: usize,
    pub peak: usize,
    pub capacity: usize,
    /// **The September-16 secondary finding.** Non-zero means opportunities
    /// were removed before they could participate in later ranking, so cohort
    /// membership is truncated and no cross-sectional claim survives.
    pub capacity_evictions: u64,
    /// Eviction *markers* that overflowed their buffer. The evictions
    /// themselves are still counted exactly.
    pub eviction_markers_dropped: u64,
    pub opportunities_opened: u64,
    pub opportunities_closed: u64,
    /// Ranking windows in which either surface's cohort was cut. Structurally
    /// zero since D6 bound the cohort to open capacity, so non-zero now means
    /// a mis-specified bound or an engine defect, and stays blocking.
    pub cohort_truncations: u64,
    pub scores_emitted: u64,

    // ---- D6 ranking cohort (additive; absent on pre-D6 reports) ----------
    /// The bound each surface's ranked cohort is held against.
    #[serde(default)]
    pub rank_cohort_capacity: usize,
    /// `cohortTruncations` split by surface, so a non-zero says which.
    #[serde(default)]
    pub early_cohort_truncations: u64,
    #[serde(default)]
    pub continuation_cohort_truncations: u64,
    /// `ranking_cohort_truncated` markers lost to their buffer.
    #[serde(default)]
    pub truncation_markers_dropped: u64,
    /// True scored N per surface: most recent window, and largest seen.
    #[serde(default)]
    pub early_cohort_last: usize,
    #[serde(default)]
    pub continuation_cohort_last: usize,
    #[serde(default)]
    pub early_cohort_peak: usize,
    #[serde(default)]
    pub continuation_cohort_peak: usize,
    #[serde(default)]
    pub ranking_windows: u64,
    /// Wall-clock cost of `rank()`, most recent window and worst, in µs.
    #[serde(default)]
    pub last_rank_micros: u64,
    #[serde(default)]
    pub peak_rank_micros: u64,

    // ---- D4 lifecycle -------------------------------------------------------
    /// `opportunitiesClosed` by the engine's close reason. Also the
    /// denominator a replay reconciles `opportunity_closed` markers against.
    #[serde(default)]
    pub closed_by_reason: crate::opportunity::ClosedByReason,

    // ---- identity date --------------------------------------------------
    /// The UTC `sessionDate` the engine is currently assigning to new
    /// opportunities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_session_date: Option<chrono::NaiveDate>,
    /// The 04:00-ET market day of the report instant.
    ///
    /// TODO(D3/D7a merge): populate from
    /// `market_data::trading_session::market_day`, which the D3/D7a branch
    /// adds; `None` until then rather than a second, divergent definition of
    /// the market day computed here. The same branch owns the feature-cache
    /// reset counters (`baselineResets`, `sessionVolumeResetStatus` in the
    /// contract's observability table), which belong beside this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_day_id: Option<String>,
}

/// One read-only answer to "did this session lose scientific evidence".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletenessReport {
    /// Shape of this document. Absent (0) on every report written before it
    /// existed; 1 is the first versioned shape (D4/D6 observability).
    #[serde(default)]
    pub report_schema_version: u32,
    pub generated_at: DateTime<Utc>,
    /// Deployed commit, so a report can never be attributed to the wrong build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oi_config_fingerprint: Option<String>,
    /// Every version the running engine stamps on its records, so preflight
    /// can verify each pinned `expected*` value of the qualification spec and
    /// not only the fingerprint. Carried as the engine's own `OiVersions`, so
    /// a version field added there later (for example a baseline policy)
    /// appears here with no change to this file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oi_versions: Option<crate::opportunity::OiVersions>,
    /// `OPPORTUNITY_OUTCOME_VERSION` of the outcome capture. D4 made this
    /// `opportunity-outcome-v2`; a v1 capture's dispositions are unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_measurement_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_schema: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal_context_schema: Option<u32>,
    /// `None` when that capture was not running at all, which is honestly
    /// different from "was running and wrote nothing".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opportunity_intelligence: Option<WriterCapture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measurement: Option<WriterCapture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery: Option<DiscoveryCapture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opportunity_engine: Option<EngineCapture>,
    /// Opportunity-native outcome capture: the writer, then the collector's
    /// own accounting. Both `None` when that capture was not running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opportunity_outcomes: Option<WriterCapture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opportunity_outcome_engine: Option<crate::opportunity_outcome::OutcomeHealth>,
}

impl CompletenessReport {
    /// Fast path for an operator: any positively-established evidence loss.
    ///
    /// Deliberately *not* the verdict. This answers "is anything already
    /// known to be wrong", not "is this session provably sound" — the second
    /// needs the artifacts, and is [`check`].
    pub fn any_known_loss(&self) -> bool {
        let writer = |w: &Option<WriterCapture>| {
            w.as_ref().is_some_and(|w| w.dropped > 0 || w.write_errors > 0)
        };
        // An outcome anchor evicted for capacity is evidence loss of exactly
        // the kind this answers, so it belongs in the fast path too.
        if self
            .opportunity_outcome_engine
            .as_ref()
            .is_some_and(|e| e.capacity_evictions > 0)
        {
            return true;
        }
        if writer(&self.opportunity_outcomes) {
            return true;
        }
        writer(&self.opportunity_intelligence)
            || writer(&self.measurement)
            || self
                .discovery
                .as_ref()
                .is_some_and(|d| d.queue_lost > 0 || d.write_errors > 0)
            || self.opportunity_engine.as_ref().is_some_and(|e| {
                e.capacity_evictions > 0
                    // D6: a cut cohort is evidence loss too, and `check`
                    // already blocks on it. Previously omitted here, so the
                    // fast path could say "no known loss" about a session the
                    // verdict marks INVALID.
                    || e.cohort_truncations > 0
                    || e.early_cohort_truncations > 0
                    || e.continuation_cohort_truncations > 0
            })
    }
}

// ---------------------------------------------------------------------------
// Evidence (section 11 input)
// ---------------------------------------------------------------------------

/// One captured file, as the checker found it on disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactEvidence {
    pub path: String,
    pub present: bool,
    pub bytes: u64,
    pub records: u64,
    /// Lines that did not parse. Any is blocking: a malformed record means the
    /// file cannot be read as a whole.
    pub malformed_records: u64,
    /// True when the final line lacked its terminator, i.e. the capture was cut
    /// mid-record.
    pub truncated: bool,
}

/// Whether every episode that opened in the session reached a settled outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettlementEvidence {
    pub settled: u64,
    /// Episodes still awaiting an outcome when capture ended. Non-zero is
    /// blocking: their horizons are censored by the capture, not by the market.
    pub unsettled: u64,
    /// Episodes force-settled because pending capacity bound.
    pub capacity_evicted: u64,
}

/// Everything the checker is allowed to look at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEvidence {
    pub session_date: String,
    /// What the session was *supposed* to run. A mismatch is blocking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_oi_config_fingerprint: Option<String>,
    /// Absent means the session's own health was never captured, which makes
    /// validity unprovable rather than false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<CompletenessReport>,
    #[serde(default)]
    pub artifacts: Vec<ArtifactEvidence>,
    /// Paths that must be present and non-empty for the session to mean
    /// anything.
    #[serde(default)]
    pub required_artifacts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settlement: Option<SettlementEvidence>,
}

// ---------------------------------------------------------------------------
// The verdict
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Verdict {
    Valid,
    Invalid,
    Indeterminate,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Verdict::Valid => "VALID",
            Verdict::Invalid => "INVALID",
            Verdict::Indeterminate => "INDETERMINATE",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub verdict: Verdict,
    /// Conditions positively establishing that evidence was lost.
    pub blocking: Vec<String>,
    /// Evidence that would be needed to prove validity and is not present.
    pub missing: Vec<String>,
    /// Non-blocking observations, recorded so a reader is not left wondering
    /// whether they were considered.
    pub notes: Vec<String>,
}

/// Decides whether a captured session is analysis-grade.
///
/// Pure and total: the same evidence always yields the same verdict, and no
/// input can make it fail rather than answer. `Invalid` dominates
/// `Indeterminate` — a session that is both incompletely evidenced and
/// positively broken is broken.
pub fn check(evidence: &SessionEvidence) -> Outcome {
    let mut blocking: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    // ---- artifacts ---------------------------------------------------------
    let present: BTreeSet<&str> = evidence
        .artifacts
        .iter()
        .filter(|a| a.present)
        .map(|a| a.path.as_str())
        .collect();
    for required in &evidence.required_artifacts {
        if !present.contains(required.as_str()) {
            blocking.push(format!("required artifact missing: {required}"));
        }
    }
    for artifact in &evidence.artifacts {
        if !artifact.present {
            continue;
        }
        if artifact.malformed_records > 0 {
            blocking.push(format!(
                "{}: {} malformed record(s); the file cannot be read as a whole",
                artifact.path, artifact.malformed_records
            ));
        }
        if artifact.truncated {
            blocking.push(format!("{}: ends mid-record (partial artifact)", artifact.path));
        }
        if evidence.required_artifacts.iter().any(|r| r == &artifact.path)
            && artifact.records == 0
        {
            blocking.push(format!("{}: present but empty", artifact.path));
        }
    }

    // ---- provenance --------------------------------------------------------
    let health = evidence.health.as_ref();
    match (&evidence.expected_commit, health.and_then(|h| h.commit.as_ref())) {
        (Some(expected), Some(actual)) if expected != actual => blocking.push(format!(
            "commit mismatch: session ran {actual}, expected {expected}"
        )),
        (Some(_), None) => missing
            .push("health evidence carries no commit, so provenance cannot be established".into()),
        _ => {}
    }
    match (
        &evidence.expected_oi_config_fingerprint,
        health.and_then(|h| h.oi_config_fingerprint.as_ref()),
    ) {
        (Some(expected), Some(actual)) if expected != actual => blocking.push(format!(
            "OI config mismatch: session ran {actual}, expected {expected}"
        )),
        (Some(_), None) => missing.push(
            "health evidence carries no OI config fingerprint, so the configuration \
             that produced these records cannot be established"
                .into(),
        ),
        _ => {}
    }

    // ---- capture health ----------------------------------------------------
    let Some(health) = health else {
        missing.push(
            "no health evidence: capture completeness cannot be established from artifacts \
             alone, because a record that was never written leaves nothing behind"
                .into(),
        );
        return finish(blocking, missing, notes);
    };

    check_writer("opportunityIntelligence", &health.opportunity_intelligence, &mut blocking, &mut missing, &mut notes);
    check_writer("measurement", &health.measurement, &mut blocking, &mut missing, &mut notes);

    match &health.discovery {
        None => missing.push("discovery capture health absent".into()),
        Some(d) => {
            if d.queue_lost > 0 {
                blocking.push(format!("discovery: {} record(s) lost to queue pressure", d.queue_lost));
            }
            if d.write_errors > 0 {
                blocking.push(format!("discovery: {} write error(s)", d.write_errors));
            }
            if d.attempted == 0 {
                missing.push("discovery: nothing attempted, so the capture proves nothing".into());
            }
            // Policy, not defect — but an analyst must know it happened.
            if d.sampled_out > 0 {
                notes.push(format!(
                    "discovery: {} record(s) deliberately downsampled under budget pressure \
                     (self-describing in-band; not evidence loss)",
                    d.sampled_out
                ));
            }
            if d.budget_dropped > 0 {
                notes.push(format!(
                    "discovery: {} record(s) refused by the daily byte budget \
                     (self-describing in-band; not queue loss)",
                    d.budget_dropped
                ));
            }
        }
    }

    match &health.opportunity_engine {
        None => missing.push("opportunity engine health absent".into()),
        Some(e) => {
            if e.capacity_evictions > 0 {
                blocking.push(format!(
                    "opportunity engine: {} capacity eviction(s) at capacity {}; opportunities \
                     were removed before they could participate in later ranking",
                    e.capacity_evictions, e.capacity
                ));
            }
            if e.eviction_markers_dropped > 0 {
                blocking.push(format!(
                    "opportunity engine: {} eviction marker(s) lost, so the artifact cannot \
                     fully describe its own truncation",
                    e.eviction_markers_dropped
                ));
            }
            if e.capacity > 0 && e.peak >= e.capacity {
                blocking.push(format!(
                    "opportunity engine: peak open {} reached capacity {}",
                    e.peak, e.capacity
                ));
            }
            if e.cohort_truncations > 0 {
                blocking.push(format!(
                    "opportunity engine: {} ranking window(s) had their cohort truncated \
                     (early {}, continuation {}; rank bound {})",
                    e.cohort_truncations,
                    e.early_cohort_truncations,
                    e.continuation_cohort_truncations,
                    e.rank_cohort_capacity
                ));
            } else if e.early_cohort_truncations + e.continuation_cohort_truncations > 0 {
                // The per-surface counters and their OR must agree; a report
                // where they do not cannot be trusted about truncation.
                blocking.push(format!(
                    "opportunity engine: per-surface cohort truncations (early {}, continuation \
                     {}) with cohortTruncations 0 -- the counters do not reconcile",
                    e.early_cohort_truncations, e.continuation_cohort_truncations
                ));
            }
            if e.opportunities_opened == 0 {
                missing.push(
                    "opportunity engine opened nothing, so the session exercised nothing".into(),
                );
            }
        }
    }

    // ---- settlement --------------------------------------------------------
    match &evidence.settlement {
        None => missing.push(
            "settlement evidence absent: whether every episode reached an outcome is unknown"
                .into(),
        ),
        Some(s) => {
            if s.unsettled > 0 {
                blocking.push(format!(
                    "settlement incomplete: {} episode(s) never reached an outcome",
                    s.unsettled
                ));
            }
            if s.capacity_evicted > 0 {
                blocking.push(format!(
                    "settlement: {} episode(s) force-settled for want of pending capacity",
                    s.capacity_evicted
                ));
            }
            if s.settled == 0 {
                missing.push("settlement evidence records nothing settled".into());
            }
        }
    }

    finish(blocking, missing, notes)
}

fn check_writer(
    name: &str,
    capture: &Option<WriterCapture>,
    blocking: &mut Vec<String>,
    missing: &mut Vec<String>,
    notes: &mut Vec<String>,
) {
    let Some(c) = capture else {
        missing.push(format!("{name} capture health absent"));
        return;
    };
    if c.dropped > 0 {
        blocking.push(format!(
            "{name}: {} record(s) dropped across {} loss span(s)",
            c.dropped, c.loss_spans
        ));
    }
    if c.write_errors > 0 {
        blocking.push(format!("{name}: {} write error(s)", c.write_errors));
    }
    if c.attempted == 0 {
        missing.push(format!("{name}: nothing attempted, so the capture proves nothing"));
        return;
    }
    // Exact reconciliation. A writer whose own counters do not add up cannot
    // be trusted to report loss correctly, which is a stronger failure than
    // any individual counter being non-zero.
    if c.written + c.dropped + c.write_errors != c.attempted {
        blocking.push(format!(
            "{name}: accounting does not reconcile -- attempted {}, written {}, dropped {}, \
             write errors {} (difference {})",
            c.attempted,
            c.written,
            c.dropped,
            c.write_errors,
            c.attempted as i128
                - (c.written as i128 + c.dropped as i128 + c.write_errors as i128),
        ));
    }
    if c.queue_capacity > 0 && c.queue_peak >= c.queue_capacity {
        notes.push(format!(
            "{name}: queue peak {} reached its {}-record bound without loss; headroom is gone \
             even though nothing was lost",
            c.queue_peak, c.queue_capacity
        ));
    }
}

fn finish(blocking: Vec<String>, missing: Vec<String>, notes: Vec<String>) -> Outcome {
    // Invalid dominates: a session that is both under-evidenced and positively
    // broken is broken, and reporting it as merely unprovable would understate
    // it.
    let verdict = if !blocking.is_empty() {
        Verdict::Invalid
    } else if !missing.is_empty() {
        Verdict::Indeterminate
    } else {
        Verdict::Valid
    };
    Outcome { verdict, blocking, missing, notes }
}

#[cfg(test)]
#[path = "completeness_tests.rs"]
mod tests;
