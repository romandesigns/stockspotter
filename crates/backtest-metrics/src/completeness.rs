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

    // ---- D5 lifecycle -------------------------------------------------------
    /// `OiVersions::lifecycle` of the running engine
    /// (`opportunity-lifecycle-move-v1` or `...-symbol-activity-v1`). Absent
    /// on a pre-D5 report, which ran the symbol-activity lifecycle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<String>,
    /// Opens refused by the `move-v1` duplicate-identity guard
    /// (preregistration section 7). Blocking: a non-zero means an
    /// out-of-order event reached an earlier opening instant, and a refused
    /// open is a move the artifact does not contain.
    #[serde(default)]
    pub duplicate_identity_refused: u64,

    // ---- identity date --------------------------------------------------
    /// The UTC `sessionDate` the engine is currently assigning to new
    /// opportunities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_session_date: Option<chrono::NaiveDate>,
    /// The 04:00-ET market day of the report instant, from
    /// `market_data::trading_session::market_day` (filled by ws-server's
    /// `research_health::engine_capture`). The readiness preflight's
    /// `timezone` check compares it with its own computation. `None` only on
    /// a report written before it existed.
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
                    || e.duplicate_identity_refused > 0
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
            if e.duplicate_identity_refused > 0 {
                blocking.push(format!(
                    "opportunity engine: {} opportunity open(s) refused by the duplicate-identity \
                     guard; the event stream reached an earlier opening instant out of order, so \
                     those moves are absent from the artifact",
                    e.duplicate_identity_refused
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

// ---------------------------------------------------------------------------
// Qualification v5 machine gates (P3 brief §16)
// ---------------------------------------------------------------------------
//
// `check` answers "did this capture lose evidence". A *designated* session has
// to clear a stricter question before any Alpha claim is computed from it:
// was it captured by exactly the pinned instrument, from before the market-day
// open, with every counter that could reveal a contract violation present and
// clean? Each of those is a gate below.
//
// # No narrative override
//
// The verdict is the AND of every row of `GATE_TABLE`, computed by
// `GateReport::passed` from the results themselves -- not from a stored flag,
// not from an argument, not from the environment. There is no `--force`, no
// env var and no "accepted with caveats" state; a failed gate is recorded and
// the session is abandoned. `qualification_gates_tests` asserts the absence
// of every such path by reading the sources, because a bypass that exists is
// eventually used.
//
// # Fail closed
//
// Every field is read by **name from the raw health document**, never
// through `CompletenessReport`, so a field an older build did not emit
// (`duplicateIdentityRefused`, `lifecycle`, `premarketVolume.*`, the move-v1
// disposition tokens all arrived in P3) is `absent` -- which is a failure --
// rather than a serde default of zero. A build that cannot report a counter
// cannot prove it is zero.

use chrono::NaiveDate;

/// Shape of the gate report. Bumped if a result's meaning changes.
pub const GATE_REPORT_VERSION: &str = "qualification-gates-v1";

/// `<capture dir>/.retention/designations/<market-day>.json`: the designation
/// record `ops/qualify/session.sh designate` writes before the market-day
/// open. A sibling of the retention registry's `protected/` and `exports/`,
/// which the registry never reads, so it cannot disturb retention.
pub const DESIGNATION_DIR: &str = "designations";
pub const DESIGNATION_SCHEMA_VERSION: u32 = 1;

/// One row of the gate table: a single, named, machine-checked condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateSpec {
    /// The gate this check belongs to. A gate passes iff all its checks pass.
    pub gate: &'static str,
    /// The field path (in the `/research/completeness` envelope) or evidence
    /// path the check reads. Unique across the table.
    pub check: &'static str,
    pub predicate: &'static str,
    pub why: &'static str,
}

const WHY_LOSS: &str = "a record that was never written leaves no trace; any loss turns the \
                        population into a sample of unknown bias";
const WHY_CAPACITY: &str = "an eviction removes an opportunity or anchor before it could be \
                            ranked or measured";
const WHY_TRUNCATION: &str = "a cut cohort reports scored opportunities as unranked and every \
                              cohort size as wrong (the 09-21..09-24 defect, D6)";
const WHY_IDENTITY: &str = "the evaluation's analytical unit is the opportunity; its key must \
                            be the move";
const WHY_PINNED: &str = "a capture from a different instrument describes a different engine; \
                          comparing it under this contract would attribute one configuration's \
                          behaviour to another";
const WHY_DESIGNATION: &str = "a session chosen after it was seen is not a prospective session";

/// The qualification gate table (`alpha-qualification-v5`). The doc table in
/// `docs/qualification-v5-gates-2026-09-25.md` is checked against this by a
/// test, and `QualificationSpec::qualification_gates` must name every gate.
pub const GATE_TABLE: &[GateSpec] = &[
    // -- the pre-P3 completeness contract, unchanged ---------------------------
    GateSpec { gate: "completeness-check", check: "completeness.verdict", predicate: "== VALID",
        why: "check(): writer reconciliation, artifacts, provenance, settlement -- the contract every session already had" },
    // -- known writer loss ------------------------------------------------------
    GateSpec { gate: "writer-loss", check: "report.opportunityIntelligence.dropped", predicate: "present and == 0", why: WHY_LOSS },
    GateSpec { gate: "writer-loss", check: "report.opportunityIntelligence.writeErrors", predicate: "present and == 0", why: WHY_LOSS },
    GateSpec { gate: "writer-loss", check: "report.opportunityIntelligence.lossSpans", predicate: "present and == 0", why: WHY_LOSS },
    GateSpec { gate: "writer-loss", check: "report.measurement.dropped", predicate: "present and == 0", why: WHY_LOSS },
    GateSpec { gate: "writer-loss", check: "report.measurement.writeErrors", predicate: "present and == 0", why: WHY_LOSS },
    GateSpec { gate: "writer-loss", check: "report.measurement.lossSpans", predicate: "present and == 0", why: WHY_LOSS },
    GateSpec { gate: "writer-loss", check: "report.opportunityOutcomes.dropped", predicate: "present and == 0", why: WHY_LOSS },
    GateSpec { gate: "writer-loss", check: "report.opportunityOutcomes.writeErrors", predicate: "present and == 0", why: WHY_LOSS },
    GateSpec { gate: "writer-loss", check: "report.opportunityOutcomes.lossSpans", predicate: "present and == 0", why: WHY_LOSS },
    GateSpec { gate: "writer-loss", check: "report.discovery.queueLost", predicate: "present and == 0", why: WHY_LOSS },
    GateSpec { gate: "writer-loss", check: "report.discovery.writeErrors", predicate: "present and == 0", why: WHY_LOSS },
    GateSpec { gate: "writer-loss", check: "report.discovery.budgetDropped", predicate: "present and == 0",
        why: "discovery is the independent reference population; a budget refusal truncates it" },
    // -- capacity eviction --------------------------------------------------------
    GateSpec { gate: "capacity-eviction", check: "report.opportunityEngine.capacityEvictions", predicate: "present and == 0", why: WHY_CAPACITY },
    GateSpec { gate: "capacity-eviction", check: "report.opportunityEngine.evictionMarkersDropped", predicate: "present and == 0",
        why: "a lost eviction marker means the artifact cannot describe its own truncation" },
    GateSpec { gate: "capacity-eviction", check: "report.opportunityOutcomeEngine.capacityEvictions", predicate: "present and == 0", why: WHY_CAPACITY },
    GateSpec { gate: "capacity-eviction", check: "measurementPending.capacityEvictions", predicate: "present and == 0", why: WHY_CAPACITY },
    // -- ranking truncation --------------------------------------------------------
    GateSpec { gate: "ranking-truncation", check: "report.opportunityEngine.cohortTruncations", predicate: "present and == 0", why: WHY_TRUNCATION },
    GateSpec { gate: "ranking-truncation", check: "report.opportunityEngine.earlyCohortTruncations", predicate: "present and == 0", why: WHY_TRUNCATION },
    GateSpec { gate: "ranking-truncation", check: "report.opportunityEngine.continuationCohortTruncations", predicate: "present and == 0", why: WHY_TRUNCATION },
    GateSpec { gate: "ranking-truncation", check: "report.opportunityEngine.truncationMarkersDropped", predicate: "present and == 0", why: WHY_TRUNCATION },
    GateSpec { gate: "ranking-truncation", check: "report.opportunityEngine.rankCohortCapacity", predicate: ">= report.opportunityEngine.capacity, and capacity > 0",
        why: "D6: the ranked cohort must be bound to open capacity so truncation is structurally impossible" },
    // -- malformed output -------------------------------------------------------------
    GateSpec { gate: "malformed-output", check: "artifacts.malformedRecords", predicate: "sum over present artifacts == 0",
        why: "a malformed record means the file cannot be read as a whole" },
    GateSpec { gate: "malformed-output", check: "artifacts.truncated", predicate: "no artifact ends mid-record",
        why: "a partial final record is a capture cut mid-write" },
    GateSpec { gate: "malformed-output", check: "artifacts.required", predicate: "every required artifact present with records > 0",
        why: "the session means nothing without its primary captures" },
    // -- duplicate durable identity -------------------------------------------------
    GateSpec { gate: "duplicate-identity", check: "report.opportunityEngine.duplicateIdentityRefused", predicate: "present and == 0",
        why: "move-v1 refuses to open a colliding opportunityId and counts it; a refusal is an opportunity that was never captured" },
    // -- opportunity lifecycle contract ------------------------------------------------
    GateSpec { gate: "lifecycle-contract", check: "report.opportunityEngine.lifecycle", predicate: "== spec.expectedLifecycle", why: WHY_IDENTITY },
    GateSpec { gate: "lifecycle-contract", check: "report.oiVersions.lifecycle", predicate: "== spec.expectedLifecycle", why: WHY_IDENTITY },
    // -- baseline truncation / incomplete initialisation -----------------------------
    GateSpec { gate: "baseline-truncation", check: "rows.baselineTruncated", predicate: "0 OI rows of the market day carry preDetection.baselineTruncated == true",
        why: "a baseline that started after 04:00 ET is not this market day's baseline (D3); true on every deploy or restart day" },
    GateSpec { gate: "baseline-truncation", check: "rows.baselineComplete", predicate: "> 0 OI rows of the market day carry baselineTruncated == false",
        why: "absence of a truncated row proves nothing unless complete rows were positively observed" },
    GateSpec { gate: "deployed-before-open", check: "designation.processStartedAt", predicate: "< market_day_open(marketDay)",
        why: "a process started after 04:00 ET cannot have observed the whole market day" },
    GateSpec { gate: "deployed-before-open", check: "designation.deployMarkerAt", predicate: "< market_day_open(marketDay)",
        why: "the authoritative deploy marker must predate the observation boundary" },
    // -- premarket volume initialisation ----------------------------------------------
    GateSpec { gate: "premarket-volume-init", check: "premarketVolume.fetchFailures", predicate: "present and == 0",
        why: "D7b: a failed premarket-volume fetch leaves funnel qualification on a stale daily bar" },
    GateSpec { gate: "premarket-volume-init", check: "premarketVolume.marketDay", predicate: "== marketDay",
        why: "the state must belong to the designated market day, not a carried-over one" },
    GateSpec { gate: "premarket-volume-init", check: "premarketVolume.initializedAt", predicate: "RFC 3339 and market_day(t) == marketDay",
        why: "initialisation must have happened inside the designated market day" },
    // -- schema / fingerprint ------------------------------------------------------------
    GateSpec { gate: "schema-fingerprint", check: "report.commit", predicate: "== expected commit (request, else designation)", why: WHY_PINNED },
    GateSpec { gate: "schema-fingerprint", check: "report.oiConfigFingerprint", predicate: "== spec.expectedOiConfigFingerprint", why: WHY_PINNED },
    GateSpec { gate: "schema-fingerprint", check: "report.oiVersions.configFingerprint", predicate: "== spec.expectedOiConfigFingerprint", why: WHY_PINNED },
    GateSpec { gate: "schema-fingerprint", check: "report.oiVersions.opportunitySchema", predicate: "== spec.expectedOpportunitySchema", why: WHY_PINNED },
    GateSpec { gate: "schema-fingerprint", check: "report.oiVersions.featureSchema", predicate: "== spec.expectedFeatureSchema", why: WHY_PINNED },
    GateSpec { gate: "schema-fingerprint", check: "report.oiVersions.regimeClassifier", predicate: "== spec.expectedRegimeClassifier", why: WHY_PINNED },
    GateSpec { gate: "schema-fingerprint", check: "report.oiVersions.priceRegime", predicate: "== spec.expectedPriceRegime", why: WHY_PINNED },
    GateSpec { gate: "schema-fingerprint", check: "report.oiVersions.earlyQualityModel", predicate: "== spec.expectedEarlyQualityModel", why: WHY_PINNED },
    GateSpec { gate: "schema-fingerprint", check: "report.oiVersions.continuationModel", predicate: "== spec.expectedContinuationModel", why: WHY_PINNED },
    GateSpec { gate: "schema-fingerprint", check: "report.oiVersions.ranking", predicate: "== spec.expectedRanking", why: WHY_PINNED },
    GateSpec { gate: "schema-fingerprint", check: "report.oiVersions.scorePolicy", predicate: "== spec.expectedScorePolicy", why: WHY_PINNED },
    GateSpec { gate: "schema-fingerprint", check: "report.oiVersions.baselinePolicy", predicate: "== spec.expectedBaselinePolicy", why: WHY_PINNED },
    GateSpec { gate: "schema-fingerprint", check: "report.signalContextSchema", predicate: "== spec.expectedSignalContextSchema", why: WHY_PINNED },
    GateSpec { gate: "schema-fingerprint", check: "report.episodeSchema", predicate: "== spec.expectedEpisodeSchema", why: WHY_PINNED },
    GateSpec { gate: "schema-fingerprint", check: "report.outcomeMeasurementVersion", predicate: "== spec.expectedOutcomeMeasurementVersion", why: WHY_PINNED },
    // -- disposition consistency -----------------------------------------------------------
    GateSpec { gate: "disposition-consistency", check: "report.opportunityOutcomeEngine.dispositionCounts", predicate: "sum over every token == anchorsSettled",
        why: "every settled row carries exactly one disposition; a mismatch means rows were counted twice or not at all" },
    GateSpec { gate: "disposition-consistency", check: "report.opportunityOutcomeEngine.closureAnchorsMarked", predicate: "settledNonStillOpen <= marked <= settledNonStillOpen + outstanding",
        why: "each marked anchor is either settled with its reason or still outstanding; outside that range closes and rows do not reconcile" },
    GateSpec { gate: "disposition-consistency", check: "report.opportunityOutcomeEngine.dispositionCounts.inactivity", predicate: "absent or == 0 (v1 token)",
        why: "move-v1 closes are setup_inactivity/invalidated; a v1 `inactivity` means the old lifecycle produced these rows" },
    GateSpec { gate: "disposition-consistency", check: "report.opportunityEngine.closedByReason.inactivity", predicate: "absent or == 0 (v1 token)",
        why: "same, on the engine side" },
    GateSpec { gate: "disposition-consistency", check: "report.opportunityOutcomeEngine.dispositionCounts.setupInactivity", predicate: "present (move-v1 token)",
        why: "a build that cannot count the move-v1 tokens cannot prove it wrote them" },
    GateSpec { gate: "disposition-consistency", check: "report.opportunityOutcomeEngine.dispositionCounts.invalidated", predicate: "present (move-v1 token)",
        why: "same" },
    // -- designation ------------------------------------------------------------------------
    GateSpec { gate: "designation", check: "designation.record", predicate: "present, parses, schemaVersion 1, marketDay == session market day, preflight == PASS", why: WHY_DESIGNATION },
    GateSpec { gate: "designation", check: "designation.designatedAt", predicate: "< market_day_open(marketDay)", why: WHY_DESIGNATION },
    GateSpec { gate: "designation", check: "designation.specSha256", predicate: "== sha256 of the contract this build carries", why: WHY_DESIGNATION },
    GateSpec { gate: "designation", check: "designation.specVersion", predicate: "== spec.version", why: WHY_DESIGNATION },
    GateSpec { gate: "designation", check: "designation.commit", predicate: "== report.commit", why: WHY_DESIGNATION },
    GateSpec { gate: "designation", check: "designation.oiConfigFingerprint", predicate: "== report.oiConfigFingerprint", why: WHY_DESIGNATION },
    GateSpec { gate: "designation", check: "protection.research", predicate: "research/.retention/protected/<day>.json is a valid `designated` record",
        why: "retention must be unable to delete the session before it is exported" },
    GateSpec { gate: "designation", check: "protection.discovery", predicate: "discovery-audit/.retention/protected/<day>.json is a valid `designated` record",
        why: "the reference population lives in discovery; same reason" },
];

/// The distinct gate names, in table order.
pub fn gate_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Vec::new();
    for spec in GATE_TABLE {
        if !names.contains(&spec.gate) {
            names.push(spec.gate);
        }
    }
    names
}

/// Everything the gates compare against, taken from the frozen contract
/// (`QualificationSpec::pins`) so the pins cannot drift from the spec's hash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QualificationPins {
    pub spec_version: String,
    pub spec_sha256: String,
    /// The commit the session was supposed to run. `None` falls back to the
    /// designation record's commit, which was pinned before the open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oi_config_fingerprint: Option<String>,
    pub opportunity_schema: u32,
    pub feature_schema: u32,
    pub signal_context_schema: u32,
    pub episode_schema: u32,
    pub outcome_measurement_version: String,
    pub baseline_policy: String,
    pub lifecycle: String,
    pub regime_classifier: String,
    pub price_regime: String,
    pub early_quality_model: String,
    pub continuation_model: String,
    pub ranking: String,
    pub score_policy: String,
}

