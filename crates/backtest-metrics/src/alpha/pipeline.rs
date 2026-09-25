//! The qualification pipeline: one session in, one immutable report out
//! (§9, §35, §37).
//!
//! # The stop gate is absolute
//!
//! Session status is decided first, from the capture's own completeness
//! evidence. If it is not VALID the pipeline stops **before any Alpha
//! evaluation exists** — not before it is printed, before it is *computed*.
//! There is no code path that produces a predictive claim about a session that
//! failed its gate, which is a stronger guarantee than a formatting rule and
//! the reason `run` returns early rather than filtering later.
//!
//! That matters because the failure mode is seductive: on 2026-09-16 the
//! capture discarded 89% of the session, and scoring the survivors would have
//! produced a confident, precise and entirely meaningless answer about V1.
//!
//! # Immutability
//!
//! The output directory must not exist. A qualification result is evidence, and
//! silently overwriting a previous one would destroy the record of what was
//! concluded before. Source artifacts are opened read-only and never written
//! to — asserted by a test that digests every input before and after a run.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::alpha::dataset::{self, DiscoveryView, OpportunityDataset, SessionArtifacts};
use crate::alpha::evaluate::{self, Evaluation};
use crate::alpha::labels::{self, ReferenceOpportunity};
use crate::alpha::ladder::{Ladder, StageEvidence};
use crate::alpha::matrix::{EvidenceStatus, QualificationMatrix};
use crate::alpha::sha256;
use crate::alpha::spec::QualificationSpec;
use crate::completeness::{
    check, qualification_gates, scan_baseline, CompletenessReport, GateInputs, GateReport,
    ProtectionEvidence, SessionEvidence, SettlementEvidence, Verdict,
};

/// The `/research/completeness` response, as the route actually returns it.
///
/// The operator captures it verbatim; parsing the real shape means the runbook
/// is a `curl` rather than a transcription step, and a transcription step is a
/// place for a session to be described by a document that does not match it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedHealth {
    pub report: CompletenessReport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measurement_pending: Option<MeasurementPending>,
}

/// The measurement collector's pending-set state at capture end.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeasurementPending {
    pub pending: usize,
    #[serde(default)]
    pub pending_peak: usize,
    #[serde(default)]
    pub pending_capacity: usize,
    #[serde(default)]
    pub capacity_evictions: u64,
    #[serde(default)]
    pub open_episodes: usize,
}

/// Accepts either the route's full response or a bare completeness report.
///
/// The bare form is legitimate — an operator may have saved only the report —
/// and yields no settlement evidence, which the checker correctly reports as
/// INDETERMINATE rather than treating as "everything settled".
pub fn parse_health(text: &str) -> Option<CapturedHealth> {
    if let Ok(full) = serde_json::from_str::<CapturedHealth>(text) {
        return Some(full);
    }
    serde_json::from_str::<CompletenessReport>(text)
        .ok()
        .map(|report| CapturedHealth { report, measurement_pending: None })
}

/// What the caller asks for.
#[derive(Debug, Clone)]
pub struct Request {
    pub session_dir: PathBuf,
    pub session_date: String,
    pub expected_commit: Option<String>,
    pub expected_oi_config: Option<String>,
    /// The contract hash the operator recorded *before* the session opened.
    ///
    /// This is what makes §29 immutability mechanical rather than a promise.
    /// The runbook records `--print-spec`'s hash before the market opens; the
    /// analyst passes that same string back after the close. If anything in
    /// the contract moved in between -- a threshold, a control, a dimension's
    /// blocking status -- the hash differs and the run refuses to start.
    /// Without it, a contract edited mid-session would produce a report that
    /// looks exactly as authoritative as one that was truly frozen.
    pub expected_spec_sha256: Option<String>,
    pub output_dir: PathBuf,
    pub spec: QualificationSpec,
}

