//! Opportunity-native outcome tests.
//!
//! The headline is `no_episode_equivalent_still_gets_a_row` and its siblings:
//! the whole measurement exists because episode-attached outcomes were missing
//! in a way that correlated with the score.

use super::*;
use chrono::TimeZone;
use std::sync::Arc;

fn at(s: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_789_344_000 + s, 0).unwrap()
}

fn session_end() -> DateTime<Utc> {
    at(86_400)
}

fn prov() -> AnchorProvenance {
    AnchorProvenance {
        measurement_version: OPPORTUNITY_OUTCOME_VERSION.into(),
        opportunity_schema: 2,
        feature_schema: 2,
        early_quality_model: "early-quality-v1-transparent".into(),
        continuation_model: "continuation-v1-transparent".into(),
        ranking: "opportunity-rank-v1".into(),
        score_policy: "score-policy-v2-core-gated-coverage-normalized".into(),
        config_fingerprint: "oi-cfg-test".into(),
    }
}

fn req(symbol: &str, anchor: DateTime<Utc>, price: f64) -> AnchorRequest {
    // One shared provenance per call is enough for tests; production shares
    // one per run.
    AnchorRequest {
        opportunity_id: format!("{symbol}:2026-09-17:1000"),
        window_id: "oiw-1".into(),
        symbol: symbol.into(),
        session_date: "2026-09-17".into(),
        anchor_at: anchor,
        signal_price: price,
        opened_at: Some(anchor - Duration::seconds(60)),
        session_end: session_end(),
        provenance: Arc::new(prov()),
    }
}

/// A close of the opportunity `req(symbol, anchor, ..)` belongs to (same id,
/// same `opened_at`), observed at `observed`, lifecycle-dated one second
/// earlier so the two instants are distinguishable in assertions.
fn notice(
    symbol: &str,
    anchor: DateTime<Utc>,
    reason: OpportunityCloseReason,
    observed: DateTime<Utc>,
) -> ClosureNotice {
    ClosureNotice {
        opportunity_id: format!("{symbol}:2026-09-17:1000"),
        symbol: symbol.into(),
        opened_at: anchor - Duration::seconds(60),
        closed_at: observed - Duration::seconds(1),
        close_observed_at: observed,
        reason,
    }
}

/// Feeds a dense forward path so nothing is censored for gaps.
fn feed(c: &mut OpportunityOutcomeCollector, symbol: &str, from: i64, to: i64, f: impl Fn(i64) -> f64) {
    let mut t = from;
    while t <= to {
        c.observe_price(symbol, at(t), f(t));
        t += 30;
    }
}

// --- the reason this measurement exists -------------------------------------

/// THE HEADLINE. An opportunity with no episode membership, no momentum, no
/// detector confluence and no forward target crossing still produces a row.
///
/// Under the episode-attached model this population (~55% of opportunities,
/// rising to 87% of Friday's top V2 decile) had a 0.00% excursion rate in
/// every score decile. Here it is indistinguishable from any other anchor,
/// because nothing about admission can see any of those things.
#[test]
fn no_episode_equivalent_still_gets_a_row() {
    let mut c = OpportunityOutcomeCollector::new();
    // Nothing in AnchorRequest names an episode, a score or a detector --
    // there is no field through which membership could influence admission.
    c.anchor(req("QUIET", at(0), 10.0));
    feed(&mut c, "QUIET", 30, 1_500, |_| 10.0); // flat: goes nowhere
    let rows = c.settle_due(at(OUTCOME_SETTLE_AFTER_SECS));
    assert_eq!(rows.len(), 1, "a no-episode anchor must still settle a row");
    let r = &rows[0];
    assert!(r.fully_observed, "and it must be fully observed, not censored");
    assert!(r.excursion.is_some(), "and carry an excursion");
    assert_eq!(r.returns.len(), OUTCOME_HORIZON_SECS.len());
    assert!(r.returns.iter().all(|h| !h.outcome.is_censored()));
}

/// Score-independence, stated as a property rather than an example: the same
/// price path produces byte-identical measurement whatever the opportunity
/// looked like, because the measurement cannot see the opportunity.
#[test]
fn measurement_is_independent_of_everything_but_price() {
    let mut rows = Vec::new();
    for (i, sym) in ["HIGHSCORE", "LOWSCORE", "NOMOMENTUM", "CLOSEDFAST"].iter().enumerate() {
        let mut c = OpportunityOutcomeCollector::new();
        c.anchor(req(sym, at(0), 10.0));
        if i == 3 {
            // This one's opportunity dies immediately. Must change nothing.
            c.apply_closure(&notice(sym, at(0), OpportunityCloseReason::Inactivity, at(10)));
        }
        feed(&mut c, sym, 30, 1_500, |t| 10.0 + t as f64 / 1_000.0);
        let mut r = c.settle_due(at(OUTCOME_SETTLE_AFTER_SECS));
        assert_eq!(r.len(), 1);
        rows.push(r.remove(0));
    }
    let first = &rows[0];
    for r in &rows[1..] {
        assert_eq!(r.returns, first.returns, "returns must not vary with the opportunity");
        assert_eq!(r.excursion, first.excursion, "nor excursion");
        assert_eq!(r.fully_observed, first.fully_observed);
        assert_eq!(r.censor_reasons, first.censor_reasons);
    }
}

