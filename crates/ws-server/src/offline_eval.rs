//! Step-4 offline evaluator: `ws-server step4-eval <command> ...`.
//!
//! Built only with `--features offline-eval`, on the topic branch
//! `topic/step4-offline-evaluator-20261001`. **Never deployed, never part of
//! the production image.** It runs off production against exported, compressed,
//! closed observer runs.
//!
//! It is a CLI *around* the frozen implementations and decides nothing itself:
//!
//! | command              | frozen code it calls                                    |
//! |----------------------|---------------------------------------------------------|
//! | `certify-run`        | `stream::assess_streaming_bound` (the L1 certifier)     |
//! | `extract-oi-session` | `oi_extract::extract` (`d6-oi-extract-v2`)              |
//! | `qualify-session`    | the certifier, `Preregistration::evaluate` (floors),    |
//! |                      | `analysis::extract_session` + `select` (discriminating) |
//! | `replay-campaign`    | `campaign::Campaign::replay`                            |
//! | `record-designation` | `Campaign::apply(Designated)`                           |
//! | `record-skip`        | (operating rule only; the frozen ledger is untouched)   |
//! | `record-capture`     | `Campaign::apply(CaptureRecorded)`                      |
//!
//! What is new here is only evidence intake (receipt-verified decompression),
//! the boundary-run check, the two GPT operating rules (designation deadline
//! 04:00 ET of market day d; every eligible regular session in calendar order,
//! skips recorded with a reason), and append-only ledger I/O.
//!
//! **There is no outcome command.** Nothing here can obtain an `OutcomeAccess`;
//! `replay-campaign` reports the firewall's refusal as evidence that it holds.
//!
//! Every command prints exactly one JSON object and fails closed: exit 0 = done
//! (or PASS), 1 = refused / FAIL, 2 = INDETERMINATE, 64 = usage.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::observation::{
    self, analysis, campaign, oi_extract, prereg, stream, BoundIdentity, CaptureVerdict, FloorVerdict, Preregistration,
};

pub const QUALIFICATION_SCHEMA: &str = "step4-eval-qualification-v1";
pub const OI_EXTRACTION_SCHEMA: &str = "step4-eval-oi-extraction-v1";
pub const SKIP_SCHEMA: &str = "step4-operational-skip-v1";
const ARCHIVE_RECEIPT: &str = "archive-receipt.json";
const RECEIPT_SCHEMA: &str = "observation-archive-receipt-v1";
/// Files an archived or exported run directory may legitimately hold besides
/// its listed `.gz` artifacts. Anything else refuses the evidence.
const EVIDENCE_SIDE_FILES: [&str; 5] =
    ["archive-manifest.json", ARCHIVE_RECEIPT, "archive-deletion.json", "export-receipt.json", "certificate.json"];
/// A rollover fires on the first 1 s tick at or after the boundary.
const BOUNDARY_TOLERANCE_SECS: i64 = 5;

const USAGE: &str = "usage: ws-server step4-eval <command>
  certify-run <evidence_dir> <prereg.json>
  extract-oi-session <session> <research_dir> <implementation_sha> <oi_fingerprint>
  qualify-session <session> <evidence_dir> <prereg.json> <oi_evidence.json>
  replay-campaign <ledger.ndjson> <skips.ndjson> <campaign_start>
  record-designation <ledger.ndjson> <skips.ndjson> <campaign_start> <session>
  record-skip <ledger.ndjson> <skips.ndjson> <campaign_start> <session> <reason>
  record-capture <ledger.ndjson> <qualification.json>";

pub fn cli(args: &[String]) -> i32 {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let (code, out) = match a.as_slice() {
        ["certify-run", dir, pre] => certify_run(Path::new(dir), Path::new(pre)),
        ["extract-oi-session", s, dir, i, f] => extract_oi_session(s, Path::new(dir), i, f),
        ["qualify-session", s, dir, pre, oi] => qualify_session(s, Path::new(dir), Path::new(pre), Path::new(oi)),
        ["replay-campaign", l, k, start] => replay_campaign(Path::new(l), Path::new(k), start),
        ["record-designation", l, k, start, s] => record_designation(Path::new(l), Path::new(k), start, s, Utc::now()),
        ["record-skip", l, k, start, s, reason] => record_skip(Path::new(l), Path::new(k), start, s, reason, Utc::now()),
        ["record-capture", l, q] => record_capture(Path::new(l), Path::new(q)),
        _ => {
            eprintln!("{USAGE}");
            return 64;
        }
    };
    println!("{out}");
    code
}

