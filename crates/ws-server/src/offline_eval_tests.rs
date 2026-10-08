//! Tests for the Step-4 offline evaluator. Evidence is produced by the frozen
//! writers themselves (the observer's `FileRunFactory`, the OI `ShadowDriver`),
//! never hand-written, and archived exactly as `observation_archive.py retain`
//! archives it (gzip + `observation-archive-receipt-v1`).
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use backtest_metrics::opportunity::OiConfig;
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use market_data::{IgnitionEventKind, ScanEvent};
use serde_json::{json, Value};

use super::*;

// The commands under default limits, so the tests below read as the commands
// are invoked. (A local item shadows the glob import of the same name.)
fn certify_run(dir: &Path, pre: &Path) -> (i32, Value) {
    super::certify_run(dir, pre, &Limits::default())
}

fn qualify_session(session: &str, dir: &Path, pre: &Path, oi: &Path) -> (i32, Value) {
    super::qualify_session(session, dir, pre, oi, &Limits::default())
}

fn extract_oi_session(session: &str, research: &Path, implementation_sha: &str, fingerprint: &str) -> (i32, Value) {
    super::extract_oi_session(session, research, implementation_sha, fingerprint, &Limits::default())
}
use crate::observation::{
    FileRunFactory, ObserverConfig, OpenCandidate, OverheadLimits, RunFactory, RunIdentity, ShadowObserver, WindowInput,
    CAPTURE_WARN_PERMILLE, PROPOSED_QUEUE_BYTES, PROPOSED_QUEUE_RECORDS, ROOT_MARKER,
};
use crate::opportunity_shadow::{ShadowDriver, ShadowRecorder};

const IMPL: &str = "0123456789abcdef0123456789abcdef01234567";
const OTHER_IMPL: &str = "89abcdef0123456789abcdef0123456789abcdef";
const SESSION: &str = "2026-10-05"; // a Monday

