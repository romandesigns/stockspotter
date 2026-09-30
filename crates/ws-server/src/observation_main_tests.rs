//! Step 4B-main: status evidence, selection, outcome evaluation, censoring and
//! inference -- synthetic fixtures only. No real trade, status, eligibility or
//! outcome data is read anywhere in this file.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, TimeZone, Utc};
use market_data::status_tap::{StatusMessage, StatusTapEvent, StreamState};
use market_data::{IgnitionEventKind, ScanEvent};

use super::analysis::*;
use super::*;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

pub(super) struct Tmp(pub(super) PathBuf);
impl Tmp {
    pub(super) fn new(tag: &str) -> Self {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "obs-main-{tag}-{}-{}-{n}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 2026-09-29 (a Tuesday, full session) at hh:mm:ss New York (EDT = UTC-4).
pub(super) fn ny(h: u32, m: u32, s: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 29, 4, 0, 0).unwrap() + Duration::seconds(i64::from(h * 3600 + m * 60 + s))
}

pub(super) fn status_msg(sym: &str, code: &str, at: DateTime<Utc>) -> StatusTapEvent {
    StatusTapEvent::Status(StatusMessage {
        symbol: sym.into(),
        status_code: code.into(),
        status_message: None,
        reason_code: Some(if code == "H" { "T12".into() } else { "".into() }),
        reason_message: None,
        tape: Some("C".into()),
        market_at: at,
        received_at: at,
    })
}

fn confirm(sym: &str, at: DateTime<Utc>, price: f64) -> ScanEvent {
    ScanEvent::IgnitionEvent { symbol: sym.into(), timestamp: at, price, kind: IgnitionEventKind::FollowThroughConfirmed }
}

pub(super) fn policy() -> ConditionPolicy {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ops/observation/trade-conditions.fixture.json");
    ConditionPolicy::bind(&std::fs::read(p).unwrap()).unwrap()
}

pub(super) fn status_policy() -> StatusPolicy {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ops/observation/trading-status-policy.fixture.json");
    StatusPolicy::bind(&std::fs::read(p).unwrap()).unwrap()
}

pub(super) const POLICY_SHA: &str = "4811c24baee8b97cdbafb4b5baf971c0db410760d2171fffc7458fc9b62a428b";
pub(super) const STATUS_SHA: &str = "d7977ab9939b548cd3b4dbec90b36089662c727d8eb1308205cba766442b8067";
pub(super) const SESSION: &str = "2026-09-29";

pub(super) fn full_status(from: DateTime<Utc>, to: DateTime<Utc>) -> StatusEvidence {
    StatusEvidence { loss_free: true, coverage: vec![(from, to)], events: BTreeMap::new() }
}

/// UTP (tape C) status events for one symbol.
pub(super) fn with_events(mut st: StatusEvidence, sym: &str, list: &[(DateTime<Utc>, &str)]) -> StatusEvidence {
    st.events.insert(
        sym.into(),
        list.iter().map(|(at, code)| StatusEvent { market_at: *at, tape: Some("C".into()), code: (*code).into(), reason: None }).collect(),
    );
    st
}

pub(super) fn trades(sym: &str, t0: DateTime<Utc>, list: &[(i64, f64, &[&str])]) -> TradeEvidence {
    TradeEvidence {
        symbol: sym.into(),
        covered_from: t0 - Duration::seconds(10),
        covered_to: t0 + Duration::seconds(400),
        response_complete: true,
        gaps: vec![],
        trades: list
            .iter()
            .map(|(ms, p, c)| Trade {
                exchange_at: t0 + Duration::milliseconds(*ms),
                price: to_micros(*p).unwrap(),
                tape: "C".into(),
                // An unannotated fixture print is a regular sale.
                conditions: if c.is_empty() { vec!["@".into()] } else { c.iter().map(|s| s.to_string()).collect() },
            })
            .collect(),
    }
}

pub(super) fn eval(t0: DateTime<Utc>, p0: f64, ev: Option<&TradeEvidence>, st: &StatusEvidence) -> Outcome {
    let (access, cp, sp) = (super::campaign::OutcomeAccess::synthetic_for_tests(&[SESSION]), policy(), status_policy());
    let ctx = EvaluationContext {
        access: &access,
        session: SESSION,
        conditions: &cp,
        statuses: &sp,
        expected_condition_sha: POLICY_SHA,
        expected_status_sha: STATUS_SHA,
    };
    evaluate(&ctx, t0, p0, "AAA", ev, st).unwrap()
}

// ===========================================================================
// §2-3 Status evidence stream
// ===========================================================================

pub(super) fn status_run(tag: &str) -> (Tmp, ObserverRun, Observer) {
    let t = Tmp::new(tag);
    let run = ObserverRun::allocate(t.path(), "main", ny(9, 0, 0), 1).unwrap();
    let writer = FileRecordWriter::create(run.dir(), RUN_FILE_NAME).unwrap();
    let o = Observer::start(&run, "main", 1, ny(9, 0, 0), Box::new(AsyncSink::new(RUN_FILE_NAME, Box::new(writer)))).unwrap();
    (t, run, o)
}

#[test]
fn status_messages_are_persisted_with_their_own_contiguous_sequence() {
    let (_t, run, mut o) = status_run("status");
    o.record_stream_state(Some(StreamState { connection: 1, full_market: true, since: ny(4, 0, 0) }), ny(9, 0, 0));
    o.on_status(&status_msg("AAA", "H", ny(10, 0, 0)));
    o.on_status(&status_msg("AAA", "T", ny(10, 5, 0)));
    o.on_receive(&confirm("AAA", ny(10, 30, 0), 5.0), ny(10, 30, 0));
    o.on_window(super::observation_preflight_support::window1("oiw-1", "AAA", ny(10, 20, 0), 5.0, ny(10, 30, 1)));
    o.set_status_tap_totals(3, 0);
    o.on_finish(ny(20, 10, 0));
    let (v, _) = stream::assess_streaming(run.dir());
    let CaptureVerdict::Pass(cert) = v else { panic!("expected PASS, got {}", v.label()) };
    assert_eq!(cert.status.recorded, 2);
    assert!(cert.status.tap_attached);
    let body = std::fs::read_to_string(run.dir().join(RUN_FILE_NAME)).unwrap();
    for field in ["\"statusSequence\":1", "\"statusSequence\":2", "\"statusCode\":\"H\"", "\"reasonCode\":\"T12\""] {
        assert!(body.contains(field), "{field}");
    }
    // A lost status row refuses authentication, in both readers.
    let lines: Vec<&str> = body.lines().filter(|l| !l.contains("\"statusSequence\":1")).collect();
    let mut tampered = lines.join("\n");
    tampered.push('\n');
    std::fs::write(run.dir().join(RUN_FILE_NAME), tampered).unwrap();
    let fix: Vec<String> = std::fs::read_to_string(run.dir().join(RUN_FILE_NAME)).unwrap().lines().map(|s| s.to_string()).collect();
    let mut fixed = fix.clone();
    let n = fixed.len() as u64 - 1;
    let mut last: serde_json::Value = serde_json::from_str(fixed.last().unwrap()).unwrap();
    last["recordsWritten"] = serde_json::json!(n);
    *fixed.last_mut().unwrap() = last.to_string();
    std::fs::write(run.dir().join(RUN_FILE_NAME), fixed.join("\n") + "\n").unwrap();
    let whole = assess(run.dir());
    let (streamed, _) = stream::assess_streaming(run.dir());
    assert_eq!(whole.label(), "FAIL");
    assert_eq!(streamed.label(), "FAIL");
    if let (CaptureVerdict::Fail(a), CaptureVerdict::Fail(b)) = (&whole, &streamed) {
        assert_eq!(a, b);
        assert!(a.contains("status sequence gap"), "{a}");
    }
}

pub(super) fn certified_extract(tag: &str, build: impl FnOnce(&mut Observer)) -> (Tmp, SessionExtract) {
    let (t, run, mut o) = status_run(tag);
    build(&mut o);
    // Every certifiable run has at least one ranking window.
    o.on_receive(&confirm("QQQQ", ny(15, 59, 0), 1.0), ny(15, 59, 0));
    o.on_window(super::observation_preflight_support::window1("oiw-last", "QQQQ", ny(15, 58, 0), 1.0, ny(15, 59, 1)));
    o.on_finish(ny(20, 10, 0));
    let e = extract_session(run.dir()).expect("certified");
    (t, e)
}

#[test]
fn status_evidence_is_complete_only_when_loss_free_and_covered() {
    let (_t, e) = certified_extract("cov", |o| {
        o.record_stream_state(Some(StreamState { connection: 1, full_market: true, since: ny(4, 0, 0) }), ny(9, 0, 0));
        o.on_status(&StatusTapEvent::StreamEnded { connection: 1, at: ny(11, 0, 0) });
        o.on_status(&StatusTapEvent::StreamStarted { connection: 2, full_market: true, at: ny(11, 1, 0) });
        o.set_status_tap_totals(2, 0);
    });
    assert!(e.status.loss_free);
    assert!(e.status.complete_over(ny(10, 0, 0), ny(10, 5, 0)));
    assert!(!e.status.complete_over(ny(10, 58, 0), ny(11, 3, 0)), "a reconnect gap breaks coverage");
    assert!(e.status.complete_over(ny(11, 2, 0), ny(11, 7, 0)));

    // Lossy tap: nothing is complete, whatever the coverage.
    let (_t2, lossy) = certified_extract("lossy", |o| {
        o.record_stream_state(Some(StreamState { connection: 1, full_market: true, since: ny(4, 0, 0) }), ny(9, 0, 0));
        o.set_status_tap_totals(10, 1);
    });
    assert!(!lossy.status.complete_over(ny(10, 0, 0), ny(10, 5, 0)));
    // Tap never attached: cannot claim zero drops.
    let (_t3, detached) = certified_extract("detached", |o| {
        o.record_stream_state(Some(StreamState { connection: 1, full_market: true, since: ny(4, 0, 0) }), ny(9, 0, 0));
    });
    assert!(!detached.status.complete_over(ny(10, 0, 0), ny(10, 5, 0)));
    // Symbol-scoped (not full-market) subscription is not coverage.
    let (_t4, partial) = certified_extract("partialsub", |o| {
        o.record_stream_state(Some(StreamState { connection: 1, full_market: false, since: ny(4, 0, 0) }), ny(9, 0, 0));
        o.set_status_tap_totals(0, 0);
    });
    assert!(!partial.status.complete_over(ny(10, 0, 0), ny(10, 5, 0)));
}

#[test]
fn halts_are_reconstructed_from_status_codes_not_from_trade_gaps() {
    let (_t, e) = certified_extract("halts", |o| {
        o.record_stream_state(Some(StreamState { connection: 1, full_market: true, since: ny(4, 0, 0) }), ny(9, 0, 0));
        o.on_status(&status_msg("AAA", "H", ny(10, 2, 0)));
        o.on_status(&status_msg("AAA", "H", ny(10, 2, 5))); // repeated halt status
        o.on_status(&status_msg("AAA", "T", ny(10, 7, 0)));
        o.on_status(&status_msg("BBB", "H", ny(15, 0, 0))); // never resumed
        o.set_status_tap_totals(4, 0);
    });
    let sp = status_policy();
    let halted = |s, e| Interruption { start: s, end: e, kind: InterruptionKind::Halted };
    assert_eq!(e.status.interruptions("AAA", &sp), vec![halted(ny(10, 2, 0), Some(ny(10, 7, 0)))]);
    assert_eq!(e.status.interruptions("BBB", &sp), vec![halted(ny(15, 0, 0), None)]);
    let first = |sym, a, b| e.status.first_interruption(sym, a, b, &sp).map(|i| i.start);
    assert_eq!(first("AAA", ny(10, 0, 0), ny(10, 5, 0)), Some(ny(10, 2, 0)));
    assert_eq!(first("AAA", ny(10, 7, 0), ny(10, 12, 0)), None, "ended exactly at start");
    assert_eq!(first("BBB", ny(15, 30, 0), ny(15, 35, 0)), Some(ny(15, 0, 0)));
}

#[test]
fn session_observer_records_run_start_state_and_attaches_tap_totals() {
    fn tap() -> ((u64, u64), Option<StreamState>) {
        ((40, 0), Some(StreamState { connection: 7, full_market: true, since: Utc.timestamp_opt(1_000, 0).unwrap() }))
    }
    let root = Tmp::new("sess-tap");
    std::fs::write(root.path().join(ROOT_MARKER), b"x").unwrap();
    let cfg = super::observation_preflight_support::config(root.path());
    let mut s = SessionObserver::with_schedule(Box::new(FileRunFactory { config: cfg }), ny(9, 0, 0), |_| ny(23, 0, 0))
        .unwrap()
        .with_tap_view(tap);
    let run = s.current_run_id().unwrap();
    s.on_status(&status_msg("AAA", "H", ny(10, 0, 0)));
    s.on_finish(ny(11, 0, 0));
    let body = std::fs::read_to_string(root.path().join(&run).join("observations-0.ndjson")).unwrap();
    assert!(body.contains("\"event\":\"run_start_state\""));
    let end: serde_json::Value = body.lines().map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()).find(|v| v["kind"] == "run_end").unwrap();
    assert_eq!(end["status"]["tapAttached"], true);
    assert_eq!(end["status"]["tapDropped"], 0);
    assert_eq!(end["status"]["recorded"], 1);
}

