//! Step 4B-main.1: status/condition policies, the outcome adapter and archive,
//! Arm-B join authentication, selection audit and the campaign firewall.
//! Synthetic fixtures only -- no real trade, status, eligibility or outcome
//! data is read anywhere in this file. The only real bytes read are the
//! committed static metadata snapshots (condition-code meanings and the
//! trading-status documentation).

use std::cell::RefCell;
use std::collections::{BTreeSet, VecDeque};
use std::path::Path;

use chrono::{DateTime, Duration, Utc};

use super::analysis::*;
use super::campaign::*;
use super::observation_main_tests::{
    all_ranked, cx, eval, extract, full_status, ny, policy, pool_of, status_policy, trades, with_events, wx, Tmp,
    POLICY_SHA, SESSION, STATUS_SHA,
};
use super::outcome::*;
use super::policy::ConditionClass;
use super::prereg::sha256_hex;

fn ops(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ops/observation").join(name)
}

fn tr(ms: i64, p: f64, tape: &str, c: &[&str], t0: DateTime<Utc>) -> Trade {
    Trade {
        exchange_at: t0 + Duration::milliseconds(ms),
        price: to_micros(p).unwrap(),
        tape: tape.into(),
        conditions: c.iter().map(|s| s.to_string()).collect(),
    }
}

fn evidence(t0: DateTime<Utc>, list: Vec<Trade>) -> TradeEvidence {
    TradeEvidence {
        symbol: "AAA".into(),
        covered_from: t0 - Duration::seconds(10),
        covered_to: t0 + Duration::seconds(400),
        response_complete: true,
        gaps: vec![],
        trades: list,
    }
}

fn st() -> StatusEvidence {
    full_status(ny(9, 30, 0), ny(16, 0, 0))
}

fn cta_events(mut s: StatusEvidence, list: &[(DateTime<Utc>, &str, &str)]) -> StatusEvidence {
    s.events.insert(
        "AAA".into(),
        list.iter()
            .map(|(at, tape, code)| StatusEvent { market_at: *at, tape: Some((*tape).into()), code: (*code).into(), reason: None })
            .collect(),
    );
    s
}

// ===========================================================================
// §3-4 Static metadata: the proposed tables are exactly the fetched codes
// ===========================================================================

#[test]
fn the_proposed_condition_table_classifies_exactly_the_fetched_codes_per_tape() {
    let bytes = std::fs::read(ops("trade-conditions-v2.proposed.json")).unwrap();
    let p = ConditionPolicy::bind(&bytes).unwrap();
    // Cross-language JCS agreement (Python computed the same identity).
    assert_eq!(p.sha256, "001eb287c65cb79454c04490749d20be4e909fb0ab9a42ef43e67fb02bcac641");
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    for src in v["sources"].as_array().unwrap() {
        let raw = std::fs::read(ops(src["file"].as_str().unwrap())).unwrap();
        assert_eq!(sha256_hex(&raw), src["sha256"].as_str().unwrap(), "raw snapshot bytes unchanged");
        let codes: BTreeSet<String> = serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&raw).unwrap().keys().cloned().collect();
        let tape = src["tape"].as_str().unwrap();
        let classified: BTreeSet<String> = p.tapes[tape].keys().cloned().collect();
        assert_eq!(classified, codes, "tape {tape}: every fetched code classified, nothing invented");
    }
    // Materially different semantics are not collapsed.
    assert_eq!(p.class_of("A", "B"), Some(ConditionClass::Exclude), "CTA B = Average Price");
    assert_eq!(p.class_of("C", "B"), Some(ConditionClass::Include), "UTP B = Bunched Trade");
    assert_eq!(p.class_of("A", "E"), Some(ConditionClass::Include), "CTA E = Automatic Execution");
    assert_eq!(p.class_of("C", "E"), Some(ConditionClass::Censor), "UTP E = Placeholder");
    assert_eq!(p.class_of("A", " "), Some(ConditionClass::Include), "CTA regular sale is a space");
    assert_eq!(p.class_of("A", "@"), None, "@ is not in the CTA metadata: open question, not guessed");
    for tape in ["A", "C"] {
        assert_eq!(p.class_of(tape, "I"), Some(ConditionClass::Exclude), "odd lots (proposed)");
        assert_eq!(p.class_of(tape, "Z"), Some(ConditionClass::Exclude), "out of sequence");
        assert_eq!(p.class_of(tape, "F"), Some(ConditionClass::Include), "intermarket sweep");
        assert_eq!(p.class_of(tape, "O"), Some(ConditionClass::Include), "opening auction");
    }
}