struct Tmp(PathBuf);
impl Tmp {
    fn new(tag: &str) -> Self {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("step4-eval-test-{}-{tag}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fp() -> String {
    OiConfig::default().fingerprint()
}

fn d(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

/// New York wall time on `day` (EDT before 2026-11-01).
fn ny(day: NaiveDate, h: u32, m: u32, s: u32) -> DateTime<Utc> {
    let offset = if day < d("2026-11-01") { 4 } else { 5 };
    Utc.from_utc_datetime(&day.and_hms_opt(0, 0, 0).unwrap()) + Duration::seconds(i64::from((h + offset) * 3600 + m * 60 + s))
}

/// A FINAL preregistration derived from the repository fixture, bound to `impl_sha`.
fn frozen_prereg(dir: &Path, impl_sha: &str) -> (PathBuf, Frozen) {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ops/observation/step4-preregistration-v1.fixture.json");
    let mut v: Value = serde_json::from_slice(&std::fs::read(base).unwrap()).unwrap();
    v["freezeStatus"] = json!("FINAL");
    v["implementationSha"] = json!(impl_sha);
    v["oiConfigFingerprint"] = json!(fp());
    let p = dir.join(format!("prereg-{impl_sha}.json"));
    std::fs::write(&p, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    let f = load_frozen(&p).expect("test preregistration loads");
    (p, f)
}

fn confirmed(sym: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ScanEvent::IgnitionEvent { symbol: sym.into(), timestamp: t, price, kind: IgnitionEventKind::FollowThroughConfirmed }
}

fn window(id: &str, cands: &[(String, DateTime<Utc>)], at: DateTime<Utc>, price: f64) -> WindowInput {
    let ids: Vec<String> = cands.iter().map(|(s, _)| format!("{s}:1")).collect();
    WindowInput {
        window_id: id.into(),
        processing_started_at: at,
        rank_completed_at: at,
        processing_started_mono: None,
        rank_completed_mono: None,
        open: cands.iter().map(|(s, o)| OpenCandidate { opportunity_id: format!("{s}:1"), symbol: s.clone(), opened_at: *o }).collect(),
        scored: ids.iter().cloned().collect::<BTreeSet<_>>(),
        engine_prices: ids.iter().map(|i| (i.clone(), price)).collect::<BTreeMap<_, _>>(),
        cohort_truncated: false,
    }
}

struct RunSpec {
    windows: usize,
    pool: usize,
    /// Seconds after the opening boundary the run starts (0 = a boundary run).
    late_start_secs: i64,
    identity: RunIdentity,
}

/// One observer run for `SESSION`, written by the frozen production writer
/// (rotating, small files so the evidence spans several).
fn observer_run(root: &Path, spec: RunSpec) -> PathBuf {
    std::fs::write(root.join(ROOT_MARKER), b"x").unwrap();
    let day = d(SESSION);
    let (start, end) = crate::observation::step4_session_bounds(day);
    let config = ObserverConfig {
        root: root.to_path_buf(),
        namespace: "stockspotter-vps".into(),
        pid: 1,
        capture_max_bytes: u64::MAX / 2,
        capture_warn_permille: CAPTURE_WARN_PERMILLE,
        rotate_bytes: 32 * 1024,
        queue_records: PROPOSED_QUEUE_RECORDS,
        queue_bytes: PROPOSED_QUEUE_BYTES,
        overhead: OverheadLimits { stop_window_micros: u64::MAX, stop_duty_ppm: u64::MAX, ..OverheadLimits::default() },
        identity: spec.identity,
    };
    let mut factory = FileRunFactory { config };
    let mut o = factory.start_run(start + Duration::seconds(spec.late_start_secs) + Duration::milliseconds(300)).unwrap();
    let run_dir = root.join(o.run_id());
    let t0 = ny(day, 10, 0, 0);
    o.on_receive(&confirmed("ZZZZ", t0, 1.0), t0);
    o.on_window(window("oiw-0", &[("ZZZZ".into(), t0 - Duration::seconds(60))], t0 + Duration::seconds(1), 1.0));
    for w in 1..=spec.windows {
        let t = t0 + Duration::seconds(120 * w as i64);
        let syms: Vec<(String, DateTime<Utc>)> = (0..spec.pool).map(|k| (format!("W{w}S{k}"), t - Duration::seconds(10))).collect();
        for (s, _) in &syms {
            o.on_receive(&confirmed(s, t, 10.0), t);
        }
        o.on_window(window(&format!("oiw-{w}"), &syms, t + Duration::seconds(1), 10.0));
    }
    o.on_finish(end + Duration::milliseconds(400));
    run_dir
}

fn bound(f: &Frozen) -> RunIdentity {
    RunIdentity { implementation_sha: Some(f.implementation_sha.clone()), preregistration_sha256: Some(f.jcs_sha256.clone()) }
}

fn sha(b: &[u8]) -> String {
    super::sha256_hex(b)
}

/// Archives `run_dir` into `out_root/<runId>/` the way an exported archive looks:
/// the gzip artifacts and the receipt `observation_archive.py retain` writes,
/// without the uncompressed sources. `run_dir` itself is only read.
fn archive(run_dir: &Path, out_root: &Path) -> PathBuf {
    let id = run_dir.file_name().unwrap().to_string_lossy().into_owned();
    let out = out_root.join(&id);
    std::fs::create_dir_all(&out).unwrap();
    let mut names: Vec<String> = std::fs::read_dir(run_dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort_by_key(|n| crate::observation::rotation_index(n));
    let first = std::fs::read(run_dir.join(&names[0])).unwrap();
    let start: Value = serde_json::from_slice(first.split(|b| *b == b'\n').next().unwrap()).unwrap();
    let (mut sources, mut compressed) = (Vec::new(), Vec::new());
    for n in &names {
        let bytes = std::fs::read(run_dir.join(n)).unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
        gz.write_all(&bytes).unwrap();
        let gz = gz.finish().unwrap();
        std::fs::write(out.join(format!("{n}.gz")), &gz).unwrap();
        sources.push(json!({"file": n, "bytes": bytes.len(), "sha256": sha(&bytes)}));
        compressed.push(json!({"file": format!("{n}.gz"), "bytes": gz.len(), "sha256": sha(&gz), "source": n,
                               "decompressedSha256": sha(&bytes), "algorithm": "gzip-6"}));
    }
    let receipt = json!({
        "schema": "observation-archive-receipt-v1", "runId": id, "namespace": start["namespace"],
        "implementationSha": start["implementationSha"], "preregistrationSha256": start["preregistrationSha256"],
        "sources": sources, "compressed": compressed,
    });
    std::fs::write(out.join("archive-receipt.json"), serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    out
}

/// A certifying `extract-oi-session` document with the fields and the
/// balances the frozen extractor emits. (The real extractor's own output is
/// held to the same rules in `extract_oi_session_runs_the_frozen_extractor`.)
fn oi_ok(f: &Frozen, session: &str) -> Value {
    json!({
        "schema": OI_EXTRACTION_SCHEMA, "extractionContract": "d6-oi-extract-v2", "session": session,
        "implementationSha": f.implementation_sha, "configFingerprint": f.oi_fingerprint, "certifies": true,
        "normalizedSha256": "ab".repeat(32), "normalizedRows": 8,
        "report": {
            "sessionMarkers": 1, "lateRecordMarkers": 0, "malformedMarkers": 0, "foreignMarkers": 0, "processId": "p-1",
            "tally": {"attempted": 8, "written": 8, "dropped": 0, "writeErrors": 0, "lossSpans": 0, "bytesWritten": 4096,
                      "flushErrors": 0, "lateAfterClose": 0},
            "rowsInRanges": 8, "foreignRowsInRanges": 0, "sessionRowsOutsideRanges": 0, "unparseableOutsideRanges": 0,
            "duplicateIdentical": 0, "duplicateConflicting": 0, "distinctWindows": 4, "knownLoss": 0,
            "completenessEstablished": true, "reasons": [],
        },
    })
}

fn write_json(dir: &Path, name: &str, v: &Value) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, serde_json::to_vec(v).unwrap()).unwrap();
    p
}

fn spec(f: &Frozen) -> RunSpec {
    RunSpec { windows: 24, pool: 6, late_start_secs: 0, identity: bound(f) }
}

// ===========================================================================
// certify-run
// ===========================================================================

#[test]
fn archived_evidence_certifies_exactly_like_the_raw_run() {
    let t = Tmp::new("cert");
    let (pre, f) = frozen_prereg(t.path(), IMPL);
    std::fs::create_dir_all(t.path().join("root")).unwrap();
    let run = observer_run(&t.path().join("root"), spec(&f));
    assert!(std::fs::read_dir(&run).unwrap().count() > 1, "evidence spans several rotated files");
    let (raw_code, raw) = certify_run(&run, &pre);
    let arch = archive(&run, &t.path().join("export"));
    let (code, out) = certify_run(&arch, &pre);
    assert_eq!((raw_code, code), (0, 0), "{raw} / {out}");
    assert_eq!(out["verdict"], "PASS");
    assert_eq!(out["evidence"], "archived");
    assert_eq!(raw["certificate"], out["certificate"], "decompressed evidence certifies identically");
}

#[test]
fn a_run_under_another_preregistration_fails_identity() {
    let t = Tmp::new("ident");
    let (pre, f) = frozen_prereg(t.path(), IMPL);
    let (_, other) = frozen_prereg(t.path(), OTHER_IMPL);
    std::fs::create_dir_all(t.path().join("root")).unwrap();
    let run = observer_run(&t.path().join("root"), RunSpec { identity: bound(&other), ..spec(&f) });
    let (code, out) = certify_run(&archive(&run, &t.path().join("export")), &pre);
    assert_eq!(code, 1, "{out}");
    assert_eq!(out["verdict"], "FAIL");
    assert!(out["detail"].as_str().unwrap().contains("identity mismatch"));
}

#[test]
fn malformed_archived_evidence_is_refused() {
    let t = Tmp::new("malformed");
    let (pre, f) = frozen_prereg(t.path(), IMPL);
    std::fs::create_dir_all(t.path().join("root")).unwrap();
    let run = observer_run(&t.path().join("root"), spec(&f));
    let fresh = |tag: &str| archive(&run, &t.path().join(tag));
    // A flipped byte in a compressed artifact.
    let a = fresh("corrupt");
    let gz = a.join("observations-0.ndjson.gz");
    let mut b = std::fs::read(&gz).unwrap();
    let mid = b.len() / 2;
    b[mid] ^= 0xFF;
    std::fs::write(&gz, b).unwrap();
    assert!(certify_run(&a, &pre).1["refused"].as_str().unwrap().contains("does not match its receipt"));
    // A receipt that vouches for a gzip of different bytes.
    let a = fresh("mismatch");
    let other = { let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default()); e.write_all(b"not the source\n").unwrap(); e.finish().unwrap() };
    std::fs::write(a.join("observations-0.ndjson.gz"), &other).unwrap();
    let rp = a.join("archive-receipt.json");
    let mut r: Value = serde_json::from_slice(&std::fs::read(&rp).unwrap()).unwrap();
    for c in r["compressed"].as_array_mut().unwrap() {
        if c["file"] == "observations-0.ndjson.gz" {
            c["sha256"] = json!(sha(&other));
            c["bytes"] = json!(other.len());
        }
    }
    std::fs::write(&rp, serde_json::to_vec(&r).unwrap()).unwrap();
    assert!(certify_run(&a, &pre).1["refused"].as_str().unwrap().contains("does not reproduce the recorded source"));
    // An unknown file.
    let a = fresh("unknown");
    std::fs::write(a.join("notes.txt"), b"x").unwrap();
    assert!(certify_run(&a, &pre).1["refused"].as_str().unwrap().contains("unknown file"));
    // A receipt for another run.
    let a = fresh("other-run");
    let renamed = a.with_file_name("stockspotter-vps-1-20261005T001000300Z-9");
    std::fs::rename(&a, &renamed).unwrap();
    assert!(certify_run(&renamed, &pre).1["refused"].as_str().unwrap().contains("different run"));
    // A missing artifact.
    let a = fresh("missing");
    std::fs::remove_file(a.join("observations-0.ndjson.gz")).unwrap();
    assert!(certify_run(&a, &pre).1["refused"].as_str().unwrap().contains("missing compressed artifact"));
}

// ===========================================================================
// qualify-session
// ===========================================================================

#[test]
fn a_clean_boundary_session_qualifies() {
    let t = Tmp::new("qualify");
    let (pre, f) = frozen_prereg(t.path(), IMPL);
    std::fs::create_dir_all(t.path().join("root")).unwrap();
    let run = observer_run(&t.path().join("root"), spec(&f));
    let arch = archive(&run, &t.path().join("export"));
    let oi = write_json(t.path(), "oi.json", &oi_ok(&f, SESSION));
    let (code, out) = qualify_session(SESSION, &arch, &pre, &oi);
    assert_eq!(code, 0, "{out}");
    let q = &out["qualification"];
    assert_eq!(out["detail"]["certificateVerdict"], "PASS", "{out}");
    assert_eq!(out["detail"]["boundaryRun"], true);
    assert_eq!(out["detail"]["floorVerdict"], "MET", "{out}");
    assert_eq!(q["discriminatingWindows"], 24, "{out}");
    assert_eq!(out["detail"]["discriminatingWindowsInPrimaryScope"], 24);
    assert_eq!(q["oiZeroLoss"], true);
    assert_eq!(out["qualifies"], true, "{out}");
    // The output is exactly what record-capture accepts.
    let ledger = format!("{}\n", event_line(&campaign::CampaignEvent::Designated { session: SESSION.into() }));
    let line = capture_line(&ledger, &out).unwrap();
    assert!(line.starts_with("{\"event\":\"captureRecorded\""));
}

#[test]
fn too_few_discriminating_windows_is_recorded_but_does_not_qualify() {
    let t = Tmp::new("few");
    let (pre, f) = frozen_prereg(t.path(), IMPL);
    std::fs::create_dir_all(t.path().join("root")).unwrap();
    let run = observer_run(&t.path().join("root"), RunSpec { windows: 19, ..spec(&f) });
    let oi = write_json(t.path(), "oi.json", &oi_ok(&f, SESSION));
    let (code, out) = qualify_session(SESSION, &archive(&run, &t.path().join("export")), &pre, &oi);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out["qualification"]["discriminatingWindows"], 19);
    assert_eq!(out["qualifies"], false);
}

#[test]
fn pools_below_the_minimum_are_not_discriminating() {
    let t = Tmp::new("small-pool");
    let (pre, f) = frozen_prereg(t.path(), IMPL);
    std::fs::create_dir_all(t.path().join("root")).unwrap();
    let run = observer_run(&t.path().join("root"), RunSpec { pool: 5, ..spec(&f) });
    let oi = write_json(t.path(), "oi.json", &oi_ok(&f, SESSION));
    let (_, out) = qualify_session(SESSION, &archive(&run, &t.path().join("export")), &pre, &oi);
    assert_eq!(out["qualification"]["discriminatingWindows"], 0, "{out}");
    assert_eq!(out["qualifies"], false);
}

#[test]
fn a_run_that_missed_the_opening_boundary_cannot_pass() {
    let t = Tmp::new("late");
    let (pre, f) = frozen_prereg(t.path(), IMPL);
    std::fs::create_dir_all(t.path().join("root")).unwrap();
    let run = observer_run(&t.path().join("root"), RunSpec { late_start_secs: 3600, ..spec(&f) });
    let oi = write_json(t.path(), "oi.json", &oi_ok(&f, SESSION));
    let (code, out) = qualify_session(SESSION, &archive(&run, &t.path().join("export")), &pre, &oi);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out["detail"]["boundaryRun"], false);
    assert_eq!(out["qualification"]["certificatePass"], false);
    assert_eq!(out["qualifies"], false);
}

#[test]
fn qualification_refuses_raw_evidence_wrong_identities_and_bad_oi_evidence() {
    let t = Tmp::new("refuse");
    let (pre, f) = frozen_prereg(t.path(), IMPL);
    std::fs::create_dir_all(t.path().join("root")).unwrap();
    let run = observer_run(&t.path().join("root"), spec(&f));
    let arch = archive(&run, &t.path().join("export"));
    let good = write_json(t.path(), "oi.json", &oi_ok(&f, SESSION));
    let why = |r: (i32, Value)| {
        assert_eq!(r.0, 1, "{}", r.1);
        r.1["refused"].as_str().unwrap().to_string()
    };
    assert!(why(qualify_session(SESSION, &run, &pre, &good)).contains("no archive receipt"));
    let mut v = oi_ok(&f, SESSION);
    v["configFingerprint"] = json!("oi-cfg-0000000000000000");
    assert!(why(qualify_session(SESSION, &arch, &pre, &write_json(t.path(), "o1.json", &v))).contains("fingerprint"));
    let mut v = oi_ok(&f, SESSION);
    v["implementationSha"] = json!(OTHER_IMPL);
    assert!(why(qualify_session(SESSION, &arch, &pre, &write_json(t.path(), "o2.json", &v))).contains("implementation SHA"));
    let mut v = oi_ok(&f, SESSION);
    v["session"] = json!("2026-10-06");
    assert!(why(qualify_session(SESSION, &arch, &pre, &write_json(t.path(), "o3.json", &v))).contains("different session"));
    assert!(why(qualify_session(SESSION, &arch, &pre, &write_json(t.path(), "o4.json", &json!({"x": 1})))).contains("unrecognised"));
    // A preregistration bound to another implementation: the receipt's identity is not the frozen one.
    let (other_pre, _) = frozen_prereg(t.path(), OTHER_IMPL);
    let mut v = oi_ok(&f, SESSION);
    v["implementationSha"] = json!(OTHER_IMPL);
    assert!(why(qualify_session(SESSION, &arch, &other_pre, &write_json(t.path(), "o5.json", &v))).contains("receipt identity"));
    assert!(why(qualify_session("2026-10-04", &arch, &pre, &good)).contains("no regular trading session"));
    assert!(why(qualify_session("2026-10-5", &arch, &pre, &good)).contains("YYYY-MM-DD"));
}

/// `base` with the value at `path` (keys from the document root) replaced.
fn with(base: &Value, path: &[&str], value: Value) -> Value {
    let mut v = base.clone();
    let mut at = &mut v;
    for key in path {
        at = &mut at[*key];
    }
    *at = value;
    v
}

/// `base` with the key at `path` removed.
fn without(base: &Value, path: &[&str]) -> Value {
    let mut v = base.clone();
    let (last, parents) = path.split_last().unwrap();
    let mut at = &mut v;
    for key in parents {
        at = &mut at[*key];
    }
    at.as_object_mut().unwrap().remove(*last);
    v
}

#[test]
fn a_well_formed_oi_extraction_is_accepted_for_what_it_says() {
    let t = Tmp::new("oi-valid");
    let (_, f) = frozen_prereg(t.path(), IMPL);
    let good = oi_ok(&f, SESSION);
    assert_eq!(oi_zero_loss(&good, SESSION, &f).unwrap(), (true, "d6-oi-extract-v2"));
    // A document that consistently reports a loss is read as "does not certify", not refused.
    let mut lossy = good.clone();
    lossy["certifies"] = json!(false);
    lossy["report"]["completenessEstablished"] = json!(false);
    lossy["report"]["reasons"] = json!(["session loss"]);
    lossy["report"]["tally"]["attempted"] = json!(9);
    lossy["report"]["tally"]["dropped"] = json!(1);
    lossy["report"]["knownLoss"] = json!(1);
    assert_eq!(oi_zero_loss(&lossy, SESSION, &f).unwrap(), (false, "d6-oi-extract-v2"));
    // One with no session marker at all.
    let mut absent = lossy.clone();
    for (k, v) in [("sessionMarkers", json!(0)), ("processId", Value::Null), ("tally", Value::Null), ("knownLoss", json!(0))] {
        absent["report"][k] = v;
    }
    assert_eq!(oi_zero_loss(&absent, SESSION, &f).unwrap().0, false);
}

#[test]
fn the_oi_extraction_fields_checked_are_the_frozen_types_fields() {
    let keys = |v: Value| -> BTreeSet<String> { v.as_object().unwrap().keys().cloned().collect() };
    let set = |names: &[&str]| -> BTreeSet<String> { names.iter().map(|s| s.to_string()).collect() };
    let report: Vec<&str> = OI_REPORT_COUNTERS.iter().chain(OI_REPORT_OTHER.iter()).copied().collect();
    assert_eq!(keys(serde_json::to_value(oi_extract::SessionReport::default()).unwrap()), set(&report));
    assert_eq!(keys(serde_json::to_value(crate::research_writer::SessionTally::default()).unwrap()), set(&OI_TALLY_COUNTERS));
    assert_eq!(oi_extract::EXTRACTION_CONTRACT, "d6-oi-extract-v2");
}

#[test]
fn oi_evidence_of_any_other_schema_or_shape_is_refused() {
    let t = Tmp::new("oi-unknown");
    let (pre, f) = frozen_prereg(t.path(), IMPL);
    let good = oi_ok(&f, SESSION);
    // The shape this port no longer accepts: a marker-accounting summary that says it certifies.
    let removed = json!({"session": SESSION, "implementationSha": f.implementation_sha, "configFingerprint": f.oi_fingerprint,
                         "certifies": true, "accountingStart": "session_boundary", "tally": {}, "ranges": []});
    assert!(oi_zero_loss(&removed, SESSION, &f).unwrap_err().contains("unrecognised OI evidence"));
    // Even with every field of a valid extraction beside it, it is not the extraction schema.
    let mut dressed = good.clone();
    dressed.as_object_mut().unwrap().remove("schema");
    assert!(oi_zero_loss(&dressed, SESSION, &f).unwrap_err().contains("unrecognised OI evidence"));
    for schema in [json!("step4-eval-oi-extraction-v2"), json!("marker-accounting-summary"), json!(""), json!(1), Value::Null] {
        let e = oi_zero_loss(&with(&good, &["schema"], schema.clone()), SESSION, &f).unwrap_err();
        assert!(e.contains("unrecognised OI evidence"), "{schema}: {e}");
    }
    for contract in [json!("d6-oi-extract-v1-legacy-process-close"), json!("d6-oi-extract-v3"), Value::Null] {
        let e = oi_zero_loss(&with(&good, &["extractionContract"], contract.clone()), SESSION, &f).unwrap_err();
        assert!(e.contains("unrecognised OI extraction contract"), "{contract}: {e}");
    }
    // Through the command: a refusal with a non-zero exit, before the evidence is opened.
    let (code, out) = qualify_session(SESSION, t.path(), &pre, &write_json(t.path(), "removed.json", &removed));
    assert_eq!(code, 1, "{out}");
    assert!(out["refused"].as_str().unwrap().contains("unrecognised OI evidence"), "{out}");
}

#[test]
fn a_malformed_oi_extraction_is_refused() {
    let t = Tmp::new("oi-malformed");
    let (pre, f) = frozen_prereg(t.path(), IMPL);
    let good = oi_ok(&f, SESSION);
    let cases = [
        ("no report", without(&good, &["report"])),
        ("report is a list", with(&good, &["report"], json!([]))),
        ("no normalizedRows", without(&good, &["normalizedRows"])),
        ("an extra top-level field", with(&good, &["accountingStart"], json!("session_boundary"))),
        ("certifies is a string", with(&good, &["certifies"], json!("true"))),
        ("certifies is absent", without(&good, &["certifies"])),
        ("normalizedSha256 is not a digest", with(&good, &["normalizedSha256"], json!("abc"))),
        ("a missing counter", without(&good, &["report", "rowsInRanges"])),
        ("a negative counter", with(&good, &["report", "rowsInRanges"], json!(-8))),
        ("a fractional counter", with(&good, &["report", "distinctWindows"], json!(4.5))),
        ("a counter as text", with(&good, &["report", "knownLoss"], json!("0"))),
        ("an extra report field", with(&good, &["report", "ranges"], json!([]))),
        ("reasons is not a list", with(&good, &["report", "reasons"], json!("none"))),
        ("reasons holds a non-string", with(&good, &["report", "reasons"], json!([1]))),
        ("completenessEstablished is a number", with(&good, &["report", "completenessEstablished"], json!(1))),
        ("processId is a number", with(&good, &["report", "processId"], json!(7))),
        ("tally is empty", with(&good, &["report", "tally"], json!({}))),
        ("tally lacks a counter", without(&good, &["report", "tally", "lateAfterClose"])),
        ("tally has an extra counter", with(&good, &["report", "tally", "recovered"], json!(0))),
        ("a tally counter as text", with(&good, &["report", "tally", "written"], json!("8"))),
    ];
    for (what, v) in &cases {
        let e = oi_zero_loss(v, SESSION, &f).unwrap_err();
        assert!(e.contains("malformed OI extraction"), "{what}: {e}");
    }
    let (code, out) = qualify_session(SESSION, t.path(), &pre, &write_json(t.path(), "malformed.json", &cases[0].1));
    assert_eq!(code, 1, "{out}");
    assert!(out["refused"].as_str().unwrap().contains("malformed OI extraction"), "{out}");
}

#[test]
fn an_oi_extraction_whose_counters_contradict_is_refused() {
    let t = Tmp::new("oi-contradiction");
    let (pre, f) = frozen_prereg(t.path(), IMPL);
    let good = oi_ok(&f, SESSION);
    let r = |key: &'static str, value: Value| with(&good, &["report", key], value);
    let tally = |key: &'static str, value: Value| with(&good, &["report", "tally", key], value);
    let cases = [
        // The flag against the report it summarises.
        ("certifies though completeness is not established", r("completenessEstablished", json!(false))),
        ("established with a reason against it", r("reasons", json!(["tally does not balance"]))),
        ("not certifying, yet no reason given", with(&r("completenessEstablished", json!(false)), &["certifies"], json!(false))),
        // Counts that do not add up.
        ("attempted is more than was accounted for", tally("attempted", json!(9))),
        ("written is more than was attempted", tally("written", json!(9))),
        ("a dropped row while certifying", with(&tally("dropped", json!(1)), &["report", "tally", "attempted"], json!(9))),
        ("a write error while certifying", with(&tally("writeErrors", json!(1)), &["report", "tally", "attempted"], json!(9))),
        ("a loss span while certifying", tally("lossSpans", json!(1))),
        ("a flush error while certifying", tally("flushErrors", json!(1))),
        ("a late record while certifying", tally("lateAfterClose", json!(1))),
        ("known loss while certifying", r("knownLoss", json!(2))),
        // Ranges against the counts.
        ("fewer rows in ranges than were normalised", r("rowsInRanges", json!(7))),
        ("more rows in ranges than were written", with(&r("rowsInRanges", json!(9)), &["normalizedRows"], json!(9))),
        ("duplicates that the row counts leave no room for", r("duplicateIdentical", json!(1))),
        ("more normalised rows than the ranges hold", with(&good, &["normalizedRows"], json!(80))),
        ("a foreign row inside a certified range", r("foreignRowsInRanges", json!(1))),
        ("session rows outside the certified ranges", r("sessionRowsOutsideRanges", json!(3))),
        ("more windows than rows", r("distinctWindows", json!(9))),
        ("rows but no window", r("distinctWindows", json!(0))),
        ("more conflicting duplicates than rows", r("duplicateConflicting", json!(9))),
        // Markers against the tally.
        ("two session markers", r("sessionMarkers", json!(2))),
        ("no session marker but a tally", r("sessionMarkers", json!(0))),
        ("one session marker but no tally", r("tally", Value::Null)),
        ("a late-record marker while certifying", r("lateRecordMarkers", json!(1))),
        ("a malformed marker while certifying", r("malformedMarkers", json!(1))),
        ("a foreign marker while certifying", r("foreignMarkers", json!(1))),
        ("certifying with no process id", r("processId", Value::Null)),
    ];
    for (what, v) in &cases {
        let e = oi_zero_loss(v, SESSION, &f).unwrap_err();
        assert!(e.contains("contradicts itself"), "{what}: {e}");
    }
    // A bare flag is worth nothing: flipping only `certifies` on a lossy document is caught.
    let mut lossy = good.clone();
    lossy["report"]["completenessEstablished"] = json!(false);
    lossy["report"]["reasons"] = json!(["session loss"]);
    assert!(oi_zero_loss(&lossy, SESSION, &f).unwrap_err().contains("contradicts itself"));
    let (code, out) = qualify_session(SESSION, t.path(), &pre, &write_json(t.path(), "contradiction.json", &cases[3].1));
    assert_eq!(code, 1, "{out}");
    assert!(out["refused"].as_str().unwrap().contains("contradicts itself"), "{out}");
}

// ===========================================================================
// extract-oi-session (frozen d6-oi-extract-v2 over real writer output)
// ===========================================================================

fn oi_session_capture(dir: &Path, session: NaiveDate) {
    let recorder = ShadowRecorder::start(dir.to_path_buf()).expect("recorder");
    let mut driver = ShadowDriver::new(OiConfig::default(), Some(recorder));
    driver.set_implementation_sha(Some(IMPL.into()));
    let prev = session - Duration::days(1);
    driver.observe_tick(ny(prev, 19, 0, 0)); // up before the opening boundary
    driver.observe_tick(ny(prev, 20, 10, 0));
    let t0 = ny(session, 10, 0, 0);
    for (i, sym) in ["AAA", "BBB"].iter().enumerate() {
        let t = t0 + Duration::seconds(i as i64);
        let m = ScanEvent::MomentumUpdate { symbol: (*sym).into(), timestamp: t, volume_confirmation: 0.5, structure: 0.5,
            ma_slope: 0.2, wick_rejection: 0.3, overall: 0.7, qualifies: true };
        let _ = driver.observe(&m, t);
        let _ = driver.observe(&confirmed(sym, t + Duration::seconds(5), 10.0), t + Duration::seconds(5));
        for k in 1..=3 {
            let tk = t0 + Duration::seconds(40 * k + i as i64);
            let _ = driver.observe(&confirmed(sym, tk, 10.0 + 0.2 * k as f64), tk);
        }
    }
    driver.observe_tick(ny(session, 20, 10, 0));
    driver.flush_capture(std::time::Duration::from_secs(20));
}

#[test]
fn extract_oi_session_runs_the_frozen_extractor() {
    let t = Tmp::new("oi");
    oi_session_capture(t.path(), d(SESSION));
    let (code, out) = extract_oi_session(SESSION, t.path(), IMPL, &fp());
    assert_eq!(code, 0, "{out}");
    assert_eq!(out["certifies"], true);
    assert_eq!(out["extractionContract"], "d6-oi-extract-v2");
    let (_, f) = frozen_prereg(t.path(), IMPL);
    assert_eq!(oi_zero_loss(&out, SESSION, &f).unwrap(), (true, "d6-oi-extract-v2"));
    // What the command prints is exactly the set of fields the validator requires.
    let printed: BTreeSet<&str> = out.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(printed, OI_EXTRACTION_KEYS.iter().copied().collect::<BTreeSet<_>>());
    assert!(out["report"]["rowsInRanges"].as_u64().unwrap() > 0, "{out}");
    let (code, out) = extract_oi_session(SESSION, t.path(), IMPL, "oi-cfg-0000000000000000");
    assert_eq!(code, 1);
    assert!(out["report"]["reasons"].to_string().contains("fingerprint"), "{out}");
    let (code, out) = extract_oi_session("2026-10-06", t.path(), IMPL, &fp());
    assert_eq!(code, 1, "{out}");
    // The extractor's own non-certifying output is consistent, so it reads as
    // "does not certify" rather than as a contradiction.
    assert_eq!(oi_zero_loss(&out, "2026-10-06", &f).unwrap(), (false, "d6-oi-extract-v2"));
}

// ===========================================================================
// Campaign ledger and operating rules
// ===========================================================================

fn designated(s: &str) -> String {
    event_line(&campaign::CampaignEvent::Designated { session: s.into() })
}

fn captured(s: &str, qualifies: bool) -> String {
    event_line(&campaign::CampaignEvent::CaptureRecorded {
        qualification: campaign::CaptureQualification {
            session: s.into(),
            run_id: format!("stockspotter-vps-1-{s}-0"),
            certificate_pass: qualifies,
            floors_satisfied: qualifies,
            discriminating_windows: if qualifies { 20 } else { 0 },
            oi_zero_loss: qualifies,
        },
    })
}

fn ledger(lines: &[String]) -> String {
    lines.iter().map(|l| format!("{l}\n")).collect()
}

/// A moment inside the designation window of `s`.
fn inside(s: &str) -> DateTime<Utc> {
    designation_window(d(s)).0 + Duration::hours(1)
}

fn sessions_from(start: &str, n: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut day = d(start);
    while out.len() < n {
        day = next_regular_session(day);
        out.push(day.to_string());
        day += Duration::days(1);
    }
    out
}

#[test]
fn designation_window_is_the_opening_boundary_to_four_am_et() {
    let (open, deadline) = designation_window(d("2026-10-05"));
    assert_eq!(open.to_rfc3339(), "2026-10-05T00:10:00+00:00"); // 20:10 ET Sunday
    assert_eq!(deadline.to_rfc3339(), "2026-10-05T08:00:00+00:00"); // 04:00 ET Monday
    let start = d("2026-10-05");
    assert!(designate("", "", start, SESSION, open + Duration::seconds(1)).is_ok());
    assert!(designate("", "", start, SESSION, open - Duration::seconds(1)).unwrap_err().contains("too early"));
    assert!(designate("", "", start, SESSION, deadline).unwrap_err().contains("deadline passed"));
    assert!(designate("", "", start, SESSION, deadline + Duration::hours(3)).unwrap_err().contains("deadline passed"));
}

#[test]
fn sessions_are_designated_in_calendar_order_and_skips_need_a_reason() {
    let start = d("2026-10-05");
    // Weekends and holidays are never designated.
    assert!(designate("", "", start, "2026-10-04", inside("2026-10-04")).unwrap_err().contains("no regular trading session"));
    // The first session after the campaign start, not a later one.
    assert!(designate("", "", start, "2026-10-06", inside("2026-10-06")).unwrap_err().contains("next session is 2026-10-05"));
    // Skipping 10-05 for an operational reason moves the cursor; nothing else does.
    let skip = skip_line("", "", start, "2026-10-05", "ws restarted at 23:40 ET", inside("2026-10-05")).unwrap();
    let skips = format!("{skip}\n");
    assert!(skip_line("", "", start, "2026-10-05", "  ", Utc::now()).unwrap_err().contains("reason"));
    assert!(skip_line("", "", start, "2026-10-07", "x", Utc::now()).unwrap_err().contains("next session is 2026-10-05"));
    let line = designate("", &skips, start, "2026-10-06", inside("2026-10-06")).unwrap();
    assert_eq!(line, designated("2026-10-06"));
    // Friday -> Monday across a weekend.
    let (_, _, _, next) = load_campaign(&ledger(&[designated("2026-10-09"), captured("2026-10-09", true)]), "", d("2026-10-09")).unwrap();
    assert_eq!(next, d("2026-10-12"));
}

#[test]
fn pending_capture_blocks_the_next_designation() {
    let start = d("2026-10-05");
    let l = ledger(&[designated("2026-10-05")]);
    let e = designate(&l, "", start, "2026-10-06", inside("2026-10-06")).unwrap_err();
    assert!(e.contains("PendingCapture"), "{e}");
    let l = ledger(&[designated("2026-10-05"), captured("2026-10-05", false)]);
    assert!(designate(&l, "", start, "2026-10-06", inside("2026-10-06")).is_ok());
}

#[test]
fn duplicate_designation_is_refused() {
    let start = d("2026-10-05");
    let l = ledger(&[designated("2026-10-05"), captured("2026-10-05", true)]);
    assert!(designate(&l, "", start, "2026-10-05", inside("2026-10-05")).unwrap_err().contains("next session is 2026-10-06"));
    let dup = ledger(&[designated("2026-10-05"), captured("2026-10-05", true), designated("2026-10-05")]);
    assert!(load_campaign(&dup, "", start).unwrap_err().contains("AlreadyDesignated"));
    // A capture for a session that was never designated.
    let q = json!({"schema": QUALIFICATION_SCHEMA, "qualification": {"session": "2026-10-05", "runId": "r", "certificatePass": true,
                   "floorsSatisfied": true, "discriminatingWindows": 20, "oiZeroLoss": true}});
    assert!(capture_line("", &q).unwrap_err().contains("NotDesignated"));
    let l = ledger(&[designated("2026-10-05"), captured("2026-10-05", true)]);
    assert!(capture_line(&l, &q).unwrap_err().contains("AlreadyRecorded"));
}

#[test]
fn the_session_cap_ends_the_campaign() {
    let start = "2026-10-05";
    let s = sessions_from(start, 21);
    let lines: Vec<String> = s[..20].iter().flat_map(|x| [designated(x), captured(x, false)]).collect();
    let l = ledger(&lines);
    let (c, _, _, next) = load_campaign(&l, "", d(start)).unwrap();
    assert_eq!(c.state(), campaign::CampaignState::MeasurementInsufficient);
    assert_eq!(next.to_string(), s[20]);
    let e = designate(&l, "", d(start), &s[20], inside(&s[20])).unwrap_err();
    assert!(e.contains("NotPermitted"), "{e}");
}

#[test]
fn the_outcome_firewall_holds_after_closure_and_there_is_no_outcome_command() {
    let start = "2026-10-05";
    let s = sessions_from(start, 10);
    let lines: Vec<String> = s.iter().flat_map(|x| [designated(x), captured(x, true)]).collect();
    let (c, _, _, _) = load_campaign(&ledger(&lines), "", d(start)).unwrap();
    assert_eq!(c.state(), campaign::CampaignState::CaptureSetClosed);
    assert!(c.outcome_access().is_err(), "closure alone never unlocks outcomes");
    for cmd in ["fetch-outcomes", "outcome", "outcomes", "evaluate", "authorize-outcomes", "outcome-access", "unlock"] {
        for extra in 0..6 {
            let mut args = vec![cmd.to_string()];
            args.extend((0..extra).map(|i| format!("arg{i}")));
            assert_eq!(cli(&args), 64, "{cmd} with {extra} arguments");
        }
    }
    // The module's claim is literal: it contains no call that could yield an
    // `OutcomeAccess`, and never names the type outside its documentation.
    let module = include_str!("offline_eval.rs");
    assert!(!module.contains("outcome_access("), "the evaluator must not call Campaign::outcome_access");
    for line in module.lines().filter(|l| l.contains("OutcomeAccess")) {
        assert!(line.trim_start().starts_with("//"), "OutcomeAccess outside a comment: {line}");
    }
    // An authorization cannot be smuggled in through record-capture.
    let q = json!({"schema": QUALIFICATION_SCHEMA, "qualification": {"event": "outcomeFetchAuthorized"}});
    assert!(capture_line(&ledger(&lines), &q).is_err());
}

#[test]
fn the_ledger_has_one_canonical_spelling() {
    let start = d("2026-10-05");
    assert!(load_campaign("{\"session\":\"2026-10-05\",\"event\":\"designated\"}\n", "", start).unwrap_err().contains("canonical"));
    assert!(load_campaign(&designated("2026-10-05"), "", start).unwrap_err().contains("newline"));
    assert!(load_campaign("{\"event\":\"somethingElse\"}\n", "", start).is_err());
    assert!(load_campaign(&format!("{}\n\n", designated("2026-10-05")), "", start).is_err());
    let bad_skip = "{\"schema\":\"step4-operational-skip-v1\",\"session\":\"2026-10-05\"}\n";
    assert!(load_campaign("", bad_skip, start).unwrap_err().contains("reason"));
}

#[test]
fn record_commands_append_exactly_one_durable_line() {
    let t = Tmp::new("append");
    let l = t.path().join("ledger.ndjson");
    let k = t.path().join("skips.ndjson");
    std::fs::write(&l, "").unwrap();
    std::fs::write(&k, "").unwrap();
    let (code, out) = record_designation(&l, &k, "2026-10-05", "2026-10-05", inside("2026-10-05"));
    assert_eq!(code, 0, "{out}");
    assert_eq!(std::fs::read_to_string(&l).unwrap(), ledger(&[designated("2026-10-05")]));
    // A refused designation leaves the file untouched.
    let (code, _) = record_designation(&l, &k, "2026-10-05", "2026-10-06", inside("2026-10-06"));
    assert_eq!(code, 1);
    assert_eq!(std::fs::read_to_string(&l).unwrap(), ledger(&[designated("2026-10-05")]));
    let (code, out) = replay_campaign(&l, &k, "2026-10-05");
    assert_eq!(code, 0);
    assert_eq!(out["pendingCapture"], json!(["2026-10-05"]));
    // The lock is read off the state; the command never asks for access.
    assert_eq!(out["state"], "COLLECTING");
    assert!(out.get("outcomeAccess").is_none(), "{out}");
    // A missing ledger file is an error, not an empty campaign.
    assert_eq!(replay_campaign(&t.path().join("absent.ndjson"), &k, "2026-10-05").0, 1);
}

// ===========================================================================
// Path safety, bounded reads, and read-only evidence
// ===========================================================================

fn refusal(r: Result<Evidence, String>) -> String {
    r.err().expect("the evidence must be refused")
}

fn rewrite_receipt(dir: &Path, f: impl FnOnce(&mut Value)) {
    let rp = dir.join("archive-receipt.json");
    let mut r: Value = serde_json::from_slice(&std::fs::read(&rp).unwrap()).unwrap();
    f(&mut r);
    std::fs::write(&rp, serde_json::to_vec(&r).unwrap()).unwrap();
}

/// Name -> SHA-256 of every file directly in `dir`.
fn tree(dir: &Path) -> BTreeMap<String, String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap())
        .map(|e| (e.file_name().to_string_lossy().into_owned(), sha(&std::fs::read(e.path()).unwrap())))
        .collect()
}