fn refuse(why: impl std::fmt::Display) -> (i32, Value) {
    (1, json!({ "refused": why.to_string() }))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ===========================================================================
// Frozen preregistration
// ===========================================================================

/// The frozen preregistration, loaded through the production validator.
pub struct Frozen {
    pub jcs_sha256: String,
    pub implementation_sha: String,
    pub oi_fingerprint: String,
    pub floors: Preregistration,
    pub selection: analysis::SelectionConfig,
}

pub fn load_frozen(path: &Path) -> Result<Frozen, String> {
    let bound = prereg::load(path).map_err(|e| format!("preregistration refused: {e}"))?;
    let v = &bound.value;
    if v["freezeStatus"].as_str() != Some("FINAL") {
        return Err("preregistration is not FINAL".into());
    }
    let int = |p: &str| -> Result<u64, String> {
        p.split('.').fold(Some(v), |acc, k| acc.and_then(|x| x.get(k))).and_then(Value::as_u64).ok_or(format!("{p} missing"))
    };
    let text = |p: &str| -> Result<String, String> {
        v.get(p).and_then(Value::as_str).map(str::to_string).ok_or(format!("{p} missing"))
    };
    let floors = Preregistration {
        protocol_version: text("protocolVersion")?,
        gate_sha256: text("gateSha256")?,
        eligibility_floor: int("floorsBasisPoints.eligibility")? as f64 / 10_000.0,
        provenance_establishment_floor: int("floorsBasisPoints.provenanceEstablishment")? as f64 / 10_000.0,
        freshness_max_age_secs: (int("freshness.primaryMaxAgeMs")? / 1000) as i64,
        // The frozen Step-4 preregistration declares no re-freeze ladder:
        // one attempt, no search for a passing threshold.
        refreeze_ladder_secs: Vec::new(),
        max_refreezes: 0,
    };
    floors.validate().map_err(|e| format!("preregistration floors refused: {e}"))?;
    Ok(Frozen {
        jcs_sha256: bound.sha256.clone(),
        implementation_sha: bound.implementation_sha().to_string(),
        oi_fingerprint: text("oiConfigFingerprint")?,
        floors,
        selection: analysis::SelectionConfig {
            budget: int("selection.budget")? as usize,
            min_pool: int("selection.minDiscriminatingPool")? as usize,
        },
    })
}

// ===========================================================================
// Evidence intake
// ===========================================================================

/// A run's records, materialised as the certifier expects them: a directory
/// named for the run holding `observations-N.ndjson`.
pub struct Evidence {
    pub dir: PathBuf,
    pub run_id: String,
    pub kind: &'static str,
    pub receipt: Option<Value>,
    pub receipt_sha256: Option<String>,
    scratch: Option<PathBuf>,
}

impl Drop for Evidence {
    fn drop(&mut self) {
        if let Some(s) = &self.scratch {
            let _ = std::fs::remove_dir_all(s);
        }
    }
}

fn is_source(name: &str) -> bool {
    name.strip_prefix("observations-")
        .and_then(|r| r.strip_suffix(".ndjson"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Opens evidence. An archived run (`archive-receipt.json` present) is
/// verified against its receipt and decompressed to scratch; every compressed
/// artifact must hash to the receipt and decompress to the recorded source
/// bytes. A raw run is accepted only where `allow_raw` (certification of
/// fixtures and local runs), never for qualification.
pub fn open_evidence(dir: &Path, allow_raw: bool) -> Result<Evidence, String> {
    let run_id = dir.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or("evidence path has no name")?;
    let names: BTreeSet<String> = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read evidence: {e}"))?
        .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    if !names.contains(ARCHIVE_RECEIPT) {
        if !allow_raw {
            return Err("no archive receipt: qualification requires archived, receipt-verified evidence".into());
        }
        if names.iter().any(|n| !is_source(n)) || names.is_empty() {
            return Err(format!("raw evidence must hold only observations-N.ndjson files: {names:?}"));
        }
        return Ok(Evidence { dir: dir.to_path_buf(), run_id, kind: "raw", receipt: None, receipt_sha256: None, scratch: None });
    }
    let rbytes = std::fs::read(dir.join(ARCHIVE_RECEIPT)).map_err(|e| e.to_string())?;
    let receipt: Value = serde_json::from_slice(&rbytes).map_err(|e| format!("malformed archive receipt: {e}"))?;
    if receipt["schema"].as_str() != Some(RECEIPT_SCHEMA) {
        return Err("not an observation archive receipt".into());
    }
    if receipt["runId"].as_str() != Some(run_id.as_str()) {
        return Err("archive receipt names a different run".into());
    }
    let sources = receipt["sources"].as_array().ok_or("receipt has no sources")?;
    let compressed = receipt["compressed"].as_array().ok_or("receipt has no compressed artifacts")?;
    let listed: BTreeSet<String> = compressed.iter().filter_map(|c| c["file"].as_str().map(str::to_string)).collect();
    for n in &names {
        if !(listed.contains(n) || EVIDENCE_SIDE_FILES.contains(&n.as_str()) || is_source(n)) {
            return Err(format!("unknown file in evidence: {n}"));
        }
    }
    let scratch_root = std::env::var_os("STEP4_EVAL_SCRATCH")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("step4-eval-{}-{}", std::process::id(), Utc::now().timestamp_nanos_opt().unwrap_or_default()));
    let out = scratch_root.join(&run_id);
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let ev = Evidence {
        dir: out.clone(),
        run_id: run_id.clone(),
        kind: "archived",
        receipt: Some(receipt.clone()),
        receipt_sha256: Some(sha256_hex(&rbytes)),
        scratch: Some(scratch_root),
    };
    if sources.len() != compressed.len() || sources.is_empty() {
        return Err("receipt sources and compressed artifacts do not pair up".into());
    }
    for s in sources {
        let src = s["file"].as_str().ok_or("receipt source without file")?;
        if !is_source(src) {
            return Err(format!("receipt source {src:?} is not an observation file"));
        }
        let c = compressed.iter().find(|c| c["source"].as_str() == Some(src)).ok_or(format!("no artifact for {src}"))?;
        let gz_name = c["file"].as_str().ok_or("artifact without file")?;
        let gz = std::fs::read(dir.join(gz_name)).map_err(|e| format!("missing compressed artifact {gz_name}: {e}"))?;
        if sha256_hex(&gz) != c["sha256"].as_str().unwrap_or_default() || Some(gz.len() as u64) != c["bytes"].as_u64() {
            return Err(format!("compressed artifact {gz_name} does not match its receipt"));
        }
        let mut dec = Vec::new();
        flate2::read::GzDecoder::new(&gz[..])
            .read_to_end(&mut dec)
            .map_err(|e| format!("corrupt compressed artifact {gz_name}: {e}"))?;
        if sha256_hex(&dec) != s["sha256"].as_str().unwrap_or_default() || Some(dec.len() as u64) != s["bytes"].as_u64() {
            return Err(format!("decompression of {gz_name} does not reproduce the recorded source"));
        }
        std::fs::write(out.join(src), &dec).map_err(|e| e.to_string())?;
    }
    Ok(ev)
}

/// `run_start.startedAt` and `run_end.endedAt` of materialised evidence.
fn run_bounds(dir: &Path) -> Result<(DateTime<Utc>, DateTime<Utc>), String> {
    let mut files: Vec<String> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .filter(|n| is_source(n))
        .collect();
    files.sort_by_key(|n| observation::rotation_index(n));
    let first = files.first().ok_or("no observation files")?;
    let last = files.last().ok_or("no observation files")?;
    let first_bytes = std::fs::read(dir.join(first)).map_err(|e| e.to_string())?;
    let start: Value = serde_json::from_slice(first_bytes.split(|b| *b == b'\n').next().unwrap_or_default())
        .map_err(|_| "malformed run_start")?;
    let last_bytes = std::fs::read(dir.join(last)).map_err(|e| e.to_string())?;
    let lines: Vec<&[u8]> = last_bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()).collect();
    let end: Value = lines
        .len()
        .checked_sub(2)
        .and_then(|i| serde_json::from_slice(lines[i]).ok())
        .ok_or("missing run_end")?;
    if start["kind"] != "run_start" || end["kind"] != "run_end" {
        return Err("run_start/run_end not where a closed run keeps them".into());
    }
    let ts = |v: &Value, k: &str| {
        v[k].as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|t| t.with_timezone(&Utc)).ok_or(format!("{k} missing"))
    };
    Ok((ts(&start, "startedAt")?, ts(&end, "endedAt")?))
}

fn parse_session(s: &str) -> Result<NaiveDate, String> {
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| format!("session {s:?} is not YYYY-MM-DD"))?;
    if d.format("%Y-%m-%d").to_string() != s {
        return Err(format!("session {s:?} is not canonical YYYY-MM-DD"));
    }
    Ok(d)
}

