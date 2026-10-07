//! Step 4B-main.3: session-bounded OI reconciliation, end to end.
//!
//! Every test drives the REAL `ShadowDriver` and `ShadowRecorder` (the
//! production writer thread, real files in a temp directory) with synthetic
//! scan events and explicit clocks, then certifies sessions with the v2
//! extractor. No real capture, trade, eligibility or outcome data is read.

use std::path::Path;
use std::time::Duration as StdDuration;

use backtest_metrics::opportunity::OiConfig;
use chrono::{DateTime, Duration, TimeZone, Utc};
use market_data::{IgnitionEventKind, ScanEvent};

use super::analysis::*;
use super::observation_main_tests::{wx, Tmp};
use super::oi_extract::*;
use crate::opportunity_shadow::{ShadowDriver, ShadowRecorder};

const IMPL: &str = "0123456789abcdef0123456789abcdef01234567";
const A: &str = "2026-09-29";
const B: &str = "2026-09-30";
const C: &str = "2026-10-01";

/// New York wall time -> UTC. EDT (UTC-4) before 2026-11-01, EST after.
fn et(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
    let offset = if (mo, d) < (11, 1) { 4 } else { 5 };
    Utc.with_ymd_and_hms(y, mo, d, 0, 0, 0).unwrap() + Duration::seconds(i64::from((h + offset) * 3600 + mi * 60 + s))
}

fn day(s: &str) -> (i32, u32, u32) {
    let d = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
    use chrono::Datelike;
    (d.year(), d.month(), d.day())
}

fn momentum(symbol: &str, t: DateTime<Utc>, overall: f64) -> ScanEvent {
    ScanEvent::MomentumUpdate {
        symbol: symbol.into(),
        timestamp: t,
        volume_confirmation: 0.5,
        structure: 0.5,
        ma_slope: 0.2,
        wick_rejection: 0.3,
        overall,
        qualifies: overall >= 0.6,
    }
}

fn confirmed(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ScanEvent::IgnitionEvent { symbol: symbol.into(), timestamp: t, price, kind: IgnitionEventKind::FollowThroughConfirmed }
}

struct Harness {
    dir: Tmp,
    driver: ShadowDriver,
    config: OiConfig,
}

fn harness(tag: &str, config: OiConfig) -> Harness {
    let dir = Tmp::new(tag);
    let driver = driver_in(dir.path(), config.clone());
    Harness { dir, driver, config }
}

fn driver_in(dir: &Path, config: OiConfig) -> ShadowDriver {
    let recorder = ShadowRecorder::start(dir.to_path_buf()).expect("recorder");
    let mut d = ShadowDriver::new(config, Some(recorder));
    d.set_implementation_sha(Some(IMPL.into()));
    d
}

impl Harness {
    fn tick(&mut self, at: DateTime<Utc>) {
        self.driver.observe_tick(at);
    }
    /// Scan events for `symbols` from `h:mi` ET on `session`: enough to open
    /// opportunities and produce several ranking windows.
    fn trade(&mut self, session: &str, h: u32, mi: u32, symbols: &[&str]) {
        let (y, mo, d) = day(session);
        let t0 = et(y, mo, d, h, mi, 0);
        for (i, sym) in symbols.iter().enumerate() {
            let t = t0 + Duration::seconds(i as i64);
            let _ = self.driver.observe(&momentum(sym, t, 0.5 + 0.1 * i as f64), t);
            let _ = self.driver.observe(&confirmed(sym, t + Duration::seconds(5), 10.0), t + Duration::seconds(5));
        }
        for k in 1..=3 {
            for (i, sym) in symbols.iter().enumerate() {
                let t = t0 + Duration::seconds(40 * k + i as i64);
                let _ = self.driver.observe(&confirmed(sym, t, 10.0 + 0.2 * k as f64), t);
            }
        }
    }
    /// The Step-4 boundary after `session`: 20:10 ET that day.
    fn close(&mut self, session: &str) {
        let (y, mo, d) = day(session);
        self.tick(et(y, mo, d, 20, 10, 0));
    }
    fn flush(&self) {
        self.driver.flush_capture(StdDuration::from_secs(20));
    }
    fn expected(&self) -> String {
        self.config.fingerprint()
    }
    fn certify(&self, session: &str) -> SessionExtraction {
        self.flush();
        certify_dir(self.dir.path(), session, &self.expected())
    }
}

