//! Frozen-contract regression suite for `consumer-received-protocol-v1`.
//!
//! Two parts:
//!
//! 1. **The L1 replay** (`l1_*`). Every semantic assertion the frozen L1 suite
//!    made (`observation_tests.rs` @ L1, SHA-256 `30a89799...6f34`, preserved
//!    at `stockspotter-research/recovered-l1-20260928/`), re-expressed against
//!    this implementation. L2.2 ran these as a scratch replay and nine of them
//!    exposed divergences; here each one asserts the **frozen L1 semantics**,
//!    so a regression back to the divergent behaviour fails the build.
//! 2. **The restoration boundary tests** (`contract_*`). The L2.3 cases the
//!    replay could not express: exact millisecond boundaries, clock steps,
//!    every stage of the close protocol failing, the capture budget, and those
//!    combined with rotation, lag and the expected identity set.
//!
//! Each test names the frozen requirement it holds. None encodes behaviour
//! merely because the implementation happens to have it.

use std::collections::{BTreeMap, BTreeSet};
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
            "obs-contract-{tag}-{}-{}-{n}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
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

fn at_ms(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(1_790_000_000_000 + ms).unwrap()
}
fn at(s: i64) -> DateTime<Utc> {
    at_ms(s * 1000)
}

/// Collects serialized lines in memory, so evidence can be corrupted in
/// exactly one way at a time before being written to disk.
#[derive(Clone, Default)]
struct Shared {
    lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    counters: std::sync::Arc<std::sync::Mutex<WriterCounters>>,
}
impl Shared {
    fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }
}
impl ObservationSink for Shared {
    fn write(&mut self, r: &ObservationRecord) -> std::io::Result<()> {
        let mut c = self.counters.lock().unwrap();
        c.attempted += 1;
        c.written += 1;
        drop(c);
        self.lines.lock().unwrap().push(serde_json::to_string(r).unwrap());
        Ok(())
    }
    fn counters(&self) -> WriterCounters {
        *self.counters.lock().unwrap()
    }
    fn close(&mut self, run_id: &str, at: DateTime<Utc>) -> std::io::Result<()> {
        let w = self.counters.lock().unwrap().written;
        let line = serde_json::to_string(&ObservationRecord::FileClose {
            run_id: run_id.into(),
            file_name: RUN_FILE_NAME.into(),
            records_written: w,
            closed_at: at,
            next_file: None,
        })
        .unwrap();
        self.lines.lock().unwrap().push(line);
        Ok(())
    }
}

fn ign(sym: &str, ts: DateTime<Utc>, price: f64, kind: IgnitionEventKind) -> ScanEvent {
    ScanEvent::IgnitionEvent { symbol: sym.into(), timestamp: ts, price, kind }
}
fn confirm(sym: &str, ts: DateTime<Utc>, price: f64) -> ScanEvent {
    ign(sym, ts, price, IgnitionEventKind::FollowThroughConfirmed)
}

fn cand(id: &str, sym: &str) -> OpenCandidate {
    OpenCandidate { opportunity_id: id.into(), symbol: sym.into(), opened_at: at(90) }
}

/// A window whose open candidates are all scored at `price`.
fn window(id: &str, open: Vec<OpenCandidate>, price: f64, end: DateTime<Utc>) -> WindowInput {
    let mut engine_prices = BTreeMap::new();
    let mut scored = BTreeSet::new();
    for c in &open {
        engine_prices.insert(c.opportunity_id.clone(), price);
        scored.insert(c.opportunity_id.clone());
    }
    WindowInput {
        window_id: id.into(),
        processing_started_at: end,
        rank_completed_at: end,
        processing_started_mono: None,
        rank_completed_mono: None,
        open,
        scored,
        engine_prices,
        cohort_truncated: false,
    }
}

fn observer(sink: &Shared, dir: &Path) -> Observer {
    let run = ObserverRun::allocate(dir, "contract", at(0), 7).unwrap();
    Observer::start(&run, "contract", 7, at(0), Box::new(sink.clone())).unwrap()
}

/// One confirmation receipt at 100 s, one window completing at 102 s.
fn clean_lines() -> Vec<String> {
    let sink = Shared::default();
    let t = Tmp::new("clean");
    let mut o = observer(&sink, t.path());
    o.on_receive(&confirm("AAA", at(100), 3.5), at(100));
    o.on_window(window("oiw-1", vec![cand("AAA:1", "AAA")], 3.5, at(102)));
    o.on_finish(at(103));
    sink.lines()
}

fn write_run(dir: &Path, lines: &[String]) {
    let mut body = lines.join("\n");
    body.push('\n');
    std::fs::write(dir.join(RUN_FILE_NAME), body).unwrap();
}

fn certify(lines: &[String]) -> Result<Certificate, String> {
    let t = Tmp::new("cert");
    write_run(t.path(), lines);
    let a = acquire(t.path()).map_err(|e| format!("acquire: {e}"))?;
    let a = authenticate(a).map_err(|e| format!("authenticate: {e}"))?;
    Certificate::issue(&a).map_err(|e| format!("certificate: {e}"))
}

fn verdict(lines: &[String]) -> CaptureVerdict {
    let t = Tmp::new("assess");
    write_run(t.path(), lines);
    assess(t.path())
}

fn values(lines: &[String]) -> Vec<serde_json::Value> {
    lines.iter().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn candidates(lines: &[String]) -> Vec<serde_json::Value> {
    values(lines).into_iter().filter(|v| v["kind"] == "candidate").collect()
}

fn reasons(c: &serde_json::Value) -> Vec<String> {
    c["eligibility"]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap().to_string())
        .collect()
}