/// `<capture dir>/.retention/designations/<day>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesignationRecord {
    pub schema_version: u32,
    pub market_day: NaiveDate,
    /// When the record was written. Must precede the market-day open.
    pub designated_at: DateTime<Utc>,
    pub designated_by: String,
    pub commit: String,
    pub oi_config_fingerprint: String,
    pub spec_version: String,
    pub spec_sha256: String,
    /// The capture process's (`ws` container's) start, from `docker inspect`.
    pub process_started_at: DateTime<Utc>,
    /// Modification time of `ops/vps/.deployed-commit`.
    pub deploy_marker_at: DateTime<Utc>,
    #[serde(default)]
    pub container_restart_counts: std::collections::BTreeMap<String, u64>,
    /// The preflight verdict the record was written under. Only `PASS` is
    /// ever written by the script; anything else fails the gate.
    pub preflight: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DesignationEvidence {
    Missing(String),
    Malformed(String),
    Present(DesignationRecord),
}

impl DesignationEvidence {
    pub fn record(&self) -> Option<&DesignationRecord> {
        match self {
            DesignationEvidence::Present(r) => Some(r),
            _ => None,
        }
    }
}

pub fn designation_path(session_dir: &std::path::Path, day: NaiveDate) -> std::path::PathBuf {
    session_dir
        .join("research")
        .join(market_data::retention_registry::REGISTRY_DIR)
        .join(DESIGNATION_DIR)
        .join(format!("{day}.json"))
}