fn sources(dir: &Path) -> (Vec<(String, Vec<u8>)>, Vec<(String, Vec<u8>)>) {
    let (mut data, mut markers) = (Vec::new(), Vec::new());
    let mut names: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    for n in names {
        let bytes = std::fs::read(dir.join(&n)).unwrap();
        if n.starts_with("opportunity-intelligence-markers-") {
            markers.push((n, bytes));
        } else if n.starts_with("opportunity-intelligence-") {
            data.push((n, bytes));
        }
    }
    (data, markers)
}

fn certify_dir(dir: &Path, session: &str, fingerprint: &str) -> SessionExtraction {
    let (data, markers) = sources(dir);
    let d: Vec<(&str, &[u8])> = data.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    let m: Vec<(&str, &[u8])> = markers.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    extract(session, &d, &m, &ExpectedSource { implementation_sha: IMPL, config_fingerprint: fingerprint }).unwrap()
}

fn pass(x: &SessionExtraction) {
    assert!(x.report.completeness_established, "{:?}", x.report.reasons);
}

fn refuse(x: &SessionExtraction, needle: &str) {
    assert!(!x.report.completeness_established, "expected refusal ({needle})");
    assert!(x.report.reasons.iter().any(|r| r.contains(needle)), "{needle}: {:?}", x.report.reasons);
}

/// Before session A, then A, B and C with no restart.
fn three_sessions(h: &mut Harness, b_symbols: &[&str]) {
    h.tick(et(2026, 9, 28, 19, 0, 0)); // process up the evening before A
    h.close("2026-09-28");
    h.trade(A, 10, 0, &["AAA", "BBB"]);
    h.trade(A, 20, 5, &["AAA"]); // 00:05Z next UTC day: still session A
    h.close(A);
    h.trade(B, 11, 0, b_symbols);
    h.close(B);
    h.trade(C, 9, 45, &["CCC", "DDD"]);
    h.close(C);
}

// ===========================================================================
// Session semantics
// ===========================================================================

#[test]
fn the_step4_session_is_the_observation_run_not_the_utc_date() {
    let s = |t| super::step4_session_of(t).to_string();
    assert_eq!(s(et(2026, 9, 29, 4, 0, 0)), A);
    assert_eq!(s(et(2026, 9, 29, 20, 9, 59)), A);
    assert_eq!(s(et(2026, 9, 29, 20, 10, 0)), B, "the rollover opens the next session");
    assert_eq!(s(et(2026, 9, 29, 20, 5, 0)), A, "00:05Z is the next UTC date but the same session");
    assert_eq!(s(et(2026, 9, 28, 21, 0, 0)), A, "the evening before belongs to the next session");
    let (start, end) = super::step4_session_bounds(chrono::NaiveDate::from_ymd_opt(2026, 9, 29).unwrap());
    assert_eq!((start, end), (et(2026, 9, 28, 20, 10, 0), et(2026, 9, 29, 20, 10, 0)));
}

// ===========================================================================
// Clean sessions
// ===========================================================================

#[test]
fn a_clean_single_session_certifies_and_joins() {
    let mut h = harness("s3-single", OiConfig::default());
    h.tick(et(2026, 9, 28, 20, 0, 0));
    h.trade(A, 10, 0, &["AAA", "BBB", "CCC"]);
    h.close(A);
    let x = h.certify(A);
    pass(&x);
    let t = x.report.tally.clone().unwrap();
    assert!(t.attempted > 0 && t.attempted == t.written, "{t:?}");
    assert_eq!(x.report.rows_in_ranges, t.written);
    assert!(x.binding.session_marker.is_some() && x.binding.process_id.is_some());
    // The authenticated join accepts it and returns the engine's ranks.
    let ranks = join_all(&x).unwrap();
    assert_eq!(ranks.rows.len() as u64, x.binding.normalized_rows);
}