#[test]
fn the_proposed_status_policy_classifies_both_families_from_the_documented_codes() {
    let bytes = std::fs::read(ops("trading-status-policy-v1.proposed.json")).unwrap();
    let p = StatusPolicy::bind(&bytes).unwrap();
    assert_eq!(p.sha256, "1e08768c2226a2a2b64e9d9d8719b3e2341417083a781914d5e599d94bbfabbe");
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let doc = std::fs::read(ops(v["source"]["file"].as_str().unwrap())).unwrap();
    assert_eq!(sha256_hex(&doc), v["source"]["sha256"].as_str().unwrap());
    let doc = String::from_utf8_lossy(&doc);
    // Every classified code is a code the documentation lists.
    for (family, codes) in &p.families {
        for code in codes.keys() {
            assert!(doc.contains(&format!("| {code} ")), "{family} {code} is documented");
        }
    }
    use StatusClass::*;
    for (tape, code, class) in [
        ("A", "2", Halt),
        ("B", "3", Resume),
        ("A", "5", Informational),
        ("A", "F", Informational),
        ("C", "H", Halt),
        ("C", "P", Pause),
        ("C", "Q", NonTradable),
        ("O", "T", Resume),
    ] {
        assert_eq!(p.classify(Some(tape), code), Some(class), "{tape} {code}");
    }
    assert_eq!(p.classify(Some("C"), "2"), None, "code spaces are per family");
    assert_eq!(p.classify(Some("A"), "H"), None);
    assert_eq!(p.classify(None, "H"), None, "unknown tape");
    assert_eq!(p.classify(Some("Z"), "H"), None);
}

// ===========================================================================
// §5 Interruption contract
// ===========================================================================

#[test]
fn an_informational_status_during_a_halt_does_not_resume_trading() {
    let t0 = ny(10, 0, 0);
    let s = |sec: i64| t0 + Duration::seconds(sec);
    // CTA halt, then an imbalance message: still halted, so a reach is censored.
    let halted = cta_events(st(), &[(s(60), "A", "2"), (s(90), "A", "7")]);
    let ev = evidence(t0, vec![tr(120_000, 10.3, "A", &[" "], t0)]);
    assert_eq!(eval(t0, 10.0, Some(&ev), &halted), Outcome::Censored(CensorReason::HaltOverlap));
    let sp = status_policy();
    assert_eq!(halted.interruptions("AAA", &sp).len(), 1);
    assert_eq!(halted.interruptions("AAA", &sp)[0].end, None, "an imbalance is not a resume");
    // A reach before the halt began is a success.
    let ev = evidence(t0, vec![tr(30_000, 10.2, "A", &[" "], t0)]);
    assert_eq!(eval(t0, 10.0, Some(&ev), &halted), Outcome::Success { at: s(30) });
    // Resumed within the horizon: a later reach is still censored.
    let resumed = cta_events(st(), &[(s(60), "A", "2"), (s(90), "A", "3")]);
    let ev = evidence(t0, vec![tr(120_000, 10.3, "A", &[" "], t0)]);
    assert_eq!(eval(t0, 10.0, Some(&ev), &resumed), Outcome::Censored(CensorReason::HaltOverlap));
}

#[test]
fn luld_pauses_and_quote_only_periods_are_interruptions() {
    let t0 = ny(10, 0, 0);
    let s = |sec: i64| t0 + Duration::seconds(sec);
    let paused = with_events(st(), "AAA", &[(s(30), "P"), (s(330), "T")]);
    assert_eq!(eval(t0, 10.0, Some(&trades("AAA", t0, &[(60_000, 10.3, &[])])), &paused), Outcome::Censored(CensorReason::HaltOverlap));
    assert_eq!(eval(t0, 10.0, Some(&trades("AAA", t0, &[(10_000, 10.3, &[])])), &paused), Outcome::Success { at: s(10) });
    // H -> Q -> T: the quotation-only period does not end the halt.
    let sp = status_policy();
    let hqt = with_events(st(), "AAA", &[(s(-600), "H"), (s(-60), "Q"), (s(120), "T")]);
    let i = hqt.interruptions("AAA", &sp);
    assert_eq!((i.len(), i[0].start, i[0].end), (1, s(-600), Some(s(120))));
    assert_eq!(eval(t0, 10.0, Some(&trades("AAA", t0, &[(10_000, 10.3, &[])])), &hqt), Outcome::Censored(CensorReason::HaltOverlap));
    // A quotation resumption alone is a non-tradable period.
    let q = with_events(st(), "AAA", &[(s(50), "Q")]);
    assert_eq!(eval(t0, 10.0, Some(&trades("AAA", t0, &[(60_000, 10.3, &[])])), &q), Outcome::Censored(CensorReason::HaltOverlap));
}