/// Keeps the file-level declared count equal to what is present, so a row
/// removal is caught by the window check and nothing earlier.
fn patch_file_close(lines: &mut [String]) {
    let n = lines.len() as u64 - 1;
    let last = lines.last_mut().unwrap();
    let mut v: serde_json::Value = serde_json::from_str(last).unwrap();
    v["recordsWritten"] = serde_json::json!(n);
    *last = v.to_string();
}

fn position(lines: &[String], kind: &str) -> usize {
    lines.iter().position(|l| l.contains(&format!("\"kind\":\"{kind}\""))).unwrap()
}

// ===========================================================================
// Part 1 -- the frozen L1 suite, replayed with L1 semantics
// ===========================================================================

/// L1 `unscored_and_semantic_reconciliation`, first half: an unscored complete
/// row reconciles.
#[test]
fn l1_1a_unscored_complete_row_certifies() {
    let sink = Shared::default();
    let t = Tmp::new("unscored");
    let mut o = observer(&sink, t.path());
    o.on_receive(&confirm("AAA", at(100), 3.5), at(100));
    let mut w = window("oiw-1", vec![cand("AAA:1", "AAA")], 3.5, at(102));
    w.scored.clear();
    w.engine_prices.clear();
    o.on_window(w);
    o.on_finish(at(103));
    let lines = sink.lines();
    assert_eq!(candidates(&lines)[0]["scored"], false);
    assert!(certify(&lines).is_ok());
}

/// L1 `unscored_and_semantic_reconciliation`, second half; clause 6. Same
/// count, different content: `Invalid::Rows`. L2.2 replay `l1_1b` certified.
#[test]
fn l1_1b_same_count_substitution_refuses() {
    let mut lines = clean_lines();
    let idx = position(&lines, "candidate");
    let mut v: serde_json::Value = serde_json::from_str(&lines[idx]).unwrap();
    v["opportunityId"] = serde_json::json!("ZZZ:substituted");
    lines[idx] = v.to_string();
    let err = certify(&lines).unwrap_err();
    assert!(err.contains("persisted set differs from expected"), "{err}");
}

/// Clause 6: content change of the same row (eligibility), same count.
#[test]
fn l1_1c_same_count_content_change_refuses() {
    let mut lines = clean_lines();
    let idx = position(&lines, "candidate");
    let mut v: serde_json::Value = serde_json::from_str(&lines[idx]).unwrap();
    assert_eq!(v["eligibility"]["eligible"], true);
    v["eligibility"] = serde_json::json!({"eligible": false, "reasons": ["market_age_exceeded"]});
    lines[idx] = v.to_string();
    assert!(certify(&lines).unwrap_err().contains("persisted set differs"));
}

/// L1 `marker_does_not_repair_actual_failed_write`; clause 6.
#[test]
fn l1_2_tidy_end_marker_does_not_repair_missing_row() {
    let mut lines = clean_lines();
    lines.retain(|l| !l.contains("\"kind\":\"candidate\""));
    patch_file_close(&mut lines);
    let err = certify(&lines).unwrap_err();
    assert!(err.contains("declared 1 candidates, counted 0"), "{err}");
}

/// L1 `missing_marker_flush_and_rotation_fail`: missing window end.
#[test]
fn l1_3a_missing_window_end_refuses() {
    let mut lines = clean_lines();
    lines.retain(|l| !l.contains("\"kind\":\"window_close\""));
    patch_file_close(&mut lines);
    assert!(certify(&lines).unwrap_err().contains("no terminal record"));
}

/// Missing expected-set declaration: no certificate without one.
#[test]
fn l1_3b_missing_window_begin_refuses() {
    let mut lines = clean_lines();
    lines.retain(|l| !l.contains("\"kind\":\"window_begin\""));
    patch_file_close(&mut lines);
    assert!(certify(&lines).unwrap_err().contains("no expected identity set"));
}

/// Missing file terminal record: L1 `Invalid::Boundary`; here INDETERMINATE,
/// which equally refuses.
#[test]
fn l1_3c_missing_file_close_never_passes() {
    let mut lines = clean_lines();
    lines.pop();
    let v = verdict(&lines);
    assert_eq!(v.label(), "INDETERMINATE");
    assert!(!v.is_pass());
}

/// L1 `partial_line`: a torn tail after the terminal record.
#[test]
fn l1_3d_partial_line_in_closed_file_refuses() {
    let lines = clean_lines();
    let t = Tmp::new("partial");
    let mut body = lines.join("\n");
    body.push_str("\n{\"kind\":\"recei");
    std::fs::write(t.path().join(RUN_FILE_NAME), body).unwrap();
    assert_eq!(assess(t.path()).label(), "FAIL");
}

/// L1 `before != after` (closed file changed): appended after close.
#[test]
fn l1_3e_content_after_close_refuses() {
    let mut lines = clean_lines();
    lines.push(lines[1].clone());
    assert_eq!(verdict(&lines).label(), "FAIL");
}