/// A session extract with one valid window per window id in the artifact,
/// anchored one second after that window's computation.
fn session_extract_for(x: &SessionExtraction) -> SessionExtract {
    let text = String::from_utf8(x.normalized.clone()).unwrap();
    let mut anchors: std::collections::BTreeMap<String, DateTime<Utc>> = Default::default();
    for l in text.lines().skip(1) {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        let at: DateTime<Utc> = v["computedAt"].as_str().unwrap().parse().unwrap();
        let e = anchors.entry(v["windowId"].as_str().unwrap().to_string()).or_insert(at);
        *e = (*e).max(at);
    }
    SessionExtract {
        run_id: "r".into(),
        first_window_id: None,
        first_window_open: Default::default(),
        first_receipt_market_at: None,
        left_censored_seen: Default::default(),
        windows: anchors.into_iter().map(|(id, at)| wx(&id, at + Duration::seconds(1), true, vec![])).collect(),
        status: StatusEvidence::default(),
        certificate: None,
    }
}

fn join_with(x: &SessionExtraction, normalized: &[u8], binding: &OiBinding, e: &SessionExtract) -> Result<OiRanks, OiJoinFailure> {
    let a = OiArtifact::parse(normalized);
    let exp = OiExpectation { normalized_sha256: &binding.normalized_sha256, implementation_sha: IMPL };
    authenticate_oi(&a, binding, &exp, e, &x.binding.session)
}

fn join_all(x: &SessionExtraction) -> Result<OiRanks, OiJoinFailure> {
    join_with(x, &x.normalized, &x.binding, &session_extract_for(x))
}

#[test]
fn a_three_session_process_certifies_each_session_independently() {
    let mut h = harness("s3-three", OiConfig::default());
    three_sessions(&mut h, &["EEE", "FFF", "GGG"]);
    let (a, b, c) = (h.certify(A), h.certify(B), h.certify(C));
    for x in [&a, &b, &c] {
        pass(x);
        assert!(join_all(x).is_ok());
    }
    // One process; three disjoint accountings.
    assert_eq!(a.binding.process_id, c.binding.process_id);
    // Session A spans two UTC-dated data files (the 20:05 ET rows).
    let files: std::collections::BTreeSet<_> = String::from_utf8(a.normalized.clone()).unwrap().lines().next().map(|l| {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        v["ranges"].as_array().unwrap().iter().map(|r| r["file"].as_str().unwrap().to_string()).collect()
    }).unwrap();
    assert_eq!(files.len(), 2, "{files:?}");
    // The 20:05 ET opportunity carries the next UTC sessionDate, yet is session A's.
    assert!(String::from_utf8(a.normalized.clone()).unwrap().contains(":2026-09-30:"));
}

#[test]
fn a_session_with_no_rows_certifies_as_empty() {
    let mut h = harness("s3-empty", OiConfig::default());
    h.tick(et(2026, 9, 28, 20, 0, 0));
    h.tick(et(2026, 9, 29, 12, 0, 0));
    h.close(A);
    let x = h.certify(A);
    pass(&x);
    assert_eq!((x.report.rows_in_ranges, x.binding.normalized_rows), (0, 0));
}

#[test]
fn an_early_close_day_certifies_on_the_same_session_boundary() {
    let mut h = harness("s3-early", OiConfig::default());
    h.tick(et(2026, 11, 26, 20, 0, 0));
    h.trade("2026-11-27", 10, 0, &["AAA", "BBB"]); // 13:00 ET close that day
    h.close("2026-11-27");
    pass(&h.certify("2026-11-27"));
}