/// The run started at the boundary that opens `session` and ended at the one
/// that closes it -- the only shape a designated session's run may have.
pub fn boundary_run(session: NaiveDate, started: DateTime<Utc>, ended: DateTime<Utc>) -> bool {
    let (start, end) = observation::step4_session_bounds(session);
    let tol = Duration::seconds(BOUNDARY_TOLERANCE_SECS);
    started >= start && started < start + tol && ended >= end && ended < end + tol
}

// ===========================================================================
// certify-run
// ===========================================================================

fn certify_run(dir: &Path, pre: &Path) -> (i32, Value) {
    let frozen = match load_frozen(pre) {
        Ok(f) => f,
        Err(e) => return refuse(e),
    };
    let ev = match open_evidence(dir, true) {
        Ok(e) => e,
        Err(e) => return refuse(e),
    };
    let expected = BoundIdentity {
        implementation_sha: frozen.implementation_sha.clone(),
        preregistration_sha256: frozen.jcs_sha256.clone(),
    };
    let (verdict, stats) = stream::assess_streaming_bound(&ev.dir, &expected);
    let (detail, certificate) = match &verdict {
        CaptureVerdict::Pass(c) => (Value::Null, serde_json::to_value(c.as_ref()).unwrap_or_default()),
        CaptureVerdict::Fail(d) => (Value::String(d.clone()), Value::Null),
        CaptureVerdict::Indeterminate(i) => (Value::String(i.to_string()), Value::Null),
    };
    let code = match verdict {
        CaptureVerdict::Pass(_) => 0,
        CaptureVerdict::Fail(_) => 1,
        CaptureVerdict::Indeterminate(_) => 2,
    };
    (code, json!({
        "schema": "observation-certificate-v1",
        "runDir": ev.run_id,
        "evidence": ev.kind,
        "archiveReceiptSha256": ev.receipt_sha256,
        "boundTo": { "implementationSha": expected.implementation_sha, "preregistrationSha256": expected.preregistration_sha256 },
        "verdict": verdict.label(),
        "detail": detail,
        "certificate": certificate,
        "bytesRead": stats.bytes_read,
    }))
}