/// L1 `hidden_loss...`: `source_lag != 0 ⇒ Invalid::Loss`; clauses 5 and 9.
/// L2.2 replay `l1_4a` left the candidate eligible and certified.
#[test]
fn l1_4a_source_lag_invalidates_the_window() {
    let sink = Shared::default();
    let t = Tmp::new("lag");
    let mut o = observer(&sink, t.path());
    o.on_receive(&confirm("AAA", at(100), 3.5), at(100));
    o.on_lag(5, at(100));
    o.on_window(window("oiw-1", vec![cand("AAA:1", "AAA")], 3.5, at(102)));
    o.on_finish(at(103));
    let lines = sink.lines();
    let c = &candidates(&lines)[0];
    assert_eq!(c["eligibility"]["eligible"], false);
    assert!(reasons(c).contains(&"window_source_lag".to_string()));
    let cert = certify(&lines).expect("the capture itself is intact");
    assert_eq!(cert.eligible, 0, "no candidate in a lag-invalid window is eligible");
    assert_eq!(cert.invalid_windows, 1);
    assert_eq!(cert.invalid_windows_by_reason.get("source_lag"), Some(&1));
}

/// L1 `hidden_loss...`: `mapping_unambiguous == false ⇒ Invalid::Mapping`
/// for the **whole window**. L2.2 replay `l1_4b` kept BBB eligible.
#[test]
fn l1_4b_ambiguous_mapping_invalidates_the_whole_window() {
    let sink = Shared::default();
    let t = Tmp::new("amb");
    let mut o = observer(&sink, t.path());
    o.on_receive(&confirm("AAA", at(100), 3.5), at(100));
    o.on_receive(&confirm("BBB", at(100), 3.5), at(100));
    o.on_window(window(
        "oiw-1",
        vec![cand("AAA:1", "AAA"), cand("AAA:2", "AAA"), cand("BBB:1", "BBB")],
        3.5,
        at(102),
    ));
    o.on_finish(at(103));
    let lines = sink.lines();
    for c in candidates(&lines) {
        assert_eq!(c["eligibility"]["eligible"], false, "{c}");
        assert!(reasons(&c).contains(&"window_mapping_ambiguous".to_string()));
    }
    let cert = certify(&lines).expect("certificate");
    assert_eq!(cert.eligible, 0, "BBB is unambiguous but its window is not");
    assert_eq!(cert.invalid_windows_by_reason.get("mapping_ambiguous"), Some(&1));
}

/// L1 `duplicates_and_run_mismatch_fail`: duplicate row.
#[test]
fn l1_5a_duplicate_row_refuses() {
    let mut lines = clean_lines();
    let idx = position(&lines, "candidate");
    let dup = lines[idx].clone();
    lines.insert(idx, dup);
    for l in lines.iter_mut() {
        if l.contains("\"kind\":\"window_close\"") {
            let mut v: serde_json::Value = serde_json::from_str(l).unwrap();
            v["entryCount"] = serde_json::json!(2);
            v["openSetSize"] = serde_json::json!(2);
            *l = v.to_string();
        }
    }
    patch_file_close(&mut lines);
    assert!(certify(&lines).unwrap_err().contains("repeats opportunity"));
}

/// L1 `duplicates_and_run_mismatch_fail`: run mismatch.
#[test]
fn l1_5b_run_mismatch_refuses() {
    let mut lines = clean_lines();
    let idx = position(&lines, "receipt");
    let mut v: serde_json::Value = serde_json::from_str(&lines[idx]).unwrap();
    v["runId"] = serde_json::json!("other-run");
    lines[idx] = v.to_string();
    assert!(certify(&lines).unwrap_err().contains("several runs"));
}