#[test]
fn the_first_event_arriving_late_in_the_session_still_certifies() {
    let mut h = harness("s3-late-first", OiConfig::default());
    h.tick(et(2026, 9, 28, 20, 0, 0)); // process up before the session
    h.trade(A, 19, 50, &["AAA", "BBB"]); // first OI activity ten minutes before close
    h.close(A);
    pass(&h.certify(A));
}

// ===========================================================================
// Loss in the middle session only
// ===========================================================================

fn b_only(tag: &str, config: OiConfig, a_c: &[&str], b: &[&str], inject: impl Fn(&Harness)) -> (SessionExtraction, SessionExtraction, SessionExtraction) {
    let mut h = harness(tag, config);
    h.tick(et(2026, 9, 28, 20, 0, 0));
    h.trade(A, 10, 0, a_c);
    h.close(A);
    // A's rows must be on disk before a writer-thread fault is armed, or the
    // fault lands on a still-queued A row (which the extractor then, rightly,
    // refuses A for).
    h.flush();
    inject(&h);
    h.trade(B, 10, 0, b);
    h.close(B);
    h.trade(C, 10, 0, a_c);
    h.close(C);
    (h.certify(A), h.certify(B), h.certify(C))
}

#[test]
fn a_queue_drop_in_the_middle_session_refuses_only_that_session() {
    let (a, b, c) = b_only("s3-drop", OiConfig::default(), &["AAA", "BBB"], &["EEE", "FFF"], |h| {
        h.driver.recorder().unwrap().writer().inject_drops.store(1, std::sync::atomic::Ordering::Relaxed);
    });
    pass(&a);
    refuse(&b, "session loss");
    assert_eq!(b.report.tally.as_ref().unwrap().dropped, 1);
    assert_eq!(b.report.tally.as_ref().unwrap().loss_spans, 1);
    pass(&c);
}

#[test]
fn a_write_error_in_the_middle_session_refuses_only_that_session() {
    let (a, b, c) = b_only("s3-werr", OiConfig::default(), &["AAA", "BBB"], &["EEE", "FFF"], |h| {
        h.driver.recorder().unwrap().writer().inject_write_errors.store(1, std::sync::atomic::Ordering::Relaxed);
    });
    pass(&a);
    refuse(&b, "session loss");
    assert_eq!(b.report.tally.as_ref().unwrap().write_errors, 1);
    pass(&c);
}

#[test]
fn a_cohort_truncation_in_the_middle_session_refuses_only_that_session() {
    let config = OiConfig { max_rank_cohort: 2, ..OiConfig::default() };
    let (a, b, c) = b_only("s3-trunc", config, &["AAA", "BBB"], &["EEE", "FFF", "GGG", "HHH"], |_| {});
    pass(&a);
    refuse(&b, "cohortTruncations");
    pass(&c);
}

#[test]
fn a_capacity_eviction_in_the_middle_session_refuses_only_that_session() {
    let config = OiConfig { supported_symbol_universe: 2, bound_safety_num: 1, bound_safety_den: 1, ..OiConfig::default() };
    assert_eq!(config.max_open_opportunities(), 2);
    let (a, b, c) = b_only("s3-evict", config, &["AAA", "BBB"], &["EEE", "FFF", "GGG"], |_| {});
    pass(&a);
    refuse(&b, "capacityEvictions");
    pass(&c);
}

// ===========================================================================
// Crash, partial session, restart
// ===========================================================================

#[test]
fn a_process_that_dies_before_the_barrier_leaves_the_session_uncertifiable() {
    let mut h = harness("s3-crash-before", OiConfig::default());
    h.tick(et(2026, 9, 28, 20, 0, 0));
    h.trade(A, 10, 0, &["AAA"]);
    h.close(A);
    h.trade(B, 10, 0, &["BBB"]); // ... and the process dies: no B barrier
    pass(&h.certify(A));
    refuse(&h.certify(B), "no oi_session_finished");
}

