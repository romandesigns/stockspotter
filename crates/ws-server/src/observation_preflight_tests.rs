//! Step 4B-preflight: live-capture engineering tests.
//!
//! Session-bounded runs, writer-side rotation under slow fsync, the streaming
//! certifier (equivalence with the whole-capture reader and its memory bound),
//! the overhead guard, queue warning, preregistration binding and the
//! persistent-root checks. All synthetic; no observer is enabled anywhere.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use market_data::{IgnitionEventKind, ScanEvent};

use super::*;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

struct Tmp(PathBuf);
impl Tmp {
    fn new(tag: &str) -> Self {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "obs-preflight-{tag}-{}-{}-{n}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    /// A provisioned observation root.
    fn root(tag: &str) -> Self {
        let t = Self::new(tag);
        std::fs::write(t.path().join(ROOT_MARKER), b"provisioned\n").unwrap();
        t
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn at(s: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_790_000_000 + s, 0).unwrap()
}

fn confirm(sym: &str, s: i64, price: f64) -> ScanEvent {
    ScanEvent::IgnitionEvent { symbol: sym.into(), timestamp: at(s), price, kind: IgnitionEventKind::FollowThroughConfirmed }
}

fn opened(sym: &str, s: i64, price: f64) -> ScanEvent {
    ScanEvent::IgnitionEvent { symbol: sym.into(), timestamp: at(s), price, kind: IgnitionEventKind::CandidateOpened }
}

fn cand(id: &str, sym: &str, opened_s: i64) -> OpenCandidate {
    OpenCandidate { opportunity_id: id.into(), symbol: sym.into(), opened_at: at(opened_s) }
}

fn window(id: &str, open: Vec<OpenCandidate>, price: f64, end_s: i64) -> WindowInput {
    let mut engine_prices = BTreeMap::new();
    let mut scored = BTreeSet::new();
    for c in &open {
        engine_prices.insert(c.opportunity_id.clone(), price);
        scored.insert(c.opportunity_id.clone());
    }
    WindowInput {
        window_id: id.into(),
        processing_started_at: at(end_s),
        rank_completed_at: at(end_s),
        processing_started_mono: None,
        rank_completed_mono: None,
        open,
        scored,
        engine_prices,
        cohort_truncated: false,
    }
}

fn config(root: &Path) -> ObserverConfig {
    ObserverConfig {
        root: root.to_path_buf(),
        namespace: "preflight".into(),
        pid: 7,
        capture_max_bytes: u64::MAX / 2,
        capture_warn_permille: CAPTURE_WARN_PERMILLE,
        rotate_bytes: DEFAULT_ROTATE_BYTES,
        queue_records: PROPOSED_QUEUE_RECORDS,
        queue_bytes: PROPOSED_QUEUE_BYTES,
        overhead: OverheadLimits { stop_window_micros: u64::MAX, stop_duty_ppm: u64::MAX, ..OverheadLimits::default() },
        identity: RunIdentity::default(),
    }
}

/// Rollover every 100 s of wall time, for tests.
fn every_100s(t: DateTime<Utc>) -> DateTime<Utc> {
    Utc.timestamp_opt((t.timestamp().div_euclid(100) + 1) * 100, 0).unwrap()
}

fn run_dir(root: &Path, run_id: &str) -> PathBuf {
    root.join(run_id)
}

fn values(dir: &Path) -> Vec<serde_json::Value> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(OBSERVATION_FILE_SUFFIX))
        .collect();
    names.sort_by_key(|n| rotation_index(n));
    names
        .iter()
        .flat_map(|n| std::fs::read_to_string(dir.join(n)).unwrap().lines().map(|l| l.to_string()).collect::<Vec<_>>())
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect()
}

/// Both readers must agree exactly: label, failure text, certificate.
fn assert_readers_agree(dir: &Path) -> (CaptureVerdict, stream::StreamStats) {
    let whole = assess(dir);
    let (streamed, stats) = stream::assess_streaming(dir);
    assert_eq!(whole.label(), streamed.label(), "verdict label differs for {}", dir.display());
    match (&whole, &streamed) {
        (CaptureVerdict::Fail(a), CaptureVerdict::Fail(b)) => assert_eq!(a, b, "failure text differs"),
        (CaptureVerdict::Pass(a), CaptureVerdict::Pass(b)) => {
            assert_eq!(format!("{a:?}"), format!("{b:?}"), "certificates differ")
        }
        (CaptureVerdict::Indeterminate(a), CaptureVerdict::Indeterminate(b)) => {
            assert_eq!(format!("{a:?}"), format!("{b:?}"), "indeterminate detail differs")
        }
        _ => unreachable!(),
    }
    (streamed, stats)
}

/// A clean single-file capture with `windows` windows of `n` candidates.
fn clean_capture(dir: &Path, windows: usize, n: usize) -> ObserverRun {
    let run = ObserverRun::allocate(dir, "fixture", at(0), 1).unwrap();
    let writer = FileRecordWriter::create(run.dir(), RUN_FILE_NAME).unwrap();
    let sink = AsyncSink::with_capacity(RUN_FILE_NAME, Box::new(writer), PROPOSED_QUEUE_RECORDS, PROPOSED_QUEUE_BYTES);
    let mut o = Observer::start(&run, "fixture", 1, at(0), Box::new(sink)).unwrap();
    for i in 0..n {
        o.on_receive(&confirm(&format!("S{i}"), 100, 3.5), at(100));
    }
    for w in 0..windows {
        let open = (0..n).map(|i| cand(&format!("S{i}:1"), &format!("S{i}"), 90)).collect();
        o.on_window(window(&format!("oiw-{w}"), open, 3.5, 101 + w as i64));
    }
    o.on_finish(at(10_000));
    run
}