/// L1 `checked_sequence...`: `next()` at `u64::MAX` is `None`; clause 1.
/// L2.2 replay `l1_6a` emitted `u64::MAX` twice.
#[test]
fn l1_6a_sequence_overflow_refuses_without_a_duplicate() {
    let sink = Shared::default();
    let t = Tmp::new("seq");
    let mut o = observer(&sink, t.path());
    o.sequence = u64::MAX - 1;
    o.on_receive(&ign("AAA", at(100), 3.5, IgnitionEventKind::CandidateOpened), at(100));
    assert_eq!(o.stopped(), None, "u64::MAX itself is a valid identity");
    o.on_receive(&ign("AAA", at(101), 3.6, IgnitionEventKind::CandidateOpened), at(101));
    assert_eq!(o.stopped(), Some(StopReason::SequenceExhausted));
    o.on_receive(&ign("AAA", at(102), 3.7, IgnitionEventKind::CandidateOpened), at(102));
    o.on_finish(at(103));
    let lines = sink.lines();
    let seqs: Vec<u64> = values(&lines)
        .iter()
        .filter(|v| v["kind"] == "receipt")
        .map(|v| v["sequence"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, vec![u64::MAX], "no duplicate receive identity is ever emitted");
    assert_eq!(values(&lines).iter().filter(|v| v["kind"] == "stopped").count(), 1);
    assert!(!verdict(&lines).is_pass());
}

/// L1 `checked_sequence...`: exclusive allocation.
#[test]
fn l1_6b_exclusive_allocation() {
    let t = Tmp::new("alloc");
    let a = ObserverRun::allocate(t.path(), "same", at(0), 1).unwrap();
    let b = ObserverRun::allocate(t.path(), "same", at(0), 1).unwrap();
    assert_ne!(a.id(), b.id());
}

/// L1 `allocate_run(.., "../bad", ..).is_err()`; RECONCILIATION §3.
/// L2.2 replay `l1_6c` sanitized it and conflated `a.b` with `a_b`.
#[test]
fn l1_6c_invalid_namespaces_refuse_and_cannot_collapse() {
    let t = Tmp::new("ns");
    for bad in ["../bad", "a.b", "a_b", ""] {
        assert!(
            matches!(
                ObserverRun::allocate(t.path(), bad, at(0), 1),
                Err(RunAllocationError::InvalidNamespace { .. })
            ),
            "{bad:?} must be refused"
        );
    }
    // Two distinct valid namespaces stay distinct.
    let x = ObserverRun::allocate(t.path(), "a-b", at(0), 1).unwrap();
    let y = ObserverRun::allocate(t.path(), "ab", at(0), 1).unwrap();
    assert!(x.id().starts_with("a-b-") && y.id().starts_with("ab-"));
}

/// L1 `ages`: incomplete provenance yields no age.
#[test]
fn l1_7a_incomplete_provenance_is_not_an_age() {
    let sink = Shared::default();
    let t = Tmp::new("prov");
    let mut o = observer(&sink, t.path());
    o.on_receive(&confirm("AAA", at(100), 3.5), at(100));
    o.on_window(window("oiw-1", vec![cand("AAA:1", "AAA")], 9.99, at(102)));
    o.on_finish(at(103));
    let c = &candidates(&sink.lines())[0];
    assert!(c["marketAgeNanos"].is_null() && c["receiptAgeNanos"].is_null());
    assert!(reasons(c).contains(&"unknown_price_provenance".to_string()));
}

/// One confirmed, scored candidate with explicit market, receipt and anchor
/// instants. `received_mono_offset` and `anchor_mono_offset` are monotonic
/// offsets from a common base; the wall times are independent of them, which
/// is what lets a clock step be expressed.
fn one_candidate(
    market_ms: i64,
    received_wall_ms: i64,
    anchor_wall_ms: i64,
    received_mono_offset: Duration,
    anchor_mono_offset: Duration,
) -> serde_json::Value {
    let sink = Shared::default();
    let t = Tmp::new("age");
    let mut o = observer(&sink, t.path());
    let base = Instant::now();
    o.on_receive_mono(&confirm("AAA", at_ms(market_ms), 3.5), at_ms(received_wall_ms), base + received_mono_offset);
    let mut w = window("oiw-1", vec![cand("AAA:1", "AAA")], 3.5, at_ms(anchor_wall_ms));
    w.processing_started_mono = Some(base + anchor_mono_offset);
    w.rank_completed_mono = Some(base + anchor_mono_offset);
    o.on_window(w);
    o.on_finish(at_ms(anchor_wall_ms + 1));
    candidates(&sink.lines()).remove(0)
}

/// Same wall and monotonic spacing: the common case.
fn aged(market_ms: i64, anchor_ms: i64) -> serde_json::Value {
    one_candidate(
        market_ms,
        market_ms,
        anchor_ms,
        Duration::ZERO,
        Duration::from_millis((anchor_ms - market_ms).max(0) as u64),
    )
}

/// Clause 4: 30.9 s is not `<= 30 s`. L2.2 replay `l1_7b` admitted it.
#[test]
fn l1_7b_thirty_point_nine_seconds_is_stale() {
    let c = aged(100_000, 130_900);
    assert_eq!(c["marketAgeNanos"], 30_900_000_000i64);
    assert_eq!(c["eligibility"]["eligible"], false);
    assert!(reasons(&c).contains(&"market_age_exceeded".to_string()));
    assert!(reasons(&c).contains(&"receipt_age_exceeded".to_string()));
}

/// L1 `ages`: `market_age < 0 ⇒ None`; clause 4. L2.2 replay `l1_7c` read
/// -0.5 s as 0 and admitted it.
#[test]
fn l1_7c_subsecond_negative_market_age_is_invalid() {
    let c = one_candidate(102_500, 101_900, 102_000, Duration::ZERO, Duration::from_millis(100));
    assert_eq!(c["marketAgeNanos"], -500_000_000i64, "measured, not clamped");
    assert!(reasons(&c).contains(&"negative_market_age".to_string()));
    assert_eq!(c["eligibility"]["eligible"], false);
}

/// L1 `CaptureBudget` + `omitted_rotation_file...`: an omitted rotated file
/// refuses.
#[test]
fn l1_8a_omitted_rotated_file_refuses() {
    let root = Tmp::new("rot");
    let run = ObserverRun::allocate(root.path(), "contract", at(0), 7).unwrap();
    let sink = RotatingSink::create(run.dir(), run.id(), 1, 64, 1 << 20).unwrap();
    let mut o = Observer::start(&run, "contract", 7, at(0), Box::new(sink)).unwrap();
    for i in 0..6 {
        o.on_receive(&ign("AAA", at(90 + i), 3.5, IgnitionEventKind::CandidateOpened), at(90 + i));
    }
    o.on_receive(&confirm("AAA", at(100), 3.5), at(100));
    o.on_window(window("oiw-1", vec![cand("AAA:1", "AAA")], 3.5, at(102)));
    o.on_finish(at(200));
    let mut files: Vec<String> = std::fs::read_dir(run.dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    files.sort_by_key(|n| rotation_index(n));
    assert!(files.len() >= 3, "{files:?}");
    assert_eq!(assess(run.dir()).label(), "PASS", "the complete chain certifies");
    std::fs::remove_file(run.dir().join(&files[1])).unwrap();
    assert_eq!(assess(run.dir()).label(), "FAIL");
}

/// L1 `CaptureBudget::admit`: exceeding the cap latches incomplete, and a
/// latched capture never closes successfully.
#[test]
fn l1_8b_capture_budget_exceedance_cannot_certify() {
    let sink = Shared::default();
    let t = Tmp::new("budget");
    let mut o = observer(&sink, t.path()).with_capture_max_bytes(2_000);
    for i in 0..100 {
        o.on_receive(&confirm("AAA", at(100 + i), 3.5), at(100 + i));
    }
    assert_eq!(o.stopped(), Some(StopReason::CaptureBudgetExceeded));
    o.on_window(window("oiw-1", vec![cand("AAA:1", "AAA")], 3.5, at(300)));
    o.on_finish(at(301));
    let lines = sink.lines();
    let v = values(&lines);
    let stop = v.iter().position(|r| r["kind"] == "stopped").expect("stop recorded");
    assert!(
        v[stop + 1..].iter().all(|r| r["kind"] == "run_end" || r["kind"] == "file_close"),
        "nothing but terminal records after the stop"
    );
    assert_eq!(v.iter().rev().find(|r| r["kind"] == "run_end").unwrap()["stopped"], "capture_budget_exceeded");
    let err = certify(&lines).unwrap_err();
    assert!(err.contains("observation stopped early"), "{err}");
}

/// A `RecordWriter` over a real file that fails one chosen stage of the close
/// protocol -- L1 `FailingFlush`, generalised to every stage.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Fault {
    DataFsync,
    TerminalWrite,
    TerminalFsync,
    Finish,
    TerminalFsyncAndRetract,
}

struct FaultyFile {
    file: std::fs::File,
    path: PathBuf,
    fault: Fault,
    len: u64,
    data_len: Option<u64>,
    syncs: u32,
}

impl RecordWriter for FaultyFile {
    fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        if self.fault == Fault::TerminalWrite && self.data_len.is_some() {
            // Tear the terminal record half way.
            let _ = self.file.write_all(&line.as_bytes()[..line.len() / 2]);
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "injected terminal write failure"));
        }
        self.file.write_all(line.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.len += line.len() as u64 + 1;
        Ok(())
    }
    fn sync(&mut self) -> std::io::Result<()> {
        self.syncs += 1;
        let fail = match self.fault {
            Fault::DataFsync => self.syncs == 1,
            Fault::TerminalFsync | Fault::TerminalFsyncAndRetract => self.syncs == 2,
            _ => false,
        };
        if self.syncs == 1 {
            self.data_len = Some(self.len);
        }
        if fail {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "injected fsync failure"));
        }
        self.file.sync_all()
    }
    fn finish(&mut self) -> std::io::Result<()> {
        if self.fault == Fault::Finish {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "injected close failure"));
        }
        Ok(())
    }
    fn retract_terminal(&mut self, _terminal_bytes: u64) -> std::io::Result<()> {
        if self.fault == Fault::TerminalFsyncAndRetract {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "injected retract failure"));
        }
        self.file.set_len(self.data_len.unwrap_or(0))?;
        self.file.sync_all()
    }
    fn mark_close_failed(&mut self) -> std::io::Result<()> {
        std::fs::write(close_failed_marker(&self.path), b"terminal record not durable\n")
    }
}