// ===========================================================================
// extract-oi-session
// ===========================================================================

fn extract_oi_session(session: &str, research: &Path, implementation_sha: &str, fingerprint: &str) -> (i32, Value) {
    let day = match parse_session(session) {
        Ok(d) => d,
        Err(e) => return refuse(e),
    };
    let (start, end) = observation::step4_session_bounds(day);
    // Data files: the UTC dates the session spans (a session crosses midnight
    // UTC). Marker files: ALL of them. A barrier is filed under the wall-clock
    // UTC date it was written, which a deferred barrier can push past the
    // session; marker files are small, and the frozen extractor itself picks
    // out this session's barrier and refuses duplicates.
    let mut dates = BTreeSet::new();
    let mut d = start.date_naive();
    while d <= end.date_naive() {
        dates.insert(d);
        d += Duration::days(1);
    }
    let read = |name: String| std::fs::read(research.join(&name)).ok().map(|b| (name, b));
    let data: Vec<(String, Vec<u8>)> = dates.iter().filter_map(|d| read(format!("opportunity-intelligence-{d}.ndjson"))).collect();
    let mut marker_names: Vec<String> = match std::fs::read_dir(research) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .filter(|n| n.starts_with("opportunity-intelligence-markers-") && n.ends_with(".ndjson"))
            .collect(),
        Err(e) => return refuse(format!("cannot read research directory: {e}")),
    };
    marker_names.sort();
    let markers: Vec<(String, Vec<u8>)> = marker_names.into_iter().filter_map(read).collect();
    let dv: Vec<(&str, &[u8])> = data.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    let mv: Vec<(&str, &[u8])> = markers.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    let expected = oi_extract::ExpectedSource { implementation_sha, config_fingerprint: fingerprint };
    match oi_extract::extract(session, &dv, &mv, &expected) {
        Err(e) => refuse(e),
        Ok(x) => {
            let ok = x.report.completeness_established;
            (if ok { 0 } else { 1 }, json!({
                "schema": OI_EXTRACTION_SCHEMA,
                "extractionContract": "d6-oi-extract-v2",
                "session": session,
                "implementationSha": implementation_sha,
                "configFingerprint": fingerprint,
                "certifies": ok,
                "report": x.report,
                "normalizedSha256": x.binding.normalized_sha256,
                "normalizedRows": x.binding.normalized_rows,
            }))
        }
    }
}