// ===========================================================================
// §4 Halt outcome semantics
// ===========================================================================

#[test]
fn a_reach_before_the_halt_is_success_and_anything_else_under_a_halt_is_censored() {
    let t0 = ny(10, 0, 0);
    let st = with_events(full_status(ny(9, 30, 0), ny(16, 0, 0)), "AAA", &[(t0 + Duration::seconds(120), "H"), (t0 + Duration::seconds(240), "T")]);
    // Reached at +60 s, halt at +120 s: success.
    let ev = trades("AAA", t0, &[(60_000, 10.20, &[])]);
    assert_eq!(eval(t0, 10.0, Some(&ev), &st), Outcome::Success { at: t0 + Duration::seconds(60) });
    // Reached only after the resume: censored, never success.
    let ev = trades("AAA", t0, &[(250_000, 10.50, &[])]);
    assert_eq!(eval(t0, 10.0, Some(&ev), &st), Outcome::Censored(CensorReason::HaltOverlap));
    // Not reached at all, halt overlaps: censored, not failure.
    let ev = trades("AAA", t0, &[(30_000, 10.01, &[])]);
    assert_eq!(eval(t0, 10.0, Some(&ev), &st), Outcome::Censored(CensorReason::HaltOverlap));
    // A halt that began before T0 and had not resumed overlaps the whole horizon.
    let st2 = with_events(full_status(ny(9, 30, 0), ny(16, 0, 0)), "AAA", &[(t0 - Duration::seconds(60), "H")]);
    assert_eq!(eval(t0, 10.0, Some(&trades("AAA", t0, &[])), &st2), Outcome::Censored(CensorReason::HaltOverlap));
    // A halt entirely before T0 does not overlap.
    let st3 = with_events(full_status(ny(9, 30, 0), ny(16, 0, 0)), "AAA", &[(t0 - Duration::seconds(600), "H"), (t0 - Duration::seconds(300), "T")]);
    assert_eq!(eval(t0, 10.0, Some(&trades("AAA", t0, &[])), &st3), Outcome::Failure);
}