/// An anchor keeps measuring after its opportunity closes, and after a
/// capacity eviction of that opportunity. Disposition is provenance only.
#[test]
fn anchor_survives_its_opportunity() {
    for reason in [
        OpportunityCloseReason::Inactivity,
        OpportunityCloseReason::SessionBoundary,
        OpportunityCloseReason::CapacityReached,
        OpportunityCloseReason::CaptureEnded,
    ] {
        let disposition = OpportunityDisposition::from(reason);
        let mut c = OpportunityOutcomeCollector::new();
        c.anchor(req("AAA", at(0), 10.0));
        c.observe_price("AAA", at(30), 10.1);
        c.apply_closure(&notice("AAA", at(0), reason, at(45)));
        // Prices keep arriving for the symbol long after the opportunity died.
        feed(&mut c, "AAA", 60, 1_500, |t| 10.0 + t as f64 / 500.0);
        let rows = c.settle_due(at(OUTCOME_SETTLE_AFTER_SECS));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].opportunity_disposition, disposition);
        assert!(rows[0].fully_observed, "closure must not censor price measurement");
        assert!(
            !rows[0].returns.iter().any(|h| h.outcome.is_censored()),
            "every horizon must still be observed"
        );
    }
}

// --- horizon semantics ------------------------------------------------------

/// The sampling rule: first observation at or AFTER `anchor + h`, never the
/// nearest and never interpolated.
#[test]
fn horizon_takes_the_first_observation_at_or_after() {
    let mut c = OpportunityOutcomeCollector::new();
    c.anchor(req("AAA", at(0), 100.0));
    // Nothing at exactly 30; the next print is at 44 and must fill the 30s slot.
    c.observe_price("AAA", at(29), 101.0);
    c.observe_price("AAA", at(44), 102.0);
    for t in (60..=1_400).step_by(30) {
        c.observe_price("AAA", at(t), 102.0);
    }
    let rows = c.settle_due(at(OUTCOME_SETTLE_AFTER_SECS));
    let h30 = rows[0].returns.iter().find(|h| h.horizon_secs == 30).unwrap();
    assert_eq!(h30.outcome, Observation::Observed(2.0), "44s print fills the 30s slot, not 29s");
}

/// Observations at or before the anchor are not forward information.
#[test]
fn pre_anchor_prices_are_ignored() {
    let mut c = OpportunityOutcomeCollector::new();
    c.anchor(req("AAA", at(100), 10.0));
    c.observe_price("AAA", at(50), 99.0); // before
    c.observe_price("AAA", at(100), 99.0); // at the anchor
    feed(&mut c, "AAA", 130, 1_400, |_| 10.0);
    let rows = c.settle_due(at(100 + OUTCOME_SETTLE_AFTER_SECS));
    let e = rows[0].excursion.unwrap();
    assert_eq!(e.mfe_pct, 0.0, "a pre-anchor spike must not become the MFE");
}

/// A horizon reaching past the session close says nothing about the symbol.
#[test]
fn horizon_past_session_close_is_session_ended() {
    let mut c = OpportunityOutcomeCollector::new();
    let mut r = req("AAA", at(0), 10.0);
    r.session_end = at(200); // only the 30/60/120 horizons fit
    c.anchor(r);
    feed(&mut c, "AAA", 30, 200, |_| 10.0);
    let rows = c.settle_due(at(OUTCOME_SETTLE_AFTER_SECS));
    let row = &rows[0];
    for h in &row.returns {
        if h.horizon_secs > 200 {
            assert_eq!(
                h.outcome,
                Observation::Censored(CensorReason::SessionEnded),
                "horizon {} reaches past the close",
                h.horizon_secs
            );
        }
    }
    assert!(row.censor_reasons.contains(&CensorReason::SessionEnded));
    assert!(!row.fully_observed);
}

// --- excursion --------------------------------------------------------------

#[test]
fn excursion_tracks_extrema_and_their_timing() {
    let mut c = OpportunityOutcomeCollector::new();
    c.anchor(req("AAA", at(0), 100.0));
    // down to 98 at 60s, up to 110 at 300s, dense elsewhere
    let path: Vec<(i64, f64)> = vec![
        (30, 99.0),
        (60, 98.0),
        (120, 99.0),
        (180, 100.0),
        (240, 105.0),
        (300, 110.0),
        (360, 104.0),
    ];
    for (t, p) in path {
        c.observe_price("AAA", at(t), p);
    }
    for t in (420..=1_400).step_by(60) {
        c.observe_price("AAA", at(t), 104.0);
    }
    let rows = c.settle_due(at(OUTCOME_SETTLE_AFTER_SECS));
    let e = rows[0].excursion.unwrap();
    assert!((e.mfe_pct - 10.0).abs() < 1e-9);
    assert!((e.mae_pct - (-2.0)).abs() < 1e-9);
    assert_eq!(e.seconds_to_mfe, 300);
    assert_eq!(e.seconds_to_mae, 60);
    assert!(
        (e.drawdown_before_mfe_pct - (-2.0)).abs() < 1e-9,
        "the worst point reached BEFORE the best, got {}",
        e.drawdown_before_mfe_pct
    );
}

/// A path that only ever rises has no drawdown before its MFE.
#[test]
fn monotone_rise_has_no_drawdown_before_mfe() {
    let mut c = OpportunityOutcomeCollector::new();
    c.anchor(req("AAA", at(0), 100.0));
    feed(&mut c, "AAA", 30, 1_400, |t| 100.0 + t as f64 / 100.0);
    let rows = c.settle_due(at(OUTCOME_SETTLE_AFTER_SECS));
    let e = rows[0].excursion.unwrap();
    assert_eq!(e.drawdown_before_mfe_pct, 0.0);
    assert_eq!(e.mae_pct, 0.0);
}

// --- target crossings -------------------------------------------------------

#[test]
fn target_crossings_latch_the_first_time_only() {
    let mut c = OpportunityOutcomeCollector::new();
    c.anchor(req("AAA", at(0), 100.0));
    c.observe_price("AAA", at(60), 102.5); // crosses +2
    c.observe_price("AAA", at(120), 101.0); // falls back
    c.observe_price("AAA", at(180), 105.5); // crosses +5
    for t in (240..=1_400).step_by(60) {
        c.observe_price("AAA", at(t), 105.5);
    }
    let rows = c.settle_due(at(OUTCOME_SETTLE_AFTER_SECS));
    let tc = &rows[0].target_crossings;
    let get = |p: f64| tc.iter().find(|x| x.target_pct == p).unwrap().seconds_to;
    assert_eq!(get(2.0), Observation::Observed(60), "first crossing, not the later one");
    assert_eq!(get(5.0), Observation::Observed(180));
    assert_eq!(get(10.0), Observation::Observed(-1), "never crossed, fully observed");
}