pub fn load_designation(session_dir: &std::path::Path, day: NaiveDate) -> DesignationEvidence {
    let path = designation_path(session_dir, day);
    match std::fs::read(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            DesignationEvidence::Missing(path.display().to_string())
        }
        Err(error) => DesignationEvidence::Malformed(format!("{}: {error}", path.display())),
        Ok(bytes) => match serde_json::from_slice::<DesignationRecord>(&bytes) {
            Ok(record) => DesignationEvidence::Present(record),
            Err(error) => DesignationEvidence::Malformed(format!("{}: {error}", path.display())),
        },
    }
}

/// Whether each capture directory's retention registry designates the day.
/// `None` is "valid `designated` record"; `Some(reason)` is why not.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProtectionEvidence {
    pub research: Option<String>,
    pub discovery: Option<String>,
}

impl ProtectionEvidence {
    pub fn load(session_dir: &std::path::Path, day: NaiveDate) -> Self {
        use market_data::retention_registry::{Protection, ProtectionClass, ProtectionIndex};
        let one = |dir: &str| -> Option<String> {
            match ProtectionIndex::load(&session_dir.join(dir)).of(day) {
                Protection::Protected { class: ProtectionClass::Designated, .. } => None,
                Protection::Protected { class, .. } => {
                    Some(format!("{dir}: protected as {class:?}, not designated"))
                }
                Protection::Malformed { error } => Some(format!("{dir}: {error}")),
                Protection::Ordinary => Some(format!("{dir}: no protection record for {day}")),
            }
        };
        Self { research: one("research"), discovery: one("discovery-audit") }
    }
}