#[test]
fn no_halt_observed_is_not_no_halt_unless_status_is_complete() {
    let t0 = ny(10, 0, 0);
    let ev = trades("AAA", t0, &[(30_000, 10.01, &[])]);
    let lossy = StatusEvidence { loss_free: false, ..full_status(ny(9, 30, 0), ny(16, 0, 0)) };
    assert_eq!(eval(t0, 10.0, Some(&ev), &lossy), Outcome::Censored(CensorReason::StatusUnknown));
    let uncovered = full_status(ny(9, 30, 0), t0 + Duration::seconds(100));
    assert_eq!(eval(t0, 10.0, Some(&ev), &uncovered), Outcome::Censored(CensorReason::StatusUnknown));
    let missing = StatusEvidence::default();
    assert_eq!(eval(t0, 10.0, Some(&ev), &missing), Outcome::Censored(CensorReason::StatusUnknown));
    assert_eq!(eval(t0, 10.0, Some(&ev), &full_status(ny(9, 30, 0), ny(16, 0, 0))), Outcome::Failure);
}

// ===========================================================================
// §10, §12, §13 Outcome evaluator
// ===========================================================================

#[test]
fn the_target_is_exact_and_inclusive_in_integer_micro_dollars() {
    assert!(reaches_target(to_micros(10.20).unwrap(), to_micros(10.00).unwrap(), 200));
    assert!(!reaches_target(to_micros(10.1999).unwrap(), to_micros(10.00).unwrap(), 200));
    assert!(reaches_target(to_micros(0.5100).unwrap(), to_micros(0.5000).unwrap(), 200));
    assert!(!reaches_target(to_micros(0.5099).unwrap(), to_micros(0.5000).unwrap(), 200));
    // 3.33 * 1.02 = 3.3966 exactly: equality reaches, a micro below does not.
    assert!(reaches_target(3_396_600, 3_330_000, 200));
    assert!(!reaches_target(3_396_599, 3_330_000, 200));
    assert_eq!(to_micros(0.0), None);
    assert_eq!(to_micros(f64::NAN), None);
}