/// A crossing we actually saw stays a fact even when the row is censored;
/// only its ABSENCE becomes uncertain.
#[test]
fn an_observed_crossing_survives_censoring() {
    let mut c = OpportunityOutcomeCollector::new();
    c.anchor(req("AAA", at(0), 100.0));
    c.observe_price("AAA", at(30), 103.0); // +3, crosses +2
    let rows = c.finish(at(60)); // capture ends immediately
    let tc = &rows[0].target_crossings;
    let get = |p: f64| tc.iter().find(|x| x.target_pct == p).unwrap().seconds_to;
    assert_eq!(get(2.0), Observation::Observed(30), "we saw it cross");
    assert_eq!(
        get(10.0),
        Observation::Censored(CensorReason::CaptureEnded),
        "we did not see it cross, and cannot claim it never did"
    );
}

// --- censoring --------------------------------------------------------------

#[test]
fn capture_end_censors_but_never_drops() {
    let mut c = OpportunityOutcomeCollector::new();
    c.anchor(req("AAA", at(0), 10.0));
    c.observe_price("AAA", at(30), 10.1);
    let rows = c.finish(at(60));
    assert_eq!(rows.len(), 1, "the row must exist");
    assert!(rows[0].censor_reasons.contains(&CensorReason::CaptureEnded));
    assert!(!rows[0].fully_observed);
    assert_eq!(c.outstanding(), 0);
}

#[test]
fn a_hole_in_the_path_is_a_data_gap() {
    let mut c = OpportunityOutcomeCollector::new();
    c.anchor(req("AAA", at(0), 10.0));
    c.observe_price("AAA", at(30), 10.0);
    // 400s of silence, well past OUTCOME_MAX_GAP_SECS
    feed(&mut c, "AAA", 430, 1_400, |_| 12.0);
    let rows = c.settle_due(at(OUTCOME_SETTLE_AFTER_SECS));
    assert!(rows[0].censor_reasons.contains(&CensorReason::DataGap));
    assert!(rows[0].excursion.is_none(), "extrema across an unobserved hole are unknown");
}

#[test]
fn a_single_forward_print_is_insufficient() {
    let mut c = OpportunityOutcomeCollector::new();
    c.anchor(req("AAA", at(0), 10.0));
    c.observe_price("AAA", at(30), 10.5);
    let rows = c.settle_due(at(OUTCOME_SETTLE_AFTER_SECS));
    assert_eq!(rows.len(), 1);
    assert!(rows[0].censor_reasons.contains(&CensorReason::InsufficientForwardData));
}

#[test]
fn no_forward_prices_at_all_still_produces_a_row() {
    let mut c = OpportunityOutcomeCollector::new();
    c.anchor(req("GHOST", at(0), 10.0));
    let rows = c.settle_due(at(OUTCOME_SETTLE_AFTER_SECS));
    assert_eq!(rows.len(), 1, "silence is censored, never dropped");
    assert_eq!(rows[0].observation_count, 0);
    assert!(rows[0].excursion.is_none());
    assert!(rows[0].returns.iter().all(|h| h.outcome.is_censored()));
}

// --- capacity ---------------------------------------------------------------

/// Capacity failure is explicit and distinct. It must never be reported as
/// `InsufficientForwardData`, which would make a queue bound look like market
/// behaviour -- the substance of defect 48-A.
#[test]
fn capacity_eviction_uses_its_own_censor_reason() {
    let mut c = OpportunityOutcomeCollector::new();
    // Fill to capacity, then force one more.
    for i in 0..MAX_OUTSTANDING_ANCHORS {
        c.anchor(req("AAA", at(i as i64 % 1_000), 10.0));
    }
    assert_eq!(c.outstanding(), MAX_OUTSTANDING_ANCHORS);
    let evicted = c.anchor(req("BBB", at(2_000), 10.0));
    assert_eq!(evicted.len(), 1, "the evicted anchor is returned, not dropped");
    assert!(evicted[0].censor_reasons.contains(&CensorReason::PendingCapacityReached));
    assert!(
        !evicted[0].censor_reasons.contains(&CensorReason::InsufficientForwardData)
            || evicted[0].observation_count < OUTCOME_MIN_OBSERVATIONS,
        "capacity must not masquerade as insufficient data"
    );
    assert_eq!(c.health().capacity_evictions, 1);
    assert_eq!(c.outstanding(), MAX_OUTSTANDING_ANCHORS);
}

/// Horizons that had already matured survive a capacity eviction.
#[test]
fn capacity_eviction_preserves_matured_horizons() {
    let mut c = OpportunityOutcomeCollector::new();
    c.anchor(req("AAA", at(0), 100.0));
    for t in (30..=200).step_by(30) {
        c.observe_price("AAA", at(t), 101.0);
    }
    // Drain it as though capacity had forced the issue.
    let rows = c.finish(at(300));
    let r = &rows[0];
    let get = |h: i64| r.returns.iter().find(|x| x.horizon_secs == h).unwrap().outcome;
    assert_eq!(get(30), Observation::Observed(1.0), "matured horizons survive");
    assert_eq!(get(120), Observation::Observed(1.0));
    assert!(get(1200).is_censored(), "and unmatured ones are censored, not zeroed");
}

