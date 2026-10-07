//! V2 tests. Every one exists to pin a preregistered invariant
//! (OI-V2-PREREGISTRATION-2026-09-19.md §26).

use super::*;
use crate::context::{
    IgnitionFeatures, IgnitionPhase, MomentumFeatures, PreDetectionContext, SignalContext,
};
use crate::opportunity::{Opportunity, OpportunityId, Regime};
use crate::signals::Strategy;
use chrono::{Duration, TimeZone, Utc};
use std::collections::BTreeMap;

fn at(s: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + s, 0).unwrap()
}

fn ctx(momentum: Option<MomentumFeatures>, ignition: Option<IgnitionFeatures>) -> SignalContext {
    SignalContext {
        schema_version: crate::context::SIGNAL_CONTEXT_SCHEMA_VERSION,
        symbol: "AAA".into(),
        session_date: "2026-09-19".into(),
        strategy: Strategy::IgnitionDetector,
        detected_at: at(0),
        captured_at: at(0),
        signal_price: 10.0,
        market: None,
        funnel: None,
        ignition,
        momentum,
        consolidation: None,
        halt: None,
        catalyst: None,
        pre_detection: Some(PreDetectionContext {
            price_1m_before: None,
            price_3m_before: None,
            price_5m_before: None,
            session_low_observed: Some(9.0),
            first_observed_price: Some(9.5),
            first_observed_at: Some(at(0)),
            move_before_detection_pct: Some(1.0),
            market_day: None,
            observation_started_at: None,
            baseline_truncated: None,
        }),
        episode_id: None,
    }
}

/// Like `ctx`, but lets a test set `detected_at` so it can build a context
/// that satisfies the real snapshot contract (`observed_at <= detected_at`).
/// The fixed-`at(0)` `ctx` helper cannot express that for any momentum bar
/// later than at(0), which is why several F2 tests used to describe states
/// production can never produce.
fn ctx_det(
    detected_at: DateTime<Utc>,
    momentum: Option<MomentumFeatures>,
    ignition: Option<IgnitionFeatures>,
) -> SignalContext {
    SignalContext { detected_at, captured_at: detected_at, ..ctx(momentum, ignition) }
}

fn momentum(observed: DateTime<Utc>, good: bool) -> MomentumFeatures {
    MomentumFeatures {
        overall: if good { 0.9 } else { 0.1 },
        volume_confirmation: if good { 0.85 } else { 0.1 },
        structure: if good { 0.9 } else { 0.1 },
        ma_slope: if good { 0.5 } else { -0.5 },
        wick_rejection: if good { 0.8 } else { 0.1 },
        qualifies: good,
        observed_at: observed,
    }
}

fn ignition(confirmations: u32, rejections: u32, candidates: u32) -> IgnitionFeatures {
    IgnitionFeatures {
        phase: IgnitionPhase::FollowThroughConfirmed,
        candidates_opened: candidates,
        confirmations,
        rejections,
        price_at_phase: 10.0,
        phase_at: at(0),
    }
}

fn opp(context: Option<SignalContext>) -> Opportunity {
    Opportunity {
        schema_version: crate::opportunity::OPPORTUNITY_SCHEMA_VERSION,
        id: OpportunityId { symbol: "AAA".into(), session_date: "2026-09-19".into(), sequence: 1 },
        symbol: "AAA".into(),
        session_date: "2026-09-19".into(),
        first_seen_at: at(0),
        opened_at: at(0),
        last_seen_at: at(300),
        opening_price: 10.0,
        latest_price: 10.5,
        first_detector: Strategy::IgnitionDetector,
        detectors_seen: BTreeMap::new(),
        raw_event_count: 5,
        detector_transitions: 0,
        episode_fragments: 1,
        invalidations_absorbed: 0,
        move_before_detection_pct: Some(1.0),
        move_from_start_pct: Some(5.0),
        max_move_pct: Some(6.0),
        min_move_pct: Some(-1.0),
        observed_high: 10.8,
        observed_low: 9.8,
        latest_context: context,
        detection_context: None,
        detection_context_emitted: false,
        closed_at: None,
        close_reason: None,
        last_relevant_at: None,
        last_evidence_kind: None,
        opened_phase: None,
    }
}

// -- THE headline regression -------------------------------------------------