/// Rewrites one run file line by line.
fn tamper(dir: &Path, f: impl Fn(&mut Vec<String>)) {
    let path = dir.join(RUN_FILE_NAME);
    let mut lines: Vec<String> = std::fs::read_to_string(&path).unwrap().lines().map(|l| l.to_string()).collect();
    f(&mut lines);
    let mut body = lines.join("\n");
    body.push('\n');
    std::fs::write(&path, body).unwrap();
}

fn fix_file_close(lines: &mut [String]) {
    let n = lines.len() as u64 - 1;
    let mut v: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    v["recordsWritten"] = serde_json::json!(n);
    *lines.last_mut().unwrap() = v.to_string();
}

fn kind(l: &str) -> String {
    serde_json::from_str::<serde_json::Value>(l).unwrap()["kind"].as_str().unwrap_or("").to_string()
}

// ---------------------------------------------------------------------------
// §9 Streaming reader: equivalence and memory bound
// ---------------------------------------------------------------------------

#[test]
fn streaming_and_whole_capture_readers_agree_on_every_fixture_shape() {
    type Mutator = fn(&mut Vec<String>);
    let mutators: Vec<(&str, Option<Mutator>)> = vec![
        ("clean", None),
        ("substituted row", Some(|l| {
            let i = l.iter().position(|x| kind(x) == "candidate").unwrap();
            let mut v: serde_json::Value = serde_json::from_str(&l[i]).unwrap();
            v["opportunityId"] = serde_json::json!("ZZ:9");
            l[i] = v.to_string();
        })),
        ("content change", Some(|l| {
            let i = l.iter().position(|x| kind(x) == "candidate").unwrap();
            let mut v: serde_json::Value = serde_json::from_str(&l[i]).unwrap();
            v["eligibility"] = serde_json::json!({"eligible": false, "reasons": ["market_age_exceeded"]});
            l[i] = v.to_string();
        })),
        ("missing row", Some(|l| {
            let i = l.iter().position(|x| kind(x) == "candidate").unwrap();
            l.remove(i);
            fix_file_close(l);
        })),
        ("missing window_close", Some(|l| {
            let i = l.iter().position(|x| kind(x) == "window_close").unwrap();
            l.remove(i);
            fix_file_close(l);
        })),
        ("missing window_begin", Some(|l| {
            let i = l.iter().position(|x| kind(x) == "window_begin").unwrap();
            l.remove(i);
            fix_file_close(l);
        })),
        ("duplicate window_begin", Some(|l| {
            let i = l.iter().position(|x| kind(x) == "window_begin").unwrap();
            let d = l[i].clone();
            l.insert(i, d);
            fix_file_close(l);
        })),
        ("missing file_close", Some(|l| {
            l.pop();
        })),
        ("records after close", Some(|l| {
            let d = l[1].clone();
            l.push(d);
        })),
        ("malformed line", Some(|l| {
            l[2] = "{not json".into();
        })),
        ("missing run_end", Some(|l| {
            let i = l.iter().position(|x| kind(x) == "run_end").unwrap();
            l.remove(i);
            fix_file_close(l);
        })),
        ("receipt gap", Some(|l| {
            let i = l.iter().position(|x| kind(x) == "receipt").unwrap();
            l.remove(i);
            fix_file_close(l);
        })),
        ("mixed run id", Some(|l| {
            let i = l.iter().position(|x| kind(x) == "receipt").unwrap();
            let mut v: serde_json::Value = serde_json::from_str(&l[i]).unwrap();
            v["runId"] = serde_json::json!("other");
            l[i] = v.to_string();
        })),
        ("protocol mismatch", Some(|l| {
            l[0] = l[0].replace("consumer-received-protocol-v1", "other-protocol");
        })),
        ("lag not applied", Some(|l| {
            let i = l.iter().position(|x| kind(x) == "window_begin").unwrap();
            let run_id = serde_json::from_str::<serde_json::Value>(&l[0]).unwrap()["runId"].as_str().unwrap().to_string();
            l.insert(i, serde_json::to_string(&ObservationRecord::Lag { run_id, sequence: 1, skipped: 1, at: at(1) }).unwrap());
            fix_file_close(l);
        })),
        ("counters violated", Some(|l| {
            let i = l.iter().position(|x| kind(x) == "run_end").unwrap();
            let mut v: serde_json::Value = serde_json::from_str(&l[i]).unwrap();
            v["counters"]["dropped"] = serde_json::json!(3);
            l[i] = v.to_string();
        })),
        ("stopped", Some(|l| {
            let i = l.iter().position(|x| kind(x) == "run_end").unwrap();
            let mut v: serde_json::Value = serde_json::from_str(&l[i]).unwrap();
            v["stopped"] = serde_json::json!("capture_budget_exceeded");
            l[i] = v.to_string();
        })),
        ("truncated cohort", Some(|l| {
            let i = l.iter().position(|x| kind(x) == "window_close").unwrap();
            let mut v: serde_json::Value = serde_json::from_str(&l[i]).unwrap();
            v["cohortTruncated"] = serde_json::json!(true);
            l[i] = v.to_string();
        })),
    ];
    for (name, m) in mutators {
        let t = Tmp::new("equiv");
        let run = clean_capture(t.path(), 3, 4);
        if let Some(m) = m {
            tamper(run.dir(), m);
        }
        let (v, _) = assert_readers_agree(run.dir());
        if name == "clean" {
            assert_eq!(v.label(), "PASS");
        } else {
            assert!(!v.is_pass(), "{name} must not pass");
        }
    }
    // Empty directory, close-failed marker.
    let t = Tmp::new("equiv-empty");
    assert_readers_agree(t.path());
    let t = Tmp::new("equiv-marker");
    let run = clean_capture(t.path(), 1, 2);
    std::fs::write(run.dir().join(format!("{RUN_FILE_NAME}{CLOSE_FAILED_SUFFIX}")), b"x").unwrap();
    assert_eq!(assert_readers_agree(run.dir()).0.label(), "FAIL");
}