/// A raw run and a fresh archive of it per tag.
fn evidence(t: &Tmp) -> (PathBuf, PathBuf, Frozen) {
    let (pre, f) = frozen_prereg(t.path(), IMPL);
    std::fs::create_dir_all(t.path().join("root")).unwrap();
    let run = observer_run(&t.path().join("root"), spec(&f));
    (pre, run, f)
}

#[test]
fn a_bare_name_is_one_plain_path_component() {
    for ok in ["observations-0.ndjson", "observations-12.ndjson.gz", "archive-receipt.json"] {
        assert_eq!(bare_name(ok), Ok(ok));
    }
    for bad in [
        "", ".", "..", "../x", "..\\x", "a/b", "a\\b", "./x", "x/", "/etc/passwd", "\\\\server\\share\\x", "C:\\x", "C:x", "c:/x",
        "x:stream", "a\0b",
    ] {
        assert!(bare_name(bad).is_err(), "{bad:?} must be refused");
    }
}

#[test]
fn receipt_names_that_leave_the_evidence_directory_are_refused() {
    let t = Tmp::new("traversal");
    let (_, run, _) = evidence(&t);
    let fresh = |tag: &str| archive(&run, &t.path().join(tag));
    // A real, byte-identical artifact outside the evidence directory: every
    // hash in the receipt would match it, so only the name check can refuse.
    let victim = fresh("victim").join("observations-0.ndjson.gz");
    let absolute = victim.to_string_lossy().into_owned();
    let cases = [
        ("dotdot", format!("../../victim/{}/observations-0.ndjson.gz", run.file_name().unwrap().to_string_lossy())),
        ("parent", "..".to_string()),
        ("absolute", absolute),
        ("rooted", "/observations-0.ndjson.gz".to_string()),
        ("drive", "C:observations-0.ndjson.gz".to_string()),
        ("unc", "\\\\host\\share\\observations-0.ndjson.gz".to_string()),
        ("separator", "sub/observations-0.ndjson.gz".to_string()),
        ("backslash", "sub\\observations-0.ndjson.gz".to_string()),
        ("empty", String::new()),
    ];
    for (tag, name) in cases {
        let a = fresh(tag);
        rewrite_receipt(&a, |r| r["compressed"][0]["file"] = json!(name));
        let before = tree(&a);
        let e = refusal(open_evidence(&a, true, &Limits::default()));
        assert!(e.contains("unsafe file name"), "{tag}: {e}");
        assert_eq!(tree(&a), before, "{tag}: a refusal leaves the evidence as it was");
    }
    assert!(victim.is_file(), "nothing outside the evidence directory was touched");
    // The same for a source name, which is also where the scratch copy would go.
    let a = fresh("source");
    rewrite_receipt(&a, |r| {
        r["sources"][0]["file"] = json!("../observations-0.ndjson");
        r["compressed"][0]["source"] = json!("../observations-0.ndjson");
    });
    assert!(refusal(open_evidence(&a, true, &Limits::default())).contains("unsafe file name"));
    // A bare name is still not free: the artifact of a source is `<source>.gz`.
    let a = fresh("swap");
    rewrite_receipt(&a, |r| r["compressed"][0]["file"] = json!("observations-1.ndjson.gz"));
    assert!(refusal(open_evidence(&a, true, &Limits::default())).contains("is not observations-0.ndjson.gz"));
    // And the command reports it as a refusal with a non-zero exit.
    let (pre, _) = frozen_prereg(t.path(), IMPL);
    let a = fresh("cli");
    rewrite_receipt(&a, |r| r["compressed"][0]["file"] = json!("../x.gz"));
    let (code, out) = certify_run(&a, &pre);
    assert_eq!(code, 1, "{out}");
    assert!(out["refused"].as_str().unwrap().contains("unsafe file name"));
}