/// What a scan of the OI capture says about D3 baseline completeness on one
/// market day.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaselineEvidence {
    pub market_day: NaiveDate,
    pub rows_scanned: u64,
    /// Rows carrying a `preDetection` of this market day with
    /// `baselineTruncated: true` (or a line that says so and does not parse).
    pub truncated_rows: u64,
    /// Rows carrying `"baselineTruncated":false` and this market day.
    pub complete_rows: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_truncated: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_error: Option<String>,
}

/// Streams an OI snapshot file for D3 baseline truncation on `market_day`.
///
/// Cheap by construction: a line is only parsed when it contains
/// `"baselineTruncated":true`, which on a clean session is none of them. The
/// market-day filter matters because a UTC day file also holds the previous
/// market day's 20:00-04:00 ET tail, whose truncation says nothing about this
/// day. (Under EST the last after-hours hour of a market day falls in the next
/// UTC file and is not scanned; the designated day's truncation is decided at
/// its start, which is always in this file.)
pub fn scan_baseline(path: &std::path::Path, market_day: NaiveDate) -> BaselineEvidence {
    match std::fs::File::open(path) {
        Ok(file) => scan_baseline_reader(std::io::BufReader::new(file), market_day),
        Err(error) => BaselineEvidence {
            market_day,
            rows_scanned: 0,
            truncated_rows: 0,
            complete_rows: 0,
            first_truncated: None,
            read_error: Some(format!("{}: {error}", path.display())),
        },
    }
}