fn capture_with_fault(fault: Fault) -> (Tmp, PathBuf, u64) {
    let root = Tmp::new("fault");
    let run = ObserverRun::allocate(root.path(), "contract", at(0), 7).unwrap();
    let path = run.dir().join(RUN_FILE_NAME);
    let file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).unwrap();
    let writer = FaultyFile { file, path, fault, len: 0, data_len: None, syncs: 0 };
    let sink = AsyncSink::new(RUN_FILE_NAME, Box::new(writer));
    let mut o = Observer::start(&run, "contract", 7, at(0), Box::new(sink)).unwrap();
    o.on_receive(&confirm("AAA", at(100), 3.5), at(100));
    o.on_window(window("oiw-1", vec![cand("AAA:1", "AAA")], 3.5, at(102)));
    o.on_finish(at(103));
    let failed = o.failed_writes();
    (root, run.dir().to_path_buf(), failed)
}

/// L1 `injected_flush_failure_cannot_close_capture`; clause 6. L2.2 replay
/// `l1_9` certified PASS after a failed fsync.
#[test]
fn l1_9_injected_fsync_failure_cannot_certify() {
    let (_root, dir, failed) = capture_with_fault(Fault::TerminalFsync);
    assert_eq!(failed, 1, "the observer knows the close failed");
    let v = assess(&dir);
    assert!(!v.is_pass(), "and the capture on disk agrees: {}", v.label());
}

// ===========================================================================
// Part 2 -- restoration boundary tests
// ===========================================================================

/// Clause 6 close order, every stage: a terminal record never attests to data
/// whose durability failed, and no failure anywhere in the close can PASS.
#[test]
fn contract_every_close_stage_failure_prevents_pass() {
    for (fault, expected) in [
        (Fault::DataFsync, "INDETERMINATE"),
        (Fault::TerminalWrite, "INDETERMINATE"),
        (Fault::TerminalFsync, "INDETERMINATE"),
        (Fault::Finish, "INDETERMINATE"),
        (Fault::TerminalFsyncAndRetract, "FAIL"),
    ] {
        let (_root, dir, failed) = capture_with_fault(fault);
        assert_eq!(failed, 1, "{fault:?}: close failure reported to the observer");
        let v = assess(&dir);
        assert_eq!(v.label(), expected, "{fault:?}");
        let body = std::fs::read_to_string(dir.join(RUN_FILE_NAME)).unwrap();
        if fault != Fault::TerminalFsyncAndRetract {
            assert!(!body.contains("\"kind\":\"file_close\""), "{fault:?}: no terminal record left behind");
        }
        if fault == Fault::DataFsync {
            assert!(body.contains("\"kind\":\"run_end\""), "data written, only its durability failed");
        }
    }
}