/// OI evidence for qualification: this tool's `extract-oi-session` output, or
/// the reviewed read-only VPS check (`phaseb_marker_check.py`), which GPT
/// accepted for sessions whose ~10 GB/day data cannot reasonably be copied.
/// Either way the session, implementation and fingerprint must be the frozen
/// ones and the evidence must certify.
pub fn oi_zero_loss(v: &Value, session: &str, frozen: &Frozen) -> Result<(bool, &'static str), String> {
    let kind = if v["schema"].as_str() == Some(OI_EXTRACTION_SCHEMA) {
        "d6-oi-extract-v2"
    } else if v.get("certifies").is_some() && v.get("tally").is_some() && v.get("ranges").is_some() {
        "phaseb_marker_check"
    } else {
        return Err("unrecognised OI evidence".into());
    };
    if v["session"].as_str() != Some(session) {
        return Err("OI evidence is for a different session".into());
    }
    if v["implementationSha"].as_str() != Some(frozen.implementation_sha.as_str()) {
        return Err("OI evidence implementation SHA is not the frozen one".into());
    }
    if v["configFingerprint"].as_str() != Some(frozen.oi_fingerprint.as_str()) {
        return Err("OI evidence configuration fingerprint is not the frozen one".into());
    }
    if kind == "phaseb_marker_check" && v["accountingStart"].as_str() != Some("session_boundary") {
        return Ok((false, kind));
    }
    Ok((v["certifies"].as_bool() == Some(true), kind))
}

// ===========================================================================
// qualify-session
// ===========================================================================