pub fn scan_baseline_reader(mut reader: impl std::io::BufRead, market_day: NaiveDate) -> BaselineEvidence {
    const TRUE: &[u8] = b"\"baselineTruncated\":true";
    const FALSE: &[u8] = b"\"baselineTruncated\":false";
    let day = market_day.to_string();
    let day_needle = format!("\"marketDay\":\"{day}\"");
    let mut evidence = BaselineEvidence {
        market_day,
        rows_scanned: 0,
        truncated_rows: 0,
        complete_rows: 0,
        first_truncated: None,
        read_error: None,
    };
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) => {
                evidence.read_error = Some(error.to_string());
                break;
            }
        }
        evidence.rows_scanned += 1;
        if contains(&line, TRUE) {
            let truncated = match serde_json::from_slice::<serde_json::Value>(&line) {
                Ok(row) => truncated_for_day(&row, &day),
                // It says it is truncated and cannot be read otherwise: fail
                // closed rather than guess which day it belonged to.
                Err(_) => true,
            };
            if truncated {
                evidence.truncated_rows += 1;
                if evidence.first_truncated.is_none() {
                    evidence.first_truncated = Some(describe_row(&line));
                }
                continue;
            }
        }
        if contains(&line, FALSE) && contains(&line, day_needle.as_bytes()) {
            evidence.complete_rows += 1;
        }
    }
    evidence
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn truncated_for_day(value: &serde_json::Value, day: &str) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            let here = map.get("baselineTruncated") == Some(&serde_json::Value::Bool(true))
                && map.get("marketDay").and_then(serde_json::Value::as_str) == Some(day);
            here || map.values().any(|v| truncated_for_day(v, day))
        }
        serde_json::Value::Array(items) => items.iter().any(|v| truncated_for_day(v, day)),
        _ => false,
    }
}

fn describe_row(line: &[u8]) -> String {
    match serde_json::from_slice::<serde_json::Value>(line) {
        Ok(row) => format!(
            "{} at {}",
            row.get("opportunityId").and_then(|v| v.as_str()).unwrap_or("?"),
            row.get("timestamp").and_then(|v| v.as_str()).unwrap_or("?")
        ),
        Err(_) => "an unparseable row".to_string(),
    }
}

/// Everything the gates may look at. Deliberately no switch, flag or mode.
pub struct GateInputs<'a> {
    /// The designated market day (04:00 ET boundary), which for a US session
    /// is the same calendar date as the UTC capture files.
    pub market_day: NaiveDate,
    /// The raw `/research/completeness` document as captured (envelope or
    /// bare report). Raw on purpose: see the fail-closed note above.
    pub health: Option<&'a serde_json::Value>,
    /// `check`'s outcome, with integrity problems already folded in.
    pub completeness: &'a Outcome,
    pub artifacts: &'a [ArtifactEvidence],
    pub required_artifacts: &'a [String],
    pub baseline: Option<&'a BaselineEvidence>,
    pub designation: &'a DesignationEvidence,
    pub protection: &'a ProtectionEvidence,
    pub pins: &'a QualificationPins,
}

/// One evaluated check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GateResult {
    pub gate: String,
    pub check: String,
    pub pass: bool,
    /// The evidence the check needs is not there. `pass` is then false: the
    /// failure is folded as *missing* (INDETERMINATE) rather than *blocking*
    /// (INVALID), but it is never a pass.
    pub absent: bool,
    pub observed: serde_json::Value,
    pub expected: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GateReport {
    pub version: String,
    pub market_day: NaiveDate,
    pub market_day_open: DateTime<Utc>,
    pub results: Vec<GateResult>,
}

impl GateReport {
    /// The AND of every gate. Recomputed from the results each time, and
    /// false unless **every** row of `GATE_TABLE` has exactly one passing
    /// result -- a check that was skipped cannot pass by omission.
    pub fn passed(&self) -> bool {
        GATE_TABLE.iter().all(|spec| {
            let mut matching = self
                .results
                .iter()
                .filter(|r| r.gate == spec.gate && r.check == spec.check);
            matches!((matching.next(), matching.next()), (Some(r), None) if r.pass && !r.absent)
        }) && self.results.len() == GATE_TABLE.len()
    }

    /// Gates with at least one non-passing check, in table order.
    pub fn failed_gates(&self) -> Vec<&'static str> {
        gate_names()
            .into_iter()
            .filter(|gate| {
                GATE_TABLE.iter().filter(|s| s.gate == *gate).any(|spec| {
                    !self
                        .results
                        .iter()
                        .any(|r| r.gate == spec.gate && r.check == spec.check && r.pass && !r.absent)
                })
            })
            .collect()
    }

    /// Folds every non-passing check into `outcome` -- absent evidence as
    /// *missing*, a violated predicate as *blocking* -- and recomputes the
    /// verdict, so one verdict covers the session. `completeness-check` is not
    /// re-folded: its causes are already in `outcome`.
    pub fn fold_into(&self, outcome: &mut Outcome) {
        for r in &self.results {
            if r.pass || r.gate == "completeness-check" {
                continue;
            }
            let line = format!(
                "gate {} / {}: observed {}, expected {}",
                r.gate, r.check, r.observed, r.expected
            );
            if r.absent {
                outcome.missing.push(line);
            } else {
                outcome.blocking.push(line);
            }
        }
        if !self.passed() && outcome.blocking.is_empty() && outcome.missing.is_empty() {
            // Unreachable while every check produces a result; kept so a
            // future bug in this function can only make the verdict stricter.
            outcome.blocking.push("qualification gates did not pass".into());
        }
        outcome.verdict = if !outcome.blocking.is_empty() {
            Verdict::Invalid
        } else if !outcome.missing.is_empty() {
            Verdict::Indeterminate
        } else {
            Verdict::Valid
        };
    }
}