/// **This test exists to fail if momentum ever becomes a mandatory V2 Early
/// core feature again.** That regression is precisely the V1 defect V2 was
/// built to remove, and it would be invisible in aggregate metrics.
#[test]
fn momentum_is_never_core() {
    assert!(
        !EARLY_V2_CORE.iter().any(|c| c.starts_with("momentum.")),
        "momentum must never appear in EARLY_V2_CORE -- that is the V1 defect"
    );
    assert!(
        !RISK_QUALITY_CORE.iter().any(|c| c.starts_with("momentum.")),
        "momentum must never gate RiskQuality either"
    );
}

/// A valid opportunity with NO momentum context whatsoever must still produce
/// an Early V2 score. Under V1 this returns `None`.
#[test]
fn an_opportunity_with_no_momentum_is_still_rankable() {
    let cfg = V2Config::default();
    let o = opp(Some(ctx(None, Some(ignition(2, 0, 1)))));
    let e = early_quality_v2(&o, at(300), MomentumAvailability::NeverSeenYet, None, &cfg);
    assert!(e.score.value.is_some(), "no momentum must not mean unrankable");
    assert_eq!(e.scoring_mode, ScoringMode::MomentumIndependent);
    assert!(e.score.core_missing.is_empty());
}

/// The same opportunity under V1 is unrankable -- the contrast this milestone
/// exists to remove.
#[test]
fn v1_cannot_score_what_v2_can() {
    let cfg = V2Config::default();
    let o = opp(Some(ctx(None, Some(ignition(2, 0, 1)))));
    let v1 = crate::opportunity::early_quality_score(&o, at(300));
    let v2 = early_quality_v2(&o, at(300), MomentumAvailability::NeverSeenYet, None, &cfg);
    assert!(v1.value.is_none(), "V1 gates on momentum core");
    assert!(v2.value_present(), "V2 must not");
}

trait ValuePresent {
    fn value_present(&self) -> bool;
}
impl ValuePresent for EarlyV2 {
    fn value_present(&self) -> bool {
        self.score.value.is_some()
    }
}

// -- missing != zero ---------------------------------------------------------

#[test]
fn missing_inputs_are_never_coerced_to_zero() {
    let cfg = V2Config::default();
    let o = opp(Some(ctx(None, None))); // no ignition at all
    let e = early_quality_v2(&o, at(300), MomentumAvailability::NeverSeenYet, None, &cfg);
    for c in &e.score.components {
        if c.raw.is_none() {
            assert!(c.transformed.is_none(), "{} fabricated a transformed value", c.feature);
            assert_eq!(c.contribution, 0.0);
        }
    }
    assert!(e.score.missing.iter().any(|m| m.starts_with("ignition.")));
    // and the absent weight is excluded from the denominator, not counted as 0
    assert!(e.score.present_weight < e.score.total_weight);
}

// -- bounded momentum --------------------------------------------------------

#[test]
fn momentum_contribution_is_bounded_by_beta() {
    let cfg = V2Config::default();
    let base = opp(Some(ctx(None, Some(ignition(2, 0, 1)))));
    let a = early_quality_v2(&base, at(300), MomentumAvailability::NeverSeenYet, None, &cfg)
        .score.value.unwrap();

    let withm = opp(Some(ctx(Some(momentum(at(300), true)), Some(ignition(2, 0, 1)))));
    let b = early_quality_v2(&withm, at(300), MomentumAvailability::CurrentlyAvailable, Some(at(300)), &cfg);
    let delta = b.score.value.unwrap() - a;
    assert!(
        delta.abs() <= BETA_MOMENTUM_MAX + 1e-9,
        "momentum moved Early by {delta}, exceeding beta {BETA_MOMENTUM_MAX}"
    );
    assert_eq!(b.scoring_mode, ScoringMode::MomentumInformed);
}

/// Strong early evidence without momentum must outrank weak early evidence
/// with perfect momentum. Preregistration §9-11 requires this explicitly.
#[test]
fn strong_early_without_momentum_beats_weak_early_with_momentum() {
    let cfg = V2Config::default();
    let mut strong = opp(Some(ctx(None, Some(ignition(3, 0, 1)))));
    strong.move_before_detection_pct = Some(0.0); // maximally early
    strong.invalidations_absorbed = 0;
    strong.episode_fragments = 1;

    let mut weak = opp(Some(ctx(Some(momentum(at(300), true)), Some(ignition(1, 5, 4)))));
    weak.move_before_detection_pct = Some(18.0); // move already consumed
    weak.invalidations_absorbed = 6;
    weak.episode_fragments = 4;

    let s = early_quality_v2(&strong, at(300), MomentumAvailability::NeverSeenYet, None, &cfg);
    let w = early_quality_v2(&weak, at(300), MomentumAvailability::CurrentlyAvailable, Some(at(300)), &cfg);
    assert!(
        s.score.value.unwrap() > w.score.value.unwrap(),
        "strong early {:?} must beat weak-early-plus-momentum {:?}",
        s.score.value, w.score.value
    );
}