/// Creates a file symlink; false where this host cannot (Windows without the
/// symlink privilege).
fn symlink_file(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).unwrap();
        true
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link).is_ok()
    }
}

#[test]
fn a_symlinked_artifact_or_receipt_is_refused() {
    let t = Tmp::new("symlink");
    let (_, run, _) = evidence(&t);
    let a = archive(&run, &t.path().join("link"));
    let gz = a.join("observations-0.ndjson.gz");
    let elsewhere = t.path().join("elsewhere.gz");
    std::fs::rename(&gz, &elsewhere).unwrap();
    let target_before = sha(&std::fs::read(&elsewhere).unwrap());
    if !symlink_file(&elsewhere, &gz) {
        eprintln!("SKIPPED a_symlinked_artifact_or_receipt_is_refused: this host cannot create symlinks");
        return;
    }
    // The link resolves to bytes the receipt vouches for; it is refused anyway.
    let e = refusal(open_evidence(&a, true, &Limits::default()));
    assert!(e.contains("not a regular file"), "{e}");
    assert_eq!(sha(&std::fs::read(&elsewhere).unwrap()), target_before);
    // A symlinked receipt.
    let b = archive(&run, &t.path().join("link-receipt"));
    let receipt = b.join("archive-receipt.json");
    let moved = t.path().join("receipt-elsewhere.json");
    std::fs::rename(&receipt, &moved).unwrap();
    assert!(symlink_file(&moved, &receipt));
    assert!(refusal(open_evidence(&b, true, &Limits::default())).contains("not a regular file"));
}