#[test]
fn streaming_reader_agrees_on_rotated_lagged_and_ambiguous_captures() {
    let t = Tmp::new("equiv-rot");
    let run = ObserverRun::allocate(t.path(), "fixture", at(0), 1).unwrap();
    let sink = RotatingSink::create(run.dir(), run.id(), 900, PROPOSED_QUEUE_RECORDS, PROPOSED_QUEUE_BYTES).unwrap();
    let mut o = Observer::start(&run, "fixture", 1, at(0), Box::new(sink)).unwrap();
    for s in ["AAA", "BBB", "CCC"] {
        o.on_receive(&confirm(s, 100, 3.5), at(100));
    }
    let open = vec![cand("AAA:1", "AAA", 90), cand("AAA:2", "AAA", 90), cand("BBB:1", "BBB", 90), cand("CCC:1", "CCC", 90)];
    o.on_window(window("oiw-1", open.clone(), 3.5, 101));
    o.on_lag(2, at(102));
    o.on_window(window("oiw-2", open[2..].to_vec(), 3.5, 103));
    o.on_window(window("oiw-3", open[2..].to_vec(), 3.5, 104));
    o.on_finish(at(200));
    assert!(std::fs::read_dir(run.dir()).unwrap().count() >= 3, "must rotate");
    let (v, _) = assert_readers_agree(run.dir());
    let CaptureVerdict::Pass(cert) = v else { panic!("expected PASS") };
    assert_eq!(cert.invalid_windows, 3);
}

/// The streaming reader holds at most the open window's sets, whatever the
/// capture's size.
#[test]
fn streaming_reader_memory_is_bounded_by_the_largest_window_not_the_capture() {
    let t = Tmp::new("bound");
    let (windows, n) = (60usize, 200usize);
    let run = clean_capture(t.path(), windows, n);
    let (v, stats) = assert_readers_agree(run.dir());
    assert_eq!(v.label(), "PASS");
    assert_eq!(stats.windows_tracked, windows as u64);
    // expected + persisted of one window, never the whole capture (12,000).
    assert!(stats.peak_held_tuples <= 2 * n as u64, "peak {} tuples", stats.peak_held_tuples);
    assert!(stats.peak_held_tuples >= n as u64);
}

// ---------------------------------------------------------------------------
// §7 Session-bounded runs
// ---------------------------------------------------------------------------

fn session(root: &Path, cfg: ObserverConfig) -> SessionObserver {
    let _ = root;
    SessionObserver::with_schedule(Box::new(FileRunFactory { config: cfg }), at(0), every_100s).unwrap()
}

#[test]
fn a_session_closes_at_its_boundary_and_the_next_starts_without_a_restart() {
    let root = Tmp::root("sess");
    let mut s = session(root.path(), config(root.path()));
    let first = s.current_run_id().unwrap();
    s.on_receive(&confirm("AAA", 50, 3.5), at(50));
    s.on_window(window("oiw-1", vec![cand("AAA:1", "AAA", 40)], 3.5, 60));
    s.on_tick(at(100)); // boundary, no event needed
    let second = s.current_run_id().unwrap();
    assert_ne!(first, second, "a new run started");
    let closed = s.join_closed();
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].run_id, first);
    assert_eq!(closed[0].failed_writes, 0);
    let (v, _) = assert_readers_agree(&run_dir(root.path(), &first));
    assert_eq!(v.label(), "PASS", "the closed session certifies");
    // The next session is live and its run_start is on disk.
    s.on_receive(&confirm("BBB", 150, 2.0), at(150));
    s.on_window(window("oiw-2", vec![cand("BBB:1", "BBB", 140)], 2.0, 160));
    s.on_finish(at(170));
    let (v2, _) = assert_readers_agree(&run_dir(root.path(), &second));
    assert_eq!(v2.label(), "PASS");
    assert_eq!(values(&run_dir(root.path(), &second))[0]["startedAt"], serde_json::json!(at(100)));
}

#[test]
fn a_boundary_with_no_data_still_closes_the_session() {
    let root = Tmp::root("sess-idle");
    let mut s = session(root.path(), config(root.path()));
    let first = s.current_run_id().unwrap();
    s.on_tick(at(99));
    assert_eq!(s.current_run_id().unwrap(), first, "not yet");
    s.on_tick(at(100));
    assert_ne!(s.current_run_id().unwrap(), first);
    s.join_closed();
    // Closed, complete, simply without windows.
    let (v, _) = assert_readers_agree(&run_dir(root.path(), &first));
    assert_eq!(v.label(), "INDETERMINATE");
    assert!(values(&run_dir(root.path(), &first)).iter().any(|r| r["kind"] == "run_end"));
    s.on_finish(at(101));
}

#[test]
fn records_queued_at_the_boundary_are_drained_into_the_closing_session() {
    let root = Tmp::root("sess-queued");
    let mut s = session(root.path(), config(root.path()));
    let first = s.current_run_id().unwrap();
    for i in 0..5_000 {
        s.on_receive(&opened(&format!("S{}", i % 300), 10 + (i % 80) as i64, 3.0), at(10 + (i % 80) as i64));
    }
    s.on_receive(&confirm("AAA", 95, 3.5), at(95));
    s.on_window(window("oiw-1", vec![cand("AAA:1", "AAA", 90)], 3.5, 99));
    s.on_tick(at(100)); // immediately, with the queue full of work
    s.join_closed();
    let receipts = values(&run_dir(root.path(), &first)).iter().filter(|r| r["kind"] == "receipt").count();
    assert_eq!(receipts, 5_001, "every queued receipt reached the closing session");
    assert_eq!(assert_readers_agree(&run_dir(root.path(), &first)).0.label(), "PASS");
    s.on_finish(at(101));
}