// -- availability state ------------------------------------------------------

#[test]
fn stale_momentum_is_not_currently_available() {
    let cfg = V2Config::default();
    // STALE now means: momentum is absent from the CURRENT context, though it
    // was seen earlier. (It used to mean "the bar is >120s older than the score
    // instant", which double-counted context age -- see F2.)
    let o = opp(Some(ctx_det(at(240), None, Some(ignition(2, 0, 1)))));
    let a = momentum_availability(&o, at(300), true, &cfg);
    assert_eq!(a, MomentumAvailability::SeenPreviouslyButStale);
    let e = early_quality_v2(&o, at(300), a, Some(at(179)), &cfg);
    assert_eq!(e.scoring_mode, ScoringMode::MomentumIndependent, "stale must not enter Mode B");
}

/// The F2 worked example, taken from the 2026-09-17 artifact:
///   scoreTimestamp 00:00:30.013, detectedAt 23:57:27.756, observedAt 23:57:00
///   A = detectedAt - observedAt = 27.8s   (fresh, correctly attached)
///   B = scoreTimestamp - detectedAt = 182.3s
///   A + B = 210s
/// The old code compared A+B to FEATURE_FRESHNESS_SECS and called this stale.
/// V1 ranked it. 40,674 rows on that session were affected. It must now be
/// CurrentlyAvailable.
#[test]
fn data_fresh_but_context_aged_is_still_available() {
    let cfg = V2Config::default();
    let detected = at(28);
    let o = opp(Some(ctx_det(detected, Some(momentum(at(0), true)), Some(ignition(2, 0, 1)))));
    let score_at = detected + Duration::milliseconds(182_300);
    assert!(
        (score_at - at(0)).num_seconds() > cfg.feature_freshness_secs,
        "fixture must actually exceed the data-freshness constant end to end"
    );
    assert_eq!(
        momentum_availability(&o, score_at, true, &cfg),
        MomentumAvailability::CurrentlyAvailable,
        "A+B exceeding FEATURE_FRESHNESS_SECS must no longer imply stale"
    );
}

/// The snapshot contract's ordering half is re-asserted, so a malformed
/// context cannot smuggle a momentum bar in from the future.
#[test]
fn momentum_may_not_postdate_the_context_carrying_it() {
    let cfg = V2Config::default();
    let o = opp(Some(ctx_det(at(100), Some(momentum(at(101), true)), Some(ignition(2, 0, 1)))));
    assert_ne!(
        momentum_availability(&o, at(300), true, &cfg),
        MomentumAvailability::CurrentlyAvailable
    );
}

/// Context older than the bound is stale regardless of how fresh the bar was.
#[test]
fn context_beyond_the_age_bound_is_stale() {
    let cfg = V2Config::default();
    let o = opp(Some(ctx_det(at(0), Some(momentum(at(0), true)), Some(ignition(2, 0, 1)))));
    let past = at(cfg.context_freshness_secs);
    assert_eq!(
        momentum_availability(&o, past, true, &cfg),
        MomentumAvailability::CurrentlyAvailable,
        "exactly at the bound is still current"
    );
    let beyond = at(cfg.context_freshness_secs + 1);
    assert_eq!(
        momentum_availability(&o, beyond, true, &cfg),
        MomentumAvailability::SeenPreviouslyButStale
    );
}

/// CONTEXT_FRESHNESS_SECS is derived from the lifecycle, not chosen. If
/// `inactivity_secs` ever moves, this must move with it or the bound stops
/// meaning "as old as an open opportunity's context can possibly be".
#[test]
fn context_freshness_tracks_the_inactivity_timeout() {
    assert_eq!(CONTEXT_FRESHNESS_SECS, crate::episode::INACTIVITY_TIMEOUT_SECS);
    assert_eq!(V2Config::default().context_freshness_secs, CONTEXT_FRESHNESS_SECS);
    assert_ne!(
        CONTEXT_FRESHNESS_SECS,
        crate::context::FEATURE_FRESHNESS_SECS,
        "context age and data age are different questions with different bounds"
    );
}

