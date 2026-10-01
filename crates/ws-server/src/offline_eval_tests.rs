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
        let _ = std::fs::remove_dir_all(&p);
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

/// Archives `run_dir` into `out_root/<runId>/` exactly as `observation_archive.py retain`
/// + `delete-source` leave it: gzip artifacts and the receipt, sources gone.
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

fn oi_ok(f: &Frozen, session: &str) -> Value {
    json!({"session": session, "implementationSha": f.implementation_sha, "configFingerprint": f.oi_fingerprint,
           "certifies": true, "accountingStart": "session_boundary", "tally": {}, "ranges": []})
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

#[test]
fn mid_session_oi_accounting_is_not_zero_loss() {
    let t = Tmp::new("oi-mid");
    let (_, f) = frozen_prereg(t.path(), IMPL);
    let mut v = oi_ok(&f, SESSION);
    v["accountingStart"] = json!("process_start_mid_session");
    assert_eq!(oi_zero_loss(&v, SESSION, &f).unwrap(), (false, "phaseb_marker_check"));
    v["accountingStart"] = json!("session_boundary");
    v["certifies"] = json!(false);
    assert_eq!(oi_zero_loss(&v, SESSION, &f).unwrap().0, false);
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
    let (code, out) = extract_oi_session(SESSION, t.path(), IMPL, "oi-cfg-0000000000000000");
    assert_eq!(code, 1);
    assert!(out["report"]["reasons"].to_string().contains("fingerprint"), "{out}");
    let (code, out) = extract_oi_session("2026-10-06", t.path(), IMPL, &fp());
    assert_eq!(code, 1, "{out}");
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
    for cmd in ["fetch-outcomes", "outcome", "evaluate", "authorize-outcomes"] {
        assert_eq!(cli(&[cmd.to_string()]), 64, "{cmd}");
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
    assert!(out["outcomeAccess"].as_str().unwrap().starts_with("LOCKED"));
    // A missing ledger file is an error, not an empty campaign.
    assert_eq!(replay_campaign(&t.path().join("absent.ndjson"), &k, "2026-10-05").0, 1);
}