fn qualify_session(session: &str, dir: &Path, pre: &Path, oi: &Path) -> (i32, Value) {
    let day = match parse_session(session) {
        Ok(d) => d,
        Err(e) => return refuse(e),
    };
    if market_data::trading_session::regular_session_open(day).is_none() {
        return refuse(format!("{session} has no regular trading session"));
    }
    let frozen = match load_frozen(pre) {
        Ok(f) => f,
        Err(e) => return refuse(e),
    };
    let oi_value: Value = match std::fs::read(oi).map_err(|e| e.to_string()).and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string())) {
        Ok(v) => v,
        Err(e) => return refuse(format!("OI evidence unreadable: {e}")),
    };
    let (oi_ok, oi_kind) = match oi_zero_loss(&oi_value, session, &frozen) {
        Ok(x) => x,
        Err(e) => return refuse(e),
    };
    let ev = match open_evidence(dir, false) {
        Ok(e) => e,
        Err(e) => return refuse(e),
    };
    let receipt = ev.receipt.as_ref().expect("archived evidence has a receipt");
    if receipt["implementationSha"].as_str() != Some(frozen.implementation_sha.as_str())
        || receipt["preregistrationSha256"].as_str() != Some(frozen.jcs_sha256.as_str())
    {
        return refuse("archive receipt identity is not the frozen implementation/preregistration");
    }
    let boundary = match run_bounds(&ev.dir) {
        Ok((s, e)) => boundary_run(day, s, e),
        Err(e) => return refuse(e),
    };
    let expected = BoundIdentity {
        implementation_sha: frozen.implementation_sha.clone(),
        preregistration_sha256: frozen.jcs_sha256.clone(),
    };
    let (verdict, _) = stream::assess_streaming_bound(&ev.dir, &expected);
    let verdict_label = verdict.label();
    let (floor_label, floors_satisfied) = match &verdict {
        CaptureVerdict::Pass(cert) => {
            let f = frozen.floors.evaluate(cert);
            (f.label(), matches!(f, FloorVerdict::Met { .. }))
        }
        _ => ("NOT_EVALUATED", false),
    };
    let (discriminating, in_scope, invalid) = if matches!(verdict, CaptureVerdict::Pass(_)) {
        match analysis::extract_session(&ev.dir) {
            Ok(x) => {
                // Pool membership does not depend on OI ranks; no rank is
                // supplied and no arm is read here.
                let oi = analysis::OiRanks { zero_loss_established: oi_ok, rows: Default::default() };
                let sel = analysis::select(&x, &oi, frozen.selection);
                let disc = sel.windows.iter().filter(|w| w.discriminating).count() as u64;
                let scope =
                    sel.windows.iter().filter(|w| w.discriminating && analysis::in_primary_scope(w.anchor_at)).count() as u64;
                (disc, scope, sel.invalid_windows)
            }
            Err(e) => return refuse(e),
        }
    } else {
        (0, 0, 0)
    };
    let q = campaign::CaptureQualification {
        session: session.to_string(),
        run_id: ev.run_id.clone(),
        // A certificate counts only for the run that spans the session from
        // boundary to boundary.
        certificate_pass: matches!(verdict, CaptureVerdict::Pass(_)) && boundary,
        floors_satisfied,
        discriminating_windows: discriminating,
        oi_zero_loss: oi_ok,
    };
    let qualifies = q.qualifies(&campaign::CampaignRules::default());
    (0, json!({
        "schema": QUALIFICATION_SCHEMA,
        "qualification": q,
        "qualifies": qualifies,
        "detail": {
            "certificateVerdict": verdict_label,
            "boundaryRun": boundary,
            "floorVerdict": floor_label,
            "discriminatingWindows": discriminating,
            "discriminatingWindowsInPrimaryScope": in_scope,
            "invalidWindows": invalid,
            "oiEvidence": oi_kind,
            "archiveReceiptSha256": ev.receipt_sha256,
            "preregistrationSha256": frozen.jcs_sha256,
            "implementationSha": frozen.implementation_sha,
        },
    }))
}

// ===========================================================================
// Campaign ledger and operating rules
// ===========================================================================

/// The ledger's canonical line for an event. Replay refuses any line that is
/// not exactly this, so the file has one spelling and no hidden fields.
pub fn event_line(e: &campaign::CampaignEvent) -> String {
    serde_json::to_string(e).expect("campaign events serialize")
}

pub fn parse_ledger(text: &str) -> Result<Vec<campaign::CampaignEvent>, String> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.is_empty() {
            return Err(format!("ledger line {} is empty", i + 1));
        }
        let e: campaign::CampaignEvent = serde_json::from_str(line).map_err(|e| format!("ledger line {}: {e}", i + 1))?;
        if event_line(&e) != line {
            return Err(format!("ledger line {} is not in canonical form", i + 1));
        }
        out.push(e);
    }
    if !text.is_empty() && !text.ends_with('\n') {
        return Err("ledger does not end with a newline".into());
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Skip {
    pub session: String,
    pub reason: String,
}

pub fn parse_skips(text: &str) -> Result<Vec<Skip>, String> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let v: Value = serde_json::from_str(line).map_err(|e| format!("skips line {}: {e}", i + 1))?;
        let session = v["session"].as_str().ok_or(format!("skips line {}: no session", i + 1))?;
        let reason = v["reason"].as_str().filter(|r| !r.trim().is_empty()).ok_or(format!("skips line {}: no reason", i + 1))?;
        if v["schema"].as_str() != Some(SKIP_SCHEMA) {
            return Err(format!("skips line {}: wrong schema", i + 1));
        }
        parse_session(session)?;
        out.push(Skip { session: session.into(), reason: reason.into() });
    }
    if !text.is_empty() && !text.ends_with('\n') {
        return Err("skips file does not end with a newline".into());
    }
    Ok(out)
}

