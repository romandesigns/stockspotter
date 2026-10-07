//! D6 ranking snapshots -> normalised `oi_rank` artifact.
//!
//! **Two modes.** [`extract`] (`d6-oi-extract-v2`, Step 4B-main.3) certifies
//! one Step-4 session from its in-band `oi_session_finished` marker, without
//! the writer process ending; it is the only mode the Step-4 join accepts.
//! [`extract_legacy_process_close`] (`d6-oi-extract-v1-legacy-process-close`)
//! is the 4B-main.2 process-bracket mode, kept for historical research on
//! captures written before session markers existed; its artifacts are
//! labelled as such and refused by the join. The notes below describe why the
//! legacy mode needed a whole process.
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

pub const EXTRACTION_CONTRACT: &str = "d6-oi-extract-v2";
/// Historical research only; never accepted for a designated session.
pub const LEGACY_EXTRACTION_CONTRACT: &str = "d6-oi-extract-v1-legacy-process-close";
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
    /// v2: the writer process whose session this is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_fingerprint: Option<String>,
    /// v2: the exact session reconciliation marker the proof rests on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_marker: Option<MarkerIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkerIdentity {
    pub file: String,
    /// SHA-256 of the marker's exact line (without its newline).
    pub sha256: String,
    pub process_id: String,
    pub session: String,
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
pub fn extract_legacy_process_close(
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
        "extractionContract": LEGACY_EXTRACTION_CONTRACT,
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
        extraction_contract: LEGACY_EXTRACTION_CONTRACT.into(),
        session: session.into(),
        implementation_sha: implementation_sha.into(),
        source_schema_version: SUPPORTED_SOURCE_SCHEMA,
        sources,
        normalized_sha256: sha256_hex(&normalized),
        normalized_rows: out.len() as u64,
        process_id: None,
        config_fingerprint: None,
        session_marker: None,
    };
    Ok(Extraction { normalized, binding, report: r })
}

/// Re-runs the extraction from the source bytes and checks that it
/// reproduces the bound normalised artifact exactly (deterministic rerun).
pub fn verify_legacy_extraction(
    binding: &OiBinding,
    normalized: &[u8],
    data: &[(&str, &[u8])],
    markers: &[(&str, &[u8])],
) -> Result<(), String> {
    let again = extract_legacy_process_close(&binding.session, data, markers, &binding.implementation_sha)?;
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

// ===========================================================================
// v2: session-bounded certification (Step 4B-main.3)
// ===========================================================================

use crate::research_writer::{BarrierResult, SessionRange, SessionTally, SESSION_MARKER_KIND};

/// The identities a designated session's OI evidence must carry.
pub struct ExpectedSource<'a> {
    pub implementation_sha: &'a str,
    pub config_fingerprint: &'a str,
}

/// Everything the v2 extractor established, reported whether or not the
/// session certifies.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionReport {
    pub session_markers: u64,
    pub late_record_markers: u64,
    pub malformed_markers: u64,
    pub foreign_markers: u64,
    pub process_id: Option<String>,
    pub tally: Option<SessionTally>,
    pub rows_in_ranges: u64,
    pub foreign_rows_in_ranges: u64,
    pub session_rows_outside_ranges: u64,
    pub unparseable_outside_ranges: u64,
    pub duplicate_identical: u64,
    pub duplicate_conflicting: u64,
    pub distinct_windows: u64,
    pub known_loss: u64,
    pub completeness_established: bool,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SessionExtraction {
    pub normalized: Vec<u8>,
    pub binding: OiBinding,
    pub report: SessionReport,
}

fn window_number(id: &str) -> Option<u64> {
    id.strip_prefix("oiw-")?.parse().ok()
}

/// Lines of `bytes` with their byte offsets (line excludes its newline).
fn lines_with_offsets(bytes: &[u8]) -> Vec<(u64, u64, &[u8])> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            out.push((start as u64, (i + 1) as u64, &bytes[start..i]));
            start = i + 1;
        }
    }
    if start < bytes.len() {
        // An unterminated tail (e.g. a crash mid-write): not a complete line.
        out.push((start as u64, bytes.len() as u64, &bytes[start..]));
    }
    out
}