#[test]
fn rotation_near_the_boundary_keeps_both_sessions_certifiable() {
    let root = Tmp::root("sess-rot");
    let mut cfg = config(root.path());
    cfg.rotate_bytes = 1_500;
    let mut s = session(root.path(), cfg);
    let first = s.current_run_id().unwrap();
    for i in 0..30 {
        s.on_receive(&confirm(&format!("S{i}"), 90, 3.5), at(90));
    }
    let open: Vec<OpenCandidate> = (0..30).map(|i| cand(&format!("S{i}:1"), &format!("S{i}"), 80)).collect();
    s.on_window(window("oiw-1", open, 3.5, 99));
    s.on_receive(&confirm("ZZZ", 100, 1.0), at(100)); // rolls, then lands in run 2
    let second = s.current_run_id().unwrap();
    s.on_window(window("oiw-2", vec![cand("ZZZ:1", "ZZZ", 100)], 1.0, 101));
    s.on_finish(at(102));
    assert!(std::fs::read_dir(run_dir(root.path(), &first)).unwrap().count() >= 3, "run 1 rotated");
    assert_eq!(assert_readers_agree(&run_dir(root.path(), &first)).0.label(), "PASS");
    assert_eq!(assert_readers_agree(&run_dir(root.path(), &second)).0.label(), "PASS");
}

#[test]
fn a_lag_just_before_the_boundary_does_not_leak_into_the_next_session() {
    let root = Tmp::root("sess-lag");
    let mut s = session(root.path(), config(root.path()));
    let first = s.current_run_id().unwrap();
    s.on_receive(&confirm("AAA", 90, 3.5), at(90));
    s.on_window(window("oiw-1", vec![cand("AAA:1", "AAA", 85)], 3.5, 95));
    s.on_lag(4, at(99));
    s.on_tick(at(100));
    let second = s.current_run_id().unwrap();
    // AAA:1 is still open in the new session: it opened before the run began,
    // so it is left-censored there, whatever its confirmations look like.
    s.on_receive(&confirm("AAA", 101, 3.6), at(101));
    s.on_window(window("oiw-2", vec![cand("AAA:1", "AAA", 85)], 3.6, 102));
    s.on_finish(at(103));
    assert_eq!(assert_readers_agree(&run_dir(root.path(), &first)).0.label(), "PASS");
    let (v, _) = assert_readers_agree(&run_dir(root.path(), &second));
    let CaptureVerdict::Pass(cert) = v else { panic!("expected PASS") };
    assert_eq!(cert.eligible, 0);
    assert_eq!(cert.ineligible_by_reason.get("left_censored"), Some(&1));
}

#[test]
fn a_budget_stop_before_the_boundary_does_not_carry_into_the_next_session() {
    let root = Tmp::root("sess-budget");
    let mut cfg = config(root.path());
    cfg.capture_max_bytes = 3_000;
    let mut s = session(root.path(), cfg);
    let first = s.current_run_id().unwrap();
    for i in 0..60 {
        s.on_receive(&opened("AAA", 10 + i, 3.0), at(10 + i));
    }
    s.on_tick(at(100));
    let second = s.current_run_id().unwrap();
    s.on_receive(&confirm("BBB", 101, 2.0), at(101));
    s.on_window(window("oiw-2", vec![cand("BBB:1", "BBB", 100)], 2.0, 102));
    let closed = s.join_closed();
    assert_eq!(closed[0].stopped, Some(StopReason::CaptureBudgetExceeded));
    s.on_finish(at(103));
    let first_verdict = assert_readers_agree(&run_dir(root.path(), &first)).0;
    assert!(!first_verdict.is_pass());
    assert_eq!(assert_readers_agree(&run_dir(root.path(), &second)).0.label(), "PASS", "fresh budget");
}

/// A factory whose first run's writer fails its terminal fsync.
struct FailFirstClose {
    root: PathBuf,
    runs: u32,
    inner: FileRunFactory,
}

