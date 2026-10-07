//! D5 `move-v1` dispositions, end to end through the real drivers.
//!
//! The same plumbing `opportunity_disposition_tests.rs` proves for the
//! symbol-activity lifecycle, driven here under the default `move-v1`
//! (`docs/opportunity-lifecycle-move-v1-preregistration-2026-09-25.md`,
//! section 5): every opportunity reaches its rows with **exactly one**
//! canonical token -- `setup_inactivity | invalidated | session_boundary |
//! capacity_reached | capture_ended` -- or `still_open` as of settlement.
//!
//! Every scenario runs with capture on through `observe_both` / `finish_both`
//! (what `main.rs` calls), and rows are read back from the artifact.
//!
//! Also here: brief item b11, outcome anchors bind to the correct move,
//! because the anchor is created in this crate.

use std::path::{Path, PathBuf};

use backtest_metrics::opportunity::{OiConfig, OpportunityCloseReason};
use backtest_metrics::opportunity_outcome::{
    disposition_as_of_settlement, ClosureNotice, OpportunityDisposition, OpportunityOutcomeRow,
};
use chrono::{DateTime, Duration, TimeZone, Utc};
use market_data::events::ConsolidationStrategy;
use market_data::{ConsolidationEventKind, IgnitionEventKind, ScanEvent};

use super::{finish_both, observe_both, OutcomeDriver, OutcomeRecorder};
use crate::opportunity_shadow::{ShadowDriver, ShadowRecorder};
use crate::research_writer::CaptureMarker;

/// 2026-09-14T13:40:00Z = 09:40 EDT: regular session, settlement windows
/// finish well before the 20:00Z session end.
fn at(s: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 14, 13, 40, 0).unwrap() + Duration::seconds(s)
}

fn ign(symbol: &str, t: DateTime<Utc>, price: f64, kind: IgnitionEventKind) -> ScanEvent {
    ScanEvent::IgnitionEvent {
        symbol: symbol.into(),
        timestamp: t,
        price,
        kind,
    }
}
fn confirmed(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ign(symbol, t, price, IgnitionEventKind::FollowThroughConfirmed)
}
fn rejected(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ign(symbol, t, price, IgnitionEventKind::FollowThroughRejected)
}
fn candidate(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ign(symbol, t, price, IgnitionEventKind::CandidateOpened)
}
fn micro(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ScanEvent::ConsolidationEvent {
        symbol: symbol.into(),
        timestamp: t,
        price,
        kind: ConsolidationEventKind::EntryTriggered,
        strategy: ConsolidationStrategy::Micropullback,
    }
}
fn momentum(symbol: &str, t: DateTime<Utc>, qualifies: bool) -> ScanEvent {
    ScanEvent::MomentumUpdate {
        symbol: symbol.into(),
        timestamp: t,
        volume_confirmation: 0.7,
        structure: 0.6,
        ma_slope: 0.5,
        wick_rejection: 0.8,
        overall: if qualifies { 0.7 } else { 0.3 },
        qualifies,
    }
}
/// An in-progress bar: a forward price for anchors and state for the move,
/// but never evidence under `move-v1`.
fn bar(symbol: &str, t: DateTime<Utc>, close: f64) -> ScanEvent {
    ScanEvent::BarUpdate {
        symbol: symbol.into(),
        timestamp: t,
        open: close,
        high: close,
        low: close,
        close,
        volume: 100,
        is_final: false,
        interval_secs: 60,
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "d5-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Live {
    shadow: ShadowDriver,
    outcomes: OutcomeDriver,
}

impl Live {
    fn new(dir: &Path, config: OiConfig) -> Self {
        let versions = config.versions();
        let shadow = ShadowDriver::new(config, ShadowRecorder::start(dir.to_path_buf()));
        let outcomes = OutcomeDriver::new(OutcomeRecorder::start(dir.to_path_buf()), &versions);
        Self { shadow, outcomes }
    }
    fn step(&mut self, event: &ScanEvent, now: DateTime<Utc>) {
        observe_both(&mut self.shadow, &mut self.outcomes, event, now);
    }
    fn finish(&mut self, now: DateTime<Utc>) {
        finish_both(&mut self.shadow, &mut self.outcomes, now);
    }
}

fn lines_of(dir: &Path, prefix: &str) -> Vec<String> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            name.starts_with(prefix) && name.ends_with(".ndjson")
        })
        .collect();
    files.sort();
    files
        .iter()
        .flat_map(|p| {
            std::fs::read_to_string(p)
                .unwrap()
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn rows(dir: &Path) -> Vec<OpportunityOutcomeRow> {
    lines_of(dir, "opportunity-outcomes-")
        .into_iter()
        .filter(|l| !l.contains("\"kind\""))
        .map(|l| serde_json::from_str(&l).expect("an outcome row"))
        .collect()
}

fn oi_markers(dir: &Path) -> Vec<CaptureMarker> {
    lines_of(dir, "opportunity-intelligence-markers-")
        .into_iter()
        .map(|l| serde_json::from_str(&l).expect("a capture marker"))
        .collect()
}

fn persisted_closures(dir: &Path) -> Vec<ClosureNotice> {
    oi_markers(dir)
        .into_iter()
        .filter(|m| m.kind == "opportunity_closed")
        .map(|m| serde_json::from_value(m.data.expect("payload")).expect("a closure notice"))
        .collect()
}

fn rows_for<'a>(rows: &'a [OpportunityOutcomeRow], symbol: &str) -> Vec<&'a OpportunityOutcomeRow> {
    rows.iter().filter(|r| r.symbol == symbol).collect()
}

