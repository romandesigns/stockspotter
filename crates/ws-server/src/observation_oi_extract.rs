//! D6 ranking snapshots -> normalised `oi_rank` artifact (Step 4B-main.2 §9-10).
//!
//! Source: the opportunity-intelligence research capture, exactly as the
//! production writer leaves it -- per UTC day, a data file of
//! `OpportunityScoreSnapshot` lines and a sibling markers file of
//! `CaptureMarker` lines. Lines are parsed as those real types, as the alpha
//! dataset reader does, so a line of any other shape is malformed.
//!
//! # Why completeness needs a whole writer process
//!
//! The writer reports queue drops in-band (`queue_loss` markers) but **write
//! errors only in its live health counters**, never in the artifact. The only
//! in-band proof that nothing was lost is reconciliation: the engine's
//! `scoresEmitted`, carried by the `capture_finished` marker, must equal the
//! rows actually on disk. Those counters span one writer *process*, which in
//! production outlives a session by days, so the extractor takes every day
//! file of one process bracket (`writer_started` .. `capture_finished`) and
//! establishes completeness only if:
//!
//! * exactly one `writer_started` and one `capture_finished` are present;
//! * the rows across all supplied data files equal `scoresEmitted`;
//! * there is no `queue_loss`, `ranking_cohort_truncated` or
//!   `opportunity_capacity_reached` marker (each is known loss);
//! * no data or marker line is malformed, of an unsupported schema, or of a
//!   foreign capture.
//!
//! Window ids (`oiw-N`) are process-scoped, which is the second reason a
//! single process is required: two processes in one session would reuse ids.
//!
//! Anything short of that is recorded, not repaired: the artifact says
//! `completenessEstablished: false` and the authenticated join fails closed.
//!
//! # Binding
//!
//! The normalised bytes are deterministic (rows sorted, header fields fixed).
//! A sidecar [`OiBinding`] binds their SHA-256 to the source files' SHA-256s,
//! the session, the implementation SHA and the extraction contract; the join
//! refuses a normalised artifact whose binding does not check out.

use std::collections::BTreeMap;

use backtest_metrics::opportunity::{OpportunityScoreSnapshot, OPPORTUNITY_SCHEMA_VERSION};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::prereg::sha256_hex;
use crate::research_writer::CaptureMarker;