/// Every regular NYSE session from `from` on, in calendar order.
fn next_regular_session(from: NaiveDate) -> NaiveDate {
    let mut d = from;
    while market_data::trading_session::regular_session_open(d).is_none() {
        d += Duration::days(1);
    }
    d
}

/// The calendar-order rule: designations and skips together must be exactly
/// the regular sessions from `campaign_start`, in order, each once. Returns
/// the next session the rule permits.
pub fn calendar_cursor(events: &[campaign::CampaignEvent], skips: &[Skip], campaign_start: NaiveDate) -> Result<NaiveDate, String> {
    let designated: Vec<NaiveDate> = events
        .iter()
        .filter_map(|e| match e {
            campaign::CampaignEvent::Designated { session } => Some(parse_session(session)),
            _ => None,
        })
        .collect::<Result<_, _>>()?;
    let skipped: Vec<NaiveDate> = skips.iter().map(|s| parse_session(&s.session)).collect::<Result<_, _>>()?;
    let mut all: Vec<NaiveDate> = designated.iter().chain(skipped.iter()).copied().collect();
    all.sort();
    let unique: BTreeSet<NaiveDate> = all.iter().copied().collect();
    if unique.len() != all.len() {
        return Err("a session is both designated and skipped, or listed twice".into());
    }
    if designated.windows(2).any(|w| w[0] >= w[1]) {
        return Err("designations are not in calendar order".into());
    }
    let mut expect = next_regular_session(campaign_start);
    for d in &all {
        if *d != expect {
            return Err(format!("calendar order broken: expected {expect}, found {d} (a skipped session must be recorded with its reason)"));
        }
        expect = next_regular_session(*d + Duration::days(1));
    }
    Ok(expect)
}