/// Control for the above: the same writer with no fault certifies, so the
/// failures are what the protocol refuses, not the fixture.
#[test]
fn contract_close_protocol_without_fault_certifies() {
    let root = Tmp::new("nofault");
    let run = ObserverRun::allocate(root.path(), "contract", at(0), 7).unwrap();
    let writer = FileRecordWriter::create(run.dir(), RUN_FILE_NAME).unwrap();
    let mut o = Observer::start(&run, "contract", 7, at(0), Box::new(AsyncSink::new(RUN_FILE_NAME, Box::new(writer)))).unwrap();
    o.on_receive(&confirm("AAA", at(100), 3.5), at(100));
    o.on_window(window("oiw-1", vec![cand("AAA:1", "AAA")], 3.5, at(102)));
    o.on_finish(at(103));
    assert_eq!(o.failed_writes(), 0);
    assert_eq!(assess(run.dir()).label(), "PASS");
}

/// Clause 4 exact boundary, market and receipt age together.
#[test]
fn contract_freshness_boundary_is_exact_to_the_millisecond() {
    assert_eq!(aged(100_000, 130_000)["eligibility"]["eligible"], true, "30,000 ms");
    let c = aged(100_000, 130_001);
    assert_eq!(c["eligibility"]["eligible"], false, "30,001 ms");
    assert!(reasons(&c).contains(&"market_age_exceeded".to_string()));
    assert!(reasons(&c).contains(&"receipt_age_exceeded".to_string()));
    assert_eq!(aged(100_000, 100_000)["eligibility"]["eligible"], true, "zero is fresh");
    // Receipt age alone, market age within bound.
    let r = one_candidate(129_000, 100_000, 130_000, Duration::ZERO, Duration::from_millis(30_001));
    assert_eq!(reasons(&r), vec!["receipt_age_exceeded".to_string()]);
    // Market age negative by a single millisecond.
    let n = one_candidate(130_001, 130_000, 130_000, Duration::ZERO, Duration::ZERO);
    assert_eq!(n["marketAgeNanos"], -1_000_000i64);
    assert!(reasons(&n).contains(&"negative_market_age".to_string()));
}

/// Receipt age and ordering use the monotonic clock: a wall clock stepping
/// back 60 s between receipt and anchor changes neither.
#[test]
fn contract_receipt_age_survives_a_backward_wall_clock_step() {
    // Receipt wall 100 s; anchor wall 40 s (stepped back 60 s); 5 s elapsed
    // on the monotonic clock.
    let c = one_candidate(100_000, 100_000, 40_000, Duration::ZERO, Duration::from_secs(5));
    assert_eq!(c["receiptAgeNanos"], 5_000_000_000i64, "monotonic, not the stepped wall");
    assert!(!reasons(&c).contains(&"receipt_age_exceeded".to_string()));
    // The wall-domain market age is honestly negative and fails closed.
    assert!(reasons(&c).contains(&"negative_market_age".to_string()));
}

/// A forward wall step of two minutes does not age the receipt.
#[test]
fn contract_receipt_age_survives_a_forward_wall_clock_step() {
    let c = one_candidate(100_000, 100_000, 100_000 + 120_000, Duration::ZERO, Duration::from_secs(5));
    assert_eq!(c["receiptAgeNanos"], 5_000_000_000i64);
    assert!(!reasons(&c).contains(&"receipt_age_exceeded".to_string()));
}

/// L1 `price_receipt <= rank_start`, on the monotonic clock.
#[test]
fn contract_price_received_after_processing_start_is_ineligible() {
    let sink = Shared::default();
    let t = Tmp::new("order");
    let mut o = observer(&sink, t.path());
    let base = Instant::now();
    o.on_receive_mono(&confirm("AAA", at(100), 3.5), at(100), base + Duration::from_secs(10));
    let mut w = window("oiw-1", vec![cand("AAA:1", "AAA")], 3.5, at(102));
    w.processing_started_mono = Some(base + Duration::from_secs(5));
    w.rank_completed_mono = Some(base + Duration::from_secs(12));
    o.on_window(w);
    o.on_finish(at(103));
    let c = &candidates(&sink.lines())[0];
    assert!(reasons(c).contains(&"price_received_after_processing_start".to_string()));
}

/// Confirmation attribution does not use the consumer wall clock: a
/// confirmation whose *receipt* wall time reads before the lifecycle opened
/// (clock stepped back) still counts, because its market time is inside the
/// lifecycle and it was received by the watermark.
#[test]
fn contract_confirmation_ordering_ignores_a_stepped_wall_clock() {
    let sink = Shared::default();
    let t = Tmp::new("confirm-step");
    let mut o = observer(&sink, t.path());
    o.on_receive(&confirm("AAA", at(100), 3.5), at(10));
    o.on_window(window("oiw-1", vec![cand("AAA:1", "AAA")], 3.5, at(102)));
    o.on_finish(at(103));
    let c = &candidates(&sink.lines())[0];
    assert_eq!(c["confirmationReceipts"], 1);
    assert_eq!(c["eligibility"]["eligible"], true);
}

/// A confirmation received after the window's watermark is not counted for
/// it: L1 `confirmation_sequence <= watermark`.
#[test]
fn contract_confirmation_after_the_watermark_is_not_counted() {
    let sink = Shared::default();
    let t = Tmp::new("watermark");
    let mut o = observer(&sink, t.path());
    o.on_receive(&confirm("AAA", at(100), 3.5), at(100));
    o.on_window(window("oiw-1", vec![cand("AAA:1", "AAA")], 3.5, at(102)));
    o.on_receive(&confirm("AAA", at(103), 3.5), at(103));
    o.on_window(window("oiw-2", vec![cand("AAA:1", "AAA")], 3.5, at(104)));
    o.on_finish(at(105));
    let c = candidates(&sink.lines());
    assert_eq!(c[0]["confirmationReceipts"], 1, "window 1 predates the second confirmation");
    assert_eq!(c[1]["confirmationReceipts"], 2);
}