/// Windows can always create a directory junction, which is a reparse point
/// but not a symlink: the check must catch it by its attribute.
#[cfg(windows)]
#[test]
fn a_reparse_point_is_treated_as_a_link() {
    let t = Tmp::new("junction");
    let (_, run, _) = evidence(&t);
    let a = archive(&run, &t.path().join("junction"));
    let target = t.path().join("junction-target");
    std::fs::create_dir_all(&target).unwrap();
    let link = a.join("observations-0.ndjson");
    let made = std::process::Command::new("cmd").arg("/C").arg("mklink").arg("/J").arg(&link).arg(&target).output().unwrap();
    assert!(made.status.success(), "mklink /J failed: {}", String::from_utf8_lossy(&made.stderr));
    let meta = std::fs::symlink_metadata(&link).unwrap();
    assert!(is_link(&meta), "a junction is a reparse point");
    assert!(!is_link(&std::fs::symlink_metadata(&target).unwrap()));
    assert!(refusal(open_evidence(&a, true, &Limits::default())).contains("not a regular file"));
    std::fs::remove_dir(&link).unwrap(); // removes the junction itself, not its target
    assert!(target.is_dir());
}

#[test]
fn limits_default_to_the_documented_sizes_and_reject_nonsense() {
    assert_eq!(Limits::from_lookup(|_| None).unwrap(), Limits::default());
    // Room for the stress session (9.4 GB) and the 16 GiB capture budget.
    assert!(DEFAULT_MAX_DECOMPRESSED_BYTES >= 16 * (1 << 30) && DEFAULT_MAX_DECOMPRESSED_BYTES > 9_420_000_000);
    assert!(DEFAULT_MAX_COMPRESSED_BYTES >= DEFAULT_MAX_DECOMPRESSED_BYTES);
    assert!(DEFAULT_MAX_OI_FILE_BYTES > 10_000_000_000);
    let one = |name: &'static str| Limits::from_lookup(move |k| (k == name).then(|| "4096".to_string())).unwrap();
    assert_eq!(one(ENV_MAX_COMPRESSED_BYTES).max_compressed_bytes, 4096);
    assert_eq!(one(ENV_MAX_DECOMPRESSED_BYTES).max_decompressed_bytes, 4096);
    assert_eq!(one(ENV_MAX_OI_FILE_BYTES).max_oi_file_bytes, 4096);
    assert_eq!(one(ENV_MAX_OI_FILE_BYTES).max_compressed_bytes, DEFAULT_MAX_COMPRESSED_BYTES);
    assert_eq!(one(ENV_MAX_OI_TOTAL_BYTES).max_oi_total_bytes, 4096);
    // Two ordinary days of OI data fit in the aggregate budget.
    assert!(DEFAULT_MAX_OI_TOTAL_BYTES > 2 * 10_000_000_000);
    for bad in ["0", "-1", "ten", "", "1e9", "1.5", "18446744073709551616"] {
        assert!(Limits::from_lookup(|_| Some(bad.to_string())).is_err(), "{bad:?}");
    }
}