fn read_text(p: &Path) -> Result<String, String> {
    match std::fs::read(p) {
        Ok(b) => String::from_utf8(b).map_err(|_| format!("{} is not UTF-8", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(format!("{} does not exist (create it empty)", p.display())),
        Err(e) => Err(e.to_string()),
    }
}

fn append_line(p: &Path, line: &str) -> Result<(), String> {
    let mut f = std::fs::OpenOptions::new().append(true).open(p).map_err(|e| e.to_string())?;
    f.write_all(format!("{line}\n").as_bytes()).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())
}

/// Loads and validates ledger + skips under both the frozen campaign rules and
/// the operating rules.
pub fn load_campaign(
    ledger: &str,
    skips: &str,
    campaign_start: NaiveDate,
) -> Result<(campaign::Campaign, Vec<campaign::CampaignEvent>, Vec<Skip>, NaiveDate), String> {
    let events = parse_ledger(ledger)?;
    let skips = parse_skips(skips)?;
    let c = campaign::Campaign::replay(campaign::CampaignRules::default(), &events).map_err(|e| format!("ledger replay refused: {e:?}"))?;
    let next = calendar_cursor(&events, &skips, campaign_start)?;
    Ok((c, events, skips, next))
}

fn replay_campaign(ledger: &Path, skips: &Path, start: &str) -> (i32, Value) {
    let r = (|| {
        let start = parse_session(start)?;
        let (c, events, skips, next) = load_campaign(&read_text(ledger)?, &read_text(skips)?, start)?;
        let pending: Vec<&String> =
            c.designated().iter().filter(|s| !events.iter().any(|e| matches!(e, campaign::CampaignEvent::CaptureRecorded { qualification } if &qualification.session == *s))).collect();
        Ok::<Value, String>(json!({
            "state": c.state(),
            "events": events.len(),
            "designated": c.designated(),
            "pendingCapture": pending,
            "qualifyingSessions": c.qualifying_sessions(),
            "skipped": skips.iter().map(|s| json!({"session": s.session, "reason": s.reason})).collect::<Vec<_>>(),
            "nextSessionByCalendarRule": next.to_string(),
            "outcomeAccess": match c.outcome_access() { Ok(_) => "AVAILABLE".to_string(), Err(e) => format!("LOCKED: {e:?}") },
        }))
    })();
    match r {
        Ok(v) => (0, v),
        Err(e) => refuse(e),
    }
}

/// The designation window for session `d`: from the boundary that opens it
/// (its run must already exist) to 04:00 ET of market day `d`.
pub fn designation_window(d: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    (observation::step4_session_bounds(d).0, market_data::trading_session::market_day_open(d))
}

pub fn designate(ledger: &str, skips: &str, campaign_start: NaiveDate, session: &str, now: DateTime<Utc>) -> Result<String, String> {
    let d = parse_session(session)?;
    if market_data::trading_session::regular_session_open(d).is_none() {
        return Err(format!("{session} has no regular trading session; it is never designated"));
    }
    let (mut c, _, _, next) = load_campaign(ledger, skips, campaign_start)?;
    if d != next {
        return Err(format!("calendar order: the next session is {next}, not {session}"));
    }
    let (open, deadline) = designation_window(d);
    if now < open {
        return Err(format!("too early: {session}'s run starts at {open}"));
    }
    if now >= deadline {
        return Err(format!("deadline passed: designation for {session} closed at {deadline} (04:00 ET); record a skip"));
    }
    let e = campaign::CampaignEvent::Designated { session: session.to_string() };
    c.apply(e.clone()).map_err(|e| format!("campaign refused: {e:?}"))?;
    Ok(event_line(&e))
}

fn record_designation(ledger: &Path, skips: &Path, start: &str, session: &str, now: DateTime<Utc>) -> (i32, Value) {
    let r = (|| {
        let line = designate(&read_text(ledger)?, &read_text(skips)?, parse_session(start)?, session, now)?;
        append_line(ledger, &line)?;
        Ok::<_, String>(line)
    })();
    match r {
        Ok(line) => (0, json!({ "appended": serde_json::from_str::<Value>(&line).unwrap_or_default(), "recordedAt": now })),
        Err(e) => refuse(e),
    }
}

pub fn skip_line(ledger: &str, skips: &str, campaign_start: NaiveDate, session: &str, reason: &str, now: DateTime<Utc>) -> Result<String, String> {
    let d = parse_session(session)?;
    if reason.trim().is_empty() {
        return Err("a skip needs a reason".into());
    }
    let (_, _, _, next) = load_campaign(ledger, skips, campaign_start)?;
    if d != next {
        return Err(format!("calendar order: the next session is {next}, not {session}"));
    }
    Ok(json!({ "schema": SKIP_SCHEMA, "session": session, "reason": reason, "recordedAt": now }).to_string())
}

fn record_skip(ledger: &Path, skips: &Path, start: &str, session: &str, reason: &str, now: DateTime<Utc>) -> (i32, Value) {
    let r = (|| {
        let line = skip_line(&read_text(ledger)?, &read_text(skips)?, parse_session(start)?, session, reason, now)?;
        append_line(skips, &line)?;
        Ok::<_, String>(line)
    })();
    match r {
        Ok(line) => (0, json!({ "appended": serde_json::from_str::<Value>(&line).unwrap_or_default() })),
        Err(e) => refuse(e),
    }
}

pub fn capture_line(ledger: &str, qualification: &Value) -> Result<String, String> {
    if qualification["schema"].as_str() != Some(QUALIFICATION_SCHEMA) {
        return Err("not a qualify-session result".into());
    }
    let q: campaign::CaptureQualification =
        serde_json::from_value(qualification["qualification"].clone()).map_err(|e| format!("malformed qualification: {e}"))?;
    let mut c = campaign::Campaign::replay(campaign::CampaignRules::default(), &parse_ledger(ledger)?)
        .map_err(|e| format!("ledger replay refused: {e:?}"))?;
    let e = campaign::CampaignEvent::CaptureRecorded { qualification: q };
    c.apply(e.clone()).map_err(|e| format!("campaign refused: {e:?}"))?;
    Ok(event_line(&e))
}

fn record_capture(ledger: &Path, q: &Path) -> (i32, Value) {
    let r = (|| {
        let qv: Value = serde_json::from_slice(&std::fs::read(q).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let line = capture_line(&read_text(ledger)?, &qv)?;
        append_line(ledger, &line)?;
        Ok::<_, String>(line)
    })();
    match r {
        Ok(line) => (0, json!({ "appended": serde_json::from_str::<Value>(&line).unwrap_or_default() })),
        Err(e) => refuse(e),
    }
}

#[cfg(test)]
#[path = "offline_eval_tests.rs"]
mod offline_eval_tests;