#[test]
fn a_process_that_dies_after_a_durable_barrier_keeps_that_session_certifiable() {
    let mut h = harness("s3-crash-after", OiConfig::default());
    h.tick(et(2026, 9, 28, 20, 0, 0));
    h.trade(A, 10, 0, &["AAA", "BBB"]);
    h.close(A);
    h.trade(B, 10, 0, &["CCC"]);
    h.flush();
    // The crash tears the next session's last line mid-write.
    let data = h.dir.path().join("opportunity-intelligence-2026-09-30.ndjson");
    let mut bytes = std::fs::read(&data).unwrap();
    bytes.extend_from_slice(b"{\"schemaVersion\":3,\"torn");
    std::fs::write(&data, bytes).unwrap();
    pass(&h.certify(A));
    refuse(&h.certify(B), "no oi_session_finished");
}

#[test]
fn a_restart_inside_a_session_fails_closed_and_the_next_session_recovers() {
    let dir = Tmp::new("s3-restart");
    let config = OiConfig::default();
    let mut first = driver_in(dir.path(), config.clone());
    first.observe_tick(et(2026, 9, 28, 20, 0, 0));
    let mut h1 = Harness { dir: Tmp(dir.path().to_path_buf()), driver: first, config: config.clone() };
    h1.trade(A, 10, 0, &["AAA"]);
    h1.close(A);
    h1.trade(B, 9, 30, &["BBB"]);
    // Graceful restart at 12:00 ET inside B: the old process closes B as
    // `process_exit`, the new one opens it mid-session.
    let _ = h1.driver.finish(et(2026, 9, 30, 12, 0, 0));
    h1.flush();
    let second = driver_in(dir.path(), config.clone());
    let mut h2 = Harness { dir: Tmp(dir.path().to_path_buf()), driver: second, config: config.clone() };
    h2.tick(et(2026, 9, 30, 12, 1, 0));
    h2.trade(B, 13, 0, &["CCC"]);
    h2.close(B);
    h2.trade(C, 10, 0, &["DDD"]);
    h2.close(C);
    let b = h2.certify(B);
    refuse(&b, "oi_session_finished markers for one session");
    pass(&h2.certify(A));
    pass(&h2.certify(C));
    std::mem::forget(h1.dir); // shares `dir`; one cleanup is enough
    std::mem::forget(h2.dir);
    // Remove only the second process's marker: its own marker still refuses.
    let (data, markers) = sources(dir.path());
    let first_pid = first_process(&markers);
    let stripped: Vec<(String, Vec<u8>)> = markers
        .into_iter()
        .map(|(n, b)| {
            let kept: Vec<&str> = String::from_utf8_lossy(&b).lines().filter(|l| !(l.contains("oi_session_finished") && l.contains(&first_pid))).map(|_| "").collect();
            let _ = kept;
            let text: String = String::from_utf8(b).unwrap().lines().filter(|l| !(l.contains("oi_session_finished") && l.contains(&first_pid))).map(|l| format!("{l}\n")).collect();
            (n, text.into_bytes())
        })
        .collect();
    let d: Vec<(&str, &[u8])> = data.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    let m: Vec<(&str, &[u8])> = stripped.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    let x = extract(B, &d, &m, &ExpectedSource { implementation_sha: IMPL, config_fingerprint: &config.fingerprint() }).unwrap();
    refuse(&x, "did not cover the whole session");
}

fn first_process(markers: &[(String, Vec<u8>)]) -> String {
    for (_, b) in markers {
        for l in String::from_utf8_lossy(b).lines() {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            if v["kind"] == "writer_started" {
                return v["processId"].as_str().unwrap().to_string();
            }
        }
    }
    panic!("no writer_started");
}

// ===========================================================================
// Marker identity and tampering
// ===========================================================================