#[test]
fn capacity_is_derived_not_picked() {
    // 180.00/s x 1320s x 1.25
    assert_eq!(OUTCOME_SETTLE_AFTER_SECS, 1_320);
    assert_eq!(MAX_OUTSTANDING_ANCHORS, 297_000);
    assert!(
        (MAX_OUTSTANDING_ANCHORS as u64) * 100
            >= SUPPORTED_ANCHOR_RATE_CENTI * OUTCOME_SETTLE_AFTER_SECS as u64,
        "capacity must hold the supported rate for a full settlement window"
    );
    // The peak anchor rate actually measured: 4,308 cohort / 30s cadence.
    let measured_peak_centi = 4_308 * 100 / 30;
    assert!(
        SUPPORTED_ANCHOR_RATE_CENTI > measured_peak_centi,
        "the supported rate must exceed the measured peak of {measured_peak_centi} centi/s"
    );
}

// --- indexing / correctness -------------------------------------------------

#[test]
fn a_price_touches_only_its_own_symbol() {
    let mut c = OpportunityOutcomeCollector::new();
    c.anchor(req("AAA", at(0), 10.0));
    c.anchor(req("BBB", at(0), 10.0));
    feed(&mut c, "AAA", 30, 1_400, |_| 11.0);
    let rows = c.settle_due(at(OUTCOME_SETTLE_AFTER_SECS));
    let a = rows.iter().find(|r| r.symbol == "AAA").unwrap();
    let b = rows.iter().find(|r| r.symbol == "BBB").unwrap();
    assert!(a.excursion.is_some());
    assert_eq!(b.observation_count, 0, "BBB saw none of AAA's prices");
}

#[test]
fn settlement_is_due_ordered_and_leaves_the_rest_outstanding() {
    let mut c = OpportunityOutcomeCollector::new();
    c.anchor(req("AAA", at(0), 10.0));
    c.anchor(req("AAA", at(600), 10.0));
    let rows = c.settle_due(at(OUTCOME_SETTLE_AFTER_SECS));
    assert_eq!(rows.len(), 1, "only the first anchor is due");
    assert_eq!(rows[0].anchor_at, at(0));
    assert_eq!(c.outstanding(), 1);
}

#[test]
fn indexes_never_disagree_after_settlement() {
    let mut c = OpportunityOutcomeCollector::new();
    for i in 0..50 {
        c.anchor(req("AAA", at(i), 10.0));
    }
    assert_eq!(c.health().symbols_tracked, 1);
    let rows = c.settle_due(at(1_000 + OUTCOME_SETTLE_AFTER_SECS));
    assert_eq!(rows.len(), 50);
    assert_eq!(c.outstanding(), 0);
    assert_eq!(c.health().symbols_tracked, 0, "the symbol index must empty with the map");
}

// --- schema -----------------------------------------------------------------

#[test]
fn a_row_round_trips_and_names_its_contract() {
    let mut c = OpportunityOutcomeCollector::new();
    c.anchor(req("AAA", at(0), 10.0));
    feed(&mut c, "AAA", 30, 1_400, |t| 10.0 + t as f64 / 1_000.0);
    let rows = c.settle_due(at(OUTCOME_SETTLE_AFTER_SECS));
    let json = serde_json::to_string(&rows[0]).unwrap();
    let back: OpportunityOutcomeRow = serde_json::from_str(&json).unwrap();

    // Identity, censoring and structure round-trip EXACTLY.
    assert_eq!(back.opportunity_id, rows[0].opportunity_id);
    assert_eq!(back.window_id, rows[0].window_id);
    assert_eq!(back.symbol, rows[0].symbol);
    assert_eq!(back.anchor_at, rows[0].anchor_at);
    assert_eq!(back.opened_at, rows[0].opened_at);
    assert_eq!(back.provenance, rows[0].provenance);
    assert_eq!(back.censor_reasons, rows[0].censor_reasons);
    assert_eq!(back.fully_observed, rows[0].fully_observed);
    assert_eq!(back.observation_count, rows[0].observation_count);
    assert_eq!(back.target_crossings, rows[0].target_crossings);

    // Floats round-trip to within 1 ULP, not bit-exactly, and the cause is
    // serde_json's PARSER, not this module: the workspace does not enable its
    // `float_roundtrip` feature, so parsing uses the fast path and can land one
    // unit in the last place away. `ryu` writes the exact shortest form, so the
    // artifact on disk is correct and a Python reader gets the exact value --
    // only a Rust reader sees the 1-ULP shift. Pinned by
    // `serde_json_float_parse_precision_is_the_cause` below. Negligible for
    // research, but reported rather than silently asserted away.
    for (b, o) in back.returns.iter().zip(&rows[0].returns) {
        assert_eq!(b.horizon_secs, o.horizon_secs);
        match (b.outcome, o.outcome) {
            (Observation::Observed(x), Observation::Observed(y)) => {
                assert!((x - y).abs() <= y.abs() * 1e-15, "{x} vs {y}");
            }
            (a, b2) => assert_eq!(a, b2),
        }
    }
    let (be, oe) = (back.excursion.unwrap(), rows[0].excursion.unwrap());
    assert!((be.mfe_pct - oe.mfe_pct).abs() <= oe.mfe_pct.abs() * 1e-15);
    assert_eq!(be.seconds_to_mfe, oe.seconds_to_mfe);
    assert_eq!(be.seconds_to_mae, oe.seconds_to_mae);
    assert!(json.contains(OPPORTUNITY_OUTCOME_VERSION), "the row names its contract");
    assert!(json.contains("\"opportunityId\""), "camelCase on the wire");
    assert!(json.contains("\"configFingerprint\""), "provenance joins back to the model");
}

/// The grid is this contract's own, not the episode contract's.
#[test]
fn the_grid_is_separate_from_the_episode_grid() {
    assert_eq!(OUTCOME_HORIZON_SECS, [30, 60, 120, 300, 600, 1200]);
    assert_ne!(
        OUTCOME_HORIZON_SECS.len(),
        crate::horizon::HORIZON_SECS.len(),
        "the two contracts must not silently converge"
    );
    assert_eq!(longest_outcome_horizon_secs(), 1_200);
}