pub const EXTRACTION_CONTRACT: &str = "d6-oi-extract-v1";
pub const OI_ARTIFACT_SCHEMA: &str = "oi-rank-artifact-v1";
pub const OI_BINDING_SCHEMA: &str = "oi-rank-binding-v1";
/// The only source unit accepted: `move-v1` opportunities (schema 3). Schema-2
/// `symbol-activity-v1` rows denote a different unit and are refused.
pub const SUPPORTED_SOURCE_SCHEMA: u32 = OPPORTUNITY_SCHEMA_VERSION;
/// The capture stem the markers must belong to.
pub const SOURCE_CAPTURE: &str = "opportunity-intelligence";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceIdentity {
    /// `data` or `markers`.
    pub role: String,
    pub name: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OiBinding {
    pub schema: String,
    pub extraction_contract: String,
    pub session: String,
    pub implementation_sha: String,
    pub source_schema_version: u32,
    pub sources: Vec<SourceIdentity>,
    pub normalized_sha256: String,
    pub normalized_rows: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionReport {
    pub data_lines: u64,
    pub session_rows: u64,
    pub other_session_rows: u64,
    pub malformed_rows: u64,
    pub unsupported_schema_rows: u64,
    pub duplicate_identical: u64,
    pub duplicate_conflicting: u64,
    pub malformed_markers: u64,
    pub foreign_markers: u64,
    pub writer_processes: u64,
    pub capture_finished: u64,
    pub scores_emitted: Option<u64>,
    pub queue_lost: u64,
    pub cohort_truncations: u64,
    pub capacity_evictions: u64,
    pub known_loss: u64,
    pub reconciled: bool,
    pub completeness_established: bool,
    pub completeness_reasons: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Extraction {
    pub normalized: Vec<u8>,
    pub binding: OiBinding,
    pub report: ExtractionReport,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Row {
    window_id: String,
    opportunity_id: String,
    rank: Option<u64>,
    computed_at: String,
}

fn valid_session(s: &str) -> bool {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok_and(|d| d.to_string() == s)
}

/// Extracts one session's `earlyQualityRank` rows from the day files of one
/// writer process. `data` and `markers` are `(file name, exact bytes)`, in any
/// order; identities are recorded in the order given, which the binding fixes.
pub fn extract(
    session: &str,
    data: &[(&str, &[u8])],
    markers: &[(&str, &[u8])],
    implementation_sha: &str,
) -> Result<Extraction, String> {
    if !valid_session(session) {
        return Err(format!("session {session:?} is not YYYY-MM-DD"));
    }
    if implementation_sha.len() != 40 || !implementation_sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("implementation SHA must be 40 hex".into());
    }
    let mut r = ExtractionReport::default();
    let mut rows: BTreeMap<(String, String), Vec<Row>> = BTreeMap::new();
    for (_, bytes) in data {
        for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            r.data_lines += 1;
            let Ok(s) = serde_json::from_slice::<OpportunityScoreSnapshot>(line) else {
                r.malformed_rows += 1;
                continue;
            };
            if s.schema_version != SUPPORTED_SOURCE_SCHEMA {
                r.unsupported_schema_rows += 1;
                continue;
            }
            // A move-v1 id is `symbol:session:sequence`; its session must be
            // the row's own, or the row is internally inconsistent.
            let parts: Vec<&str> = s.opportunity_id.split(':').collect();
            if s.window_id.is_empty() || parts.len() != 3 || parts[1] != s.session_date || parts[0].is_empty() {
                r.malformed_rows += 1;
                continue;
            }
            if s.session_date != session {
                r.other_session_rows += 1;
                continue;
            }
            r.session_rows += 1;
            let row = Row {
                window_id: s.window_id.clone(),
                opportunity_id: s.opportunity_id.clone(),
                rank: s.early_quality_rank.map(|x| x as u64),
                computed_at: s.timestamp.to_rfc3339_opts(SecondsFormat::Nanos, true),
            };
            rows.entry((row.window_id.clone(), row.opportunity_id.clone())).or_default().push(row);
        }
    }
    for (_, bytes) in markers {
        for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            let Ok(m) = serde_json::from_slice::<CaptureMarker>(line) else {
                r.malformed_markers += 1;
                continue;
            };
            if m.capture != SOURCE_CAPTURE {
                r.foreign_markers += 1;
                continue;
            }
            match m.kind.as_str() {
                "writer_started" => r.writer_processes += 1,
                "capture_finished" => {
                    r.capture_finished += 1;
                    match m.data.as_ref().and_then(|d| d["scoresEmitted"].as_u64()) {
                        Some(n) => r.scores_emitted = Some(r.scores_emitted.unwrap_or(0) + n),
                        None => r.malformed_markers += 1,
                    }
                }
                "queue_loss" => match m.queue_loss {
                    Some(span) => r.queue_lost += span.lost,
                    None => r.malformed_markers += 1,
                },
                "ranking_cohort_truncated" => r.cohort_truncations += 1,
                "opportunity_capacity_reached" => r.capacity_evictions += 1,
                _ => {}
            }
        }
    }
    // Duplicates: identical rows (same rank AND same computation time)
    // collapse; anything else is kept, both rows, for the join to refuse.
    let mut out: Vec<Row> = Vec::new();
    for (_, mut group) in rows {
        group.sort();
        let before = group.len() as u64;
        group.dedup();
        r.duplicate_identical += before - group.len() as u64;
        if group.len() > 1 {
            r.duplicate_conflicting += group.len() as u64 - 1;
        }
        out.extend(group);
    }
    r.known_loss = r.queue_lost + r.cohort_truncations + r.capacity_evictions;
    r.reconciled = r.scores_emitted == Some(r.data_lines);
    let mut reasons = Vec::new();
    if r.writer_processes != 1 {
        reasons.push(format!("{} writer_started markers (exactly one process required)", r.writer_processes));
    }
    if r.capture_finished != 1 {
        reasons.push(format!("{} capture_finished markers (exactly one required)", r.capture_finished));
    }
    if !r.reconciled {
        reasons.push(format!("rows on disk {} != scoresEmitted {:?}", r.data_lines, r.scores_emitted));
    }
    if r.known_loss != 0 {
        reasons.push(format!("known loss {}", r.known_loss));
    }
    if r.malformed_rows + r.unsupported_schema_rows + r.malformed_markers + r.foreign_markers != 0 {
        reasons.push("malformed, unsupported or foreign source lines".into());
    }
    r.completeness_established = reasons.is_empty();
    r.completeness_reasons = reasons;

    let sources: Vec<SourceIdentity> = data
        .iter()
        .map(|(n, b)| ("data", n, b))
        .chain(markers.iter().map(|(n, b)| ("markers", n, b)))
        .map(|(role, name, b)| SourceIdentity { role: role.into(), name: (*name).into(), sha256: sha256_hex(b), bytes: b.len() as u64 })
        .collect();
    let header = json!({
        "kind": "oi_rank_artifact",
        "schema": OI_ARTIFACT_SCHEMA,
        "extractionContract": EXTRACTION_CONTRACT,
        "session": session,
        "implementationSha": implementation_sha,
        "sourceSchemaVersion": SUPPORTED_SOURCE_SCHEMA,
        "sources": sources,
        "knownLoss": r.known_loss,
        "sourceMalformedRows": r.malformed_rows + r.unsupported_schema_rows + r.malformed_markers + r.foreign_markers,
        "completenessEstablished": r.completeness_established,
        "completenessReasons": r.completeness_reasons,
        "report": r,
    });
    let mut normalized = serde_json::to_vec(&header).map_err(|e| e.to_string())?;
    normalized.push(b'\n');
    for row in &out {
        let line = json!({
            "kind": "oi_rank",
            "session": session,
            "windowId": row.window_id,
            "opportunityId": row.opportunity_id,
            "earlyQualityRank": row.rank,
            "computedAt": row.computed_at,
        });
        normalized.extend(serde_json::to_vec(&line).map_err(|e| e.to_string())?);
        normalized.push(b'\n');
    }
    let binding = OiBinding {
        schema: OI_BINDING_SCHEMA.into(),
        extraction_contract: EXTRACTION_CONTRACT.into(),
        session: session.into(),
        implementation_sha: implementation_sha.into(),
        source_schema_version: SUPPORTED_SOURCE_SCHEMA,
        sources,
        normalized_sha256: sha256_hex(&normalized),
        normalized_rows: out.len() as u64,
    };
    Ok(Extraction { normalized, binding, report: r })
}

/// Re-runs the extraction from the source bytes and checks that it
/// reproduces the bound normalised artifact exactly (deterministic rerun).
pub fn verify_extraction(
    binding: &OiBinding,
    normalized: &[u8],
    data: &[(&str, &[u8])],
    markers: &[(&str, &[u8])],
) -> Result<(), String> {
    let again = extract(&binding.session, data, markers, &binding.implementation_sha)?;
    if again.binding != *binding {
        return Err("re-extraction does not reproduce the binding (a source differs)".into());
    }
    if again.normalized != normalized {
        return Err("re-extraction does not reproduce the normalised bytes".into());
    }
    Ok(())
}

/// For tests and tooling: the RFC 3339 form `computedAt` uses.
pub fn computed_at(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Nanos, true)
}