fn clean_ab(tag: &str) -> Harness {
    let mut h = harness(tag, OiConfig::default());
    h.tick(et(2026, 9, 28, 20, 0, 0));
    h.trade(A, 10, 0, &["AAA", "BBB"]);
    h.close(A);
    h.flush();
    h
}

/// Rewrites the session marker line for `A` in the marker files.
fn edit_marker(h: &Harness, f: impl Fn(&mut serde_json::Value), duplicate: bool) {
    for (n, b) in sources(h.dir.path()).1 {
        let mut out = String::new();
        for l in String::from_utf8(b).unwrap().lines() {
            let mut v: serde_json::Value = serde_json::from_str(l).unwrap();
            if v["kind"] == "oi_session_finished" && v["data"]["session"] == A {
                if duplicate {
                    out.push_str(l);
                    out.push('\n');
                }
                let original = v.clone();
                f(&mut v);
                // An unedited marker stays byte-identical (a true duplicate).
                if v == original {
                    out.push_str(l);
                } else {
                    out.push_str(&v.to_string());
                }
            } else {
                out.push_str(l);
            }
            out.push('\n');
        }
        std::fs::write(h.dir.path().join(n), out).unwrap();
    }
}

#[test]
fn duplicate_and_conflicting_session_markers_refuse() {
    let h = clean_ab("s3-dup");
    edit_marker(&h, |_| {}, true);
    refuse(&certify_dir(h.dir.path(), A, &h.expected()), "(duplicate)");
    let h = clean_ab("s3-conflict");
    edit_marker(&h, |v| v["data"]["windowsWithRows"] = serde_json::json!(999), true);
    refuse(&certify_dir(h.dir.path(), A, &h.expected()), "(conflicting)");
}

#[test]
fn wrong_process_implementation_config_or_schema_refuses() {
    let h = clean_ab("s3-pid");
    edit_marker(&h, |v| v["data"]["processId"] = serde_json::json!("1-2"), false);
    refuse(&certify_dir(h.dir.path(), A, &h.expected()), "process identity");

    let h = clean_ab("s3-impl");
    let (data, markers) = sources(h.dir.path());
    let d: Vec<(&str, &[u8])> = data.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    let m: Vec<(&str, &[u8])> = markers.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    let fp = h.expected();
    let x = extract(A, &d, &m, &ExpectedSource { implementation_sha: &"f".repeat(40), config_fingerprint: &fp }).unwrap();
    refuse(&x, "implementation SHA");
    let x = extract(A, &d, &m, &ExpectedSource { implementation_sha: IMPL, config_fingerprint: "oi-cfg-0000000000000000" }).unwrap();
    refuse(&x, "configuration fingerprint");

    let h = clean_ab("s3-schema");
    edit_marker(&h, |v| v["data"]["sourceSchema"] = serde_json::json!(2), false);
    refuse(&certify_dir(h.dir.path(), A, &h.expected()), "source schema");

    // A build without a recorded commit cannot certify.
    let mut h = harness("s3-nocommit", OiConfig::default());
    h.driver.set_implementation_sha(None);
    h.tick(et(2026, 9, 28, 20, 0, 0));
    h.trade(A, 10, 0, &["AAA"]);
    h.close(A);
    refuse(&h.certify(A), "implementation SHA");
}

#[test]
fn tampering_with_the_source_fails_the_range_hash() {
    let h = clean_ab("s3-tamper");
    let data = h.dir.path().join("opportunity-intelligence-2026-09-29.ndjson");
    let text = std::fs::read_to_string(&data).unwrap();
    let tampered = text.replacen("\"earlyQualityRank\":1", "\"earlyQualityRank\":7", 1);
    assert_ne!(text, tampered);
    std::fs::write(&data, tampered).unwrap();
    refuse(&certify_dir(h.dir.path(), A, &h.expected()), "does not hash");
}