/// Certifies Step-4 session `session` from the in-band reconciliation.
///
/// `data` are the UTC-day data files that may hold the session's rows (a
/// session spans two UTC dates), `markers` the marker files that may hold
/// its `oi_session_finished`, each as `(file name, exact bytes)`. The
/// session certifies only if ALL of:
///
/// * exactly one `oi_session_finished` for the session exists, well formed,
///   closed by the session boundary, with accounting that began at or before
///   the session start (no restart inside the session), no late records;
/// * its implementation, configuration and source schema are the expected;
/// * its tally balances (`attempted == written + dropped + write_errors`)
///   with zero dropped, write errors, loss spans and flush errors, and
///   `engine.scoresEmitted == attempted`;
/// * its barrier flushed and synced with exact ranges;
/// * zero truncations, capacity evictions and dropped markers of either;
/// * every range's bytes hash to the marker's SHA-256 and hold exactly its
///   row count, all of this session; no row of this session exists outside
///   the ranges; the rows reconcile with `written`, and their distinct
///   windows with `windowsWithRows` and the first/last window ids;
/// * no marker line is malformed or foreign.
pub fn extract(
    session: &str,
    data: &[(&str, &[u8])],
    markers: &[(&str, &[u8])],
    expected: &ExpectedSource<'_>,
) -> Result<SessionExtraction, String> {
    if !valid_session(session) {
        return Err(format!("session {session:?} is not YYYY-MM-DD"));
    }
    let day = chrono::NaiveDate::parse_from_str(session, "%Y-%m-%d").map_err(|e| e.to_string())?;
    let mut rep = SessionReport::default();
    let mut reasons: Vec<String> = Vec::new();

    // --- markers ----------------------------------------------------------
    let mut found: Vec<(String, Vec<u8>, CaptureMarker)> = Vec::new();
    for (name, bytes) in markers {
        for (_, _, line) in lines_with_offsets(bytes) {
            if line.is_empty() {
                continue;
            }
            let Ok(m) = serde_json::from_slice::<CaptureMarker>(line) else {
                rep.malformed_markers += 1;
                continue;
            };
            if m.capture != SOURCE_CAPTURE {
                rep.foreign_markers += 1;
                continue;
            }
            let about = m.data.as_ref().and_then(|d| d["session"].as_str()) == Some(session);
            match m.kind.as_str() {
                k if k == SESSION_MARKER_KIND && about => found.push(((*name).to_string(), line.to_vec(), m)),
                "oi_session_late_record" if about => rep.late_record_markers += 1,
                _ => {}
            }
        }
    }
    rep.session_markers = found.len() as u64;
    if rep.malformed_markers + rep.foreign_markers > 0 {
        reasons.push("malformed or foreign marker lines (a session marker could be among them)".into());
    }
    if rep.late_record_markers > 0 {
        reasons.push(format!("{} records arrived after the session barrier", rep.late_record_markers));
    }
    let marker = match found.len() {
        1 => Some(found.remove(0)),
        0 => {
            reasons.push("no oi_session_finished for the session (process ended before its barrier?)".into());
            None
        }
        n => {
            let identical = found.windows(2).all(|w| w[0].1 == w[1].1);
            reasons.push(format!("{n} oi_session_finished markers for one session ({})", if identical { "duplicate" } else { "conflicting" }));
            None
        }
    };

    let mut ranges: Vec<SessionRange> = Vec::new();
    let mut tally = SessionTally::default();
    let mut windows_with_rows = 0u64;
    let (mut first_window, mut last_window) = (None::<String>, None::<String>);
    let mut process_id = None::<String>;
    let mut marker_identity = None::<MarkerIdentity>;
    let mut marker_config = None::<String>;
    if let Some((file, line, m)) = &marker {
        let d = m.data.clone().unwrap_or_default();
        let text = |k: &str| d[k].as_str().map(str::to_string);
        let num = |v: &serde_json::Value| v.as_u64();
        process_id = m.process_id.clone();
        if process_id.is_none() || text("processId") != process_id {
            reasons.push("marker process identity missing or inconsistent".into());
        }
        if text("closedBy").as_deref() != Some("session_boundary") {
            reasons.push(format!("session closed by {:?}, not the session boundary", text("closedBy")));
        }
        match text("accountingStart").as_deref() {
            Some("session_boundary" | "process_start_before_session") => {}
            other => reasons.push(format!("accounting began {other:?}: the process did not cover the whole session")),
        }
        if text("implementationSha").as_deref() != Some(expected.implementation_sha) {
            reasons.push("implementation SHA differs".into());
        }
        marker_config = text("configFingerprint");
        if marker_config.as_deref() != Some(expected.config_fingerprint) {
            reasons.push("configuration fingerprint differs".into());
        }
        if d["sourceSchema"].as_u64() != Some(u64::from(SUPPORTED_SOURCE_SCHEMA)) {
            reasons.push("source schema is not the supported one".into());
        }
        let (start, end) = crate::observation::step4_session_bounds(day);
        let at = |k: &str| d[k].as_str().and_then(|s| s.parse::<DateTime<Utc>>().ok());
        if at("sessionStart") != Some(start) || at("sessionEnd") != Some(end) {
            reasons.push("session bounds differ from the Step-4 session definition".into());
        }
        match serde_json::from_value::<SessionTally>(d["tally"].clone()) {
            Ok(t) => tally = t,
            Err(_) => reasons.push("tally missing".into()),
        }
        let barrier: BarrierResult = serde_json::from_value(d["barrier"].clone()).unwrap_or_default();
        if !(barrier.flushed && barrier.synced && barrier.range_integrity) {
            reasons.push(format!("session barrier incomplete: {:?}", barrier.errors));
        }
        match serde_json::from_value::<Vec<SessionRange>>(d["ranges"].clone()) {
            Ok(r) => ranges = r,
            Err(_) => reasons.push("ranges missing".into()),
        }
        let e = &d["engine"];
        let eng = |k: &str| num(&e[k]);
        if tally.attempted != tally.written + tally.dropped + tally.write_errors {
            reasons.push("tally does not balance".into());
        }
        if tally.dropped + tally.write_errors + tally.loss_spans + tally.flush_errors + tally.late_after_close > 0 {
            reasons.push(format!("session loss: {tally:?}"));
        }
        if eng("scoresEmitted") != Some(tally.attempted) {
            reasons.push("engine scoresEmitted does not equal the rows offered".into());
        }
        for k in ["capacityEvictions", "cohortTruncations", "evictionMarkersDropped", "truncationMarkersDropped"] {
            match eng(k) {
                Some(0) => {}
                other => reasons.push(format!("engine {k} = {other:?}")),
            }
        }
        rep.known_loss = tally.dropped
            + tally.write_errors
            + ["capacityEvictions", "cohortTruncations", "evictionMarkersDropped", "truncationMarkersDropped"]
                .iter()
                .filter_map(|k| eng(k))
                .sum::<u64>();
        windows_with_rows = num(&d["windowsWithRows"]).unwrap_or(u64::MAX);
        first_window = text("firstWindowId");
        last_window = text("lastWindowId");
        marker_identity = Some(MarkerIdentity {
            file: file.clone(),
            sha256: sha256_hex(line),
            process_id: process_id.clone().unwrap_or_default(),
            session: session.into(),
        });
    }
    rep.process_id = process_id.clone();
    rep.tally = marker.as_ref().map(|_| tally.clone());

    // --- rows ---------------------------------------------------------------
    let source_by_name: BTreeMap<&str, &[u8]> = data.iter().map(|(n, b)| (*n, *b)).collect();
    let mut rows: BTreeMap<(String, String), Vec<Row>> = BTreeMap::new();
    let mut covered: BTreeMap<&str, Vec<(u64, u64)>> = BTreeMap::new();
    for r in &ranges {
        let Some(bytes) = source_by_name.get(r.file.as_str()) else {
            reasons.push(format!("range file {} not supplied", r.file));
            continue;
        };
        let (a, b) = (r.start_offset as usize, r.end_offset as usize);
        if a > b || b > bytes.len() {
            reasons.push(format!("range {}[{a}..{b}) is outside the file", r.file));
            continue;
        }
        let slice = &bytes[a..b];
        if sha256_hex(slice) != r.sha256 {
            reasons.push(format!("range {}[{a}..{b}) does not hash to the marker's SHA-256", r.file));
            continue;
        }
        covered.entry(r.file.as_str()).or_default().push((r.start_offset, r.end_offset));
        let mut n = 0u64;
        for (_, _, line) in lines_with_offsets(slice) {
            let row = serde_json::from_slice::<OpportunityScoreSnapshot>(line).ok().filter(|s| {
                s.schema_version == SUPPORTED_SOURCE_SCHEMA
                    && crate::observation::step4_session_of(s.timestamp) == day
                    && { let p: Vec<&str> = s.opportunity_id.split(':').collect(); p.len() == 3 && !p[0].is_empty() && p[1] == s.session_date }
                    && !s.window_id.is_empty()
            });
            let Some(s) = row else {
                rep.foreign_rows_in_ranges += 1;
                continue;
            };
            n += 1;
            let row = Row {
                window_id: s.window_id.clone(),
                opportunity_id: s.opportunity_id.clone(),
                rank: s.early_quality_rank.map(|x| x as u64),
                computed_at: s.timestamp.to_rfc3339_opts(SecondsFormat::Nanos, true),
            };
            rows.entry((row.window_id.clone(), row.opportunity_id.clone())).or_default().push(row);
        }
        if n != r.rows {
            reasons.push(format!("range {} holds {n} session rows, marker says {}", r.file, r.rows));
        }
        rep.rows_in_ranges += n;
    }
    if rep.foreign_rows_in_ranges > 0 {
        reasons.push(format!("{} lines inside certified ranges are not this session's rows", rep.foreign_rows_in_ranges));
    }
    for (name, bytes) in data {
        let spans = covered.get(name).cloned().unwrap_or_default();
        for (a, b, line) in lines_with_offsets(bytes) {
            if spans.iter().any(|(s, e)| a >= *s && b <= *e) || line.is_empty() {
                continue;
            }
            match serde_json::from_slice::<OpportunityScoreSnapshot>(line) {
                Ok(s) if crate::observation::step4_session_of(s.timestamp) == day => rep.session_rows_outside_ranges += 1,
                Ok(_) => {}
                Err(_) => rep.unparseable_outside_ranges += 1,
            }
        }
    }
    if rep.session_rows_outside_ranges > 0 {
        reasons.push(format!("{} rows of this session lie outside the certified ranges", rep.session_rows_outside_ranges));
    }
    if marker.is_some() && rep.rows_in_ranges != tally.written {
        reasons.push(format!("rows in ranges {} != written {}", rep.rows_in_ranges, tally.written));
    }

    let mut out: Vec<Row> = Vec::new();
    for (_, mut group) in rows {
        group.sort();
        let before = group.len() as u64;
        group.dedup();
        rep.duplicate_identical += before - group.len() as u64;
        if group.len() > 1 {
            rep.duplicate_conflicting += group.len() as u64 - 1;
        }
        out.extend(group);
    }
    let windows: std::collections::BTreeSet<&str> = out.iter().map(|r| r.window_id.as_str()).collect();
    rep.distinct_windows = windows.len() as u64;
    if marker.is_some() {
        if rep.distinct_windows != windows_with_rows {
            reasons.push(format!("{} distinct windows in rows, marker says {windows_with_rows}", rep.distinct_windows));
        }
        let lo = windows.iter().filter_map(|w| window_number(w)).min();
        let hi = windows.iter().filter_map(|w| window_number(w)).max();
        if lo != first_window.as_deref().and_then(window_number) || hi != last_window.as_deref().and_then(window_number) {
            reasons.push("first/last window ids differ from the rows".into());
        }
    }
    rep.completeness_established = reasons.is_empty();
    rep.reasons = reasons;

    // --- normalised artifact -----------------------------------------------
    let sources: Vec<SourceIdentity> = data
        .iter()
        .map(|(n, b)| ("data", n, b))
        .chain(markers.iter().map(|(n, b)| ("markers", n, b)))
        .map(|(role, name, b)| SourceIdentity { role: role.into(), name: (*name).into(), sha256: sha256_hex(b), bytes: b.len() as u64 })
        .collect();
    let relevant_malformed = rep.foreign_rows_in_ranges + rep.malformed_markers + rep.foreign_markers;
    let header = json!({
        "kind": "oi_rank_artifact",
        "schema": OI_ARTIFACT_SCHEMA,
        "extractionContract": EXTRACTION_CONTRACT,
        "session": session,
        "processId": process_id,
        "implementationSha": expected.implementation_sha,
        "configFingerprint": marker_config,
        "sourceSchemaVersion": SUPPORTED_SOURCE_SCHEMA,
        "sources": sources,
        "sessionMarker": marker_identity,
        "ranges": ranges,
        "knownLoss": rep.known_loss,
        "sourceMalformedRows": relevant_malformed,
        "completenessEstablished": rep.completeness_established,
        "completenessReasons": rep.reasons,
        "report": rep,
    });
    let mut normalized = serde_json::to_vec(&header).map_err(|e| e.to_string())?;
    normalized.push(b'\n');
    for row in &out {
        let line = json!({
            "kind": "oi_rank",
            "session": session,
            "processId": process_id,
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
        implementation_sha: expected.implementation_sha.into(),
        source_schema_version: SUPPORTED_SOURCE_SCHEMA,
        sources,
        normalized_sha256: sha256_hex(&normalized),
        normalized_rows: out.len() as u64,
        process_id,
        config_fingerprint: marker_config,
        session_marker: marker_identity,
    };
    Ok(SessionExtraction { normalized, binding, report: rep })
}

/// Re-runs the v2 extraction from the source bytes; it must reproduce the
/// bound artifact byte for byte.
pub fn verify_extraction(
    binding: &OiBinding,
    normalized: &[u8],
    data: &[(&str, &[u8])],
    markers: &[(&str, &[u8])],
    expected: &ExpectedSource<'_>,
) -> Result<(), String> {
    let again = extract(&binding.session, data, markers, expected)?;
    if again.binding != *binding {
        return Err("re-extraction does not reproduce the binding (a source differs)".into());
    }
    if again.normalized != normalized {
        return Err("re-extraction does not reproduce the normalised bytes".into());
    }
    Ok(())
}