#[test]
fn the_horizon_excludes_t0_and_includes_its_end() {
    let t0 = ny(10, 0, 0);
    let st = full_status(ny(9, 30, 0), ny(16, 0, 0));
    assert_eq!(eval(t0, 10.0, Some(&trades("AAA", t0, &[(0, 11.0, &[])])), &st), Outcome::Failure, "at T0: excluded");
    assert_eq!(
        eval(t0, 10.0, Some(&trades("AAA", t0, &[(300_000, 11.0, &[])])), &st),
        Outcome::Success { at: t0 + Duration::seconds(300) },
        "at T0+300 s: included"
    );
    assert_eq!(eval(t0, 10.0, Some(&trades("AAA", t0, &[(300_001, 11.0, &[])])), &st), Outcome::Failure, "after: excluded");
}

#[test]
fn every_evidence_defect_censors_and_a_complete_quiet_horizon_fails() {
    let t0 = ny(10, 0, 0);
    let st = full_status(ny(9, 30, 0), ny(16, 0, 0));
    assert_eq!(eval(t0, 10.0, None, &st), Outcome::Censored(CensorReason::MissingTradeEvidence));
    let mut ev = trades("AAA", t0, &[]);
    ev.response_complete = false;
    assert_eq!(eval(t0, 10.0, Some(&ev), &st), Outcome::Censored(CensorReason::IncompleteResponse));
    let mut ev = trades("AAA", t0, &[]);
    ev.covered_to = t0 + Duration::seconds(299);
    assert_eq!(eval(t0, 10.0, Some(&ev), &st), Outcome::Censored(CensorReason::PartialHorizon));
    let mut ev = trades("AAA", t0, &[]);
    ev.gaps.push((t0 + Duration::seconds(100), t0 + Duration::seconds(130)));
    assert_eq!(eval(t0, 10.0, Some(&ev), &st), Outcome::Censored(CensorReason::FeedGap));
    assert_eq!(eval(t0, f64::NAN, Some(&trades("AAA", t0, &[])), &st), Outcome::Censored(CensorReason::InvalidAnchorPrice));
    assert_eq!(eval(t0, 10.0, Some(&trades("AAA", t0, &[])), &st), Outcome::Failure, "complete, no trade");
}