/// The whole result, also written to disk as `qualification.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Qualification {
    pub generated_at: chrono::DateTime<Utc>,
    pub session_date: String,
    pub session_status: Verdict,
    pub evidence_status: EvidenceStatus,
    pub spec_version: String,
    pub spec_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oi_config_fingerprint: Option<String>,
    pub completeness: crate::completeness::Outcome,
    /// Qualification v4's machine gates, one result per `GATE_TABLE` row.
    /// Already folded into `completeness`; kept whole so every gate's
    /// observed and expected value is on the record, pass or fail.
    pub gates: GateReport,
    pub integrity: dataset::IntegrityReport,
    pub matrix: QualificationMatrix,
    /// Absent when the session did not pass its gate — the evidence was never
    /// computed, not merely withheld.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluation: Option<Evaluation>,
}

/// Why a run could not start at all.
#[derive(Debug)]
pub enum Error {
    OutputExists(PathBuf),
    Io(std::io::Error),
    SpecInvalid(String),
    SpecMismatch { expected: String, actual: String },
    NoArtifacts(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::OutputExists(path) => write!(
                f,
                "output directory {} already exists; a qualification result is evidence and is \
                 never overwritten — choose a new directory",
                path.display()
            ),
            Error::Io(error) => write!(f, "{error}"),
            Error::SpecInvalid(reason) => write!(f, "qualification specification invalid: {reason}"),
            Error::SpecMismatch { expected, actual } => write!(
                f,
                "qualification contract mismatch: this build carries {actual}, but the session was \
                  opened against {expected}. The contract must be frozen before the session \
                  it judges, so the difference is not reconcilable after the fact -- evaluate \
                  with the build that carries the recorded contract, or declare a new session."
            ),
            Error::NoArtifacts(reason) => write!(f, "{reason}"),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::Io(error)
    }
}