/// An out-of-order confirmation from before the lifecycle cannot be
/// attributed; the candidate is ineligible rather than guessed.
#[test]
fn contract_unattributable_out_of_order_confirmation_is_ineligible() {
    let sink = Shared::default();
    let t = Tmp::new("ooo");
    let mut o = observer(&sink, t.path());
    o.on_receive(&confirm("AAA", at(100), 3.5), at(100));
    o.on_receive(&confirm("AAA", at(80), 3.5), at(101)); // arrives late, market time before opening
    o.on_receive(&ign("AAA", at(101), 3.5, IgnitionEventKind::CandidateOpened), at(101));
    o.on_window(window("oiw-1", vec![cand("AAA:1", "AAA")], 3.5, at(102)));
    o.on_finish(at(103));
    let c = &candidates(&sink.lines())[0];
    assert!(reasons(c).contains(&"confirmation_ordering_ambiguous".to_string()));
}

/// Lag taints every lifecycle open across it for as long as it stays open;
/// a lifecycle first seen after the lag-absorbing window is clean.
#[test]
fn contract_lag_taints_lifecycles_open_across_it_until_they_close() {
    let sink = Shared::default();
    let t = Tmp::new("taint");
    let mut o = observer(&sink, t.path());
    o.on_receive(&confirm("AAA", at(100), 3.5), at(100));
    o.on_window(window("oiw-1", vec![cand("AAA:1", "AAA")], 3.5, at(101)));
    o.on_lag(3, at(102));
    o.on_window(window("oiw-2", vec![cand("AAA:1", "AAA")], 3.5, at(103)));
    o.on_window(window("oiw-3", vec![cand("AAA:1", "AAA")], 3.5, at(104)));
    o.on_receive(&confirm("BBB", at(105), 2.0), at(105));
    let mut w4 = window("oiw-4", vec![cand("BBB:1", "BBB")], 2.0, at(106));
    w4.open[0].opened_at = at(105);
    o.on_window(w4);
    o.on_finish(at(107));
    let lines = sink.lines();
    let closes: Vec<bool> = values(&lines)
        .iter()
        .filter(|v| v["kind"] == "window_close")
        .map(|v| v["sourceLagInvalid"].as_bool().unwrap())
        .collect();
    assert_eq!(closes, vec![false, true, true, false]);
    let cert = certify(&lines).expect("certificate");
    assert_eq!(cert.invalid_windows, 2);
    assert_eq!(cert.eligible, 2, "windows 1 and 4 only");
}

/// The certificate re-checks lag from the records rather than trusting the
/// observer: a lag followed by a window claiming to be clean refuses.
#[test]
fn contract_a_lag_the_window_does_not_reflect_refuses() {
    let sink = Shared::default();
    let t = Tmp::new("lag-tamper");
    let mut o = observer(&sink, t.path());
    o.on_receive(&confirm("AAA", at(100), 3.5), at(100));
    o.on_lag(1, at(100));
    o.on_window(window("oiw-1", vec![cand("AAA:1", "AAA")], 3.5, at(102)));
    o.on_finish(at(103));
    let mut lines = sink.lines();
    let idx = position(&lines, "window_close");
    let mut v: serde_json::Value = serde_json::from_str(&lines[idx]).unwrap();
    v["sourceLagInvalid"] = serde_json::json!(false);
    lines[idx] = v.to_string();
    assert!(certify(&lines).unwrap_err().contains("was not invalidated"));
}

/// Nothing in an invalid window may be eligible, even if the rows and the
/// declaration are altered consistently.
#[test]
fn contract_eligible_in_an_invalid_window_refuses() {
    let sink = Shared::default();
    let t = Tmp::new("inv-tamper");
    let mut o = observer(&sink, t.path());
    o.on_receive(&confirm("AAA", at(100), 3.5), at(100));
    o.on_receive(&confirm("BBB", at(100), 3.5), at(100));
    o.on_window(window(
        "oiw-1",
        vec![cand("AAA:1", "AAA"), cand("AAA:2", "AAA"), cand("BBB:1", "BBB")],
        3.5,
        at(102),
    ));
    o.on_finish(at(103));
    let mut lines = sink.lines();
    let mut fixed = String::new();
    for l in lines.iter_mut() {
        let mut v: serde_json::Value = serde_json::from_str(l).unwrap();
        if v["kind"] == "candidate" && v["opportunityId"] == "BBB:1" {
            let before = format!("BBB:1|ineligible:{}|", reasons(&v).join(","));
            v["eligibility"] = serde_json::json!({"eligible": true, "reasons": []});
            fixed = before;
            *l = v.to_string();
        }
    }
    for l in lines.iter_mut() {
        let mut v: serde_json::Value = serde_json::from_str(l).unwrap();
        if v["kind"] == "window_begin" {
            let exp: Vec<String> = v["expected"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| {
                    let s = e.as_str().unwrap().to_string();
                    if s.starts_with(&fixed) { s.replacen(&fixed, "BBB:1|eligible|", 1) } else { s }
                })
                .collect();
            v["expected"] = serde_json::json!(exp);
            *l = v.to_string();
        }
    }
    let err = certify(&lines).unwrap_err();
    assert!(err.contains("is invalid but BBB:1 is marked eligible"), "{err}");
}