#[test]
fn the_condition_policy_is_versioned_bound_and_applied() {
    let t0 = ny(10, 0, 0);
    let st = full_status(ny(9, 30, 0), ny(16, 0, 0));
    let p = policy();
    assert_eq!(p.sha256, POLICY_SHA, "the fixture's identity is pinned");
    // Excluded trade never reaches; an included one does.
    assert_eq!(eval(t0, 10.0, Some(&trades("AAA", t0, &[(10_000, 11.0, &["W"])])), &st), Outcome::Failure);
    assert!(matches!(eval(t0, 10.0, Some(&trades("AAA", t0, &[(10_000, 11.0, &["@", "F"])])), &st), Outcome::Success { .. }));
    // A code the table does not classify: censored, never guessed.
    assert_eq!(
        eval(t0, 10.0, Some(&trades("AAA", t0, &[(10_000, 11.0, &["Q9"])])), &st),
        Outcome::Censored(CensorReason::UnknownCondition)
    );
    // A policy the preregistration does not bind is refused.
    let (access, sp) = (super::campaign::OutcomeAccess::synthetic_for_tests(&[SESSION]), status_policy());
    let zeros = "0".repeat(64);
    let ctx = EvaluationContext {
        access: &access,
        session: SESSION,
        conditions: &p,
        statuses: &sp,
        expected_condition_sha: &zeros,
        expected_status_sha: STATUS_SHA,
    };
    let err = evaluate(&ctx, t0, 10.0, "AAA", Some(&trades("AAA", t0, &[])), &st).unwrap_err();
    assert!(matches!(err, EvaluationError::PolicyIdentity { .. }));
    // A code classified twice for one tape is refused at bind.
    assert!(ConditionPolicy::bind(
        br#"{"schema":"trade-condition-policy-v2","version":"x","tapes":{"C":{"include":["A"],"exclude":["A"],"censor":[]}}}"#
    )
    .is_err());
}

// ===========================================================================
// §11 RTH primary scope
// ===========================================================================

#[test]
fn primary_scope_requires_the_whole_horizon_inside_the_regular_session() {
    assert!(!in_primary_scope(ny(9, 29, 59)));
    assert!(in_primary_scope(ny(9, 30, 0)));
    assert!(in_primary_scope(ny(15, 54, 59) + Duration::milliseconds(999)));
    assert!(!in_primary_scope(ny(15, 55, 0)));
    // 2026-11-27, the day after Thanksgiving: the project calendar closes at 13:00.
    let d = |h: u32, m: u32, s: u32| Utc.with_ymd_and_hms(2026, 11, 27, h + 5, m, s).unwrap(); // EST
    let day = chrono::NaiveDate::from_ymd_opt(2026, 11, 27).unwrap();
    assert_eq!(market_data::trading_session::regular_session_close(day), Some(d(13, 0, 0)), "early close per the project calendar");
    assert!(in_primary_scope(d(12, 54, 59)));
    assert!(!in_primary_scope(d(12, 55, 0)));
    assert!(!in_primary_scope(d(14, 0, 0)));
    // Holiday and weekend: no session.
    assert!(!in_primary_scope(Utc.with_ymd_and_hms(2026, 11, 26, 15, 0, 0).unwrap()));
    assert!(!in_primary_scope(Utc.with_ymd_and_hms(2026, 10, 3, 15, 0, 0).unwrap()));
}

// ===========================================================================
// §5-9 Selection
// ===========================================================================

pub(super) fn cx(id: &str, sym: &str, seq: u64, opened: DateTime<Utc>) -> CandidateExtract {
    CandidateExtract {
        opportunity_id: id.into(),
        symbol: sym.into(),
        opened_at: opened,
        confirmation_sequence: Some(seq),
        anchor_price: Some(10.0),
    }
}

pub(super) fn wx(id: &str, at: DateTime<Utc>, valid: bool, eligible: Vec<CandidateExtract>) -> WindowExtract {
    WindowExtract { window_id: id.into(), anchor_at: at, valid, open_count: eligible.len() as u64, eligible }
}

pub(super) fn extract(windows: Vec<WindowExtract>) -> SessionExtract {
    SessionExtract {
        run_id: "r".into(),
        first_window_id: Some("w0".into()),
        first_window_open: HashSet::from(["OLD".to_string()]),
        first_receipt_market_at: Some(ny(9, 0, 0)),
        left_censored_seen: Default::default(),
        windows: std::iter::once(wx("w0", ny(9, 1, 0), true, vec![])).chain(windows).collect(),
        status: StatusEvidence::default(),
        certificate: None,
    }
}

pub(super) fn pool_of(n: usize, window: &str) -> Vec<CandidateExtract> {
    (0..n).map(|i| cx(&format!("{window}-L{i}"), &format!("S{i}"), 100 - i as u64, ny(9, 30, 0))).collect()
}

pub(super) fn all_ranked(e: &SessionExtract) -> OiRanks {
    let mut rows = HashMap::new();
    for w in &e.windows {
        for (i, c) in w.eligible.iter().enumerate() {
            rows.insert((w.window_id.clone(), c.opportunity_id.clone()), Some(i as u64 + 1));
        }
    }
    OiRanks { zero_loss_established: true, rows }
}

#[test]
fn pool_sizes_0_1_5_6_and_more_set_k_and_discrimination() {
    for (n, k, disc) in [(0usize, 0usize, false), (1, 1, false), (5, 5, false), (6, 5, true), (9, 5, true)] {
        let e = extract(vec![wx("w1", ny(10, 0, 0), true, pool_of(n, "w1"))]);
        let sel = select(&e, &all_ranked(&e), SelectionConfig::default());
        let w = sel.windows.iter().find(|w| w.window_id == "w1").unwrap();
        assert_eq!((w.pool.len(), w.k, w.discriminating), (n, k, disc), "pool {n}");
        if n <= 5 {
            let a: HashSet<_> = w.arm_a.iter().collect();
            let b: HashSet<_> = w.arm_b.iter().collect();
            assert_eq!(a, b, "pool {n}: both arms select the whole pool");
        }
    }
}