/// Runs the pipeline.
///
/// Never modifies a source artifact, never opens a network connection, never
/// places an order, and never tunes a model. It reads files and writes one new
/// directory.
pub fn run(request: &Request) -> Result<Qualification, Error> {
    request.spec.validate().map_err(Error::SpecInvalid)?;
    // Before anything is read. A contract that changed after the session began
    // cannot be reconciled by inspecting the session, so there is nothing to
    // learn by continuing.
    if let Some(expected) = &request.expected_spec_sha256 {
        let actual = request.spec.sha256();
        if !expected.eq_ignore_ascii_case(&actual) {
            return Err(Error::SpecMismatch { expected: expected.clone(), actual });
        }
    }
    if request.output_dir.exists() {
        return Err(Error::OutputExists(request.output_dir.clone()));
    }

    // --- stage 1: artifacts and provenance (§10) --------------------------
    let artifacts = SessionArtifacts::discover(&request.session_dir, &request.session_date);
    if artifacts.oi_snapshots.is_none() {
        return Err(Error::NoArtifacts(format!(
            "no Opportunity Intelligence capture for {} under {}",
            request.session_date,
            request.session_dir.display()
        )));
    }
    let integrity = dataset::integrity(&artifacts);

    // --- stage 2: the completeness gate (§11) -----------------------------
    let captured = artifacts
        .health
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| parse_health(&text));
    let health = captured.as_ref().map(|c| c.report.clone());
    // Settlement rides with the health document, because it is the one place
    // that knows it: an episode still awaiting its outcome was never offered to
    // a writer, so no writer counter can see it. Absent, the checker answers
    // INDETERMINATE rather than assuming everything settled.
    let settlement = captured.as_ref().and_then(|c| {
        let pending = c.measurement_pending.as_ref()?;
        let settled = integrity
            .artifacts
            .iter()
            .find(|a| a.path.contains("episodes-"))
            .map(|a| a.records)
            .unwrap_or(0);
        Some(SettlementEvidence {
            settled,
            unsettled: pending.pending as u64,
            capacity_evicted: pending.capacity_evictions,
        })
    });
    let evidence = SessionEvidence {
        session_date: request.session_date.clone(),
        expected_commit: request.expected_commit.clone(),
        expected_oi_config_fingerprint: request
            .expected_oi_config
            .clone()
            .or_else(|| request.spec.expected_oi_config_fingerprint.clone()),
        health: health.clone(),
        artifacts: integrity.artifacts.clone(),
        required_artifacts: artifacts.required(),
        settlement,
    };
    let mut completeness = check(&evidence);
    // Integrity problems are blocking in their own right, and are folded in so
    // one verdict covers the whole session rather than two that can disagree.
    for problem in &integrity.blocking {
        completeness.blocking.push(problem.clone());
    }
    if !completeness.blocking.is_empty() {
        completeness.verdict = Verdict::Invalid;
    }

    // --- stage 2b: qualification v4 machine gates (P3 §16) ------------------
    //
    // The AND of every gate, folded into the same verdict. The health document
    // is re-read *raw*: the gates look fields up by name so one this build's
    // `CompletenessReport` does not know is absent -- a failure -- instead of
    // a serde default of zero.
    let market_day = dataset::session_date_from(&request.session_date);
    let health_value: Option<serde_json::Value> = artifacts
        .health
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok());
    let gates = match market_day {
        Some(day) => {
            let baseline = artifacts.oi_snapshots.as_ref().map(|path| scan_baseline(path, day));
            let designation = crate::completeness::load_designation(&request.session_dir, day);
            let protection = ProtectionEvidence::load(&request.session_dir, day);
            let pins = request.spec.pins(request.expected_commit.clone());
            qualification_gates(&GateInputs {
                market_day: day,
                health: health_value.as_ref(),
                completeness: &completeness,
                artifacts: &evidence.artifacts,
                required_artifacts: &evidence.required_artifacts,
                baseline: baseline.as_ref(),
                designation: &designation,
                protection: &protection,
                pins: &pins,
            })
        }
        None => {
            return Err(Error::NoArtifacts(format!(
                "session date {:?} is not YYYY-MM-DD, so its market day cannot be gated",
                request.session_date
            )))
        }
    };
    gates.fold_into(&mut completeness);

    let mut qualification = Qualification {
        generated_at: Utc::now(),
        session_date: request.session_date.clone(),
        session_status: completeness.verdict,
        evidence_status: EvidenceStatus::NotEvaluated,
        spec_version: request.spec.version.clone(),
        spec_sha256: request.spec.sha256(),
        capture_commit: health.as_ref().and_then(|h| h.commit.clone()),
        oi_config_fingerprint: health.as_ref().and_then(|h| h.oi_config_fingerprint.clone()),
        completeness,
        gates,
        integrity,
        matrix: QualificationMatrix::not_evaluated(),
        evaluation: None,
    };

    // --- §37: stop here unless the session is VALID -----------------------
    //
    // Deliberately a return, not a flag. Nothing below this line runs for a
    // session that failed its gate, so no predictive claim about it can exist
    // even as an unprinted value.
    if qualification.session_status != Verdict::Valid {
        write_outputs(request, &qualification, None)?;
        return Ok(qualification);
    }

    // --- stage 3: the dataset and the independent reference ---------------
    let top_k = vec![1usize, 3, 5, 10];
    let opportunities =
        dataset::read_opportunities(artifacts.oi_snapshots.as_ref().unwrap(), &top_k)?;
    let discovery = dataset::read_discovery(&artifacts.discovery, &request.session_date)?;
    let session_date = dataset::session_date_from(&request.session_date)
        .unwrap_or_else(|| Utc::now().date_naive());
    let reference = build_reference(&discovery, session_date, &request.spec);
    let ladders = build_ladders(&opportunities, &discovery, &reference, &request.spec);

    // --- stage 4: evaluation ----------------------------------------------
    let evaluation = evaluate::evaluate(&evaluate::Inputs {
        spec: &request.spec,
        dataset: &opportunities,
        discovery: &discovery,
        ladders: &ladders,
        reference: &reference,
    });
    qualification.matrix = QualificationMatrix::assemble(
        &request.spec,
        Verdict::Valid,
        evaluation.results.clone(),
        evaluation.shortfalls.clone(),
        evaluation.notes.clone(),
    );
    qualification.evidence_status = qualification.matrix.evidence_status;
    qualification.evaluation = Some(evaluation);

    write_outputs(request, &qualification, Some((&opportunities, &reference, &ladders)))?;
    Ok(qualification)
}