#[test]
fn the_compressed_size_cap_refuses_instead_of_reading_on() {
    let t = Tmp::new("cap-gz");
    let (pre, run, _) = evidence(&t);
    let a = archive(&run, &t.path().join("export"));
    let before = tree(&a);
    let total: u64 = std::fs::read_dir(&a)
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".gz"))
        .map(|e| e.metadata().unwrap().len())
        .sum();
    let cap = |n: u64| Limits { max_compressed_bytes: n, ..Limits::default() };
    assert!(refusal(open_evidence(&a, true, &cap(64))).contains("compressed-size cap exceeded"));
    // The cap is on the run, not on one file: one byte short still refuses.
    assert!(refusal(open_evidence(&a, true, &cap(total - 1))).contains("compressed-size cap exceeded"));
    assert!(open_evidence(&a, true, &cap(total)).is_ok());
    let (code, out) = super::certify_run(&a, &pre, &cap(64));
    assert_eq!(code, 1, "{out}");
    assert!(out["refused"].as_str().unwrap().contains(ENV_MAX_COMPRESSED_BYTES), "{out}");
    assert_eq!(tree(&a), before);
}

#[test]
fn the_decompressed_size_cap_stops_a_bomb_while_streaming() {
    let t = Tmp::new("cap-raw");
    let (pre, run, _) = evidence(&t);
    let a = archive(&run, &t.path().join("export"));
    let before = tree(&a);
    let total: u64 = std::fs::read_dir(&run).unwrap().map(|e| e.unwrap().metadata().unwrap().len()).sum();
    let cap = |n: u64| Limits { max_decompressed_bytes: n, ..Limits::default() };
    // An honest receipt is refused on what it declares, before anything is decompressed.
    let e = refusal(open_evidence(&a, true, &cap(total - 1)));
    assert!(e.contains("decompressed-size cap exceeded") && e.contains("receipt declares"), "{e}");
    assert!(open_evidence(&a, true, &cap(total)).is_ok());
    // A receipt that understates its sources passes the declaration and is
    // stopped by the stream itself, with no scratch left behind.
    let b = archive(&run, &t.path().join("bomb"));
    rewrite_receipt(&b, |r| {
        for s in r["sources"].as_array_mut().unwrap() {
            s["bytes"] = json!(1);
        }
    });
    let scratch = t.path().join("bomb-scratch");
    let e = refusal(open_evidence_in(&b, true, &cap(1024), scratch.clone()));
    assert!(e.contains("decompressed-size cap exceeded at observations-0.ndjson.gz"), "{e}");
    assert!(!scratch.exists(), "the partial scratch copy is removed");
    let (code, out) = super::certify_run(&b, &pre, &cap(1024));
    assert_eq!(code, 1, "{out}");
    assert!(out["refused"].as_str().unwrap().contains(ENV_MAX_DECOMPRESSED_BYTES), "{out}");
    assert_eq!(tree(&a), before);
}