#[test]
fn fresh_momentum_is_currently_available() {
    let cfg = V2Config::default();
    // Physically valid: bar closed at 240, context built at 240, scored at 300.
    let o = opp(Some(ctx_det(at(240), Some(momentum(at(240), true)), Some(ignition(2, 0, 1)))));
    assert_eq!(
        momentum_availability(&o, at(300), true, &cfg),
        MomentumAvailability::CurrentlyAvailable
    );
}

#[test]
fn never_seen_is_distinct_from_stale() {
    let cfg = V2Config::default();
    let o = opp(Some(ctx(None, Some(ignition(2, 0, 1)))));
    assert_eq!(momentum_availability(&o, at(300), false, &cfg), MomentumAvailability::NeverSeenYet);
    assert_eq!(
        momentum_availability(&o, at(300), true, &cfg),
        MomentumAvailability::SeenPreviouslyButStale
    );
}

// -- risk directionality -----------------------------------------------------

#[test]
fn risk_quality_decreases_with_drawdown() {
    let cfg = V2Config::default();
    let mut shallow = opp(Some(ctx(None, Some(ignition(2, 0, 1)))));
    shallow.observed_high = 10.6;
    shallow.latest_price = 10.5;
    let mut deep = shallow.clone();
    deep.observed_high = 13.0; // same price, much deeper drawdown

    let a = risk_quality(&shallow, &cfg).value.unwrap();
    let b = risk_quality(&deep, &cfg).value.unwrap();
    assert!(b <= a, "deeper drawdown must not raise RiskQuality ({b} vs {a})");
}

#[test]
fn risk_quality_decreases_with_instability() {
    let cfg = V2Config::default();
    let calm = opp(Some(ctx(None, Some(ignition(2, 0, 1)))));
    let mut choppy = calm.clone();
    choppy.invalidations_absorbed = 9;
    choppy.episode_fragments = 5;
    let a = risk_quality(&calm, &cfg).value.unwrap();
    let b = risk_quality(&choppy, &cfg).value.unwrap();
    assert!(b <= a, "more instability must not raise RiskQuality ({b} vs {a})");
}

#[test]
fn risk_quality_is_scoreable_without_halt_context() {
    let cfg = V2Config::default();
    let o = opp(Some(ctx(None, Some(ignition(2, 0, 1)))));
    let r = risk_quality(&o, &cfg);
    assert!(r.value.is_some(), "halt is optional; risk must remain scoreable");
    assert!(r.missing.iter().any(|m| m == "risk.haltProximity"));
}

// -- evidence confidence -----------------------------------------------------

#[test]
fn no_momentum_does_not_by_itself_mean_low_confidence() {
    let cfg = V2Config::default();
    let o = opp(Some(ctx(None, Some(ignition(2, 0, 1)))));
    let e = early_quality_v2(&o, at(300), MomentumAvailability::NeverSeenYet, None, &cfg);
    let c = evidence_confidence(&e.score, MomentumAvailability::NeverSeenYet, &cfg);
    assert!(c >= 0.80, "full universal evidence without momentum should reach ~0.85, got {c}");
}

// -- priority ----------------------------------------------------------------

#[test]
fn priority_weights_differ_by_regime_and_sum_to_one() {
    let cfg = V2Config::default();
    for (_, w) in &cfg.regime_weights {
        let s: f64 = w.iter().sum();
        assert!((s - 1.0).abs() < 1e-12, "regime weights must sum to 1, got {s}");
    }
    let o = opp(Some(ctx(None, Some(ignition(2, 0, 1)))));
    let e = early_quality_v2(&o, at(300), MomentumAvailability::NeverSeenYet, None, &cfg);
    let r = risk_quality(&o, &cfg);
    let early_p = opportunity_priority_v2(&e, Some(0.5), &r, Regime::EarlyEmerging, &cfg);
    let cont_p = opportunity_priority_v2(&e, Some(0.5), &r, Regime::ContinuationAcceleration, &cfg);
    assert_eq!(early_p.weight_early, 0.50);
    assert_eq!(cont_p.weight_continuation, 0.55);
}