#[test]
fn arm_a_orders_by_confirmation_sequence_then_opportunity_id() {
    let mut pool = pool_of(7, "w1");
    pool[3].confirmation_sequence = Some(1);
    pool[4].confirmation_sequence = Some(1); // tie with 3: id breaks it
    let e = extract(vec![wx("w1", ny(10, 0, 0), true, pool)]);
    let sel = select(&e, &all_ranked(&e), SelectionConfig::default());
    let w = &sel.windows[1];
    assert_eq!(w.arm_a[0], "w1-L3");
    assert_eq!(w.arm_a[1], "w1-L4");
    assert_eq!(w.arm_a.len(), 5);
}

#[test]
fn arm_b_orders_by_rank_with_missing_rank_last_and_missing_rows_indeterminate() {
    let pool = pool_of(7, "w1");
    let e = extract(vec![wx("w1", ny(10, 0, 0), true, pool.clone())]);
    let mut oi = OiRanks { zero_loss_established: true, rows: HashMap::new() };
    let ranks = [Some(7), None, Some(2), Some(2), None, Some(1), Some(9)];
    for (c, r) in pool.iter().zip(ranks) {
        oi.rows.insert(("w1".into(), c.opportunity_id.clone()), r);
    }
    let sel = select(&e, &oi, SelectionConfig::default());
    assert_eq!(sel.windows[1].arm_b, vec!["w1-L5", "w1-L2", "w1-L3", "w1-L0", "w1-L6"]);
    // An absent OI row: arm B cannot be formed for that window.
    oi.rows.remove(&("w1".into(), "w1-L6".into()));
    let sel = select(&e, &oi, SelectionConfig::default());
    assert_eq!(sel.windows[1].oi_missing, vec!["w1-L6".to_string()]);
    assert!(sel.windows[1].arm_b.is_empty());
    // OI loss not excluded: the whole comparison is indeterminate.
    oi.zero_loss_established = false;
    assert!(select(&e, &oi, SelectionConfig::default()).comparison_indeterminate.is_some());
}

#[test]
fn retirement_is_final_there_is_no_refill_and_a_new_lifecycle_is_new() {
    let w1 = pool_of(6, "x");
    let mut w2 = w1.clone(); // same lifecycles, eligible again
    w2.push(cx("x-NEW-S0", "S0", 1, ny(10, 5, 0))); // same symbol as x-L0, new lifecycle
    let e = extract(vec![wx("w1", ny(10, 0, 0), true, w1), wx("w2", ny(10, 10, 0), true, w2)]);
    let sel = select(&e, &all_ranked(&e), SelectionConfig::default());
    let w2 = sel.windows.iter().find(|w| w.window_id == "w2").unwrap();
    assert_eq!(w2.pool.len(), 1, "only the new lifecycle; retired ones never re-enter");
    assert_eq!(w2.pool[0].opportunity_id, "x-NEW-S0");
}

#[test]
fn an_invalid_window_forms_no_pool_and_unknown_provenance_never_pools() {
    let mut pool = pool_of(6, "y");
    pool[0].anchor_price = None;
    let e = extract(vec![wx("w1", ny(10, 0, 0), false, pool_of(6, "z")), wx("w2", ny(10, 10, 0), true, pool)]);
    let sel = select(&e, &all_ranked(&e), SelectionConfig::default());
    assert_eq!(sel.invalid_windows, 1);
    assert!(sel.windows.iter().all(|w| w.window_id != "w1"));
    let w2 = sel.windows.iter().find(|w| w.window_id == "w2").unwrap();
    assert_eq!(w2.pool.len(), 5, "the unestablishable candidate is not a member");
    assert!(!w2.discriminating);
}

#[test]
fn left_censored_lifecycles_are_recorded_and_never_pooled() {
    let mut pool = pool_of(6, "c");
    pool.push(cx("OLD", "OLD", 1, ny(9, 30, 0))); // open in the first window
    pool.push(cx("EARLY", "E", 2, ny(8, 59, 0))); // opened before the first receipt's market time
    let e = extract(vec![wx("w1", ny(10, 0, 0), true, pool)]);
    let sel = select(&e, &all_ranked(&e), SelectionConfig::default());
    assert!(sel.left_censored.contains("OLD") && sel.left_censored.contains("EARLY"));
    assert!(sel.windows[1].pool.iter().all(|c| c.opportunity_id != "OLD" && c.opportunity_id != "EARLY"));
    // Nothing known about the first receipt: everything is left-censored.
    let mut e2 = extract(vec![wx("w1", ny(10, 0, 0), true, pool_of(6, "d"))]);
    e2.first_receipt_market_at = None;
    assert!(select(&e2, &all_ranked(&e2), SelectionConfig::default()).windows[1].pool.is_empty());
}