// --- load -------------------------------------------------------------------

/// Representative load: one regular-session ranking window at the observed
/// peak cohort, settled through a full horizon grid.
///
/// Asserts the shape of the cost rather than a wall-clock number, which would
/// be a flaky assertion about the machine rather than about the code.
#[test]
fn representative_load_one_peak_window() {
    const COHORT: usize = 4_308; // the measured Friday peak
    let mut c = OpportunityOutcomeCollector::new();
    for i in 0..COHORT {
        c.anchor(req(&format!("S{i:05}"), at(0), 10.0));
    }
    assert_eq!(c.outstanding(), COHORT);
    assert_eq!(c.health().symbols_tracked, COHORT);

    // One price per symbol per 30s for the whole window.
    for t in (30..=1_320).step_by(30) {
        for i in 0..COHORT {
            c.observe_price(&format!("S{i:05}"), at(t), 10.0 + t as f64 / 10_000.0);
        }
    }
    let rows = c.settle_due(at(OUTCOME_SETTLE_AFTER_SECS));
    assert_eq!(rows.len(), COHORT, "every anchor settles exactly one row");
    assert!(rows.iter().all(|r| r.fully_observed), "and none is censored");
    assert_eq!(c.outstanding(), 0);
    assert_eq!(c.health().capacity_evictions, 0, "one peak window must not hit capacity");
    println!(
        "load: {} anchors, {} settled, peak outstanding {}, capacity {}",
        c.health().anchors_created,
        c.health().anchors_settled,
        c.health().peak_outstanding,
        c.health().capacity
    );
}

/// Steady state across a full settlement window at the supported rate is the
/// population the capacity was derived for, and must not evict.
#[test]
fn steady_state_at_the_measured_peak_does_not_evict() {
    // 40 windows of 4,308 = 172,320 outstanding, the measured-peak steady state.
    const COHORT: usize = 4_308;
    let windows = (OUTCOME_SETTLE_AFTER_SECS / 30) as usize;
    let mut c = OpportunityOutcomeCollector::new();
    for w in 0..windows {
        for i in 0..COHORT {
            c.anchor(req(&format!("S{i:05}"), at((w as i64) * 30), 10.0));
        }
    }
    let outstanding = c.outstanding();
    println!(
        "steady state: {outstanding} outstanding vs capacity {MAX_OUTSTANDING_ANCHORS} \
         ({:.1}% utilised)",
        100.0 * outstanding as f64 / MAX_OUTSTANDING_ANCHORS as f64
    );
    assert_eq!(c.health().capacity_evictions, 0, "the measured peak must not bind");
    assert!(outstanding < MAX_OUTSTANDING_ANCHORS);
}

/// Confirms the 1-ULP parse difference is serde_json's parser, not this
/// module: a bare f64 shows the same behaviour with no code of ours involved.
#[test]
fn serde_json_float_parse_precision_is_the_cause() {
    let v = 11.999999999999993_f64;
    let s = serde_json::to_string(&v).unwrap();
    let back: f64 = serde_json::from_str(&s).unwrap();
    println!("serialized {s}, parsed back {back:?}, exact = {}", back == v);
    assert_eq!(s, "11.999999999999993", "ryu writes the exact shortest form");
}

/// Measured memory footprint of one outstanding anchor, so the capacity
/// derivation can be costed rather than guessed.
#[test]
fn anchor_memory_footprint_is_measured() {
    let outstanding = std::mem::size_of::<Outstanding>();
    let request = std::mem::size_of::<AnchorRequest>();
    let key = std::mem::size_of::<AnchorKey>();
    // Heap: the five identity strings an anchor owns, at representative
    // lengths (opportunityId ~20, windowId ~8, symbol ~5, sessionDate 10,
    // plus the provenance strings, which dominate at ~140 bytes).
    // Provenance is shared via Arc, so only the per-anchor identity strings
    // are counted here.
    let heap_est = 20 + 8 + 5 + 10;
    let per_anchor = outstanding + key + 16 + heap_est; // +16 for the index entry
    println!(
        "anchor footprint: Outstanding={outstanding} B (incl. AnchorRequest={request} B),          key={key} B, heap~{heap_est} B => ~{per_anchor} B/anchor"
    );
    println!(
        "at capacity {}: ~{:.0} MB",
        MAX_OUTSTANDING_ANCHORS,
        (MAX_OUTSTANDING_ANCHORS * per_anchor) as f64 / 1e6
    );
    println!(
        "at the measured-peak steady state 172,320: ~{:.0} MB",
        (172_320 * per_anchor) as f64 / 1e6
    );
    // Breakdown of the 392 B: AnchorRequest 152 (four identity Strings at 24,
    // three timestamps, the price, and an 8-byte Arc to shared provenance),
    // horizon slots 96, target latches 48, extrema and timing 64, counters and
    // cursors ~32.
    assert!(
        outstanding <= 448,
        "an anchor must stay small -- provenance is shared, not repeated; got {outstanding} B"
    );
    // The assertion that actually matters: the whole outstanding set at
    // capacity must fit a budget a realtime process can hold alongside
    // everything else on the box.
    let at_capacity_mb = (MAX_OUTSTANDING_ANCHORS * per_anchor) as f64 / 1e6;
    assert!(
        at_capacity_mb < 256.0,
        "capacity x footprint must stay under 256 MB; got {at_capacity_mb:.0} MB"
    );
}