/// Resolves an envelope path exactly where the route puts it: `report.…`,
/// `measurementPending.…`, and `premarketVolume.…` beside `retention` (D7b
/// reports detector-input coverage there, not inside the capture verdict).
/// A bare report resolves only `report.` paths, so it can never satisfy an
/// envelope-level gate. `null` is absent.
fn resolve<'a>(doc: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    fn walk<'a>(root: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
        let mut node = root;
        for key in path.split('.') {
            node = node.get(key)?;
        }
        (!node.is_null()).then_some(node)
    }
    if doc.get("report").is_some() {
        walk(doc, path)
    } else {
        path.strip_prefix("report.").and_then(|rest| walk(doc, rest))
    }
}

struct Gates<'a> {
    inputs: &'a GateInputs<'a>,
    open: DateTime<Utc>,
    results: Vec<GateResult>,
}

#[derive(Clone, Copy)]
enum Status {
    Pass,
    Fail,
    Absent,
}

impl<'a> Gates<'a> {
    fn spec(check: &str) -> &'static GateSpec {
        GATE_TABLE
            .iter()
            .find(|s| s.check == check)
            .unwrap_or_else(|| panic!("gate check {check} is not in GATE_TABLE"))
    }

    fn push(&mut self, check: &str, status: Status, observed: serde_json::Value, expected: serde_json::Value) {
        let spec = Self::spec(check);
        self.results.push(GateResult {
            gate: spec.gate.to_string(),
            check: spec.check.to_string(),
            pass: matches!(status, Status::Pass),
            absent: matches!(status, Status::Absent),
            observed,
            expected,
        });
    }

    fn field(&self, path: &str) -> Option<&'a serde_json::Value> {
        self.inputs.health.and_then(|doc| resolve(doc, path))
    }

    fn zero(&mut self, path: &str) {
        let status = match self.field(path) {
            None => Status::Absent,
            Some(v) if v.as_u64() == Some(0) => Status::Pass,
            Some(_) => Status::Fail,
        };
        let observed = self.field(path).cloned().unwrap_or(serde_json::Value::Null);
        self.push(path, status, observed, serde_json::json!(0));
    }

    /// `absent or == 0`: for a legacy token a new build may not emit at all.
    fn zero_or_absent(&mut self, path: &str) {
        let observed = self.field(path).cloned();
        let status = match &observed {
            None => Status::Pass,
            Some(v) if v.as_u64() == Some(0) => Status::Pass,
            Some(_) => Status::Fail,
        };
        self.push(path, status, observed.unwrap_or(serde_json::Value::Null), serde_json::json!("absent or 0"));
    }

    fn present_count(&mut self, path: &str) {
        let observed = self.field(path).cloned();
        let status = match &observed {
            None => Status::Absent,
            Some(v) if v.as_u64().is_some() => Status::Pass,
            Some(_) => Status::Fail,
        };
        self.push(path, status, observed.unwrap_or(serde_json::Value::Null), serde_json::json!("a count"));
    }

    fn eq_str(&mut self, path: &str, expected: Option<&str>) {
        let observed = self.field(path).cloned();
        let status = match (&observed, expected) {
            (None, _) | (_, None) => Status::Absent,
            (Some(v), Some(e)) if v.as_str() == Some(e) => Status::Pass,
            _ => Status::Fail,
        };
        self.push(
            path,
            status,
            observed.unwrap_or(serde_json::Value::Null),
            expected.map_or(serde_json::Value::Null, |e| serde_json::json!(e)),
        );
    }

    fn eq_u64(&mut self, path: &str, expected: u32) {
        let observed = self.field(path).cloned();
        let status = match &observed {
            None => Status::Absent,
            Some(v) if v.as_u64() == Some(u64::from(expected)) => Status::Pass,
            Some(_) => Status::Fail,
        };
        self.push(path, status, observed.unwrap_or(serde_json::Value::Null), serde_json::json!(expected));
    }

    fn before_open(&mut self, check: &str, at: Option<DateTime<Utc>>) {
        let status = match at {
            None => Status::Absent,
            Some(t) if t < self.open => Status::Pass,
            Some(_) => Status::Fail,
        };
        let open = self.open;
        self.push(
            check,
            status,
            at.map_or(serde_json::Value::Null, |t| serde_json::json!(t)),
            serde_json::json!(format!("< {}", open.to_rfc3339())),
        );
    }

    fn designation_eq(&mut self, check: &str, observed: Option<&str>, expected: Option<&str>) {
        let status = match (observed, expected) {
            (Some(o), Some(e)) if o == e => Status::Pass,
            (Some(_), Some(_)) => Status::Fail,
            _ => Status::Absent,
        };
        self.push(
            check,
            status,
            observed.map_or(serde_json::Value::Null, |o| serde_json::json!(o)),
            expected.map_or(serde_json::Value::Null, |e| serde_json::json!(e)),
        );
    }
}