#[test]
fn extraction_is_deterministic_and_reverifiable_from_its_sources() {
    let h = clean_ab("s3-determ");
    let (x, y) = (certify_dir(h.dir.path(), A, &h.expected()), certify_dir(h.dir.path(), A, &h.expected()));
    assert_eq!((x.normalized.clone(), x.binding.clone()), (y.normalized.clone(), y.binding.clone()));
    let (data, markers) = sources(h.dir.path());
    let d: Vec<(&str, &[u8])> = data.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    let m: Vec<(&str, &[u8])> = markers.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    let fp = h.expected();
    let exp = ExpectedSource { implementation_sha: IMPL, config_fingerprint: &fp };
    verify_extraction(&x.binding, &x.normalized, &d, &m, &exp).unwrap();
    let mut grown = data[0].1.clone();
    grown.extend_from_slice(b"\n");
    let err = verify_extraction(&x.binding, &x.normalized, &[(d[0].0, &grown)], &m, &exp).unwrap_err();
    assert!(err.contains("binding"), "{err}");
}

// ===========================================================================
// Process-level accounting still reconciles
// ===========================================================================

#[test]
fn process_close_reconciles_with_the_sum_of_its_sessions() {
    let mut h = harness("s3-process", OiConfig::default());
    three_sessions(&mut h, &["EEE", "FFF"]);
    h.trade("2026-10-02", 10, 0, &["ZZZ"]); // an open, partial session at exit
    let _ = h.driver.finish(et(2026, 10, 2, 11, 0, 0));
    h.flush();
    let (_, markers) = sources(h.dir.path());
    let (mut sessions_attempted, mut scores) = (0u64, None);
    let mut exits = 0;
    for (_, b) in &markers {
        for l in String::from_utf8_lossy(b).lines() {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            match v["kind"].as_str() {
                Some("oi_session_finished") => {
                    sessions_attempted += v["data"]["tally"]["attempted"].as_u64().unwrap();
                    if v["data"]["closedBy"] == "process_exit" {
                        exits += 1;
                    }
                }
                Some("capture_finished") => scores = v["data"]["scoresEmitted"].as_u64(),
                _ => {}
            }
        }
    }
    assert_eq!(exits, 1, "the open session closes as process_exit");
    assert_eq!(Some(sessions_attempted), scores, "process accounting == sum of session accounting");
    refuse(&certify_dir(h.dir.path(), "2026-10-02", &h.expected()), "process_exit");
}

// ===========================================================================
// The join, on v2 evidence
// ===========================================================================

/// Rewrites the rows of a genuine v2 artifact (as a faulty extractor might)
/// and re-binds it, so the join's own row checks are what is under test.
fn forged(x: &SessionExtraction, f: impl Fn(&mut Vec<serde_json::Value>)) -> (Vec<u8>, OiBinding) {
    let text = String::from_utf8(x.normalized.clone()).unwrap();
    let mut lines = text.lines();
    let header = lines.next().unwrap().to_string();
    let mut rows: Vec<serde_json::Value> = lines.map(|l| serde_json::from_str(l).unwrap()).collect();
    f(&mut rows);
    let mut out = header.into_bytes();
    out.push(b'\n');
    for r in &rows {
        out.extend(r.to_string().into_bytes());
        out.push(b'\n');
    }
    let mut binding = x.binding.clone();
    binding.normalized_sha256 = super::prereg::sha256_hex(&out);
    binding.normalized_rows = rows.len() as u64;
    (out, binding)
}