#[test]
fn a_session_start_with_already_open_opportunities_left_censors_them() {
    // Real observer path: AAA opens before the run, BBB after the first receipt.
    let (_t, e) = certified_extract("leftcensor", |o| {
        o.on_receive(&confirm("AAA", ny(10, 0, 0), 5.0), ny(10, 0, 0));
        o.on_receive(&confirm("BBB", ny(10, 0, 1), 5.0), ny(10, 0, 1));
        let w = |id: &str, t: DateTime<Utc>| {
            let mut w = super::observation_preflight_support::window1(id, "AAA", ny(8, 0, 0), 5.0, t);
            w.open.push(OpenCandidate { opportunity_id: "BBB:1".into(), symbol: "BBB".into(), opened_at: ny(10, 0, 1) });
            w.scored.insert("BBB:1".into());
            w.engine_prices.insert("BBB:1".into(), 5.0);
            w
        };
        o.on_window(w("oiw-1", ny(10, 0, 5)));
        o.on_window(w("oiw-2", ny(10, 0, 30)));
    });
    let sel = select(&e, &all_ranked(&e), SelectionConfig::default());
    assert!(sel.left_censored.contains("AAA:1"), "opened before the run: left-censored");
    assert!(sel.left_censored.contains("BBB:1"), "open in the first window: left-censored");
}

// ===========================================================================
// §13-15 Censoring and the session statistic
// ===========================================================================

fn disc_window(id: &str, at: DateTime<Utc>) -> PoolWindow {
    let pool = pool_of(6, id);
    PoolWindow {
        window_id: id.into(),
        anchor_at: at,
        arm_a: pool[0..5].iter().map(|c| c.opportunity_id.clone()).collect(),
        arm_b: pool[1..6].iter().map(|c| c.opportunity_id.clone()).collect(),
        k: 5,
        discriminating: true,
        arm_a_order: pool.iter().map(|c| c.opportunity_id.clone()).collect(),
        arm_b_order: pool[1..].iter().chain(&pool[..1]).map(|c| c.opportunity_id.clone()).collect(),
        oi_missing: vec![],
        retired: pool.iter().map(|c| c.opportunity_id.clone()).collect(),
        exclusions: vec![],
        pool,
    }
}

#[test]
fn paired_censoring_excludes_the_whole_window_from_both_arms() {
    let sel = Selection {
        windows: vec![
            disc_window("a", ny(10, 0, 0)),
            disc_window("b", ny(11, 0, 0)),
            disc_window("late", ny(15, 57, 0)), // out of scope
            PoolWindow { discriminating: false, ..disc_window("small", ny(12, 0, 0)) },
        ],
        ..Selection::default()
    };
    let stat = session_statistic(&sel, "r", |w, c| match (w.window_id.as_str(), c.opportunity_id.as_str()) {
        ("a", "a-L0") => Outcome::Success { at: w.anchor_at },  // arm A only
        ("a", "a-L5") => Outcome::Failure,                      // arm B only
        ("b", "b-L5") => Outcome::Censored(CensorReason::FeedGap), // arm B's pick censored
        _ => Outcome::Failure,
    });
    assert_eq!(stat.windows_used, 1);
    assert_eq!((stat.hits_a, stat.hits_b, stat.total_k), (1, 0, 5));
    assert_eq!(stat.censored_windows, 1);
    assert_eq!(stat.censor_log, vec![CensorEntry { window_id: "b".into(), opportunity_id: "b-L5".into(), reason: CensorReason::FeedGap }]);
    assert_eq!(stat.discriminating_in_scope, 2);
    assert_eq!(stat.out_of_scope_discriminating, 1);
    assert_eq!(stat.non_discriminating, 1);
    assert_eq!(stat.d, Some(0.2));
    assert_eq!(stat.censoring_rate(), Some(0.5));
    assert_eq!(stat.exceeds_censoring_limit(100), Some(true), "the proposed 10% is a parameter");
    assert_eq!(stat.exceeds_censoring_limit(600), Some(false));
}

#[test]
fn a_session_with_no_usable_window_has_no_statistic() {
    let stat = session_statistic(&Selection::default(), "r", |_, _| Outcome::Failure);
    assert_eq!(stat.d, None);
    assert_eq!(stat.censoring_rate(), None);
}

// ===========================================================================
// §16-18 Inference
// ===========================================================================

#[test]
fn the_exact_sign_flip_test_is_exact_and_deterministic() {
    let zeros = [0.0; 10];
    assert_eq!(sign_flip_exact(&zeros).p_value, 1.0);
    let pos = [0.2; 10];
    let r = sign_flip_exact(&pos);
    assert_eq!((r.extreme, r.total), (2, 1024));
    assert_eq!(sign_flip_exact(&[-0.2; 10]).p_value, 2.0 / 1024.0);
    let balanced = [0.2, -0.2, 0.2, -0.2, 0.2, -0.2, 0.2, -0.2, 0.2, -0.2];
    assert_eq!(sign_flip_exact(&balanced).p_value, 1.0);
    // Ties with zeros: zero sessions do not change any pattern's sum.
    let with_zeros = [0.2, 0.2, 0.2, 0.0, 0.0];
    let r = sign_flip_exact(&with_zeros);
    assert_eq!((r.extreme, r.total), (8, 32), "2 of 8 sign patterns of the non-zero part, times 4 for the zeros");
    // Deterministic.
    let mixed = [0.1, 0.3, -0.05, 0.2, 0.0, 0.15, -0.1, 0.25, 0.05, 0.12];
    assert_eq!(sign_flip_exact(&mixed), sign_flip_exact(&mixed));
    // Exact sums of ratios that differ only in float order are ties.
    let r = sign_flip_exact(&[0.1, 0.2, 0.3]);
    assert!(r.extreme >= 2);
}