/// Evaluates every row of `GATE_TABLE`. Pure: the same inputs always give the
/// same report, and nothing but the inputs is consulted.
pub fn qualification_gates(inputs: &GateInputs) -> GateReport {
    let day = inputs.market_day;
    let open = market_data::trading_session::market_day_open(day);
    let mut g = Gates { inputs, open, results: Vec::with_capacity(GATE_TABLE.len()) };
    let pins = inputs.pins;
    let designation = inputs.designation.record();

    // completeness-check
    let verdict = inputs.completeness.verdict;
    g.push(
        "completeness.verdict",
        if verdict == Verdict::Valid { Status::Pass } else { Status::Fail },
        serde_json::json!(verdict.to_string()),
        serde_json::json!("VALID"),
    );

    // writer-loss, capacity-eviction, ranking-truncation, duplicate-identity:
    // plain present-and-zero counters.
    for spec in GATE_TABLE {
        if spec.predicate == "present and == 0" && !matches!(spec.gate, "premarket-volume-init") {
            g.zero(spec.check);
        }
    }
    {
        let rank = g.field("report.opportunityEngine.rankCohortCapacity").and_then(|v| v.as_u64());
        let capacity = g.field("report.opportunityEngine.capacity").and_then(|v| v.as_u64());
        let status = match (rank, capacity) {
            (Some(r), Some(c)) if c > 0 && r >= c => Status::Pass,
            (Some(_), Some(_)) => Status::Fail,
            _ => Status::Absent,
        };
        g.push(
            "report.opportunityEngine.rankCohortCapacity",
            status,
            serde_json::json!(rank),
            serde_json::json!(format!(">= capacity ({})", capacity.map_or("absent".to_string(), |c| c.to_string()))),
        );
    }

    // malformed-output
    let present: Vec<&ArtifactEvidence> = inputs.artifacts.iter().filter(|a| a.present).collect();
    let malformed: u64 = present.iter().map(|a| a.malformed_records).sum();
    g.push(
        "artifacts.malformedRecords",
        if present.is_empty() { Status::Absent } else if malformed == 0 { Status::Pass } else { Status::Fail },
        serde_json::json!(malformed),
        serde_json::json!(0),
    );
    let cut: Vec<&str> = present.iter().filter(|a| a.truncated).map(|a| a.path.as_str()).collect();
    g.push(
        "artifacts.truncated",
        if present.is_empty() { Status::Absent } else if cut.is_empty() { Status::Pass } else { Status::Fail },
        serde_json::json!(cut),
        serde_json::json!([]),
    );
    let unmet: Vec<&str> = inputs
        .required_artifacts
        .iter()
        .filter(|req| !present.iter().any(|a| &a.path == *req && a.records > 0))
        .map(|s| s.as_str())
        .collect();
    g.push(
        "artifacts.required",
        if inputs.required_artifacts.is_empty() {
            Status::Absent
        } else if unmet.is_empty() {
            Status::Pass
        } else {
            Status::Fail
        },
        serde_json::json!({ "required": inputs.required_artifacts, "missingOrEmpty": unmet }),
        serde_json::json!("all present, records > 0"),
    );

    // lifecycle-contract
    g.eq_str("report.opportunityEngine.lifecycle", Some(&pins.lifecycle));
    g.eq_str("report.oiVersions.lifecycle", Some(&pins.lifecycle));

    // baseline-truncation
    match inputs.baseline {
        None => {
            g.push("rows.baselineTruncated", Status::Absent, serde_json::Value::Null, serde_json::json!(0));
            g.push("rows.baselineComplete", Status::Absent, serde_json::Value::Null, serde_json::json!("> 0"));
        }
        Some(b) => {
            let readable = b.read_error.is_none();
            g.push(
                "rows.baselineTruncated",
                if !readable {
                    Status::Absent
                } else if b.truncated_rows == 0 {
                    Status::Pass
                } else {
                    Status::Fail
                },
                serde_json::json!({
                    "truncatedRows": b.truncated_rows,
                    "firstTruncated": b.first_truncated,
                    "readError": b.read_error,
                }),
                serde_json::json!(0),
            );
            g.push(
                "rows.baselineComplete",
                if !readable {
                    Status::Absent
                } else if b.complete_rows > 0 {
                    Status::Pass
                } else {
                    Status::Absent
                },
                serde_json::json!({ "completeRows": b.complete_rows, "rowsScanned": b.rows_scanned }),
                serde_json::json!("> 0"),
            );
        }
    }

    // deployed-before-open
    g.before_open("designation.processStartedAt", designation.map(|d| d.process_started_at));
    g.before_open("designation.deployMarkerAt", designation.map(|d| d.deploy_marker_at));

    // premarket-volume-init
    g.zero("premarketVolume.fetchFailures");
    g.eq_str("premarketVolume.marketDay", Some(&day.to_string()));
    {
        let raw = g.field("premarketVolume.initializedAt").cloned();
        let parsed = raw
            .as_ref()
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc));
        let status = match (&raw, parsed) {
            (None, _) => Status::Absent,
            (Some(_), Some(t)) if market_data::trading_session::market_day(t) == day => Status::Pass,
            _ => Status::Fail,
        };
        g.push(
            "premarketVolume.initializedAt",
            status,
            raw.unwrap_or(serde_json::Value::Null),
            serde_json::json!(format!("RFC 3339 within market day {day}")),
        );
    }

    // schema-fingerprint
    let expected_commit = pins.commit.as_deref().or(designation.map(|d| d.commit.as_str()));
    g.eq_str("report.commit", expected_commit);
    g.eq_str("report.oiConfigFingerprint", pins.oi_config_fingerprint.as_deref());
    g.eq_str("report.oiVersions.configFingerprint", pins.oi_config_fingerprint.as_deref());
    g.eq_u64("report.oiVersions.opportunitySchema", pins.opportunity_schema);
    g.eq_u64("report.oiVersions.featureSchema", pins.feature_schema);
    g.eq_str("report.oiVersions.regimeClassifier", Some(&pins.regime_classifier));
    g.eq_str("report.oiVersions.priceRegime", Some(&pins.price_regime));
    g.eq_str("report.oiVersions.earlyQualityModel", Some(&pins.early_quality_model));
    g.eq_str("report.oiVersions.continuationModel", Some(&pins.continuation_model));
    g.eq_str("report.oiVersions.ranking", Some(&pins.ranking));
    g.eq_str("report.oiVersions.scorePolicy", Some(&pins.score_policy));
    g.eq_str("report.oiVersions.baselinePolicy", Some(&pins.baseline_policy));
    g.eq_u64("report.signalContextSchema", pins.signal_context_schema);
    g.eq_u64("report.episodeSchema", pins.episode_schema);
    g.eq_str("report.outcomeMeasurementVersion", Some(&pins.outcome_measurement_version));

    // disposition-consistency
    let counts = g.field("report.opportunityOutcomeEngine.dispositionCounts").cloned();
    let settled = g.field("report.opportunityOutcomeEngine.anchorsSettled").and_then(|v| v.as_u64());
    let outstanding = g.field("report.opportunityOutcomeEngine.outstanding").and_then(|v| v.as_u64());
    let marked = g.field("report.opportunityOutcomeEngine.closureAnchorsMarked").and_then(|v| v.as_u64());
    // Every numeric member is a token, so a token added later is summed
    // without an edit here; a non-numeric member is a malformed count.
    let token_sum = counts.as_ref().and_then(|c| c.as_object()).map(|map| {
        map.values().try_fold(0u64, |acc, v| v.as_u64().map(|n| acc + n))
    });
    {
        let status = match (token_sum, settled) {
            (Some(Some(sum)), Some(s)) if sum == s => Status::Pass,
            (Some(None), _) => Status::Fail,
            (Some(Some(_)), Some(_)) => Status::Fail,
            _ => Status::Absent,
        };
        g.push(
            "report.opportunityOutcomeEngine.dispositionCounts",
            status,
            serde_json::json!({ "sum": token_sum.flatten(), "counts": counts }),
            serde_json::json!({ "anchorsSettled": settled }),
        );
    }
    {
        let still_open = counts
            .as_ref()
            .and_then(|c| c.get("stillOpen"))
            .and_then(|v| v.as_u64());
        let closed = match (token_sum.flatten(), still_open) {
            (Some(sum), Some(open)) => sum.checked_sub(open),
            _ => None,
        };
        let status = match (closed, marked, outstanding) {
            (Some(c), Some(m), Some(o)) if c <= m && m <= c + o => Status::Pass,
            (Some(_), Some(_), Some(_)) => Status::Fail,
            _ => Status::Absent,
        };
        g.push(
            "report.opportunityOutcomeEngine.closureAnchorsMarked",
            status,
            serde_json::json!({ "marked": marked, "settledNonStillOpen": closed, "outstanding": outstanding }),
            serde_json::json!("settledNonStillOpen <= marked <= settledNonStillOpen + outstanding"),
        );
    }
    g.zero_or_absent("report.opportunityOutcomeEngine.dispositionCounts.inactivity");
    g.zero_or_absent("report.opportunityEngine.closedByReason.inactivity");
    g.present_count("report.opportunityOutcomeEngine.dispositionCounts.setupInactivity");
    g.present_count("report.opportunityOutcomeEngine.dispositionCounts.invalidated");

    // designation
    {
        let (status, observed) = match inputs.designation {
            DesignationEvidence::Missing(path) => (Status::Absent, serde_json::json!({ "missing": path })),
            DesignationEvidence::Malformed(error) => (Status::Fail, serde_json::json!({ "malformed": error })),
            DesignationEvidence::Present(r) => {
                let ok = r.schema_version == DESIGNATION_SCHEMA_VERSION
                    && r.market_day == day
                    && r.preflight == "PASS"
                    && !r.designated_by.trim().is_empty();
                (
                    if ok { Status::Pass } else { Status::Fail },
                    serde_json::json!({
                        "schemaVersion": r.schema_version,
                        "marketDay": r.market_day,
                        "preflight": r.preflight,
                        "designatedBy": r.designated_by,
                    }),
                )
            }
        };
        g.push(
            "designation.record",
            status,
            observed,
            serde_json::json!({ "schemaVersion": DESIGNATION_SCHEMA_VERSION, "marketDay": day, "preflight": "PASS" }),
        );
    }
    g.before_open("designation.designatedAt", designation.map(|d| d.designated_at));
    g.designation_eq(
        "designation.specSha256",
        designation.map(|d| d.spec_sha256.as_str()),
        Some(pins.spec_sha256.as_str()),
    );
    g.designation_eq(
        "designation.specVersion",
        designation.map(|d| d.spec_version.as_str()),
        Some(pins.spec_version.as_str()),
    );
    let running_commit = g.field("report.commit").and_then(|v| v.as_str());
    g.designation_eq("designation.commit", designation.map(|d| d.commit.as_str()), running_commit);
    let running_fingerprint = g.field("report.oiConfigFingerprint").and_then(|v| v.as_str());
    g.designation_eq(
        "designation.oiConfigFingerprint",
        designation.map(|d| d.oi_config_fingerprint.as_str()),
        running_fingerprint,
    );
    for (check, reason) in [
        ("protection.research", &inputs.protection.research),
        ("protection.discovery", &inputs.protection.discovery),
    ] {
        g.push(
            check,
            if reason.is_none() { Status::Pass } else { Status::Fail },
            serde_json::json!(reason.clone().unwrap_or_else(|| "designated".into())),
            serde_json::json!("designated"),
        );
    }

    // Table order, so the report reads like the doc table.
    let order = |r: &GateResult| GATE_TABLE.iter().position(|s| s.check == r.check).unwrap_or(usize::MAX);
    g.results.sort_by_key(order);
    GateReport {
        version: GATE_REPORT_VERSION.to_string(),
        market_day: day,
        market_day_open: open,
        results: g.results,
    }
}

#[cfg(test)]
#[path = "completeness_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "qualification_gates_tests.rs"]
mod gate_tests;