/// Labels every symbol discovery saw, using the frozen reference definition.
fn build_reference(
    discovery: &DiscoveryView,
    session_date: chrono::NaiveDate,
    spec: &QualificationSpec,
) -> Vec<ReferenceOpportunity> {
    discovery
        .series
        .iter()
        .map(|(symbol, series)| {
            labels::label(symbol, session_date, series, &spec.reference_label)
        })
        .collect()
}

/// Builds each reference opportunity's stage ladder.
///
/// Three-valued throughout: a stage is `false` only where the evidence
/// positively shows absence, and unevidenced everywhere else.
fn build_ladders(
    opportunities: &OpportunityDataset,
    discovery: &DiscoveryView,
    reference: &[ReferenceOpportunity],
    spec: &QualificationSpec,
) -> Vec<Ladder> {
    let by_symbol: BTreeMap<&str, &crate::alpha::dataset::OpportunityRow> = opportunities
        .rows
        .iter()
        .map(|row| (row.symbol.as_str(), row))
        .collect();
    let primary_top_k = 5usize;

    reference
        .iter()
        .filter(|r| r.ineligible.is_none())
        .map(|r| {
            let opportunity = by_symbol.get(r.symbol.as_str());
            let visible = if discovery.visible.contains(&r.symbol) {
                StageEvidence { reached: Some(true), first_at: r.start_at }
            } else {
                // The discovery capture is the only evidence of visibility, so
                // a symbol absent from it is unevidenced rather than absent.
                StageEvidence::unevidenced()
            };
            let detected = if discovery.detected.contains(&r.symbol) {
                StageEvidence { reached: Some(true), first_at: r.start_at }
            } else if discovery.visible.contains(&r.symbol) {
                // Visible in the ignition stream with no confirmed stage: the
                // detector demonstrably ran on it and did not confirm.
                StageEvidence::absent()
            } else {
                StageEvidence::unevidenced()
            };
            let created = match opportunity {
                Some(row) => StageEvidence {
                    reached: Some(true),
                    first_at: Some(row.first_ranked_at),
                },
                // The OI capture covers the whole session, so a detected symbol
                // with no opportunity is positively absent rather than unknown.
                None if detected.reached == Some(true) => StageEvidence::absent(),
                None => StageEvidence::unevidenced(),
            };
            let early_available = match opportunity {
                Some(row) if row.early_quality_available => {
                    StageEvidence { reached: Some(true), first_at: Some(row.first_ranked_at) }
                }
                Some(_) => StageEvidence::absent(),
                None => StageEvidence::unevidenced(),
            };
            let continuation_available = match opportunity {
                Some(row) if row.continuation_available => {
                    StageEvidence { reached: Some(true), first_at: Some(row.first_ranked_at) }
                }
                Some(_) => StageEvidence::absent(),
                None => StageEvidence::unevidenced(),
            };
            let ranked = |rank: Option<usize>, at: Option<chrono::DateTime<Utc>>| match rank {
                Some(_) => StageEvidence { reached: Some(true), first_at: at },
                None if opportunity.is_some() => StageEvidence::absent(),
                None => StageEvidence::unevidenced(),
            };
            let top_k = match opportunity {
                Some(row) => match row.first_top_k_early.get(&primary_top_k) {
                    Some(at) => StageEvidence::reached_at(*at),
                    None => StageEvidence::absent(),
                },
                None => StageEvidence::unevidenced(),
            };
            let crossing = r.crossings.first().and_then(|c| match c {
                labels::Crossing::Crossed { at, .. } => Some(*at),
                _ => None,
            });
            let remaining = match (r.session_high, opportunity) {
                (Some(high), Some(row)) if row.first_price > 0.0 => {
                    Some((high - row.first_price) / row.first_price * 100.0)
                }
                _ => None,
            };
            Ladder {
                symbol: r.symbol.clone(),
                visible,
                detected,
                opportunity_created: created,
                early_quality_available: early_available,
                early_ranked: ranked(
                    opportunity.and_then(|row| row.first_window_early_rank),
                    opportunity.map(|row| row.first_ranked_at),
                ),
                continuation_available,
                continuation_ranked: ranked(
                    opportunity.and_then(|row| row.first_window_continuation_rank),
                    opportunity.map(|row| row.first_ranked_at),
                ),
                top_k,
                primary_crossing_at: crossing,
                remaining_excursion_pct: remaining,
            }
        })
        .map(|ladder| {
            let _ = spec;
            ladder
        })
        .collect()
}