/// Unbuffered file writer that fails its second fsync (the terminal record's)
/// and can retract, like the production writer.
struct FailingSync(std::fs::File, u32, u64);
impl RecordWriter for FailingSync {
    fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        self.0.write_all(line.as_bytes())?;
        self.0.write_all(b"\n")?;
        self.2 += line.len() as u64 + 1;
        Ok(())
    }
    fn retract_terminal(&mut self, terminal_bytes: u64) -> std::io::Result<()> {
        self.0.set_len(self.2 - terminal_bytes)
    }
    fn sync(&mut self) -> std::io::Result<()> {
        self.1 += 1;
        if self.1 == 2 {
            Err(std::io::Error::new(std::io::ErrorKind::Other, "injected terminal fsync failure"))
        } else {
            Ok(())
        }
    }
    fn finish(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl RunFactory for FailFirstClose {
    fn start_run(&mut self, at: DateTime<Utc>) -> std::io::Result<Observer> {
        self.runs += 1;
        if self.runs > 1 {
            return self.inner.start_run(at);
        }
        let run = ObserverRun::allocate(&self.root, "failing", at, 1).unwrap();
        let file = std::fs::OpenOptions::new().write(true).create_new(true).open(run.dir().join(RUN_FILE_NAME))?;
        let sink = AsyncSink::new(RUN_FILE_NAME, Box::new(FailingSync(file, 0, 0)));
        Observer::start(&run, "failing", 1, at, Box::new(sink))
    }
}

#[test]
fn a_close_failure_condemns_only_the_closing_session() {
    let root = Tmp::root("sess-fail");
    let factory = FailFirstClose { root: root.path().to_path_buf(), runs: 0, inner: FileRunFactory { config: config(root.path()) } };
    let mut s = SessionObserver::with_schedule(Box::new(factory), at(0), every_100s).unwrap();
    let first = s.current_run_id().unwrap();
    s.on_receive(&confirm("AAA", 50, 3.5), at(50));
    s.on_window(window("oiw-1", vec![cand("AAA:1", "AAA", 40)], 3.5, 60));
    s.on_tick(at(100));
    let second = s.current_run_id().unwrap();
    let closed = s.join_closed();
    assert_eq!(closed[0].failed_writes, 1, "the close failure was reported");
    s.on_receive(&confirm("BBB", 150, 2.0), at(150));
    s.on_window(window("oiw-2", vec![cand("BBB:1", "BBB", 140)], 2.0, 160));
    s.on_finish(at(170));
    // Retracted terminal record: the failed session reads open, never PASS.
    assert_eq!(assert_readers_agree(&run_dir(root.path(), &first)).0.label(), "INDETERMINATE");
    assert_eq!(assert_readers_agree(&run_dir(root.path(), &second)).0.label(), "PASS");
}

#[test]
fn rollover_times_fall_at_2010_new_york_across_both_offsets() {
    // EDT: 2026-09-28 -> 20:10 ET = 00:10Z on 09-29.
    let t = Utc.with_ymd_and_hms(2026, 9, 28, 14, 0, 0).unwrap();
    assert_eq!(next_rollover_after(t), Utc.with_ymd_and_hms(2026, 9, 29, 0, 10, 0).unwrap());
    // Exactly at the rollover: the next one, a day later.
    assert_eq!(
        next_rollover_after(Utc.with_ymd_and_hms(2026, 9, 29, 0, 10, 0).unwrap()),
        Utc.with_ymd_and_hms(2026, 9, 30, 0, 10, 0).unwrap()
    );
    // EST: 2026-12-01 -> 20:10 ET = 01:10Z on 12-02.
    let w = Utc.with_ymd_and_hms(2026, 12, 1, 15, 0, 0).unwrap();
    assert_eq!(next_rollover_after(w), Utc.with_ymd_and_hms(2026, 12, 2, 1, 10, 0).unwrap());
    // Before the 04:00 open (still the previous market day).
    let early = Utc.with_ymd_and_hms(2026, 9, 29, 7, 0, 0).unwrap(); // 03:00 ET
    assert_eq!(next_rollover_after(early), Utc.with_ymd_and_hms(2026, 9, 30, 0, 10, 0).unwrap());
}

// ---------------------------------------------------------------------------
// §8 Rotation off the consumer thread, under slow fsync
// ---------------------------------------------------------------------------

#[test]
fn slow_fsync_during_rotation_never_reaches_the_consumer() {
    let t = Tmp::new("slowfsync");
    let run = ObserverRun::allocate(t.path(), "slow", at(0), 1).unwrap();
    let delay = Duration::from_millis(150);
    let sink = RotatingSink::create_with_sync_delay(
        run.dir(), run.id(), 2_000, PROPOSED_QUEUE_RECORDS, PROPOSED_QUEUE_BYTES, delay,
    )
    .unwrap();
    let mut o = Observer::start(&run, "slow", 1, at(0), Box::new(sink))
        .unwrap()
        .with_close_timeout(SESSION_CLOSE_TIMEOUT);
    for i in 0..20 {
        o.on_receive(&confirm(&format!("S{i}"), 100, 3.5), at(100));
    }
    let open: Vec<OpenCandidate> = (0..20).map(|i| cand(&format!("S{i}:1"), &format!("S{i}"), 90)).collect();
    let mut worst = Duration::ZERO;
    let started = Instant::now();
    for w in 0..15 {
        let s = Instant::now();
        o.on_window(window(&format!("oiw-{w}"), open.clone(), 3.5, 101 + w));
        worst = worst.max(s.elapsed());
    }
    let consumer_total = started.elapsed();
    o.on_finish(at(1_000));
    let files = std::fs::read_dir(run.dir()).unwrap().count();
    // Each rotation costs the writer two fsyncs at >= 150 ms.
    assert!(files >= 8, "{files} files");
    let writer_floor = delay * 2 * (files as u32 - 1);
    assert!(worst < delay, "worst consumer window {worst:?} must not wait on a {delay:?} fsync");
    assert!(consumer_total < writer_floor / 4, "consumer {consumer_total:?} vs writer >= {writer_floor:?}");
    assert_eq!(o.failed_writes(), 0, "the close outlasted the slow disk");
    let (v, _) = assert_readers_agree(run.dir());
    assert_eq!(v.label(), "PASS", "no row missing or duplicated across rotations");
}

/// The other side of the same trade: a close that cannot finish in its
/// timeout is a failed close, and the capture is refused -- never passed.
#[test]
fn a_close_that_outlives_its_timeout_fails_closed() {
    let t = Tmp::new("slowclose");
    let run = ObserverRun::allocate(t.path(), "slow", at(0), 1).unwrap();
    let sink = RotatingSink::create_with_sync_delay(
        run.dir(), run.id(), 1_000, PROPOSED_QUEUE_RECORDS, PROPOSED_QUEUE_BYTES, Duration::from_millis(200),
    )
    .unwrap();
    let mut o = Observer::start(&run, "slow", 1, at(0), Box::new(sink))
        .unwrap()
        .with_close_timeout(Duration::from_millis(300));
    for i in 0..40 {
        o.on_receive(&confirm(&format!("S{i}"), 100, 3.5), at(100));
    }
    o.on_window(window("oiw-1", (0..40).map(|i| cand(&format!("S{i}:1"), &format!("S{i}"), 90)).collect(), 3.5, 101));
    o.on_finish(at(102));
    assert!(o.failed_writes() >= 1);
    drop(o); // let the writer run to the end of what it had
    assert!(!assess(run.dir()).is_pass());
}

// ---------------------------------------------------------------------------
// §10 Overhead guard
// ---------------------------------------------------------------------------

fn guarded(limits: OverheadLimits) -> (Tmp, ObserverRun, Observer) {
    let t = Tmp::new("guard");
    let run = ObserverRun::allocate(t.path(), "guard", at(0), 1).unwrap();
    let writer = FileRecordWriter::create(run.dir(), RUN_FILE_NAME).unwrap();
    let o = Observer::start(&run, "guard", 1, at(0), Box::new(AsyncSink::new(RUN_FILE_NAME, Box::new(writer))))
        .unwrap()
        .with_overhead_limits(limits);
    (t, run, o)
}

#[test]
fn an_overhead_warning_is_exposed_without_stopping_observation() {
    let limits = OverheadLimits { warn_window_micros: 0, warn_duty_ppm: 0, stop_window_micros: u64::MAX, stop_duty_ppm: u64::MAX, duty_window_secs: 60 };
    let (_t, run, mut o) = guarded(limits);
    o.on_receive(&confirm("AAA", 100, 3.5), at(100));
    o.on_window(window("oiw-1", vec![cand("AAA:1", "AAA", 90)], 3.5, 101));
    assert!(o.overhead_warning());
    assert_eq!(o.stopped(), None);
    o.on_finish(at(102));
    let end = values(run.dir()).into_iter().find(|r| r["kind"] == "run_end").unwrap();
    assert_eq!(end["overhead"]["warned"], true);
    assert_eq!(assess(run.dir()).label(), "PASS", "a warning alone does not invalidate");
}

#[test]
fn a_window_over_the_hard_limit_stops_observation_and_the_capture_cannot_certify() {
    let limits = OverheadLimits { stop_window_micros: 0, ..OverheadLimits::default() };
    let (_t, run, mut o) = guarded(limits);
    o.on_receive(&confirm("AAA", 100, 3.5), at(100));
    o.on_window(window("oiw-1", vec![cand("AAA:1", "AAA", 90)], 3.5, 101));
    assert_eq!(o.stopped(), Some(StopReason::ConsumerOverheadExceeded));
    // Detached: later hooks do nothing and cost nothing measurable.
    let s = Instant::now();
    for _ in 0..10_000 {
        o.on_receive(&confirm("AAA", 102, 3.5), at(102));
    }
    assert!(s.elapsed() < Duration::from_millis(200));
    o.on_finish(at(103));
    let v = values(run.dir());
    let stop = v.iter().position(|r| r["kind"] == "stopped").unwrap();
    assert_eq!(v[stop]["reason"], "consumer_overhead_exceeded");
    assert!(v[stop + 1..].iter().all(|r| r["kind"] == "run_end" || r["kind"] == "file_close"));
    assert!(assess(run.dir()).label() == "FAIL");
}

#[test]
fn duty_over_the_hard_limit_stops_observation() {
    // A 1 s window so a debug-build receipt's microseconds register in ppm.
    let limits = OverheadLimits { stop_duty_ppm: 0, duty_window_secs: 1, ..OverheadLimits::default() };
    let (_t, _run, mut o) = guarded(limits);
    o.on_receive(&confirm("AAA", 100, 3.5), at(100));
    assert_eq!(o.stopped(), Some(StopReason::ConsumerOverheadExceeded));
    o.on_finish(at(101));
}

#[test]
fn the_duty_window_forgets_cost_older_than_its_span() {
    let limits = OverheadLimits { duty_window_secs: 2, ..OverheadLimits::default() };
    let mut g = OverheadGuard::new(limits);
    let t0 = g.epoch;
    assert!(!g.charge(t0, Duration::from_millis(10), false));
    assert!(g.total_nanos >= 10_000_000);
    g.charge(t0 + Duration::from_secs(5), Duration::ZERO, false);
    assert_eq!(g.total_nanos, 0, "cost older than the window is gone");
    // 2 s window at 2% stop = 40 ms of cost inside it.
    assert!(!g.charge(t0 + Duration::from_secs(5), Duration::from_millis(39), false));
    assert!(g.charge(t0 + Duration::from_secs(5), Duration::from_millis(2), false));
}

// ---------------------------------------------------------------------------
// §11 Queue configuration
// ---------------------------------------------------------------------------

struct Gate(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl RecordWriter for Gate {
    fn write_line(&mut self, _l: &str) -> std::io::Result<()> {
        while self.0.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }
    fn finish(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn the_queue_warns_at_a_quarter_of_either_bound_and_loss_is_the_first_drop() {
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut sink = AsyncSink::with_capacity(RUN_FILE_NAME, Box::new(Gate(gate.clone())), 8, 1 << 20);
    let rec = ObservationRecord::Lag { run_id: "r".into(), sequence: 1, skipped: 1, at: at(0) };
    sink.write(&rec).unwrap(); // the writer takes one and blocks on it
    std::thread::sleep(Duration::from_millis(50));
    sink.write(&rec).unwrap();
    assert!(!sink.queue_warning(), "1 of 8 queued");
    sink.write(&rec).unwrap();
    assert!(sink.queue_warning(), "2 of 8 queued = 25%");
    assert!(sink.telemetry_snapshot().queue_warning);
    assert_eq!(sink.counters().dropped, 0);
    for _ in 0..20 {
        let _ = sink.write(&rec);
    }
    assert!(sink.counters().dropped > 0, "overflow drops, and a drop refuses the capture");
    gate.store(false, std::sync::atomic::Ordering::SeqCst);
    sink.close("r", at(1)).unwrap();
}

#[test]
fn the_proposed_queue_configuration_is_the_live_default() {
    assert_eq!(PROPOSED_QUEUE_RECORDS, 65_536);
    assert_eq!(PROPOSED_QUEUE_BYTES, 128 * 1024 * 1024);
    assert_eq!(QUEUE_WARN_PERMILLE, 250);
    let c = config(Path::new("x"));
    assert_eq!(c.queue_records, PROPOSED_QUEUE_RECORDS);
}

// ---------------------------------------------------------------------------
// §14 Preregistration binding
// ---------------------------------------------------------------------------

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ops/observation/step4-preregistration-v1.fixture.json")
}

/// Pinned identity of the fixture. The Python tool computes the same value
/// (`python/tests/test_observation_archive.py`), so the two implementations of
/// RFC 8785 canonicalisation are held to each other.
const FIXTURE_SHA256: &str = "8dc8a34afb761d3d8b4ffcee147ff605ff9f75ef5cdd4882752e6cebf92b083e";

#[test]
fn the_fixture_preregistration_has_its_pinned_canonical_identity() {
    let bound = prereg::load(&fixture_path()).expect("fixture binds");
    assert_eq!(bound.sha256, FIXTURE_SHA256);
    assert_eq!(bound.implementation_sha(), "0000000000000000000000000000000000000000");
}

#[test]
fn canonical_identity_ignores_formatting_and_key_order_but_not_content() {
    let a = br#"{"b":1,"a":{"y":"x","x":[1,2]}}"#;
    let b = b"{ \"a\" : { \"x\" : [ 1 , 2 ], \"y\" : \"x\" },\n \"b\" : 1 }";
    let va: serde_json::Value = serde_json::from_slice(a).unwrap();
    let vb: serde_json::Value = serde_json::from_slice(b).unwrap();
    assert_eq!(prereg::canonicalize(&va).unwrap(), br#"{"a":{"x":[1,2],"y":"x"},"b":1}"#.to_vec());
    assert_eq!(prereg::canonicalize(&va).unwrap(), prereg::canonicalize(&vb).unwrap());
    let vc: serde_json::Value = serde_json::from_slice(br#"{"b":2,"a":{"y":"x","x":[1,2]}}"#).unwrap();
    assert_ne!(prereg::canonicalize(&va).unwrap(), prereg::canonicalize(&vc).unwrap());
    // JCS string escaping.
    let vs: serde_json::Value = serde_json::json!({"s": "a\u{1}\n\"/é"});
    assert_eq!(prereg::canonicalize(&vs).unwrap(), "{\"s\":\"a\\u0001\\n\\\"/é\"}".as_bytes().to_vec());
}

#[test]
fn floats_and_non_ascii_keys_are_refused_rather_than_approximated() {
    let f: serde_json::Value = serde_json::json!({"floor": 0.3});
    assert!(matches!(prereg::canonicalize(&f), Err(prereg::PreregBindingError::Float { .. })));
    let k: serde_json::Value = serde_json::json!({"clé": 1});
    assert!(matches!(prereg::canonicalize(&k), Err(prereg::PreregBindingError::NonAsciiKey { .. })));
}

#[test]
fn a_preregistration_whose_constants_differ_from_the_build_is_refused() {
    let root = Tmp::root("prereg");
    let mut cfg = config(root.path());
    cfg.overhead = OverheadLimits::default();
    cfg.capture_max_bytes = DEFAULT_CAPTURE_MAX_BYTES;
    let bound = bind_preregistration(cfg.clone(), Some(fixture_path())).expect("fixture matches the build");
    assert_eq!(bound.identity.preregistration_sha256.as_deref(), Some(FIXTURE_SHA256));
    let mut other = cfg.clone();
    other.queue_records = 4_096;
    match bind_preregistration(other, Some(fixture_path())) {
        Err(ConfigError::Prereg(prereg::PreregBindingError::ConstantMismatch { field, .. })) => assert_eq!(field, "queue.records"),
        other => panic!("expected mismatch, got {other:?}"),
    }
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(fixture_path()).unwrap()).unwrap();
    v["freshness"]["primaryMaxAgeMs"] = serde_json::json!(31_000);
    let p = root.path().join("bad.json");
    std::fs::write(&p, v.to_string()).unwrap();
    assert!(matches!(
        bind_preregistration(cfg, Some(p)),
        Err(ConfigError::Prereg(prereg::PreregBindingError::ConstantMismatch { field: "freshness.primaryMaxAgeMs", .. }))
    ));
}

#[test]
fn a_bound_run_records_its_identity_and_the_certificate_verifies_it() {
    let t = Tmp::new("bound-run");
    let run = ObserverRun::allocate(t.path(), "bound", at(0), 1).unwrap();
    let writer = FileRecordWriter::create(run.dir(), RUN_FILE_NAME).unwrap();
    let identity = RunIdentity {
        implementation_sha: Some("a".repeat(40)),
        preregistration_sha256: Some(FIXTURE_SHA256.to_string()),
    };
    let mut o = Observer::start_bound(&run, "bound", 1, at(0), Box::new(AsyncSink::new(RUN_FILE_NAME, Box::new(writer))), identity).unwrap();
    o.on_receive(&confirm("AAA", 100, 3.5), at(100));
    o.on_window(window("oiw-1", vec![cand("AAA:1", "AAA", 90)], 3.5, 101));
    o.on_finish(at(102));
    let expected = BoundIdentity { implementation_sha: "a".repeat(40), preregistration_sha256: FIXTURE_SHA256.into() };
    assert_eq!(assess_bound(run.dir(), &expected).label(), "PASS");
    assert_eq!(stream::assess_streaming_bound(run.dir(), &expected).0.label(), "PASS");
    let wrong = BoundIdentity { preregistration_sha256: "f".repeat(64), ..expected.clone() };
    assert_eq!(assess_bound(run.dir(), &wrong).label(), "FAIL");
    // An unbound run is never a bound PASS.
    let t2 = Tmp::new("unbound-run");
    let run2 = clean_capture(t2.path(), 1, 1);
    assert_eq!(assess_bound(run2.dir(), &expected).label(), "INDETERMINATE");
}

// ---------------------------------------------------------------------------
// §6 Persistent observation root
// ---------------------------------------------------------------------------

#[test]
fn an_unprovisioned_root_fails_closed_and_is_never_created() {
    let t = Tmp::new("root");
    let missing = t.path().join("not-mounted");
    assert!(matches!(check_root(&missing), Err(ConfigError::RootUnavailable { .. })));
    assert!(!missing.exists(), "the root is never created");
    assert!(matches!(check_root(t.path()), Err(ConfigError::RootUnavailable { .. })), "no marker");
    std::fs::write(t.path().join(ROOT_MARKER), b"x").unwrap();
    assert!(check_root(t.path()).is_ok());
    // The factory re-checks at every run start, so a volume unmounted between
    // sessions stops observation instead of writing to the container layer.
    let mut f = FileRunFactory { config: config(t.path()) };
    assert!(f.start_run(at(0)).is_ok());
    std::fs::remove_file(t.path().join(ROOT_MARKER)).unwrap();
    assert!(f.start_run(at(1)).is_err());
}

#[test]
fn a_root_inside_a_retention_managed_tree_is_refused() {
    let t = Tmp::new("root-ret");
    for sub in ["research", "discovery-audit"] {
        let r = t.path().join(sub).join("observation");
        std::fs::create_dir_all(&r).unwrap();
        std::fs::write(r.join(ROOT_MARKER), b"x").unwrap();
        assert!(matches!(check_root(&r), Err(ConfigError::RootUnavailable { .. })), "{sub}");
    }
    let ok = t.path().join("data").join("observation");
    std::fs::create_dir_all(&ok).unwrap();
    std::fs::write(ok.join(ROOT_MARKER), b"x").unwrap();
    assert!(check_root(&ok).is_ok());
}

// ---------------------------------------------------------------------------
// §9/§13 Streaming-reader memory characterisation (ignored; run per process)
// ---------------------------------------------------------------------------
//
// Three separate invocations, so each reader's peak RSS is its own process's:
//
//   OBS_BENCH_DIR=/tmp/cap cargo test --release -p ws-server step4b_generate_capture -- --ignored --nocapture
//   OBS_BENCH_DIR=/tmp/cap cargo test --release -p ws-server step4b_read_streaming  -- --ignored --nocapture
//   OBS_BENCH_DIR=/tmp/cap cargo test --release -p ws-server step4b_read_whole      -- --ignored --nocapture

fn peak_rss() -> String {
    std::fs::read_to_string("/proc/self/status")
        .map(|s| s.lines().filter(|l| l.starts_with("VmHWM") || l.starts_with("VmRSS")).collect::<Vec<_>>().join(", "))
        .unwrap_or_else(|_| "n/a (no /proc)".into())
}

fn bench_dir() -> PathBuf {
    PathBuf::from(std::env::var("OBS_BENCH_DIR").expect("set OBS_BENCH_DIR"))
}

#[test]
#[ignore = "Step 4B characterisation"]
fn step4b_generate_capture() {
    let dir = bench_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(ROOT_MARKER), b"bench").unwrap();
    let windows: usize = std::env::var("OBS_BENCH_WINDOWS").ok().and_then(|v| v.parse().ok()).unwrap_or(60);
    let n: usize = std::env::var("OBS_BENCH_COHORT").ok().and_then(|v| v.parse().ok()).unwrap_or(16_375);
    let run = ObserverRun::allocate(&dir, "bench", at(0), 1).unwrap();
    let sink = RotatingSink::create(run.dir(), run.id(), DEFAULT_ROTATE_BYTES, PROPOSED_QUEUE_RECORDS, PROPOSED_QUEUE_BYTES).unwrap();
    let mut o = Observer::start(&run, "bench", 1, at(0), Box::new(sink))
        .unwrap()
        .with_capture_max_bytes(u64::MAX / 2)
        .with_close_timeout(SESSION_CLOSE_TIMEOUT)
        // Back-to-back windows are ~100% consumer duty and would (correctly)
        // trip the guard; live windows are >= 30 s apart. Off for generation.
        .with_overhead_limits(OverheadLimits {
            stop_window_micros: u64::MAX,
            stop_duty_ppm: u64::MAX,
            ..OverheadLimits::default()
        });
    for i in 0..n {
        o.on_receive(&confirm(&format!("S{i}"), 100, 1.0 + (i % 997) as f64 * 0.01), at(100));
    }
    let open: Vec<OpenCandidate> = (0..n).map(|i| cand(&format!("S{i}:2026-09-29:{}", 28_800_000 + i), &format!("S{i}"), 90)).collect();
    let mut w = window("x", open, 0.0, 0);
    w.engine_prices = (0..n).map(|i| (format!("S{i}:2026-09-29:{}", 28_800_000 + i), 1.0 + (i % 997) as f64 * 0.01)).collect();
    for k in 0..windows {
        let mut wi = w.clone();
        wi.window_id = format!("oiw-{k}");
        wi.processing_started_at = at(101 + k as i64);
        wi.rank_completed_at = at(101 + k as i64);
        o.on_window(wi);
        // Live windows are >= 30 s apart; the writer clears one in about
        // 20 ms. Back-to-back generation would overrun the queue instead.
        o.sink.drain(Duration::from_secs(120)).unwrap();
    }
    o.on_finish(at(100_000));
    let bytes: u64 = std::fs::read_dir(run.dir()).unwrap().map(|e| e.unwrap().metadata().unwrap().len()).sum();
    println!("GENERATED run={} windows={windows} cohort={n} bytes={bytes} failed_writes={}", run.id(), o.failed_writes());
}

fn only_run(dir: &Path) -> PathBuf {
    std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).find(|p| p.is_dir()).expect("a run directory")
}

#[test]
#[ignore = "Step 4B characterisation"]
fn step4b_read_streaming() {
    let run = only_run(&bench_dir());
    let before = peak_rss();
    let s = Instant::now();
    let (v, stats) = stream::assess_streaming(&run);
    if let CaptureVerdict::Fail(d) = &v { println!("STREAMING detail={d}"); }
    println!(
        "STREAMING verdict={} time={:?} bytes_read={} peak_held_tuples={} peak_held_bytes={} windows={} rss_before=[{before}] rss_after=[{}]",
        v.label(), s.elapsed(), stats.bytes_read, stats.peak_held_tuples, stats.peak_held_bytes, stats.windows_tracked, peak_rss()
    );
}

#[test]
#[ignore = "Step 4B characterisation"]
fn step4b_read_whole() {
    let run = only_run(&bench_dir());
    let before = peak_rss();
    let s = Instant::now();
    let v = assess(&run);
    println!("WHOLE verdict={} time={:?} rss_before=[{before}] rss_after=[{}]", v.label(), s.elapsed(), peak_rss());
}