/// Rotation + expected identity set + lag: a window's declaration and rows
/// straddle files and still reconcile exactly; the lag window is invalid;
/// deleting a middle file refuses.
#[test]
fn contract_rotation_identity_set_and_lag_combine() {
    let root = Tmp::new("combo");
    let run = ObserverRun::allocate(root.path(), "contract", at(0), 7).unwrap();
    let sink = RotatingSink::create(run.dir(), run.id(), 700, 4_096, 1 << 20).unwrap();
    let mut o = Observer::start(&run, "contract", 7, at(0), Box::new(sink)).unwrap();
    let syms = ["AAA", "BBB", "CCC", "DDD"];
    for (i, s) in syms.iter().enumerate() {
        o.on_receive(&confirm(s, at(100 + i as i64), 3.5), at(100 + i as i64));
    }
    let open: Vec<OpenCandidate> = syms.iter().map(|s| cand(&format!("{s}:1"), s)).collect();
    o.on_window(window("oiw-1", open.clone(), 3.5, at(110)));
    o.on_lag(2, at(111));
    o.on_window(window("oiw-2", open, 3.5, at(112)));
    o.on_finish(at(113));
    let files = std::fs::read_dir(run.dir()).unwrap().count();
    assert!(files >= 3, "rotation must have split the run, got {files} files");
    let v = assess(run.dir());
    let CaptureVerdict::Pass(cert) = v else { panic!("expected PASS, got {}", v.label()) };
    assert_eq!(cert.windows, 2);
    assert_eq!(cert.eligible, 4, "window 1 only");
    assert_eq!(cert.invalid_windows, 1);
    let mut names: Vec<String> = std::fs::read_dir(run.dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    names.sort_by_key(|n| rotation_index(n));
    std::fs::remove_file(run.dir().join(&names[1])).unwrap();
    assert_eq!(assess(run.dir()).label(), "FAIL");
}

/// Rotation + budget: the budget spans every file, so rotating cannot walk
/// past it. Every file stays well under the budget, yet the capture stops.
#[test]
fn contract_capture_budget_spans_all_rotated_files() {
    let root = Tmp::new("combo-budget");
    let run = ObserverRun::allocate(root.path(), "contract", at(0), 7).unwrap();
    let sink = RotatingSink::create(run.dir(), run.id(), 1_000, 4_096, 1 << 20).unwrap();
    let mut o = Observer::start(&run, "contract", 7, at(0), Box::new(sink))
        .unwrap()
        .with_capture_max_bytes(5_000);
    for i in 0..200 {
        o.on_receive(&confirm("AAA", at(100 + i), 3.5), at(100 + i));
    }
    assert_eq!(o.stopped(), Some(StopReason::CaptureBudgetExceeded));
    o.on_finish(at(400));
    let sizes: Vec<u64> = std::fs::read_dir(run.dir())
        .unwrap()
        .map(|e| e.unwrap().metadata().unwrap().len())
        .collect();
    assert!(sizes.len() >= 4, "{sizes:?}");
    assert!(sizes.iter().all(|s| *s < 5_000), "no single file reached the budget: {sizes:?}");
    assert!(!assess(run.dir()).is_pass());
}

/// Rotation + expected set: substituting a row in a *rotated-away* file is
/// still refused.
#[test]
fn contract_substitution_across_a_rotation_boundary_refuses() {
    let root = Tmp::new("combo-subst");
    let run = ObserverRun::allocate(root.path(), "contract", at(0), 7).unwrap();
    let sink = RotatingSink::create(run.dir(), run.id(), 600, 4_096, 1 << 20).unwrap();
    let mut o = Observer::start(&run, "contract", 7, at(0), Box::new(sink)).unwrap();
    for (i, s) in ["AAA", "BBB", "CCC"].iter().enumerate() {
        o.on_receive(&confirm(s, at(100 + i as i64), 3.5), at(100 + i as i64));
    }
    let open: Vec<OpenCandidate> =
        ["AAA", "BBB", "CCC"].iter().map(|s| cand(&format!("{s}:1"), s)).collect();
    o.on_window(window("oiw-1", open, 3.5, at(110)));
    o.on_finish(at(111));
    assert_eq!(assess(run.dir()).label(), "PASS");
    // Replace BBB:1's row with a fabricated XXX:1 row, wherever it landed.
    for e in std::fs::read_dir(run.dir()).unwrap() {
        let p = e.unwrap().path();
        let body = std::fs::read_to_string(&p).unwrap();
        if body.contains("\"opportunityId\":\"BBB:1\"") {
            std::fs::write(&p, body.replace("\"opportunityId\":\"BBB:1\"", "\"opportunityId\":\"XXX:1\"")).unwrap();
        }
    }
    let v = assess(run.dir());
    assert_eq!(v.label(), "FAIL");
}

/// The async sink's durable close under a stopped capture: the stop, the
/// terminal records and the file close all land, and the result refuses.
#[test]
fn contract_stopped_capture_on_disk_refuses() {
    let root = Tmp::new("stop-disk");
    let run = ObserverRun::allocate(root.path(), "contract", at(0), 7).unwrap();
    let writer = FileRecordWriter::create(run.dir(), RUN_FILE_NAME).unwrap();
    let mut o = Observer::start(&run, "contract", 7, at(0), Box::new(AsyncSink::new(RUN_FILE_NAME, Box::new(writer))))
        .unwrap()
        .with_capture_max_bytes(1_500);
    for i in 0..50 {
        o.on_receive(&confirm("AAA", at(100 + i), 3.5), at(100 + i));
    }
    o.on_finish(at(200));
    assert_eq!(o.failed_writes(), 0, "the capture closed cleanly");
    let v = assess(run.dir());
    assert_eq!(v.label(), "FAIL", "but it is incomplete");
}