type Extras<'a> = (
    &'a OpportunityDataset,
    &'a [ReferenceOpportunity],
    &'a [Ladder],
);

/// Writes the immutable output directory (§35).
fn write_outputs(
    request: &Request,
    qualification: &Qualification,
    extras: Option<Extras>,
) -> Result<(), Error> {
    let dir = &request.output_dir;
    std::fs::create_dir_all(dir)?;

    let mut written: Vec<String> = Vec::new();
    let mut write = |name: &str, content: &str| -> Result<(), Error> {
        std::fs::write(dir.join(name), content)?;
        written.push(name.to_string());
        Ok(())
    };

    write("qualification-spec.json", &request.spec.canonical_json())?;
    write("qualification-spec.sha256", &format!("{}  qualification-spec.json\n", request.spec.sha256()))?;
    write("completeness.json", &serde_json::to_string_pretty(&qualification.completeness).unwrap())?;
    write("integrity.json", &serde_json::to_string_pretty(&qualification.integrity).unwrap())?;
    write("qualification.json", &serde_json::to_string_pretty(qualification).unwrap())?;

    if let Some((opportunities, reference, ladders)) = extras {
        write("opportunities.ndjson", &ndjson(&opportunities.rows))?;
        write("reference-opportunities.ndjson", &ndjson(reference))?;
        write("stage-ladder.ndjson", &ndjson(ladders))?;
        if let Some(evaluation) = &qualification.evaluation {
            write("surfaces.json", &serde_json::to_string_pretty(&evaluation.surfaces).unwrap())?;
            write("segmentation.json", &serde_json::to_string_pretty(&evaluation.segments).unwrap())?;
            write(
                "secondary-direction.json",
                &serde_json::to_string_pretty(&evaluation.secondary_direction).unwrap(),
            )?;
            write("recall.json", &serde_json::to_string_pretty(&evaluation.ladder).unwrap())?;
        }
    }

    let report = crate::alpha::report::render(request, qualification);
    write("FINAL-ALPHA-QUALIFICATION.md", &report)?;

    let manifest = serde_json::json!({
        "generatedAt": qualification.generated_at,
        "sessionDate": qualification.session_date,
        "sessionDir": request.session_dir.display().to_string(),
        "sessionStatus": qualification.session_status,
        "evidenceStatus": qualification.evidence_status,
        "specVersion": qualification.spec_version,
        "specSha256": qualification.spec_sha256,
        "captureCommit": qualification.capture_commit,
        "oiConfigFingerprint": qualification.oi_config_fingerprint,
        "sourceDigests": qualification.integrity.digests,
        "files": written,
    });
    std::fs::write(dir.join("manifest.json"), serde_json::to_string_pretty(&manifest).unwrap())?;
    written.push("manifest.json".to_string());

    // SHA256SUMS last, over everything else, in the form `sha256sum -c` reads.
    let mut sums = String::new();
    for name in &written {
        let digest = sha256::hex_file(&dir.join(name))?;
        sums.push_str(&format!("{digest}  {name}\n"));
    }
    std::fs::write(dir.join("SHA256SUMS"), sums)?;
    Ok(())
}

fn ndjson<T: Serialize>(rows: &[T]) -> String {
    let mut out = String::new();
    for row in rows {
        out.push_str(&serde_json::to_string(row).unwrap_or_default());
        out.push('\n');
    }
    out
}

#[cfg(test)]
#[path = "pipeline_tests.rs"]
mod tests;