/// The measured window is closed by the CONTRACT, not by when the caller
/// happens to settle. Two identical price streams settled at different
/// cadences must produce identical rows.
#[test]
fn observation_stops_at_the_settlement_deadline() {
    let mut early = OpportunityOutcomeCollector::new();
    let mut late = OpportunityOutcomeCollector::new();
    for c in [&mut early, &mut late] {
        c.anchor(req("AAA", at(0), 100.0));
        feed(c, "AAA", 30, OUTCOME_SETTLE_AFTER_SECS, |_| 101.0);
        // A spike AFTER the deadline, which must not be measured.
        c.observe_price("AAA", at(OUTCOME_SETTLE_AFTER_SECS + 30), 200.0);
    }
    let a = early.settle_due(at(OUTCOME_SETTLE_AFTER_SECS));
    let b = late.settle_due(at(OUTCOME_SETTLE_AFTER_SECS + 600));
    assert_eq!(a[0].observation_count, b[0].observation_count, "cadence must not change the count");
    assert_eq!(a[0].excursion, b[0].excursion, "nor the excursion");
    let e = a[0].excursion.unwrap();
    assert!((e.mfe_pct - 1.0).abs() < 1e-9, "the post-deadline spike must not be the MFE");
}

// --- D4: disposition as of settlement ----------------------------------------
//
// Numbered after `docs/measurement-correctness-contract-2026-09-25.md` D4.6.
// These exercise the collector directly; the engine-to-collector plumbing
// (inactivity, session, capacity, capture end, invalidation, restart) is
// tested through the real drivers in `ws-server::opportunity_outcomes_tests`.

/// Dense forward path, one print per 30s from 30s to 1,410s, rising gently.
fn full_path(c: &mut OpportunityOutcomeCollector, symbol: &str) {
    feed(c, symbol, 30, 1_410, |t| 10.0 + t as f64 / 1_000.0);
}

/// A control row: same anchor, same prices, no close at all.
fn control_row() -> OpportunityOutcomeRow {
    let mut c = OpportunityOutcomeCollector::new();
    c.anchor(req("AAA", at(0), 10.0));
    full_path(&mut c, "AAA");
    c.settle_due(at(OUTCOME_SETTLE_AFTER_SECS)).remove(0)
}

/// Everything a disposition must never change: the measurement itself.
fn assert_measurement_equal(row: &OpportunityOutcomeRow, control: &OpportunityOutcomeRow) {
    assert_eq!(row.returns, control.returns, "horizons must not move with the disposition");
    assert_eq!(row.excursion, control.excursion, "nor excursion");
    assert_eq!(row.target_crossings, control.target_crossings, "nor targets");
    assert_eq!(row.censor_reasons, control.censor_reasons, "a close is never a censor");
    assert_eq!(row.fully_observed, control.fully_observed);
    assert_eq!(row.observation_count, control.observation_count);
}