#[test]
fn the_v2_join_fails_closed_on_row_level_defects_and_foreign_processes() {
    let h = clean_ab("s3-join");
    let x = certify_dir(h.dir.path(), A, &h.expected());
    pass(&x);
    let e = session_extract_for(&x);
    let run = |f: &dyn Fn(&mut Vec<serde_json::Value>)| {
        let (bytes, binding) = forged(&x, f);
        join_with(&x, &bytes, &binding, &e)
    };
    let conflict = run(&|r| {
        let mut dup = r[0].clone();
        dup["earlyQualityRank"] = serde_json::json!(999);
        r.push(dup);
    });
    assert!(matches!(conflict, Err(OiJoinFailure::ConflictingRanks { .. })), "{conflict:?}");
    let nulled = run(&|r| {
        let mut dup = r[0].clone();
        dup["earlyQualityRank"] = if dup["earlyQualityRank"].is_null() { serde_json::json!(1) } else { serde_json::Value::Null };
        r.push(dup);
    });
    assert!(matches!(nulled, Err(OiJoinFailure::ConflictingRanks { .. })), "{nulled:?}");
    let future = run(&|r| r[0]["computedAt"] = serde_json::json!("2026-09-29T23:59:00Z"));
    assert!(matches!(future, Err(OiJoinFailure::FutureInformation { .. })), "{future:?}");
    let unknown = run(&|r| r[0]["windowId"] = serde_json::json!("oiw-9999"));
    assert!(matches!(unknown, Err(OiJoinFailure::UnknownWindow(_))), "{unknown:?}");
    let wrong_session = run(&|r| r[0]["session"] = serde_json::json!(B));
    assert!(matches!(wrong_session, Err(OiJoinFailure::SessionMismatch { .. })), "{wrong_session:?}");
    // A binding claiming another process does not match the header.
    let mut other = x.binding.clone();
    other.process_id = Some("999-1".into());
    assert!(matches!(join_with(&x, &x.normalized, &other, &e), Err(OiJoinFailure::Binding(_))));
    // An uncertifiable session never joins, whatever its rows.
    let mut h = harness("s3-join-refused", OiConfig::default());
    h.tick(et(2026, 9, 28, 20, 0, 0));
    h.trade(A, 10, 0, &["AAA"]);
    h.driver.recorder().unwrap().writer().inject_drops.store(1, std::sync::atomic::Ordering::Relaxed);
    h.trade(A, 11, 0, &["BBB"]);
    h.close(A);
    let lossy = h.certify(A);
    assert!(!lossy.report.completeness_established);
    assert!(join_all(&lossy).is_err());
}

// ===========================================================================
// The barrier never blocks the producer and is never dropped
// ===========================================================================

#[test]
fn a_barrier_meeting_a_full_queue_is_deferred_not_dropped_or_blocking() {
    use crate::research_writer::{Bounds, Naming, ResearchWriter};
    let dir = Tmp::new("s3-defer");
    let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
    let naming = Naming { dir: dir.path().to_path_buf(), stem: SOURCE_CAPTURE.into() };
    // Two slots, writer held at the gate. `writer_started` takes one.
    let w = ResearchWriter::start_inner(naming, Bounds { records: 2, bytes: u64::MAX / 2 }, Some(gate.clone())).unwrap();
    let s: std::sync::Arc<str> = A.into();
    let date = chrono::NaiveDate::from_ymd_opt(2026, 9, 29).unwrap();
    w.record_in_session(&serde_json::json!({"n": 1}), date, &s);
    let started = std::time::Instant::now();
    w.close_session(&s, serde_json::json!({ "session": A }), None); // queue full: deferred
    assert!(started.elapsed() < StdDuration::from_millis(500), "close_session must not block");
    gate.wait(); // writer drains
    std::thread::sleep(StdDuration::from_millis(200));
    w.send_pending_barriers(); // a later tick
    w.flush(StdDuration::from_secs(10));
    let markers: String = sources(dir.path()).1.iter().map(|(_, b)| String::from_utf8_lossy(b).into_owned()).collect();
    let m: serde_json::Value = markers
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|v| v["kind"] == "oi_session_finished")
        .expect("the deferred barrier was delivered");
    assert_eq!(m["data"]["tally"]["attempted"], 1);
    assert_eq!(m["data"]["tally"]["written"], 1);
    assert_eq!(m["data"]["barrier"]["synced"], true);
    assert_eq!(m["data"]["ranges"][0]["rows"], 1);
}