/// The persisted closes reproduce every row's disposition offline, through
/// the same pure rule the live collector enforces.
fn assert_replayable(rows: &[OpportunityOutcomeRow], closures: &[ClosureNotice]) {
    for r in rows {
        let (d, _) =
            disposition_as_of_settlement(&r.opportunity_id, r.opened_at, r.anchor_at, closures);
        if r.opportunity_disposition == OpportunityDisposition::CaptureEnded
            && d == OpportunityDisposition::StillOpen
        {
            // Capture end is applied from `finish`, which the pure rule is
            // not given here; every other token must replay exactly.
            continue;
        }
        assert_eq!(d, r.opportunity_disposition, "replay disagrees for {r:?}");
    }
}

/// A background symbol whose evidence keeps arriving, so the receipt clock --
/// and with it expiry and ranking -- keeps moving, and it stays open.
fn tick_other(live: &mut Live, t: i64) {
    live.step(&confirmed("OTHER", at(t), 5.0), at(t));
}

// ---------------------------------------------------------------------------

/// A normal move close: evidence stops, the symbol keeps trading, and every
/// anchor of the move carries `setup_inactivity` dated `last_relevant + T_move`.
/// The anchors keep settling after the close (the close censors nothing).
#[test]
fn m1_normal_move_close_reaches_the_row_and_settlement_continues() {
    let dir = temp_dir("move-close");
    let mut live = Live::new(&dir, OiConfig::default());
    live.step(&confirmed("AAA", at(0), 10.0), at(0));
    live.step(&candidate("AAA", at(100), 10.1), at(100)); // last evidence
    for t in (10..=2_000).step_by(10) {
        live.step(&bar("AAA", at(t), 10.0 + t as f64 / 10_000.0), at(t));
        tick_other(&mut live, t);
    }
    live.finish(at(2_010));
    let rows = rows(&dir);
    let closures = persisted_closures(&dir);
    let aaa = rows_for(&rows, "AAA");
    assert!(
        aaa.len() >= 10,
        "anchors for every window while the move was open"
    );
    for r in &aaa {
        assert_eq!(
            r.opportunity_disposition,
            OpportunityDisposition::SetupInactivity
        );
        assert_eq!(r.opportunity_closed_at, Some(at(400)), "100 + T_move");
        assert_eq!(r.opportunity_close_observed_at, Some(at(400)));
        assert!(r.anchor_at < at(400), "no anchor after the move ended");
        // Settlement is independent of the close: every horizon was observed
        // from bars that arrived after the move had ended.
        assert!(
            r.returns.iter().all(|h| h.outcome.observed().is_some()),
            "{r:?}"
        );
        assert!(r.fully_observed);
    }
    let aaa_closes: Vec<&ClosureNotice> = closures.iter().filter(|c| c.symbol == "AAA").collect();
    assert_eq!(aaa_closes.len(), 1);
    assert_eq!(
        aaa_closes[0].reason,
        OpportunityCloseReason::SetupInactivity
    );
    assert_replayable(&rows, &closures);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Invalidation: a rejection followed by evidence silence reaches the rows as
/// `invalidated`, dated at the last positive evidence + `T_move`.
#[test]
fn m2_invalidation_reaches_the_row() {
    let dir = temp_dir("invalidated");
    let mut live = Live::new(&dir, OiConfig::default());
    live.step(&confirmed("AAA", at(0), 10.0), at(0));
    live.step(&rejected("AAA", at(200), 9.5), at(200));
    for t in (10..=1_800).step_by(10) {
        live.step(&bar("AAA", at(t), 9.5), at(t));
        tick_other(&mut live, t);
    }
    live.finish(at(1_810));
    let rows = rows(&dir);
    let aaa = rows_for(&rows, "AAA");
    assert!(!aaa.is_empty());
    for r in aaa {
        assert_eq!(
            r.opportunity_disposition,
            OpportunityDisposition::Invalidated
        );
        assert_eq!(
            r.opportunity_closed_at,
            Some(at(300)),
            "0 + T_move; the reject never refreshes"
        );
    }
    let json = lines_of(&dir, "opportunity-outcomes-").join("\n");
    assert!(json.contains("\"opportunityDisposition\":\"invalidated\""));
    assert_replayable(&rows, &persisted_closures(&dir));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Inactivity, both tokens. The same stream -- a confirmation, then bars
/// only -- gives `setup_inactivity` under move-v1 but `still_open` under
/// symbol-activity-v1, whose bars keep the container alive; that lifecycle's
/// own `inactivity` token still flows when every event stops, and a
/// schema <= 2 row carrying it still parses.
#[test]
fn m3_inactivity_under_both_lifecycles() {
    let run = |config: OiConfig, bars_continue: bool| {
        let dir = temp_dir("inactivity");
        let mut live = Live::new(&dir, config);
        live.step(&confirmed("AAA", at(0), 10.0), at(0));
        for t in (10..=1_800).step_by(10) {
            if bars_continue || t < 60 {
                live.step(&bar("AAA", at(t), 10.0), at(t));
            }
            tick_other(&mut live, t);
        }
        live.finish(at(1_810));
        let out: Vec<OpportunityDisposition> = rows_for(&rows(&dir), "AAA")
            .iter()
            .map(|r| r.opportunity_disposition)
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        out
    };
    let moving = run(OiConfig::default(), true);
    assert!(moving
        .iter()
        .all(|d| *d == OpportunityDisposition::SetupInactivity));
    let legacy_trading = run(OiConfig::symbol_activity_v1(), true);
    assert!(
        legacy_trading.iter().all(|d| matches!(
            d,
            OpportunityDisposition::StillOpen | OpportunityDisposition::CaptureEnded
        )),
        "bars kept the symbol-activity container alive: {legacy_trading:?}"
    );
    let legacy_quiet = run(OiConfig::symbol_activity_v1(), false);
    assert!(legacy_quiet
        .iter()
        .all(|d| *d == OpportunityDisposition::Inactivity));

    let legacy: OpportunityDisposition = serde_json::from_str("\"inactivity\"").unwrap();
    assert_eq!(legacy, OpportunityDisposition::Inactivity);
}

/// Session boundary at 04:00 ET, on the event path and the expiry path; and a
/// winter after-hours move crossing UTC midnight is NOT a boundary.
#[test]
fn m4_session_boundary_is_the_market_day() {
    // Event path: 03:58 EST move, same symbol at 04:00:30 EST.
    let dir = temp_dir("boundary-event");
    let mut live = Live::new(&dir, OiConfig::default());
    let t0 = Utc.with_ymd_and_hms(2026, 1, 16, 8, 58, 0).unwrap();
    live.step(&confirmed("AAA", t0, 5.0), t0);
    let t1 = t0 + Duration::seconds(150);
    live.step(&confirmed("AAA", t1, 5.1), t1);
    live.finish(t1 + Duration::seconds(10));
    let out = rows(&dir);
    let old = out.iter().find(|r| r.anchor_at == t0).unwrap();
    assert_eq!(
        old.opportunity_disposition,
        OpportunityDisposition::SessionBoundary
    );
    assert_eq!(old.opportunity_closed_at, Some(t1));
    let new = out.iter().find(|r| r.anchor_at == t1).unwrap();
    assert_ne!(new.opportunity_id, old.opportunity_id);
    let _ = std::fs::remove_dir_all(&dir);

    // Expiry path: the next processed event after a quiet 04:00 is another
    // symbol's; the close is never dated before an anchor.
    let dir = temp_dir("boundary-expiry");
    let mut live = Live::new(&dir, OiConfig::default());
    let t0 = Utc.with_ymd_and_hms(2026, 1, 16, 8, 57, 0).unwrap();
    live.step(&confirmed("AAA", t0, 5.0), t0);
    let t1 = t0 + Duration::seconds(60);
    live.step(&bar("AAA", t1, 5.0), t1); // ranked again: anchor at t1
    let found = Utc.with_ymd_and_hms(2026, 1, 16, 9, 10, 0).unwrap();
    live.step(&confirmed("OTHER", found, 1.0), found);
    live.finish(found + Duration::seconds(10));
    let rows = rows(&dir);
    for r in rows_for(&rows, "AAA") {
        assert_eq!(
            r.opportunity_disposition,
            OpportunityDisposition::SessionBoundary
        );
        assert!(
            r.opportunity_closed_at.unwrap() >= r.anchor_at,
            "D4-5 floor"
        );
        assert_eq!(r.opportunity_close_observed_at, Some(found));
    }
    let _ = std::fs::remove_dir_all(&dir);

    // Winter after-hours across UTC midnight (19:00 EST): no boundary.
    let dir = temp_dir("boundary-utc");
    let mut live = Live::new(&dir, OiConfig::default());
    let t0 = Utc.with_ymd_and_hms(2026, 1, 15, 23, 55, 0).unwrap();
    live.step(&confirmed("AAA", t0, 5.0), t0);
    for k in 1..=10 {
        let t = t0 + Duration::seconds(60 * k);
        live.step(&momentum("AAA", t, true), t);
    }
    live.finish(t0 + Duration::seconds(700));
    let closures = persisted_closures(&dir);
    assert_eq!(closures.len(), 1);
    assert_eq!(closures[0].reason, OpportunityCloseReason::CaptureEnded);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Capture end: a move still open at shutdown gives its anchors
/// `capture_ended`; a move already closed keeps its earlier token.
#[test]
fn m5_capture_end() {
    let dir = temp_dir("capture-end");
    let mut live = Live::new(&dir, OiConfig::default());
    live.step(&confirmed("AAA", at(0), 10.0), at(0));
    live.step(&confirmed("BBB", at(0), 20.0), at(0));
    for t in (10..=400).step_by(10) {
        live.step(&momentum("BBB", at(t), true), at(t)); // BBB's move continues
    }
    live.finish(at(410)); // AAA closed at 300, BBB still open
    let rows = rows(&dir);
    assert!(rows_for(&rows, "AAA")
        .iter()
        .all(|r| r.opportunity_disposition == OpportunityDisposition::SetupInactivity));
    let bbb = rows_for(&rows, "BBB");
    assert!(!bbb.is_empty());
    assert!(bbb
        .iter()
        .all(|r| r.opportunity_disposition == OpportunityDisposition::CaptureEnded));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Capacity: an evicted move's anchors carry `capacity_reached`, and the
/// eviction marker is still written.
#[test]
fn m6_capacity() {
    let config = OiConfig {
        supported_symbol_universe: 2,
        bound_safety_num: 1,
        bound_safety_den: 1,
        max_rank_cohort: 2,
        ..OiConfig::default()
    };
    let dir = temp_dir("capacity");
    let mut live = Live::new(&dir, config);
    live.step(&confirmed("AAA", at(0), 10.0), at(0));
    live.step(&confirmed("BBB", at(40), 5.0), at(40));
    live.step(&confirmed("CCC", at(50), 7.0), at(50)); // evicts AAA
    live.finish(at(60));
    let rows = rows(&dir);
    let aaa = rows_for(&rows, "AAA");
    assert!(!aaa.is_empty());
    assert!(aaa
        .iter()
        .all(|r| r.opportunity_disposition == OpportunityDisposition::CapacityReached));
    assert!(oi_markers(&dir)
        .iter()
        .any(|m| m.kind == "opportunity_capacity_reached"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A repeated terminal event yields exactly one disposition: after the move
/// closes, further rejections, bars and level readings for the symbol neither
/// re-open it nor close anything again, and a duplicate or conflicting notice
/// applied while the rows are still settling changes nothing.
#[test]
fn m7_repeated_terminal_event_is_exactly_one_disposition() {
    let dir = temp_dir("repeat");
    let mut live = Live::new(&dir, OiConfig::default());
    let mut seen: Vec<ClosureNotice> = Vec::new();
    // `observe_both`, unrolled, so the step's closes are visible here.
    let mut step = |live: &mut Live, e: &ScanEvent, now: DateTime<Utc>| {
        live.outcomes.observe_price(e, now);
        let s = live.shadow.observe(e, now);
        live.outcomes.advance(&s.closures, &s.snapshots, now);
        seen.extend(s.closures);
    };
    step(&mut live, &bar("AAA", at(-1), 10.0), at(-1));
    step(&mut live, &momentum("AAA", at(0), true), at(0)); // edge: opens
    step(&mut live, &rejected("AAA", at(100), 9.9), at(100));
    for t in (110..=1_800).step_by(10) {
        step(&mut live, &confirmed("OTHER", at(t), 5.0), at(t));
        if t > 450 {
            step(&mut live, &rejected("AAA", at(t), 9.8), at(t));
            step(&mut live, &bar("AAA", at(t), 9.8), at(t));
            step(&mut live, &momentum("AAA", at(t), true), at(t)); // a level
        }
    }
    let aaa: Vec<ClosureNotice> = seen.iter().filter(|c| c.symbol == "AAA").cloned().collect();
    assert_eq!(aaa.len(), 1, "one close per opportunity, ever");
    // Momentum was not re-asserted between 0 and 450, so the last positive is
    // the edge at 0, and the rejection at 100 labels the ending.
    assert_eq!(aaa[0].reason, OpportunityCloseReason::Invalidated);
    assert_eq!(aaa[0].closed_at, at(300));
    // Rows anchored at 0..300 settle at +1320, i.e. up to 1,620 -- so some are
    // still outstanding at 1,500. A duplicate and then a conflicting notice
    // applied now must change none of them.
    let marked_before = live.outcomes.health().closure_anchors_marked;
    live.outcomes.apply_closures(&aaa);
    let mut conflicting = aaa[0].clone();
    conflicting.reason = OpportunityCloseReason::CaptureEnded;
    live.outcomes.apply_closures(&[conflicting]);
    assert_eq!(live.outcomes.health().closure_anchors_marked, marked_before);
    live.finish(at(1_810));

    let persisted = persisted_closures(&dir);
    assert_eq!(persisted.iter().filter(|c| c.symbol == "AAA").count(), 1);
    let rows = rows(&dir);
    let aaa_rows = rows_for(&rows, "AAA");
    assert!(!aaa_rows.is_empty());
    assert!(aaa_rows
        .iter()
        .all(|r| r.opportunity_disposition == OpportunityDisposition::Invalidated));
    let keys: std::collections::BTreeSet<(String, String)> = rows
        .iter()
        .map(|r| (r.opportunity_id.clone(), r.window_id.clone()))
        .collect();
    assert_eq!(keys.len(), rows.len(), "exactly one row per anchor");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Re-arm: after a close, the next opening edge opens a new move whose
/// anchors are its own. The first move's rows keep its token; the second's
/// carry theirs.
#[test]
fn m8_rearm_after_close() {
    let dir = temp_dir("rearm");
    let mut live = Live::new(&dir, OiConfig::default());
    live.step(&confirmed("AAA", at(0), 10.0), at(0));
    for t in (10..=2_400).step_by(10) {
        tick_other(&mut live, t);
        live.step(&bar("AAA", at(t), 10.0 + t as f64 / 10_000.0), at(t));
        if t == 600 {
            live.step(&micro("AAA", at(t), 10.1), at(t)); // re-arm: a new edge
        }
    }
    live.finish(at(2_410));
    let rows = rows(&dir);
    let closures = persisted_closures(&dir);
    let aaa = rows_for(&rows, "AAA");
    let ids: std::collections::BTreeSet<&str> =
        aaa.iter().map(|r| r.opportunity_id.as_str()).collect();
    assert_eq!(ids.len(), 2, "two moves");
    for r in &aaa {
        let (expected_open, expected_close) = if r.anchor_at < at(600) {
            (at(0), at(300))
        } else {
            (at(600), at(900))
        };
        assert_eq!(r.opened_at, Some(expected_open));
        assert_eq!(
            r.opportunity_disposition,
            OpportunityDisposition::SetupInactivity
        );
        assert_eq!(r.opportunity_closed_at, Some(expected_close));
    }
    assert_eq!(closures.iter().filter(|c| c.symbol == "AAA").count(), 2);
    assert_replayable(&rows, &closures);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An outcome is still settling after its opportunity closes, and the close
/// changes nothing about the measurement: rows equal the pipeline that drops
/// closes, apart from the three disposition fields.
#[test]
fn m9_outcome_still_settling_after_the_close_is_unchanged_by_it() {
    let run = |apply: bool| {
        let dir = temp_dir(if apply { "settle" } else { "settle-ctl" });
        let mut live = Live::new(&dir, OiConfig::default());
        let step = |live: &mut Live, e: &ScanEvent, now: DateTime<Utc>| {
            if apply {
                live.step(e, now);
            } else {
                live.outcomes.observe_price(e, now);
                let s = live.shadow.observe(e, now);
                live.outcomes.anchor_and_settle(&s.snapshots, now);
            }
        };
        step(&mut live, &confirmed("AAA", at(0), 10.0), at(0));
        for t in (10..=2_000).step_by(10) {
            step(
                &mut live,
                &bar("AAA", at(t), 10.0 + (t % 97) as f64 / 100.0),
                at(t),
            );
            step(&mut live, &confirmed("OTHER", at(t), 5.0), at(t));
        }
        live.finish(at(2_010));
        let out = rows(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        out
    };
    let with = run(true);
    let without = run(false);
    assert_eq!(
        with.len(),
        without.len(),
        "a close never adds or removes a row"
    );
    let mut compared = 0;
    for (a, b) in with.iter().zip(&without) {
        assert_eq!(a.opportunity_id, b.opportunity_id);
        // Both sides scrubbed: the control still applies the capture-end
        // closes in `finish_both`, so only the scoring path differs.
        let scrub = |r: &OpportunityOutcomeRow| {
            let mut r = r.clone();
            r.opportunity_disposition = OpportunityDisposition::StillOpen;
            r.opportunity_closed_at = None;
            r.opportunity_close_observed_at = None;
            r
        };
        assert_eq!(scrub(a), scrub(b));
        if a.symbol == "AAA" {
            assert_eq!(
                a.opportunity_disposition,
                OpportunityDisposition::SetupInactivity
            );
            assert_eq!(b.opportunity_disposition, OpportunityDisposition::StillOpen);
            compared += 1;
        }
    }
    assert!(compared > 5);
}

/// b11. Outcome anchors bind to the correct move: two successive moves on one
/// symbol, each anchor names the move open at its instant, and the first
/// move's close never touches the second move's rows.
#[test]
fn b11_outcome_anchors_bind_to_the_correct_move() {
    let dir = temp_dir("bind");
    let mut live = Live::new(&dir, OiConfig::default());
    let mut open_moves: Vec<(DateTime<Utc>, DateTime<Utc>)> = Vec::new(); // (open, anchors until)
    live.step(&confirmed("AAA", at(0), 10.0), at(0));
    open_moves.push((at(0), at(300)));
    for t in (10..=1_500).step_by(10) {
        tick_other(&mut live, t);
        match t {
            450 => {
                live.step(&confirmed("AAA", at(t), 11.0), at(t)); // move 2
                open_moves.push((at(450), at(1_510)));
            }
            _ if t > 450 && t % 120 == 0 => live.step(&candidate("AAA", at(t), 11.0), at(t)),
            _ => live.step(&bar("AAA", at(t), 10.5), at(t)),
        }
    }
    live.finish(at(1_510));
    let rows = rows(&dir);
    let aaa = rows_for(&rows, "AAA");
    let first_id = aaa
        .iter()
        .find(|r| r.anchor_at == at(0))
        .unwrap()
        .opportunity_id
        .clone();
    let second: Vec<&&OpportunityOutcomeRow> =
        aaa.iter().filter(|r| r.anchor_at >= at(450)).collect();
    assert!(!second.is_empty());
    for r in &aaa {
        let (opened, _) = open_moves
            .iter()
            .rev()
            .find(|(o, until)| r.anchor_at >= *o && r.anchor_at < *until)
            .unwrap_or_else(|| panic!("anchor at {} while no move was open", r.anchor_at));
        assert_eq!(
            r.opened_at,
            Some(*opened),
            "anchor {} bound to the wrong move",
            r.anchor_at
        );
    }
    for r in &second {
        assert_ne!(r.opportunity_id, first_id);
        assert_ne!(
            r.opportunity_disposition,
            OpportunityDisposition::SetupInactivity,
            "move 1's close must not reach move 2's rows"
        );
    }
    assert!(aaa
        .iter()
        .filter(|r| r.opportunity_id == first_id)
        .all(|r| r.opportunity_disposition == OpportunityDisposition::SetupInactivity));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The live health surface carries the D5 fields the preflight and the
/// completeness verdict read: lifecycle, closes by the new reasons, and the
/// duplicate-identity gate.
#[test]
fn health_reports_lifecycle_and_new_close_reasons() {
    let research = std::sync::Arc::new(crate::research_health::ResearchHealth::default());
    let mut driver = ShadowDriver::new(OiConfig::default(), None);
    research.set_engine(driver.engine_health().clone());
    let _ = driver.observe(&confirmed("AAA", at(0), 10.0), at(0));
    let _ = driver.observe(&confirmed("BBB", at(0), 10.0), at(0));
    let _ = driver.observe(&rejected("BBB", at(10), 9.0), at(10));
    let _ = driver.observe(&confirmed("CCC", at(400), 10.0), at(400));
    let report = research.report();
    let engine = report.opportunity_engine.expect("engine registered");
    assert_eq!(
        engine.lifecycle.as_deref(),
        Some("opportunity-lifecycle-move-v1")
    );
    assert_eq!(engine.closed_by_reason.setup_inactivity, 1);
    assert_eq!(engine.closed_by_reason.invalidated, 1);
    assert_eq!(engine.duplicate_identity_refused, 0);
    let json = serde_json::to_value(&engine).unwrap();
    for key in ["lifecycle", "duplicateIdentityRefused"] {
        assert!(
            json.get(key).is_some(),
            "{key} missing from opportunityEngine"
        );
    }
    for key in ["setupInactivity", "invalidated"] {
        assert!(
            json["closedByReason"].get(key).is_some(),
            "{key} missing from closedByReason"
        );
    }
}