#[test]
fn an_unclassified_status_censors_unless_the_reach_came_first_and_is_never_failure() {
    let t0 = ny(10, 0, 0);
    let s = |sec: i64| t0 + Duration::seconds(sec);
    let unknown = with_events(st(), "AAA", &[(s(100), "Z9")]);
    assert_eq!(eval(t0, 10.0, Some(&trades("AAA", t0, &[(50_000, 10.2, &[])])), &unknown), Outcome::Success { at: s(50) });
    assert_eq!(
        eval(t0, 10.0, Some(&trades("AAA", t0, &[(150_000, 10.2, &[])])), &unknown),
        Outcome::Censored(CensorReason::StatusUnclassified)
    );
    assert_eq!(eval(t0, 10.0, Some(&trades("AAA", t0, &[])), &unknown), Outcome::Censored(CensorReason::StatusUnclassified), "never failure");
    // An unknown tape makes even a known code unclassified.
    let no_tape = {
        let mut x = st();
        x.events.insert("AAA".into(), vec![StatusEvent { market_at: s(100), tape: None, code: "H".into(), reason: None }]);
        x
    };
    assert_eq!(eval(t0, 10.0, Some(&trades("AAA", t0, &[])), &no_tape), Outcome::Censored(CensorReason::StatusUnclassified));
    // Ended by a classified resume before T0: no overlap, a complete quiet horizon fails.
    let ended = with_events(st(), "AAA", &[(s(-100), "Z9"), (s(-50), "T")]);
    assert_eq!(eval(t0, 10.0, Some(&trades("AAA", t0, &[])), &ended), Outcome::Failure);
    // An unclassified interval upgraded by a halt is one halt from then on.
    let sp = status_policy();
    let up = with_events(st(), "AAA", &[(s(10), "Z9"), (s(20), "H")]);
    let kinds: Vec<_> = up.interruptions("AAA", &sp).iter().map(|i| i.kind).collect();
    assert_eq!(kinds, vec![InterruptionKind::Unclassified, InterruptionKind::Halted]);
}

// ===========================================================================
// §3 Tape-specific conditions
// ===========================================================================

#[test]
fn conditions_are_tape_specific_and_uncertainty_only_censors_a_would_be_hit() {
    let t0 = ny(10, 0, 0);
    let one = |t: Trade| eval(t0, 10.0, Some(&evidence(t0, vec![t])), &st());
    let hit = Outcome::Success { at: t0 + Duration::seconds(10) };
    assert_eq!(one(tr(10_000, 10.3, "A", &["B"], t0)), Outcome::Failure, "CTA average price: ignored");
    assert_eq!(one(tr(10_000, 10.3, "C", &["B"], t0)), hit, "UTP bunched: counts");
    assert_eq!(one(tr(10_000, 10.3, "A", &[" "], t0)), hit);
    assert_eq!(one(tr(10_000, 10.3, "A", &["@"], t0)), Outcome::Censored(CensorReason::UnknownCondition), "@ unlisted on CTA");
    assert_eq!(one(tr(10_000, 10.3, "C", &[], t0)), Outcome::Censored(CensorReason::UnknownCondition), "empty list");
    assert_eq!(one(tr(10_000, 10.3, "Q", &["@"], t0)), Outcome::Censored(CensorReason::UnknownCondition), "unknown tape");
    assert_eq!(one(tr(10_000, 10.3, "C", &["E"], t0)), Outcome::Censored(CensorReason::UnknownCondition), "censor class");
    assert_eq!(one(tr(10_000, 10.3, "C", &["@", "I"], t0)), Outcome::Failure, "exclude dominates");
    // Uncertain prints below the target cannot matter.
    assert_eq!(one(tr(10_000, 10.1, "C", &["E"], t0)), Outcome::Failure);
    assert_eq!(one(tr(10_000, 10.1, "C", &[], t0)), Outcome::Failure);
    // An uncertain print after a counted reach is irrelevant.
    let ev = evidence(t0, vec![tr(10_000, 10.2, "C", &["@"], t0), tr(20_000, 10.5, "C", &["E"], t0)]);
    assert_eq!(eval(t0, 10.0, Some(&ev), &st()), hit);
    // ... and one before it censors.
    let ev = evidence(t0, vec![tr(5_000, 10.5, "C", &["E"], t0), tr(10_000, 10.2, "C", &["@"], t0)]);
    assert_eq!(eval(t0, 10.0, Some(&ev), &st()), Outcome::Censored(CensorReason::UnknownCondition));
}

#[test]
fn the_evaluator_binds_both_policies_and_the_firewall() {
    let t0 = ny(10, 0, 0);
    let (cp, sp) = (policy(), status_policy());
    let ev = trades("AAA", t0, &[]);
    let run = |access: &OutcomeAccess, cond: &str, stat: &str| {
        let ctx = EvaluationContext {
            access,
            session: SESSION,
            conditions: &cp,
            statuses: &sp,
            expected_condition_sha: cond,
            expected_status_sha: stat,
        };
        evaluate(&ctx, t0, 10.0, "AAA", Some(&ev), &st())
    };
    let ok = OutcomeAccess::synthetic_for_tests(&[SESSION]);
    assert_eq!(run(&ok, POLICY_SHA, STATUS_SHA), Ok(Outcome::Failure));
    assert!(matches!(run(&ok, POLICY_SHA, &"1".repeat(64)), Err(EvaluationError::StatusPolicyIdentity { .. })));
    let other = OutcomeAccess::synthetic_for_tests(&["2026-09-30"]);
    assert!(matches!(run(&other, POLICY_SHA, STATUS_SHA), Err(EvaluationError::OutcomeFirewall { .. })));
    // The fixture preregistration binds exactly these identities.
    let pre: serde_json::Value = serde_json::from_slice(&std::fs::read(ops("step4-preregistration-v1.fixture.json")).unwrap()).unwrap();
    assert_eq!(pre["outcome"]["tradeConditionTableSha256"], POLICY_SHA);
    assert_eq!(pre["status"]["policySha256"], STATUS_SHA);
    assert_eq!(pre["outcome"]["fetchContract"], FETCH_CONTRACT);
    assert_eq!(pre["freezeStatus"], "NOT-FINAL-NOT-FROZEN");
}