/// The token set is the engine's close reasons plus `still_open`, spelled
/// identically, so no translation table exists to drift. Also pins the
/// deserialize-only alias and the dropped v1 token.
#[test]
fn d4_disposition_tokens_are_the_engines_close_reason_tokens() {
    for reason in [
        OpportunityCloseReason::Inactivity,
        OpportunityCloseReason::SessionBoundary,
        OpportunityCloseReason::CapacityReached,
        OpportunityCloseReason::CaptureEnded,
    ] {
        assert_eq!(
            serde_json::to_string(&reason).unwrap(),
            serde_json::to_string(&OpportunityDisposition::from(reason)).unwrap(),
            "{reason:?} must serialize to the same token on both types"
        );
    }
    assert_eq!(
        serde_json::to_string(&OpportunityDisposition::StillOpen).unwrap(),
        r#""still_open""#
    );
    // v1's never-written `capacity_evicted` reads as the same event.
    let alias: OpportunityDisposition = serde_json::from_str(r#""capacity_evicted""#).unwrap();
    assert_eq!(alias, OpportunityDisposition::CapacityReached);
    // v1's `closed` carried no reason, and accepting it would invent one.
    assert!(serde_json::from_str::<OpportunityDisposition>(r#""closed""#).is_err());
}

#[test]
fn d4_the_outcome_contract_is_v2() {
    assert_eq!(OPPORTUNITY_OUTCOME_VERSION, "opportunity-outcome-v2");
}

/// A v1 row -- every one of which says `still_open` -- still parses, and names
/// its version so a reader can treat that disposition as unknown.
#[test]
fn d4_a_v1_row_still_parses_and_is_distinguishable() {
    let mut row = control_row();
    row.provenance.measurement_version = "opportunity-outcome-v1".into();
    let json = serde_json::to_string(&row).unwrap();
    assert!(!json.contains("opportunityClosedAt"), "v1 rows never carried the close instants");
    let back: OpportunityOutcomeRow = serde_json::from_str(&json).unwrap();
    assert_eq!(back.opportunity_disposition, OpportunityDisposition::StillOpen);
    assert_eq!(back.provenance.measurement_version, "opportunity-outcome-v1");
    assert_eq!(back.opportunity_closed_at, None);
}

/// D4.6-6. No close at all: `still_open`, with no close instants.
#[test]
fn d4_6_still_open_when_nothing_closed() {
    let row = control_row();
    assert_eq!(row.opportunity_disposition, OpportunityDisposition::StillOpen);
    assert_eq!(row.opportunity_closed_at, None);
    assert_eq!(row.opportunity_close_observed_at, None);
}

/// D4.6-7. A close at anchor+100 reaches the row, and the row is otherwise
/// byte-identical to the no-close control. Extends
/// `measurement_is_independent_of_everything_but_price`.
#[test]
fn d4_7_disposition_before_settlement_changes_nothing_but_disposition() {
    let control = control_row();
    let mut c = OpportunityOutcomeCollector::new();
    c.anchor(req("AAA", at(0), 10.0));
    feed(&mut c, "AAA", 30, 90, |t| 10.0 + t as f64 / 1_000.0);
    c.apply_closure(&notice("AAA", at(0), OpportunityCloseReason::Inactivity, at(100)));
    feed(&mut c, "AAA", 120, 1_410, |t| 10.0 + t as f64 / 1_000.0);
    let row = c.settle_due(at(OUTCOME_SETTLE_AFTER_SECS)).remove(0);
    assert_eq!(row.opportunity_disposition, OpportunityDisposition::Inactivity);
    assert_eq!(row.opportunity_closed_at, Some(at(99)));
    assert_eq!(row.opportunity_close_observed_at, Some(at(100)));
    assert_measurement_equal(&row, &control);
}

/// D4.6-8. A close after the 300s horizon but before 1,200s still reaches the
/// row, and every horizon value equals the control's.
#[test]
fn d4_8_disposition_after_an_earlier_horizon_but_before_1200s() {
    let control = control_row();
    let mut c = OpportunityOutcomeCollector::new();
    c.anchor(req("AAA", at(0), 10.0));
    feed(&mut c, "AAA", 30, 630, |t| 10.0 + t as f64 / 1_000.0);
    c.apply_closure(&notice("AAA", at(0), OpportunityCloseReason::SessionBoundary, at(650)));
    feed(&mut c, "AAA", 660, 1_410, |t| 10.0 + t as f64 / 1_000.0);
    let row = c.settle_due(at(OUTCOME_SETTLE_AFTER_SECS)).remove(0);
    assert_eq!(row.opportunity_disposition, OpportunityDisposition::SessionBoundary);
    for h in &row.returns {
        assert!(!h.outcome.is_censored(), "horizon {} must still be observed", h.horizon_secs);
    }
    assert_measurement_equal(&row, &control);
}

/// D4.6-9. A close after settlement cannot reach a row already written, and
/// produces no second row.
#[test]
fn d4_9_close_after_settlement_leaves_the_written_row_alone() {
    let mut c = OpportunityOutcomeCollector::new();
    c.anchor(req("AAA", at(0), 10.0));
    full_path(&mut c, "AAA");
    let rows = c.settle_due(at(OUTCOME_SETTLE_AFTER_SECS));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].opportunity_disposition, OpportunityDisposition::StillOpen);
    let marked =
        c.apply_closure(&notice("AAA", at(0), OpportunityCloseReason::Inactivity, at(1_400)));
    assert_eq!(marked, 0);
    assert!(c.settle_due(at(10_000)).is_empty(), "no second row");
    assert!(c.finish(at(10_000)).is_empty());
}

/// D4.6-9, the cadence-free half. A row that is merely LATE to settle (no
/// event arrived at its deadline) must not absorb a close observed after its
/// deadline: the rule is on the observed instant, not on when `settle_due`
/// happened to run. The deadline itself is inclusive.
#[test]
fn d4_9_a_late_settlement_does_not_absorb_a_post_deadline_close() {
    let deadline = at(OUTCOME_SETTLE_AFTER_SECS);
    let mut on_time = OpportunityOutcomeCollector::new();
    on_time.anchor(req("AAA", at(0), 10.0));
    on_time.apply_closure(&notice("AAA", at(0), OpportunityCloseReason::Inactivity, deadline));
    assert_eq!(
        on_time.settle_due(deadline)[0].opportunity_disposition,
        OpportunityDisposition::Inactivity,
        "a close observed AT the deadline is known when the row settles"
    );

    let mut c = OpportunityOutcomeCollector::new();
    c.anchor(req("AAA", at(0), 10.0));
    full_path(&mut c, "AAA");
    let late = deadline + Duration::seconds(1);
    assert_eq!(
        c.apply_closure(&notice("AAA", at(0), OpportunityCloseReason::Inactivity, late)),
        0
    );
    let row = c.settle_due(at(OUTCOME_SETTLE_AFTER_SECS + 600)).remove(0);
    assert_eq!(row.opportunity_disposition, OpportunityDisposition::StillOpen);
}

/// D4.6-10. The same notice twice, then a conflicting one: first terminal wins
/// and the row count is unchanged.
#[test]
fn d4_10_duplicate_and_conflicting_notices_are_no_ops() {
    let mut c = OpportunityOutcomeCollector::new();
    c.anchor(req("AAA", at(0), 10.0));
    let first = notice("AAA", at(0), OpportunityCloseReason::Inactivity, at(310));
    assert_eq!(c.apply_closure(&first), 1);
    assert_eq!(c.apply_closure(&first), 0, "a duplicate is a no-op");
    let conflicting = notice("AAA", at(0), OpportunityCloseReason::CaptureEnded, at(400));
    assert_eq!(c.apply_closure(&conflicting), 0, "a later terminal never overwrites");
    full_path(&mut c, "AAA");
    let rows = c.settle_due(at(OUTCOME_SETTLE_AFTER_SECS));
    assert_eq!(rows.len(), 1, "notices never create or remove rows");
    assert_eq!(rows[0].opportunity_disposition, OpportunityDisposition::Inactivity);
    assert_eq!(rows[0].opportunity_close_observed_at, Some(at(310)));
    assert_eq!(c.health().closure_notices, 3);
    assert_eq!(c.health().closure_anchors_marked, 1);
}

/// D4.6-11. A notice with the right id but a different `opened_at` names a
/// different opportunity and does nothing; so does one for an anchor that
/// carries no `opened_at` at all.
#[test]
fn d4_11_id_and_opened_at_guard() {
    let mut c = OpportunityOutcomeCollector::new();
    c.anchor(req("AAA", at(0), 10.0));
    let mut wrong = notice("AAA", at(0), OpportunityCloseReason::Inactivity, at(310));
    wrong.opened_at += Duration::milliseconds(1);
    assert_eq!(c.apply_closure(&wrong), 0);

    let mut unbound = req("BBB", at(0), 10.0);
    unbound.opened_at = None;
    c.anchor(unbound);
    assert_eq!(
        c.apply_closure(&notice("BBB", at(0), OpportunityCloseReason::Inactivity, at(310))),
        0,
        "an anchor that cannot be bound is left alone rather than guessed at"
    );
    let rows = c.settle_due(at(OUTCOME_SETTLE_AFTER_SECS));
    assert!(rows.iter().all(|r| r.opportunity_disposition == OpportunityDisposition::StillOpen));
}

/// D4.6-12. Anchors from five windows of one opportunity all take the reason;
/// an anchor for the symbol's NEXT opportunity (same symbol, reopened later,
/// different id and `opened_at`) is untouched.
#[test]
fn d4_12_several_anchors_for_one_opportunity() {
    let mut c = OpportunityOutcomeCollector::new();
    let opened = at(-60);
    for w in 0..5 {
        let mut r = req("AAA", at(w * 30), 10.0);
        r.opened_at = Some(opened);
        r.window_id = format!("oiw-{w}");
        c.anchor(r);
    }
    let mut next = req("AAA", at(900), 10.0);
    next.opportunity_id = "AAA:2026-09-17:2000".into();
    next.opened_at = Some(at(800));
    c.anchor(next);

    let n = ClosureNotice {
        opportunity_id: "AAA:2026-09-17:1000".into(),
        symbol: "AAA".into(),
        opened_at: opened,
        closed_at: at(420),
        close_observed_at: at(425),
        reason: OpportunityCloseReason::Inactivity,
    };
    assert_eq!(c.apply_closure(&n), 5);
    let rows = c.finish(at(5_000));
    let (first, second): (Vec<_>, Vec<_>) =
        rows.iter().partition(|r| r.opportunity_id.ends_with(":1000"));
    assert_eq!(first.len(), 5);
    assert!(first.iter().all(|r| r.opportunity_disposition == OpportunityDisposition::Inactivity));
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].opportunity_disposition, OpportunityDisposition::StillOpen);
}