#[test]
fn the_oi_file_cap_refuses_instead_of_loading_the_file() {
    let t = Tmp::new("cap-oi");
    oi_session_capture(t.path(), d(SESSION));
    let before = tree(t.path());
    let small = Limits { max_oi_file_bytes: 16, ..Limits::default() };
    let (code, out) = super::extract_oi_session(SESSION, t.path(), IMPL, &fp(), &small);
    assert_eq!(code, 1, "{out}");
    assert!(out["refused"].as_str().unwrap().contains(ENV_MAX_OI_FILE_BYTES), "{out}");
    assert_eq!(tree(t.path()), before);
}

#[test]
fn the_oi_aggregate_cap_refuses_before_reading_past_the_budget() {
    let t = Tmp::new("cap-oi-total");
    oi_session_capture(t.path(), d(SESSION));
    // Marker files are small each but unbounded in number: four more, far
    // under the per-file limit, that the extractor ignores (blank lines).
    for day in 1..=4 {
        std::fs::write(t.path().join(format!("opportunity-intelligence-markers-2031-01-0{day}.ndjson")), vec![b'\n'; 4096]).unwrap();
    }
    let before = tree(t.path());
    let len = |name: &str| std::fs::metadata(t.path().join(name)).map(|m| m.len()).unwrap_or(0);
    // The two UTC dates the session spans are the only data files read.
    let data = len("opportunity-intelligence-2026-10-05.ndjson") + len("opportunity-intelligence-2026-10-06.ndjson");
    let markers: u64 = std::fs::read_dir(t.path())
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.file_name().to_string_lossy().starts_with("opportunity-intelligence-markers-"))
        .map(|e| e.metadata().unwrap().len())
        .sum();
    assert!(data > 0 && markers > 4 * 4096);
    let run = |budget: u64| {
        super::extract_oi_session(SESSION, t.path(), IMPL, &fp(), &Limits { max_oi_total_bytes: budget, ..Limits::default() })
    };
    // Exactly enough is enough.
    let (code, out) = run(data + markers);
    assert_eq!(code, 0, "{out}");
    // One byte short refuses, naming the knob.
    let (code, out) = run(data + markers - 1);
    assert_eq!(code, 1, "{out}");
    let why = out["refused"].as_str().unwrap();
    assert!(why.contains("aggregate in-memory cap exceeded") && why.contains(ENV_MAX_OI_TOTAL_BYTES), "{out}");
    // The data fits; it is the marker files together that overflow, each of
    // them a tiny fraction of its own per-file limit.
    let (code, out) = run(data + 4096);
    assert_eq!(code, 1, "{out}");
    let why = out["refused"].as_str().unwrap();
    assert!(why.contains("aggregate in-memory cap exceeded at opportunity-intelligence-markers-"), "{out}");
    // Not even the data fits.
    let (code, out) = run(8);
    assert_eq!(code, 1, "{out}");
    assert!(out["refused"].as_str().unwrap().contains("aggregate in-memory cap exceeded at opportunity-intelligence-2026-10-0"), "{out}");
    assert_eq!(tree(t.path()), before);
}