#[test]
fn primary_inference_is_indeterminate_below_the_session_minimum() {
    assert_eq!(primary_inference(&[0.1; 9], 10), PrimaryInference::Indeterminate { sessions: 9, minimum: 10 });
    assert!(matches!(primary_inference(&[0.1; 10], 10), PrimaryInference::Computed(_)));
}

#[test]
fn the_descriptive_interval_is_labelled_and_uses_df_s_minus_1() {
    let d = descriptive_t(&[1.0, 2.0, 3.0]).unwrap();
    assert_eq!(d.label, "DESCRIPTIVE");
    assert_eq!(d.df, 2);
    assert_eq!(d.t_critical, 4.303);
    assert!((d.mean - 2.0).abs() < 1e-12 && (d.sd - 1.0).abs() < 1e-12);
    assert!((d.high - (2.0 + 4.303 / 3f64.sqrt())).abs() < 1e-9);
    assert!(descriptive_t(&[1.0]).is_none());
    assert_eq!(descriptive_t(&[0.0; 10]).unwrap().t_critical, 2.262);
}

#[test]
fn cluster_rows_cover_used_windows_only() {
    let sel = Selection { windows: vec![disc_window("a", ny(10, 0, 0)), disc_window("b", ny(11, 0, 0))], ..Selection::default() };
    let rows = cluster_rows(&sel, "s1", |w, c| {
        if w.window_id == "b" && c.opportunity_id == "b-L0" { Outcome::Censored(CensorReason::FeedGap) } else { Outcome::Failure }
    });
    assert_eq!(rows.len(), 10, "window a only, 5 per arm");
    assert!(rows.iter().all(|r| r.window_id == "a" && r.session == "s1"));
}

// ===========================================================================
// End to end on the real observer path (synthetic events and trades)
// ===========================================================================

#[test]
fn end_to_end_from_capture_to_session_statistic() {
    let n = 7;
    let (_t, e) = certified_extract("e2e", |o| {
        o.record_stream_state(Some(StreamState { connection: 1, full_market: true, since: ny(4, 0, 0) }), ny(9, 0, 0));
        o.on_receive(&confirm("ZZ", ny(9, 0, 1), 1.0), ny(9, 0, 1)); // first receipt
        o.on_window(super::observation_preflight_support::window1("oiw-1", "ZZ", ny(9, 0, 0), 1.0, ny(9, 0, 2)));
        for i in 0..n {
            o.on_receive(&confirm(&format!("S{i}"), ny(10, 0, i as u32), 10.0), ny(10, 0, i as u32));
        }
        let mut w = super::observation_preflight_support::window1("oiw-2", "S0", ny(9, 59, 0), 10.0, ny(10, 0, 10));
        for i in 1..n {
            w.open.push(OpenCandidate { opportunity_id: format!("S{i}:1"), symbol: format!("S{i}"), opened_at: ny(9, 59, 0) });
            w.scored.insert(format!("S{i}:1"));
            w.engine_prices.insert(format!("S{i}:1"), 10.0);
        }
        o.on_window(w);
        o.set_status_tap_totals(1, 0);
    });
    let mut oi = all_ranked(&e);
    for i in 0..n {
        oi.rows.insert(("oiw-2".into(), format!("S{i}:1")), Some((n - i) as u64)); // B prefers the latest
    }
    let sel = select(&e, &oi, SelectionConfig::default());
    let w = sel.windows.iter().find(|w| w.window_id == "oiw-2").unwrap();
    assert!(w.discriminating);
    assert_eq!(w.arm_a, vec!["S0:1", "S1:1", "S2:1", "S3:1", "S4:1"]);
    assert_eq!(w.arm_b, vec!["S6:1", "S5:1", "S4:1", "S3:1", "S2:1"]);
    // Synthetic trade evidence: S0 and S6 reach +2%, nobody else.
    let evidence: HashMap<String, TradeEvidence> = (0..n)
        .map(|i| {
            let sym = format!("S{i}");
            let hit = i == 0 || i == 6;
            (sym.clone(), trades(&sym, w.anchor_at, if hit { &[(60_000, 10.2, &[])] } else { &[(60_000, 10.1, &[])] }))
        })
        .collect();
    let (access, p, sp) = (super::campaign::OutcomeAccess::synthetic_for_tests(&[SESSION]), policy(), status_policy());
    let ctx = EvaluationContext {
        access: &access,
        session: SESSION,
        conditions: &p,
        statuses: &sp,
        expected_condition_sha: POLICY_SHA,
        expected_status_sha: STATUS_SHA,
    };
    let stat = session_statistic(&sel, &e.run_id, |w, c| {
        evaluate(&ctx, w.anchor_at, c.anchor_price.unwrap(), &c.symbol, evidence.get(&c.symbol), &e.status).unwrap()
    });
    assert_eq!((stat.hits_a, stat.hits_b, stat.total_k), (1, 1, 5));
    assert_eq!(stat.d, Some(0.0));
}