#[test]
fn priority_contributions_reconstruct_the_value() {
    let cfg = V2Config::default();
    let o = opp(Some(ctx(None, Some(ignition(2, 0, 1)))));
    let e = early_quality_v2(&o, at(300), MomentumAvailability::NeverSeenYet, None, &cfg);
    let r = risk_quality(&o, &cfg);
    let p = opportunity_priority_v2(&e, Some(0.5), &r, Regime::EarlyEmerging, &cfg);
    let den = p.weight_early + p.weight_continuation + p.weight_risk;
    let raw = (p.early_contribution + p.continuation_contribution + p.risk_contribution) / den;
    assert!((raw - p.raw_priority).abs() < 1e-9, "contributions must reconstruct raw priority");
    let v = (p.raw_priority * p.confidence_multiplier).clamp(0.0, 1.0);
    assert!((v - p.value.unwrap()).abs() < 1e-9);
}

#[test]
fn priority_survives_a_missing_component() {
    let cfg = V2Config::default();
    let o = opp(Some(ctx(None, Some(ignition(2, 0, 1)))));
    let e = early_quality_v2(&o, at(300), MomentumAvailability::NeverSeenYet, None, &cfg);
    let r = risk_quality(&o, &cfg);
    let p = opportunity_priority_v2(&e, None, &r, Regime::EarlyEmerging, &cfg);
    assert!(p.value.is_some(), "missing continuation must not make priority unrankable");
    assert!(p.renormalised);
}

// -- determinism, ranges, fingerprint ---------------------------------------

#[test]
fn scores_are_deterministic_and_bounded() {
    let cfg = V2Config::default();
    let o = opp(Some(ctx(Some(momentum(at(300), true)), Some(ignition(2, 1, 2)))));
    for _ in 0..8 {
        let e = early_quality_v2(&o, at(300), MomentumAvailability::CurrentlyAvailable, Some(at(300)), &cfg);
        let r = risk_quality(&o, &cfg);
        let p = opportunity_priority_v2(&e, Some(0.4), &r, Regime::EarlyEmerging, &cfg);
        for v in [e.score.value, r.value, p.value].into_iter().flatten() {
            assert!((0.0..=1.0).contains(&v), "score out of range: {v}");
        }
        let e2 = early_quality_v2(&o, at(300), MomentumAvailability::CurrentlyAvailable, Some(at(300)), &cfg);
        assert_eq!(e.score.value, e2.score.value, "scoring must be deterministic");
    }
}

#[test]
fn config_fingerprint_changes_when_config_changes() {
    let a = V2Config::default();
    let base = a.fingerprint();
    assert!(base.starts_with("oi-v2-cfg-"));

    let mut b = V2Config::default();
    b.beta_momentum_max = 0.30;
    assert_ne!(base, b.fingerprint(), "beta change must change the fingerprint");

    let mut c = V2Config::default();
    c.regime_weights[0].1 = [0.6, 0.2, 0.2];
    assert_ne!(base, c.fingerprint(), "regime weight change must change the fingerprint");

    let mut d = V2Config::default();
    d.early_weights[0].1 = 0.25;
    assert_ne!(base, d.fingerprint(), "component weight change must change the fingerprint");

    // and it is stable across runs
    assert_eq!(base, V2Config::default().fingerprint());
}

#[test]
fn v2_fingerprint_is_independent_of_v1() {
    let v1 = crate::opportunity::OiConfig::default().fingerprint();
    let v2 = V2Config::default().fingerprint();
    assert_ne!(v1, v2);
    assert!(v1.starts_with("oi-cfg-"));
    assert!(v2.starts_with("oi-v2-cfg-"));
}

// -- persistence -------------------------------------------------------------

#[test]
fn rank_persistence_is_bounded_and_tracks_bands() {
    let mut p = RankPersistence::default();
    for i in 0..500 {
        p.observe(at(i * 30), Some(0.5), Some(3), 100);
    }
    assert_eq!(p.consecutive_top5, 500);
    assert_eq!(p.total_top5_windows, 500);
    assert_eq!(p.best_rank, Some(3));
    // bounded: the struct has no growable field
    assert_eq!(std::mem::size_of_val(&p), std::mem::size_of::<RankPersistence>());
}