/// D4.6-14. A close and a settlement in the same step: applied first, the
/// close wins, as the drivers order it.
#[test]
fn d4_14_close_and_settlement_in_the_same_step() {
    let mut c = OpportunityOutcomeCollector::new();
    c.anchor(req("AAA", at(0), 10.0));
    full_path(&mut c, "AAA");
    let step = at(OUTCOME_SETTLE_AFTER_SECS);
    c.apply_closures(&[notice("AAA", at(0), OpportunityCloseReason::Inactivity, step)]);
    let rows = c.settle_due(step);
    assert_eq!(rows[0].opportunity_disposition, OpportunityDisposition::Inactivity);
}

/// D4.6-15. Applying a notice touches only its own symbol's anchors -- asserted
/// by counter, not wall clock -- however large the outstanding set.
#[test]
fn d4_15_a_notice_scans_only_its_own_symbol() {
    let mut c = OpportunityOutcomeCollector::new();
    for i in 0..5_000 {
        c.anchor(req(&format!("S{i:05}"), at(0), 10.0));
    }
    for w in 0..3 {
        c.anchor(req("AAA", at(w * 30), 10.0));
    }
    c.apply_closure(&notice("AAA", at(0), OpportunityCloseReason::Inactivity, at(310)));
    assert_eq!(c.health().closure_anchors_scanned, 3, "3 anchors for AAA, not 5,003");
    // A symbol with nothing outstanding scans nothing.
    c.apply_closure(&notice("NONE", at(0), OpportunityCloseReason::Inactivity, at(310)));
    assert_eq!(c.health().closure_anchors_scanned, 3);
}

/// The disposition counts cover every row writer, and sum to the rows written.
#[test]
fn d4_disposition_counts_cover_every_row_writer() {
    let mut c = OpportunityOutcomeCollector::new();
    c.anchor(req("AAA", at(0), 10.0));
    c.anchor(req("BBB", at(0), 10.0));
    c.anchor(req("CCC", at(600), 10.0));
    c.apply_closure(&notice("AAA", at(0), OpportunityCloseReason::Inactivity, at(310)));
    c.apply_closure(&notice("CCC", at(600), OpportunityCloseReason::CaptureEnded, at(700)));
    let settled = c.settle_due(at(OUTCOME_SETTLE_AFTER_SECS)); // AAA, BBB
    let ended = c.finish(at(700)); // CCC
    let h = c.health();
    assert_eq!(settled.len() + ended.len(), 3);
    assert_eq!(h.disposition_counts.total(), h.anchors_settled);
    assert_eq!(h.disposition_counts.inactivity, 1);
    assert_eq!(h.disposition_counts.still_open, 1);
    assert_eq!(h.disposition_counts.capture_ended, 1);
}

/// The pure replay rule agrees with the live collector on every case above:
/// visible, invisible, wrong `opened_at`, unbound, first-wins.
#[test]
fn d4_replay_rule_agrees_with_the_collector() {
    let notices = vec![
        notice("AAA", at(0), OpportunityCloseReason::Inactivity, at(310)),
        notice("AAA", at(0), OpportunityCloseReason::CaptureEnded, at(900)),
        notice("LATE", at(0), OpportunityCloseReason::Inactivity, at(1_321)),
    ];
    let cases = [
        ("AAA", Some(at(-60)), OpportunityDisposition::Inactivity),
        ("AAA", Some(at(-59)), OpportunityDisposition::StillOpen),
        ("AAA", None, OpportunityDisposition::StillOpen),
        ("LATE", Some(at(-60)), OpportunityDisposition::StillOpen),
        ("ZZZ", Some(at(-60)), OpportunityDisposition::StillOpen),
    ];
    for (symbol, opened_at, expected) in cases {
        let mut c = OpportunityOutcomeCollector::new();
        let mut r = req(symbol, at(0), 10.0);
        r.opened_at = opened_at;
        c.anchor(r);
        c.apply_closures(&notices);
        let live = c.finish(at(5_000)).remove(0).opportunity_disposition;
        let (pure, _) = disposition_as_of_settlement(
            &format!("{symbol}:2026-09-17:1000"),
            opened_at,
            at(0),
            &notices,
        );
        assert_eq!(live, expected, "{symbol} {opened_at:?}");
        assert_eq!(pure, live, "the replay rule must agree with the live collector");
    }
}