// ===========================================================================
// §6 Historical-trades adapter (mock transport only)
// ===========================================================================

struct Mock {
    responses: VecDeque<Result<PageResponse, String>>,
    urls: RefCell<Vec<String>>,
}

impl Mock {
    fn new(list: Vec<Result<PageResponse, String>>) -> Self {
        Self { responses: list.into(), urls: RefCell::new(Vec::new()) }
    }
}

impl PageSource for Mock {
    fn get(&mut self, url: &str) -> Result<PageResponse, String> {
        self.urls.borrow_mut().push(url.to_string());
        self.responses.pop_front().expect("unexpected extra request")
    }
}

fn page(status: u16, body: &str) -> Result<PageResponse, String> {
    Ok(PageResponse { http_status: status, retry_after_secs: None, body: body.as_bytes().to_vec(), retrieved_at: ny(17, 0, 0) })
}

fn t(sec: i64, price: &str, conds: &str, id: u64) -> String {
    let at = (ny(10, 0, 0) + Duration::seconds(sec)).to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
    format!(r#"{{"t":"{at}","x":"V","p":{price},"s":100,"c":[{conds}],"i":{id},"z":"C"}}"#)
}

fn body(trades: &[String], next: Option<&str>) -> String {
    let next = next.map_or("null".to_string(), |n| format!("\"{n}\""));
    format!(r#"{{"trades":[{}],"symbol":"AAA","next_page_token":{next}}}"#, trades.join(","))
}

fn request() -> TradeRequest {
    TradeRequest { session: SESSION.into(), symbol: "AAA".into(), start: ny(9, 59, 0), end: ny(10, 6, 0), feed: "sip".into() }
}

fn access() -> OutcomeAccess {
    OutcomeAccess::synthetic_for_tests(&[SESSION])
}

fn run_fetch(mock: &mut Mock) -> (FetchResult, Vec<u64>) {
    let mut slept = Vec::new();
    let r = fetch_trades(&access(), &request(), mock, FetchConfig::default(), ny(17, 0, 0), &mut |s| slept.push(s)).unwrap();
    (r, slept)
}

#[test]
fn the_adapter_paginates_and_normalises_exactly() {
    let mut m = Mock::new(vec![
        page(200, &body(&[t(10, "10.1", r#""@""#, 1), t(20, "10.2", r#""@","F""#, 2)], Some("tok+/="))),
        page(200, &body(&[t(30, "3.3966", r#""I""#, 3)], None)),
    ]);
    let (r, _) = run_fetch(&mut m);
    assert!(r.complete && r.errors.is_empty(), "{:?}", r.errors);
    let urls = m.urls.borrow();
    assert!(!urls[0].contains("page_token"));
    assert!(urls[1].ends_with("&page_token=tok%2B%2F%3D"), "{}", urls[1]);
    assert!(urls[0].contains("feed=sip") && urls[0].contains("sort=asc") && urls[0].contains("limit=10000"));
    let ev = r.evidence();
    assert_eq!((ev.symbol.as_str(), ev.covered_from, ev.covered_to, ev.response_complete), ("AAA", ny(9, 59, 0), ny(10, 6, 0), true));
    assert_eq!(ev.trades.len(), 3);
    assert_eq!(ev.trades[2].price, 3_396_600, "exact micro-dollars");
    assert_eq!((ev.trades[1].tape.as_str(), ev.trades[1].conditions.clone()), ("C", vec!["@".to_string(), "F".to_string()]));
    // Fed straight to the evaluator: 10.2 at +20 s reaches the +2% target from 10.0.
    assert_eq!(eval(ny(10, 0, 0), 10.0, Some(&ev), &st()), Outcome::Success { at: ny(10, 0, 20) });
}

#[test]
fn an_empty_complete_result_is_complete_and_a_valid_failure() {
    for b in [r#"{"trades":[],"symbol":"AAA","next_page_token":null}"#, r#"{"trades":null,"symbol":"AAA","next_page_token":null}"#] {
        let (r, _) = run_fetch(&mut Mock::new(vec![page(200, b)]));
        assert!(r.complete && r.trades.is_empty());
        assert_eq!(eval(ny(10, 0, 0), 10.0, Some(&r.evidence()), &st()), Outcome::Failure);
    }
}

#[test]
fn every_fetch_defect_ends_incomplete_with_its_reason() {
    let ok1 = || page(200, &body(&[t(10, "10.1", r#""@""#, 1)], Some("n1")));
    let cases: Vec<(Vec<Result<PageResponse, String>>, &str)> = vec![
        (vec![ok1(), page(500, "{}")], "HTTP 500"),
        (vec![ok1(), page(200, r#"{"trades":[{"t":"2026-09-29T14:00:10Z","p":10"#)], "unparseable"),
        (vec![ok1(), Err("connection reset".into())], "transport"),
        (vec![page(200, &body(&[t(10, "10.1", r#""@""#, 1)], Some("n1"))), page(200, &body(&[], Some("n1")))], "repeated page token"),
        (vec![page(200, &body(&[t(3600, "10.1", r#""@""#, 1)], None))], "outside the requested interval"),
        (vec![page(200, r#"{"trades":[],"symbol":"BBB","next_page_token":null}"#)], "page symbol"),
        (vec![page(200, &body(&[t(20, "10.1", r#""@""#, 2), t(10, "10.1", r#""@""#, 1)], None))], "out of time order"),
    ];
    for (responses, reason) in cases {
        let (r, _) = run_fetch(&mut Mock::new(responses));
        assert!(!r.complete, "{reason}");
        assert!(r.errors.iter().any(|e| e.contains(reason)), "{reason}: {:?}", r.errors);
        assert_eq!(eval(ny(10, 0, 0), 10.0, Some(&r.evidence()), &st()), Outcome::Censored(CensorReason::IncompleteResponse));
    }
    // Page cap.
    let mut slept = Vec::new();
    let r = fetch_trades(
        &access(),
        &request(),
        &mut Mock::new(vec![ok1()]),
        FetchConfig { max_pages: 1, ..FetchConfig::default() },
        ny(17, 0, 0),
        &mut |s| slept.push(s),
    )
    .unwrap();
    assert!(!r.complete && r.errors[0].contains("page cap"));
}

#[test]
fn rate_limits_back_off_boundedly_and_are_logged() {
    let limited = |after: Option<u64>| {
        Ok(PageResponse { http_status: 429, retry_after_secs: after, body: b"{\"message\":\"too many\"}".to_vec(), retrieved_at: ny(17, 0, 0) })
    };
    let (r, slept) = run_fetch(&mut Mock::new(vec![limited(Some(3)), limited(None), page(200, &body(&[], None))]));
    assert!(r.complete);
    assert_eq!(slept, vec![3, 4], "Retry-After honoured, then exponential default");
    assert_eq!(r.exchanges.iter().map(|e| e.role).collect::<Vec<_>>(), vec!["rate-limited", "rate-limited", "page"]);
    let (r, _) = run_fetch(&mut Mock::new((0..6).map(|_| limited(None)).collect()));
    assert!(!r.complete && r.errors[0].contains("retries exhausted"));
}

#[test]
fn requests_are_refused_before_any_io() {
    let mut m = Mock::new(vec![]);
    let mut noop = |_| {};
    let go = |req: &TradeRequest, acc: &OutcomeAccess, now, m: &mut Mock, s: &mut dyn FnMut(u64)| {
        fetch_trades(acc, req, m, FetchConfig::default(), now, s).map(|_| ())
    };
    let locked = OutcomeAccess::synthetic_for_tests(&[]);
    assert!(matches!(go(&request(), &locked, ny(17, 0, 0), &mut m, &mut noop), Err(FetchRefusal::OutcomeFirewall(_))));
    assert_eq!(go(&request(), &access(), ny(15, 0, 0), &mut m, &mut noop), Err(FetchRefusal::SessionNotClosed), "post-session only");
    let mut bad = request();
    bad.feed = "iex".into();
    assert!(matches!(go(&bad, &access(), ny(17, 0, 0), &mut m, &mut noop), Err(FetchRefusal::InvalidRequest(_))));
    let mut span = request();
    span.end = ny(10, 0, 0) + Duration::days(1);
    assert!(matches!(go(&span, &access(), ny(17, 0, 0) + Duration::days(1), &mut m, &mut noop), Err(FetchRefusal::InvalidRequest(_))));
    let mut wrong = request();
    wrong.session = "2026-09-30".into();
    let acc30 = OutcomeAccess::synthetic_for_tests(&["2026-09-30"]);
    assert!(matches!(go(&wrong, &acc30, ny(17, 0, 0) + Duration::days(1), &mut m, &mut noop), Err(FetchRefusal::InvalidRequest(_))));
    assert!(m.urls.borrow().is_empty(), "nothing was requested");
}

// ===========================================================================
// §7-8 Outcome evidence archive
// ===========================================================================

fn binding() -> EvidenceBinding {
    EvidenceBinding {
        condition_table_sha256: POLICY_SHA.into(),
        status_policy_sha256: STATUS_SHA.into(),
        implementation_sha: "1".repeat(40),
        preregistration_sha256: "2".repeat(64),
    }
}

fn two_page_fetch() -> FetchResult {
    let limited = Ok(PageResponse { http_status: 429, retry_after_secs: Some(1), body: b"slow down".to_vec(), retrieved_at: ny(17, 0, 0) });
    run_fetch(&mut Mock::new(vec![
        page(200, &body(&[t(10, "10.1", r#""@""#, 1)], Some("n1"))),
        limited,
        page(200, &body(&[t(20, "10.25", r#""@""#, 2)], None)),
    ]))
    .0
}

#[test]
fn archives_reproduce_evidence_from_raw_pages_and_never_overwrite() {
    let root = Tmp::new("oa-arch");
    let r = two_page_fetch();
    assert!(r.complete);
    let (dir, manifest_sha) = archive_fetch(root.path(), &r, &binding(), 1).unwrap();
    let a = verify_archive(&dir).unwrap();
    let b = verify_archive(&dir).unwrap();
    assert_eq!(a, b, "verification is idempotent");
    assert_eq!((a.complete, a.pages, a.normalized_count, a.manifest_sha256.as_str()), (true, 2, 2, manifest_sha.as_str()));
    assert!(!dir.join("INCOMPLETE").exists());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 5, "3 exchange bodies, normalized, manifest");
    let (ev, _) = load_evidence(&access(), &dir, &binding()).unwrap();
    assert_eq!(ev, r.evidence(), "reconstructed from the raw pages");
    assert!(archive_fetch(root.path(), &r, &binding(), 1).unwrap_err().contains("never overwritten"));
    assert!(archive_fetch(root.path(), &r, &binding(), 2).is_ok(), "a new attempt is a new archive");
    let m: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
    for k in ["session", "symbol", "requestedFrom", "requestedTo", "feed", "fetchContract", "normalizedSha256"] {
        assert!(!m[k].is_null(), "{k}");
    }
    for k in ["conditionTableSha256", "statusPolicySha256", "implementationSha", "preregistrationSha256"] {
        assert!(m["binding"][k].is_string(), "{k}");
    }
    assert_eq!(m["exchanges"][1]["role"], "rate-limited");
    assert!(m["exchanges"][0]["retrievedAt"].is_string() && m["exchanges"][0]["sha256"].is_string());
}

#[test]
fn any_tampering_fails_verification() {
    let root = Tmp::new("oa-tamper");
    let r = two_page_fetch();
    let tamper = |attempt: u32, f: &dyn Fn(&Path)| {
        let (dir, _) = archive_fetch(root.path(), &r, &binding(), attempt).unwrap();
        f(&dir);
        verify_archive(&dir).unwrap_err()
    };
    let e = tamper(1, &|d| std::fs::write(d.join("exchange-0000.body"), body(&[t(10, "11.0", r#""@""#, 1)], Some("n1"))).unwrap());
    assert!(e.contains("SHA-256"), "{e}");
    let e = tamper(2, &|d| {
        let mut n = std::fs::read(d.join("normalized.ndjson")).unwrap();
        n.push(b'\n');
        std::fs::write(d.join("normalized.ndjson"), n).unwrap();
    });
    assert!(e.contains("re-derive"), "{e}");
    let e = tamper(3, &|d| std::fs::write(d.join("INCOMPLETE"), b"x").unwrap());
    assert!(e.contains("INCOMPLETE"), "{e}");
    let e = tamper(4, &|d| {
        let p = d.join("manifest.json");
        let s = std::fs::read_to_string(&p).unwrap().replace("\"pageTokenIn\": \"n1\"", "\"pageTokenIn\": \"n2\"");
        std::fs::write(p, s).unwrap();
    });
    assert!(e.contains("page chain"), "{e}");
}

#[test]
fn an_incomplete_fetch_stays_visibly_incomplete() {
    let root = Tmp::new("oa-inc");
    let (r, _) = run_fetch(&mut Mock::new(vec![page(200, &body(&[t(10, "10.3", r#""@""#, 1)], Some("n1"))), page(503, "down")]));
    let (dir, _) = archive_fetch(root.path(), &r, &binding(), 1).unwrap();
    assert!(dir.join("INCOMPLETE").exists());
    let rep = verify_archive(&dir).unwrap();
    assert!(!rep.complete);
    let (ev, _) = load_evidence(&access(), &dir, &binding()).unwrap();
    assert!(!ev.response_complete);
    // Even though the partial page holds a +3% print, the outcome is censored.
    assert_eq!(eval(ny(10, 0, 0), 10.0, Some(&ev), &st()), Outcome::Censored(CensorReason::IncompleteResponse));
}

#[test]
fn loading_is_behind_the_firewall_and_bound_to_the_registered_identities() {
    let root = Tmp::new("oa-fw");
    let (dir, _) = archive_fetch(root.path(), &two_page_fetch(), &binding(), 1).unwrap();
    let e = load_evidence(&OutcomeAccess::synthetic_for_tests(&["2026-09-30"]), &dir, &binding()).unwrap_err();
    assert!(e.contains("firewall"), "{e}");
    let mut other = binding();
    other.status_policy_sha256 = "f".repeat(64);
    assert!(load_evidence(&access(), &dir, &other).unwrap_err().contains("binding"));
}

// §9 Arm-B join authentication: see observation_main2_tests.rs (real D6 sources).

// ===========================================================================
// §10 Selection audit; indeterminate comparison
// ===========================================================================

#[test]
fn every_window_yields_an_auditable_selection_record() {
    let mut w1 = pool_of(6, "w1");
    w1.push(cx("OLD", "OLDS", 1, ny(9, 30, 0))); // open in the first window: left-censored
    let mut no_anchor = cx("NA", "NAS", 2, ny(9, 30, 0));
    no_anchor.anchor_price = None;
    w1.push(no_anchor);
    let e = extract(vec![
        wx("w1", ny(10, 0, 0), true, w1),
        wx("w2", ny(10, 1, 0), false, pool_of(3, "w2")),
        wx("w3", ny(10, 2, 0), true, vec![cx("w1-L0", "S0", 1, ny(9, 30, 0))]),
    ]);
    let sel = select(&e, &all_ranked(&e), SelectionConfig::default());
    let audit = selection_audit(&sel, SESSION, "run-1", &e);
    assert_eq!(audit.iter().map(|r| r.window_id.as_str()).collect::<Vec<_>>(), vec!["w0", "w1", "w2", "w3"]);
    let a1 = &audit[1];
    assert_eq!((a1.valid, a1.k, a1.discriminating, a1.pool.len()), (true, 5, true, 6));
    assert_eq!((a1.arm_a_order.len(), a1.arm_b_order.len(), a1.arm_a_selected.len(), a1.arm_b_selected.len()), (6, 6, 5, 5));
    assert_eq!(&a1.arm_a_order[..5], &a1.arm_a_selected[..]);
    assert_eq!(a1.retired.len(), 8, "the pool plus the two excluded lifecycles");
    let reasons: Vec<(&str, &str)> = a1.exclusions.iter().map(|x| (x.opportunity_id.as_str(), x.reason.as_str())).collect();
    assert!(reasons.contains(&("OLD", "left-censored")) && reasons.contains(&("NA", "no-anchor-price")), "{reasons:?}");
    assert!(!audit[2].valid && audit[2].exclusions[0].reason == "invalid-window");
    assert_eq!(audit[3].exclusions[0].reason, "retired-earlier", "no re-entry, visibly");
    let json = serde_json::to_value(&audit[1]).unwrap();
    for k in ["schema", "session", "windowId", "pool", "retired", "armAOrder", "armASelected", "armBOrder", "armBSelected", "k", "discriminating", "exclusions"] {
        assert!(!json[k].is_null(), "{k}");
    }
    assert_eq!(json["schema"], SELECTION_AUDIT_SCHEMA);
}

#[test]
fn an_indeterminate_comparison_has_no_statistic_and_consults_no_outcome() {
    let e = extract(vec![wx("w1", ny(10, 0, 0), true, pool_of(7, "w1"))]);
    let mut oi = all_ranked(&e);
    oi.zero_loss_established = false;
    let sel = select(&e, &oi, SelectionConfig::default());
    let mut consulted = 0;
    let s = session_statistic(&sel, "r", |_, _| {
        consulted += 1;
        Outcome::Failure
    });
    assert_eq!((s.d, consulted, s.windows_used), (None, 0, 0));
    assert!(s.indeterminate.is_some());
    assert!(cluster_rows(&sel, "r", |_, _| unreachable!("no outcome may be consulted")).is_empty());
    assert!(selection_audit(&sel, SESSION, "r", &e)[1].comparison_indeterminate.is_some());
}

// ===========================================================================
// §11-12 Campaign state machine and outcome firewall
// ===========================================================================

fn q(session: &str, qualifies: bool) -> CaptureQualification {
    CaptureQualification {
        session: session.into(),
        run_id: format!("run-{session}"),
        certificate_pass: true,
        floors_satisfied: true,
        discriminating_windows: if qualifies { 20 } else { 19 },
        oi_zero_loss: true,
    }
}

fn day(i: usize) -> String {
    format!("2026-10-{:02}", i + 1)
}

fn capture(c: &mut Campaign, i: usize, qualifies: bool) {
    c.apply(CampaignEvent::Designated { session: day(i) }).unwrap();
    c.apply(CampaignEvent::CaptureRecorded { qualification: q(&day(i), qualifies) }).unwrap();
}

fn authorize() -> CampaignEvent {
    CampaignEvent::OutcomeFetchAuthorized { authorized_by: "GPT/user".into(), reference: "step-5 authorization".into(), at: ny(12, 0, 0) }
}

#[test]
fn the_first_session_and_nine_qualifying_sessions_cannot_unlock_outcomes() {
    let mut c = Campaign::new(CampaignRules::default());
    capture(&mut c, 0, true);
    assert_eq!(c.state(), CampaignState::Collecting);
    assert!(matches!(c.outcome_access(), Err(CampaignError::OutcomesLocked(CampaignState::Collecting))));
    for i in 1..9 {
        capture(&mut c, i, true);
    }
    assert_eq!((c.state(), c.qualifying_sessions().len()), (CampaignState::Collecting, 9));
    assert!(c.outcome_access().is_err());
    assert!(matches!(c.apply(authorize()), Err(CampaignError::NotPermitted { .. })), "no authorization before closure");
}

#[test]
fn ten_qualifying_sessions_close_the_set_which_then_admits_nothing() {
    let mut c = Campaign::new(CampaignRules::default());
    capture(&mut c, 0, false); // non-qualifying sessions count toward the cap, not the target
    for i in 1..=10 {
        capture(&mut c, i, true);
    }
    assert_eq!(c.state(), CampaignState::CaptureSetClosed);
    assert!(c.outcome_access().is_err(), "closure alone does not unlock");
    assert!(matches!(c.apply(CampaignEvent::Designated { session: day(11) }), Err(CampaignError::NotPermitted { .. })));
    assert!(matches!(
        c.apply(CampaignEvent::CaptureRecorded { qualification: q(&day(11), true) }),
        Err(CampaignError::NotPermitted { .. })
    ));
    assert_eq!(
        c.apply(CampaignEvent::OutcomeFetchAuthorized { authorized_by: " ".into(), reference: "x".into(), at: ny(12, 0, 0) }),
        Err(CampaignError::EmptyAuthorization)
    );
    c.apply(authorize()).unwrap();
    let access = c.outcome_access().unwrap();
    assert_eq!(access.sessions().len(), 10);
    assert!(access.allows(&day(1)) && !access.allows(&day(0)), "only qualifying sessions unlock");
    assert!(matches!(c.apply(CampaignEvent::Designated { session: day(12) }), Err(CampaignError::NotPermitted { .. })));
    // Verified evidence for every qualifying session: ANALYSIS_READY.
    assert_eq!(
        c.apply(CampaignEvent::OutcomeEvidenceVerified { session: day(0), archive_manifest_sha256: "a".repeat(64) }),
        Err(CampaignError::NotQualifying(day(0)))
    );
    for i in 1..=10 {
        assert_eq!(c.state(), CampaignState::OutcomeFetchAuthorized);
        c.apply(CampaignEvent::OutcomeEvidenceVerified { session: day(i), archive_manifest_sha256: "a".repeat(64) }).unwrap();
    }
    assert_eq!(c.state(), CampaignState::AnalysisReady);
    assert!(c.outcome_access().is_ok());
}

#[test]
fn twenty_designated_with_fewer_than_ten_qualifying_is_measurement_insufficient() {
    let mut c = Campaign::new(CampaignRules::default());
    for i in 0..20 {
        capture(&mut c, i, i % 3 == 0); // 7 qualify
    }
    assert_eq!(c.state(), CampaignState::MeasurementInsufficient);
    assert!(matches!(c.outcome_access(), Err(CampaignError::OutcomesLocked(CampaignState::MeasurementInsufficient))));
    assert!(c.apply(authorize()).is_err(), "no outcome analysis in this state");
    assert!(c.apply(CampaignEvent::Designated { session: day(21) }).is_err());
    // The designation cap binds even while collecting.
    let mut d = Campaign::new(CampaignRules { max_designated: 2, qualifying_target: 10, min_discriminating_windows: 20 });
    capture(&mut d, 0, true);
    d.apply(CampaignEvent::Designated { session: day(1) }).unwrap();
    assert_eq!(d.apply(CampaignEvent::Designated { session: day(2) }), Err(CampaignError::PendingCapture(day(1))));
}

#[test]
fn qualification_is_measurement_evidence_only() {
    let rules = CampaignRules::default();
    assert!(q("s", true).qualifies(&rules));
    assert!(!q("s", false).qualifies(&rules), "19 discriminating windows");
    for f in [
        |x: &mut CaptureQualification| x.certificate_pass = false,
        |x: &mut CaptureQualification| x.floors_satisfied = false,
        |x: &mut CaptureQualification| x.oi_zero_loss = false,
    ] {
        let mut x = q("s", true);
        f(&mut x);
        assert!(!x.qualifies(&rules));
    }
}

#[test]
fn the_ledger_replays_to_the_same_state_and_refuses_illegal_histories() {
    let mut c = Campaign::new(CampaignRules::default());
    for i in 0..10 {
        capture(&mut c, i, true);
    }
    c.apply(authorize()).unwrap();
    let json = serde_json::to_string(c.events()).unwrap();
    assert!(json.contains("\"event\":\"outcomeFetchAuthorized\"") && json.contains("\"authorizedBy\""), "{json}");
    let events: Vec<CampaignEvent> = serde_json::from_str(&json).unwrap();
    let r = Campaign::replay(CampaignRules::default(), &events).unwrap();
    assert_eq!((r.state(), r.qualifying_sessions()), (c.state(), c.qualifying_sessions()));
    // An authorization moved before closure is refused on replay.
    let mut early = events.clone();
    let auth = early.pop().unwrap();
    early.insert(4, auth);
    assert!(Campaign::replay(CampaignRules::default(), &early).is_err());
    // A capture for an undesignated session is refused.
    let bad = vec![CampaignEvent::CaptureRecorded { qualification: q(&day(0), true) }];
    assert_eq!(Campaign::replay(CampaignRules::default(), &bad).unwrap_err(), CampaignError::NotDesignated(day(0)));
    // A replay under different rules does not reach the same state.
    let stricter = CampaignRules { qualifying_target: 11, ..CampaignRules::default() };
    assert_eq!(Campaign::replay(stricter, &events[..20]).unwrap().state(), CampaignState::Collecting);
}