#[test]
fn a_directory_that_cannot_be_listed_is_an_error_not_an_empty_listing() {
    let t = Tmp::new("listing");
    std::fs::write(t.path().join("observations-0.ndjson"), b"x\n").unwrap();
    assert_eq!(file_names(t.path()).unwrap(), vec!["observations-0.ndjson".to_string()]);
    let missing = t.path().join("absent");
    assert!(file_names(&missing).unwrap_err().contains("cannot list"));
    assert!(run_bounds(&missing).unwrap_err().contains("cannot list"));
    let (code, out) = extract_oi_session(SESSION, &missing, IMPL, &fp());
    assert_eq!(code, 1, "{out}");
    // No listing in the module drops an entry it failed to read.
    let module = include_str!("offline_eval.rs");
    assert!(!module.contains("e.ok()"), "an enumeration error must be propagated, never filtered out");
    assert_eq!(module.matches("read_dir(").count(), 2, "evidence_root and file_names are the only listings");
}

#[test]
fn scratch_is_created_fresh_and_cleanup_removes_only_what_it_made() {
    let t = Tmp::new("scratch");
    let (_, run, _) = evidence(&t);
    let a = archive(&run, &t.path().join("export"));
    let before = tree(&a);
    let scratch = t.path().join("scratch-a");
    let ev = open_evidence_in(&a, true, &Limits::default(), scratch.clone()).unwrap();
    assert_eq!(ev.dir.parent(), Some(scratch.as_path()));
    let materialised: Vec<PathBuf> = std::fs::read_dir(&ev.dir).unwrap().map(|e| e.unwrap().path()).collect();
    assert!(!materialised.is_empty());
    // An existing directory is never adopted -- and refusing it removes nothing from it.
    let e = refusal(open_evidence_in(&a, true, &Limits::default(), scratch.clone()));
    assert!(e.contains("fresh scratch directory"), "{e}");
    assert!(materialised.iter().all(|p| p.is_file()));
    // A file this process did not create survives cleanup, and so does the directory holding it.
    let foreign = scratch.join("not-ours.txt");
    std::fs::write(&foreign, b"keep").unwrap();
    let inside_run = ev.dir.join("also-not-ours.txt");
    std::fs::write(&inside_run, b"keep").unwrap();
    drop(ev);
    assert!(materialised.iter().all(|p| !p.exists()), "its own copies are removed");
    assert_eq!(std::fs::read(&foreign).unwrap(), b"keep");
    assert_eq!(std::fs::read(&inside_run).unwrap(), b"keep");
    // With nothing foreign in it, the scratch directory goes entirely.
    let clean = t.path().join("scratch-b");
    drop(open_evidence_in(&a, true, &Limits::default(), clean.clone()).unwrap());
    assert!(!clean.exists());
    assert_eq!(tree(&a), before);
}

#[test]
fn there_is_no_command_that_deletes() {
    for cmd in ["delete-source", "delete-archive", "delete", "remove", "rm", "prune", "purge", "truncate", "retain", "clean"] {
        for extra in 0..6 {
            let mut args = vec![cmd.to_string()];
            args.extend((0..extra).map(|i| format!("arg{i}")));
            assert_eq!(cli(&args), 64, "{cmd} with {extra} arguments must be a usage error");
        }
    }
    // The module's only removals are the scratch cleanup: one `remove_file`
    // over the files it created and two non-recursive `remove_dir`s. It never
    // creates-or-truncates, renames or copies over a path it was given.
    let module = include_str!("offline_eval.rs");
    assert!(!module.contains("remove_dir_all"));
    assert_eq!(module.matches("remove_file(").count(), 1);
    assert_eq!(module.matches("remove_dir(").count(), 2);
    for forbidden in ["truncate(", "File::create(", "fs::write(", "fs::rename(", "fs::copy(", "set_len(", ".create(true)"] {
        assert!(!module.contains(forbidden), "{forbidden} in the evaluator");
    }
}

#[test]
fn every_command_leaves_the_evidence_byte_identical() {
    let t = Tmp::new("readonly");
    let (pre, run, f) = evidence(&t);
    let arch = archive(&run, &t.path().join("export"));
    let research = t.path().join("research");
    std::fs::create_dir_all(&research).unwrap();
    oi_session_capture(&research, d(SESSION));
    let oi = write_json(t.path(), "oi.json", &oi_ok(&f, SESSION));
    let pre_before = sha(&std::fs::read(&pre).unwrap());
    let oi_before = sha(&std::fs::read(&oi).unwrap());
    let (raw_before, arch_before, research_before) = (tree(&run), tree(&arch), tree(&research));
    let ledger_path = t.path().join("ledger.ndjson");
    let skips_path = t.path().join("skips.ndjson");
    std::fs::write(&ledger_path, "").unwrap();
    std::fs::write(&skips_path, "").unwrap();

    assert_eq!(certify_run(&run, &pre).0, 0);
    assert_eq!(certify_run(&arch, &pre).0, 0);
    assert_eq!(extract_oi_session(SESSION, &research, IMPL, &fp()).0, 0);
    let (code, qualification) = qualify_session(SESSION, &arch, &pre, &oi);
    assert_eq!(code, 0, "{qualification}");
    assert_eq!(replay_campaign(&ledger_path, &skips_path, SESSION).0, 0);
    assert_eq!(record_designation(&ledger_path, &skips_path, SESSION, SESSION, inside(SESSION)).0, 0);
    let after_designation = std::fs::read(&ledger_path).unwrap();
    let q = write_json(t.path(), "qualification.json", &qualification);
    let q_before = sha(&std::fs::read(&q).unwrap());
    assert_eq!(record_capture(&ledger_path, &q).0, 0);
    assert_eq!(record_skip(&ledger_path, &skips_path, SESSION, "2026-10-06", "observer not started", Utc::now()).0, 0);
    assert_eq!(replay_campaign(&ledger_path, &skips_path, SESSION).0, 0);
    // Refusals too.
    assert_eq!(certify_run(&research, &pre).0, 1);
    assert_eq!(qualify_session(SESSION, &run, &pre, &oi).0, 1);
    assert_eq!(record_capture(&ledger_path, &q).0, 1);

    assert_eq!(tree(&run), raw_before, "raw run");
    assert_eq!(tree(&arch), arch_before, "archived run");
    assert_eq!(tree(&research), research_before, "research directory");
    assert_eq!(sha(&std::fs::read(&pre).unwrap()), pre_before);
    assert_eq!(sha(&std::fs::read(&oi).unwrap()), oi_before);
    assert_eq!(sha(&std::fs::read(&q).unwrap()), q_before);
    // The record commands only append: what was there is still a prefix.
    let ledger_now = std::fs::read(&ledger_path).unwrap();
    assert!(ledger_now.starts_with(&after_designation) && ledger_now.len() > after_designation.len());
    assert_eq!(ledger_now.iter().filter(|b| **b == b'\n').count(), 2);
    assert_eq!(std::fs::read(&skips_path).unwrap().iter().filter(|b| **b == b'\n').count(), 1);
}