#[test]
fn rank_persistence_resets_streaks_when_unranked() {
    let mut p = RankPersistence::default();
    p.observe(at(0), Some(0.9), Some(1), 100);
    assert_eq!(p.consecutive_top5, 1);
    p.observe(at(30), Some(0.9), None, 100);
    assert_eq!(p.consecutive_top5, 0, "an unranked window breaks the streak");
    assert_eq!(p.total_top5_windows, 1, "but the total is retained");
    assert_eq!(p.best_rank, Some(1), "and the best rank is not forgotten");
}

#[test]
fn persistence_has_zero_formula_weight_in_v2_0() {
    // Priority must not read persistence at all in V2.0.
    let cfg = V2Config::default();
    let o = opp(Some(ctx(None, Some(ignition(2, 0, 1)))));
    let e = early_quality_v2(&o, at(300), MomentumAvailability::NeverSeenYet, None, &cfg);
    let r = risk_quality(&o, &cfg);
    let p1 = opportunity_priority_v2(&e, Some(0.5), &r, Regime::EarlyEmerging, &cfg);
    let p2 = opportunity_priority_v2(&e, Some(0.5), &r, Regime::EarlyEmerging, &cfg);
    assert_eq!(p1.value, p2.value, "priority must not depend on any persistence state");
}

// -- causality ---------------------------------------------------------------

#[test]
fn no_future_information_enters_a_score() {
    let cfg = V2Config::default();
    // momentum observed AFTER the score instant must not be treated as fresh
    let o = opp(Some(ctx(Some(momentum(at(600), true)), Some(ignition(2, 0, 1)))));
    let a = momentum_availability(&o, at(300), false, &cfg);
    assert_ne!(
        a, MomentumAvailability::CurrentlyAvailable,
        "momentum observed in the future must never be currently available"
    );
}

#[test]
fn sub_second_freshness_boundaries() {
    let cfg = V2Config::default();
    // Re-aimed at CONTEXT age. `num_seconds()` truncates toward zero, so the
    // boundary sits between 300.999s (truncates to 300, current) and 301.0s.
    let detected = at(0);
    for (offset_ms, expect_fresh) in [
        (0i64, true),
        (1, true),
        (999, true),
        (300_000, true),       // exactly at the bound
        (300_999, true),       // truncates to 300
        (301_000, false),      // truncates to 301 -> stale
        (600_000, false),
        // Sub-second "future" is absorbed by truncation toward zero, exactly as
        // it is on the data-age side. A whole second in the future is not.
        (-1, true),
        (-999, true),
        (-1_000, false),       // truncates to -1 -> outside the range, fails closed
    ] {
        let o = opp(Some(ctx_det(detected, Some(momentum(detected, true)), Some(ignition(2, 0, 1)))));
        let score_at = detected + Duration::milliseconds(offset_ms);
        let a = momentum_availability(&o, score_at, true, &cfg);
        let fresh = a == MomentumAvailability::CurrentlyAvailable;
        assert_eq!(
            fresh, expect_fresh,
            "context offset {offset_ms}ms: expected fresh={expect_fresh}, got {a:?}"
        );
    }
}

#[test]
fn serialization_round_trips() {
    let cfg = V2Config::default();
    let o = opp(Some(ctx(Some(momentum(at(300), true)), Some(ignition(2, 1, 2)))));
    let e = early_quality_v2(&o, at(300), MomentumAvailability::CurrentlyAvailable, Some(at(300)), &cfg);
    let r = risk_quality(&o, &cfg);
    let p = opportunity_priority_v2(&e, Some(0.4), &r, Regime::EarlyEmerging, &cfg);
    for json in [
        serde_json::to_string(&e).unwrap(),
        serde_json::to_string(&r).unwrap(),
        serde_json::to_string(&p).unwrap(),
        serde_json::to_string(&cfg).unwrap(),
    ] {
        assert!(!json.is_empty());
    }
    let back: EarlyV2 = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
    assert_eq!(back.score.value, e.score.value);
    assert_eq!(back.momentum_availability, e.momentum_availability);
}

/// The F2 correction adds `context_freshness_secs` to the config, so the V2
/// fingerprint necessarily moves. Printed so the report can state both values
/// and so the frozen V2.0 replay outputs stay attributable to the OLD one.
#[test]
fn f2_fingerprint_is_reported() {
    println!("V2 config fingerprint after F2 correction: {}", V2Config::default().fingerprint());
    println!("context_freshness_secs = {}", V2Config::default().context_freshness_secs);
    println!("feature_freshness_secs = {}", V2Config::default().feature_freshness_secs);
}
