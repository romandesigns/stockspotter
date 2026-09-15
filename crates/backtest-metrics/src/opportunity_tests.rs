//! Tests for Opportunity Intelligence V1, named after the invariant each one
//! pins. Grouped to match the milestone's §19 requirement list.

use super::*;
use chrono::TimeZone;
use market_data::events::ConsolidationStrategy;

fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_789_344_000 + secs, 0).unwrap()
}

fn ign(symbol: &str, t: DateTime<Utc>, price: f64, kind: IgnitionEventKind) -> ScanEvent {
    ScanEvent::IgnitionEvent { symbol: symbol.into(), timestamp: t, price, kind }
}
fn confirmed(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ign(symbol, t, price, IgnitionEventKind::FollowThroughConfirmed)
}
fn rejected(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ign(symbol, t, price, IgnitionEventKind::FollowThroughRejected)
}
fn momentum(symbol: &str, t: DateTime<Utc>, overall: f64, ma_slope: f64) -> ScanEvent {
    ScanEvent::MomentumUpdate {
        symbol: symbol.into(),
        timestamp: t,
        volume_confirmation: 0.7,
        structure: 0.6,
        ma_slope,
        wick_rejection: 0.8,
        overall,
        qualifies: overall >= 0.60,
    }
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
fn engine() -> OpportunityIntelligence {
    OpportunityIntelligence::new(OiConfig::default())
}

// --- A. Opportunity identity / collapse ------------------------------------

#[test]
fn a1_repeated_detector_events_remain_one_opportunity() {
    let mut oi = engine();
    for i in 0..50 {
        oi.observe(&confirmed("AAA", at(i), 10.0 + i as f64 * 0.01), at(i));
    }
    assert_eq!(oi.open_count(), 1, "50 confirmations must be one opportunity");
    let op = oi.open_opportunities().next().unwrap();
    assert_eq!(op.raw_event_count, 50);
    assert_eq!(op.id.sequence, 1);
}

#[test]
fn a2_inactivity_boundary_closes_the_opportunity() {
    let mut oi = engine();
    oi.observe(&confirmed("AAA", at(0), 10.0), at(0));
    let closed = oi.observe(&confirmed("BBB", at(400), 5.0), at(400));
    assert!(closed
        .iter()
        .any(|o| o.symbol == "AAA" && o.close_reason == Some(OpportunityCloseReason::Inactivity)));
}

#[test]
fn a3_a_subsequent_move_creates_sequence_plus_one() {
    let mut oi = engine();
    oi.observe(&confirmed("AAA", at(0), 10.0), at(0));
    oi.observe(&confirmed("ZZZ", at(400), 1.0), at(400));
    oi.observe(&confirmed("AAA", at(500), 12.0), at(500));
    let op = oi.open_opportunities().find(|o| o.symbol == "AAA").unwrap();
    assert_eq!(op.id.sequence, 2, "a new move after closure is sequence 2");
}

#[test]
fn a4_different_symbols_never_share_an_opportunity() {
    let mut oi = engine();
    oi.observe(&confirmed("AAA", at(0), 10.0), at(0));
    oi.observe(&confirmed("BBB", at(1), 20.0), at(1));
    assert_eq!(oi.open_count(), 2);
    let mut ids: Vec<String> = oi.open_opportunities().map(|o| o.id.as_key()).collect();
    ids.sort();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
}

#[test]
fn a5_session_boundary_closes_deterministically() {
    let mut oi = engine();
    let day1 = Utc.with_ymd_and_hms(2026, 9, 14, 19, 0, 0).unwrap();
    let day2 = Utc.with_ymd_and_hms(2026, 9, 15, 14, 0, 0).unwrap();
    oi.observe(&confirmed("AAA", day1, 10.0), day1);
    let closed = oi.observe(&confirmed("AAA", day2, 11.0), day2);
    assert!(closed
        .iter()
        .any(|o| o.close_reason == Some(OpportunityCloseReason::SessionBoundary)));
}

#[test]
fn a6_out_of_order_timestamps_cannot_invert_the_lifecycle() {
    let mut oi = engine();
    let opened = Utc.with_ymd_and_hms(2026, 9, 14, 0, 0, 0).unwrap();
    oi.observe(&confirmed("AAA", opened, 10.0), opened);
    let earlier = Utc.with_ymd_and_hms(2026, 9, 13, 23, 59, 0).unwrap();
    let closed = oi.observe(&confirmed("AAA", earlier, 9.0), earlier);
    for op in closed {
        assert!(
            op.closed_at.unwrap() >= op.opened_at,
            "closedAt {:?} precedes openedAt {:?}",
            op.closed_at,
            op.opened_at
        );
    }
    for op in oi.finish(earlier) {
        assert!(op.closed_at.unwrap() >= op.opened_at);
    }
}

#[test]
fn a7_repeated_ignition_cannot_inflate_the_candidate_count() {
    // The defect this layer exists to fix: invalidation must NOT fragment the
    // rankable candidate the way EpisodeTracker's Rule 4 does.
    let mut oi = engine();
    for i in 0..20 {
        oi.observe(&confirmed("AAA", at(i * 3), 10.0), at(i * 3));
        oi.observe(&rejected("AAA", at(i * 3 + 1), 10.0), at(i * 3 + 1));
    }
    assert_eq!(oi.open_count(), 1, "confirm/reject churn must stay one opportunity");
    let op = oi.open_opportunities().next().unwrap();
    assert_eq!(op.invalidations_absorbed, 20);
    assert!(op.episode_fragments > 1, "fragment count stays measurable");
    assert_eq!(op.id.sequence, 1);
}

// --- B. Causality / no lookahead -------------------------------------------

#[test]
fn b10_future_prices_cannot_alter_an_earlier_early_quality_score() {
    let mut oi = engine();
    oi.observe(&momentum("AAA", at(0), 0.7, 0.66), at(0));
    oi.observe(&confirmed("AAA", at(1), 10.0), at(1));
    let before = early_quality_score(oi.open_opportunities().next().unwrap(), at(2));
    oi.observe(&confirmed("AAA", at(3), 99.0), at(3));
    let after = early_quality_score(oi.open_opportunities().next().unwrap(), at(2));
    assert_eq!(
        before.value, after.value,
        "early quality must not move because price later spiked"
    );
}

#[test]
fn b11_continuation_may_use_move_magnitude_without_rewriting_history() {
    let mut oi = engine();
    oi.observe(&confirmed("AAA", at(0), 10.0), at(0));
    let snap_a = continuation_confidence(oi.open_opportunities().next().unwrap(), at(1));
    let json_a = serde_json::to_string(&snap_a).unwrap();
    oi.observe(&confirmed("AAA", at(2), 13.0), at(2));
    let snap_b = continuation_confidence(oi.open_opportunities().next().unwrap(), at(3));
    // The earlier snapshot object is immutable; a later one may legitimately
    // differ because continuation is allowed to consume move magnitude.
    assert_eq!(json_a, serde_json::to_string(&snap_a).unwrap());
    assert_ne!(snap_a.value, snap_b.value);
}

#[test]
fn b13_replaying_a_prefix_is_identical_regardless_of_later_events() {
    let events: Vec<ScanEvent> =
        (0..12).map(|i| confirmed("AAA", at(i), 10.0 + i as f64 * 0.1)).collect();
    let mut short = engine();
    for e in events.iter().take(6) {
        short.observe(e, at(0));
    }
    let mut long = engine();
    for (i, e) in events.iter().enumerate() {
        long.observe(e, at(0));
        if i == 5 {
            let a = serde_json::to_string(short.open_opportunities().next().unwrap()).unwrap();
            let b = serde_json::to_string(long.open_opportunities().next().unwrap()).unwrap();
            assert_eq!(a, b, "prefix decisions must not depend on appended events");
        }
    }
}

// --- C. Regime classification ----------------------------------------------

#[test]
fn c14_identical_causal_state_always_produces_identical_regime() {
    let cfg = OiConfig::default();
    for _ in 0..5 {
        assert_eq!(classify_regime(Some(0.5), Some(0.1), &cfg), Regime::EarlyEmerging);
        assert_eq!(
            classify_regime(Some(8.0), Some(1.0), &cfg),
            Regime::ContinuationAcceleration
        );
    }
}

#[test]
fn c15_reversal_and_continuation_do_not_collapse_together() {
    let cfg = OiConfig::default();
    let reversal = classify_regime(Some(-12.0), Some(3.0), &cfg);
    let continuation = classify_regime(Some(12.0), Some(3.0), &cfg);
    assert_eq!(reversal, Regime::ReversalRecovery);
    assert_eq!(continuation, Regime::ContinuationAcceleration);
    assert_ne!(reversal, continuation);
}

#[test]
fn c16_missing_evidence_produces_unclassified() {
    let cfg = OiConfig::default();
    assert_eq!(classify_regime(None, Some(5.0), &cfg), Regime::Unclassified);
    // Still falling from a deep negative is not a recovery.
    assert_eq!(classify_regime(Some(-20.0), Some(-3.0), &cfg), Regime::Unclassified);
}

#[test]
fn c17_price_regime_bands_are_deterministic_and_reject_unknown() {
    let cfg = OiConfig::default();
    assert_eq!(classify_price_regime(0.30, &cfg).unwrap().band, 0);
    assert_eq!(classify_price_regime(0.75, &cfg).unwrap().band, 1);
    assert_eq!(classify_price_regime(2.50, &cfg).unwrap().band, 2);
    assert_eq!(classify_price_regime(10.0, &cfg).unwrap().band, 3);
    let top = classify_price_regime(50.0, &cfg).unwrap();
    assert_eq!(top.band, 4);
    assert!(top.upper.is_none());
    assert!(classify_price_regime(0.0, &cfg).is_none(), "non-positive price is unknown");
    assert!(classify_price_regime(f64::NAN, &cfg).is_none());
}

// --- D. Continuous feature preservation ------------------------------------

#[test]
fn d18_momentum_components_survive_without_boolean_reduction() {
    let mut oi = engine();
    oi.observe(&momentum("AAA", at(0), 0.73, 0.66), at(0));
    oi.observe(&confirmed("AAA", at(1), 10.0), at(1));
    let op = oi.open_opportunities().next().unwrap();
    let m = op.latest_context.as_ref().unwrap().momentum.as_ref().unwrap();
    assert!((m.overall - 0.73).abs() < 1e-9, "continuous overall retained");
    assert!((m.ma_slope - 0.66).abs() < 1e-9);
    assert!((m.volume_confirmation - 0.7).abs() < 1e-9);
    assert!((m.wick_rejection - 0.8).abs() < 1e-9);
}

#[test]
fn d20_features_are_refreshed_as_the_opportunity_evolves() {
    // The defect this pins, found in Phase E review: `latest_context` was set
    // once at open and never again, so momentum that arrived after the opening
    // detection was invisible to every later score. On a 200-event replay that
    // left 25 of 31 ranking rows with no momentum inputs at all -- absence
    // manufactured by wiring, not by the market.
    let mut oi = engine();
    oi.observe(&confirmed("AAA", at(0), 10.0), at(0));
    assert!(
        oi.open_opportunities().next().unwrap().latest_context.as_ref().unwrap().momentum.is_none(),
        "nothing was known about momentum at detection"
    );

    oi.observe(&momentum("AAA", at(5), 0.71, 0.64), at(5));
    oi.observe(&confirmed("AAA", at(6), 10.5), at(6));

    let op = oi.open_opportunities().next().unwrap();
    let m = op
        .latest_context
        .as_ref()
        .unwrap()
        .momentum
        .as_ref()
        .expect("momentum observed after detection must reach the latest surface");
    assert!((m.overall - 0.71).abs() < 1e-9);
    assert!((m.ma_slope - 0.64).abs() < 1e-9);

    // And the score that depends on it becomes available, rather than staying
    // permanently unscorable.
    let score = early_quality_score(op, at(6));
    assert!(score.value.is_some(), "the score must become computable once inputs exist");
}

#[test]
fn d21_the_detection_time_surface_survives_a_refresh() {
    // Earliness (§6) is a question about what was knowable *at detection*.
    // Refreshing a single field would have answered it with whatever arrived
    // later, which is the acausal reading of the same data.
    let mut oi = engine();
    oi.observe(&confirmed("AAA", at(0), 10.0), at(0));
    oi.observe(&momentum("AAA", at(5), 0.71, 0.64), at(5));
    oi.observe(&confirmed("AAA", at(6), 10.5), at(6));

    let op = oi.open_opportunities().next().unwrap();
    let detection = op.detection_context.as_ref().expect("detection surface retained");
    assert!(
        detection.momentum.is_none(),
        "the detection surface must not acquire information that arrived later"
    );
    assert_eq!(detection.detected_at, at(0), "and it must stay dated to detection");
    assert!(op.latest_context.as_ref().unwrap().momentum.is_some(), "while the latest one moves");
}

#[test]
fn d21b_the_detection_surface_is_persisted_once_not_every_window() {
    // Present on the first ranking row, absent afterwards. Absence there means
    // "carried forward", and the field's doc comment says so -- the same
    // change-based convention the discovery reduction uses.
    let mut oi = engine();
    oi.observe(&confirmed("AAA", at(0), 10.0), at(0));
    let first = oi.rank(at(0)).expect("first window");
    assert_eq!(first.len(), 1);
    assert!(first[0].detection_features.is_some(), "first row carries it");

    oi.observe(&confirmed("AAA", at(40), 10.4), at(40));
    let second = oi.rank(at(40)).expect("second window");
    assert_eq!(second.len(), 1);
    assert!(second[0].detection_features.is_none(), "later rows do not repeat it");
    assert!(second[0].features.is_some(), "but the evolving surface is still there");
}

#[test]
fn d19_unknown_is_absent_and_never_serialized_as_zero() {
    let mut oi = engine();
    oi.observe(&confirmed("AAA", at(0), 10.0), at(0));
    let op = oi.open_opportunities().next().unwrap();
    let score = early_quality_score(op, at(1));
    assert!(!score.missing.is_empty(), "missing features must be listed");
    for c in &score.components {
        if c.raw.is_none() {
            assert!(c.transformed.is_none(), "absent input must not become a value");
            assert_eq!(c.contribution, 0.0);
        }
    }
    assert!(serde_json::to_string(&score).unwrap().contains("\"missing\""));
}

#[test]
fn d22_detector_first_seen_and_order_survive_serialization() {
    let mut oi = engine();
    oi.observe(&confirmed("AAA", at(0), 10.0), at(0));
    oi.observe(&micro("AAA", at(5), 10.5), at(5));
    let op = oi.open_opportunities().next().unwrap();
    let json = serde_json::to_string(op).unwrap();
    let back: Opportunity = serde_json::from_str(&json).unwrap();
    assert_eq!(back.detectors_seen.len(), 2);
    let ignition = back.detectors_seen.get("IgnitionDetector").unwrap();
    let mp = back.detectors_seen.get("Micropullback").unwrap();
    assert_eq!(ignition.arrival_index, 1);
    assert_eq!(mp.arrival_index, 2);
    assert_eq!(ignition.first_seen_at, at(0));
    assert_eq!(mp.first_seen_at, at(5));
}

// --- E / F. Shadow scores ---------------------------------------------------

#[test]
fn e25_deterministic_config_produces_deterministic_score() {
    let mut oi = engine();
    oi.observe(&momentum("AAA", at(0), 0.7, 0.66), at(0));
    oi.observe(&confirmed("AAA", at(1), 10.0), at(1));
    let op = oi.open_opportunities().next().unwrap();
    let a = early_quality_score(op, at(2));
    let b = early_quality_score(op, at(2));
    assert_eq!(a.value, b.value);
    assert_eq!(
        serde_json::to_string(&a).unwrap(),
        serde_json::to_string(&b).unwrap()
    );
}

#[test]
fn e26_missing_features_follow_an_explicit_tested_policy() {
    let op = Opportunity {
        schema_version: OPPORTUNITY_SCHEMA_VERSION,
        id: OpportunityId {
            symbol: "X".into(),
            session_date: "2026-09-14".into(),
            sequence: 1,
        },
        symbol: "X".into(),
        session_date: "2026-09-14".into(),
        first_seen_at: at(0),
        opened_at: at(0),
        last_seen_at: at(0),
        opening_price: 1.0,
        latest_price: 1.0,
        first_detector: Strategy::IgnitionDetector,
        detectors_seen: BTreeMap::new(),
        raw_event_count: 1,
        detector_transitions: 0,
        episode_fragments: 1,
        invalidations_absorbed: 0,
        move_before_detection_pct: None,
        move_from_start_pct: None,
        max_move_pct: None,
        min_move_pct: None,
        observed_high: 1.0,
        observed_low: 1.0,
        latest_context: None,
        detection_context: None,
        detection_context_emitted: false,
        closed_at: None,
        close_reason: None,
    };
    let eq = early_quality_score(&op, at(1));
    assert!(eq.value.is_none(), "too little evidence must yield no score, not zero");
    assert_eq!(eq.present_inputs, 0);
    assert_eq!(eq.missing.len(), eq.required_inputs);
}

#[test]
fn e27_transform_layer_represents_nonlinear_relationships() {
    // volumeConfirmation == 1.0 must NOT be the maximum: the baseline showed
    // the extreme is not automatically superior.
    let t = Transform::Bins {
        bounds: vec![0.40, 0.60, 0.80, 1.00],
        weights: vec![0.0, 0.35, 0.80, 1.00, 0.70],
    };
    assert!(t.apply(0.90) > t.apply(1.00), "extreme must not dominate");
    assert_eq!(t.apply(0.10), 0.0);

    let p = Transform::Presence { threshold: 0.0 };
    assert_eq!(p.apply(0.0), 0.0, "zero slope is materially different from positive");
    assert_eq!(p.apply(0.33), 1.0);

    let pw = Transform::Piecewise { knots: vec![(0.0, 0.1), (10.0, 0.8), (15.0, 1.0)] };
    assert!((pw.apply(5.0) - 0.45).abs() < 1e-9);
    assert_eq!(pw.apply(-5.0), 0.1, "clamped below");
    assert_eq!(pw.apply(99.0), 1.0, "clamped above, never extrapolated");
}

#[test]
fn f29_continuation_is_independent_from_early_quality() {
    let mut oi = engine();
    oi.observe(&momentum("AAA", at(0), 0.7, 0.66), at(0));
    oi.observe(&confirmed("AAA", at(1), 10.0), at(1));
    let op = oi.open_opportunities().next().unwrap();
    let eq = early_quality_score(op, at(2));
    let cc = continuation_confidence(op, at(2));
    assert_ne!(eq.model_version, cc.model_version);
    assert!(
        !eq.components.iter().any(|c| c.feature.contains("moveFromStartPct")),
        "early quality must not consume move magnitude"
    );
    assert!(cc.components.iter().any(|c| c.feature.contains("priorMovePct")));
}

#[test]
fn f32_missing_inputs_do_not_become_fabricated_neutral_values() {
    let mut oi = engine();
    oi.observe(&confirmed("AAA", at(0), 10.0), at(0));
    let cc = continuation_confidence(oi.open_opportunities().next().unwrap(), at(1));
    for c in &cc.components {
        if c.raw.is_none() {
            assert_eq!(c.contribution, 0.0);
            assert!(c.transformed.is_none());
        }
    }
}

// --- G. Cross-sectional ranking --------------------------------------------

#[test]
fn g34_ranking_operates_on_opportunities_not_raw_events() {
    let mut oi = engine();
    for i in 0..40 {
        oi.observe(&momentum("AAA", at(i), 0.7, 0.66), at(i));
        oi.observe(&confirmed("AAA", at(i), 10.0), at(i));
    }
    let snaps = oi.rank(at(60)).expect("cadence elapsed");
    assert_eq!(snaps.len(), 1, "40 events on one symbol is one ranked candidate");
}

#[test]
fn g35_g39_one_opportunity_occupies_at_most_one_slot() {
    let mut oi = engine();
    for s in ["AAA", "BBB", "CCC"] {
        oi.observe(&momentum(s, at(0), 0.7, 0.66), at(0));
        oi.observe(&confirmed(s, at(1), 10.0), at(1));
        for i in 0..10 {
            oi.observe(&confirmed(s, at(2 + i), 10.0), at(2 + i));
        }
    }
    let snaps = oi.rank(at(60)).unwrap();
    let ranks: Vec<usize> = snaps.iter().filter_map(|s| s.early_quality_rank).collect();
    let mut deduped = ranks.clone();
    deduped.sort_unstable();
    deduped.dedup();
    assert_eq!(ranks.len(), deduped.len(), "no duplicate rank slots");
    assert_eq!(snaps.len(), 3);
}

#[test]
fn g36_ties_are_deterministic() {
    let scored = vec![
        (
            OpportunityId { symbol: "BBB".into(), session_date: "d".into(), sequence: 1 },
            1.0,
        ),
        (
            OpportunityId { symbol: "AAA".into(), session_date: "d".into(), sequence: 1 },
            1.0,
        ),
        (
            OpportunityId { symbol: "AAA".into(), session_date: "d".into(), sequence: 2 },
            1.0,
        ),
    ];
    let a = rank_cohort(scored.clone(), vec![], "w".into(), at(0), 100);
    let b = rank_cohort(scored, vec![], "w".into(), at(0), 100);
    assert_eq!(a.entries, b.entries);
    assert_eq!(a.entries[0].opportunity_id, "AAA:d:1");
    assert_eq!(a.entries[1].opportunity_id, "AAA:d:2");
    assert_eq!(a.entries[2].symbol, "BBB");
}

#[test]
fn g37_unscorable_candidates_are_unranked_not_ranked_worst() {
    let mut oi = engine();
    oi.observe(&momentum("AAA", at(0), 0.7, 0.66), at(0));
    oi.observe(&confirmed("AAA", at(1), 10.0), at(1));
    // No momentum for BBB -> early quality has too few inputs to score.
    oi.observe(&confirmed("BBB", at(1), 5.0), at(1));
    let snaps = oi.rank(at(60)).unwrap();
    let bbb = snaps.iter().find(|s| s.symbol == "BBB").unwrap();
    assert!(bbb.early_quality.value.is_none());
    assert!(bbb.early_quality_rank.is_none(), "unscorable must be unranked");
}

#[test]
fn g38_rank_metadata_is_stable_under_replay() {
    let build = || {
        let mut oi = engine();
        for s in ["AAA", "BBB"] {
            oi.observe(&momentum(s, at(0), 0.7, 0.66), at(0));
            oi.observe(&confirmed(s, at(1), 10.0), at(1));
        }
        oi.rank(at(60)).unwrap()
    };
    let a = build();
    let b = build();
    assert_eq!(
        serde_json::to_string(&a).unwrap(),
        serde_json::to_string(&b).unwrap()
    );
    assert!(a.iter().all(|s| s.window_id == "oiw-1"));
}

#[test]
fn g40_early_and_continuation_ranks_are_distinct_quantities() {
    let mut oi = engine();
    oi.observe(&momentum("AAA", at(0), 0.8, 0.66), at(0));
    oi.observe(&confirmed("AAA", at(1), 10.0), at(1));
    oi.observe(&momentum("BBB", at(0), 0.5, 0.0), at(0));
    oi.observe(&confirmed("BBB", at(1), 10.0), at(1));
    oi.observe(&confirmed("BBB", at(2), 13.0), at(2));
    let snaps = oi.rank(at(60)).unwrap();
    assert!(
        snaps
            .iter()
            .any(|s| s.early_quality_rank != s.continuation_rank),
        "the two rankings must be able to disagree"
    );
}

// --- H. Confluence ----------------------------------------------------------

#[test]
fn h41_h43_confluence_is_recorded_with_order_and_timing() {
    let mut oi = engine();
    oi.observe(&confirmed("AAA", at(0), 10.0), at(0));
    oi.observe(&micro("AAA", at(12), 10.5), at(12));
    let op = oi.open_opportunities().next().unwrap();
    assert_eq!(op.confluence_count(), 2);
    assert_eq!(op.confirmation_span_secs(), Some(12));
    assert_eq!(op.detectors_seen["IgnitionDetector"].arrival_index, 1);
    assert_eq!(op.detectors_seen["Micropullback"].arrival_index, 2);
}

#[test]
fn h44_confluence_alone_does_not_gate_anything() {
    // Confluence is one weighted term among several; a single-detector
    // opportunity must still be scorable and rankable.
    let mut oi = engine();
    oi.observe(&momentum("AAA", at(0), 0.7, 0.66), at(0));
    oi.observe(&confirmed("AAA", at(1), 10.0), at(1));
    let snaps = oi.rank(at(60)).unwrap();
    assert_eq!(snaps[0].confluence_count, 1);
    assert!(snaps[0].early_quality.value.is_some(), "one detector still scores");
}

// --- K. Bounds / saturation -------------------------------------------------

#[test]
fn k57_open_opportunities_stay_bounded_under_symbol_churn() {
    let cfg = OiConfig {
        supported_open_rate_centi: 100,
        inactivity_secs: 300,
        ..OiConfig::default()
    };
    let bound = cfg.max_open_opportunities();
    assert!(bound > 0);
    let mut oi = OpportunityIntelligence::new(cfg);
    for i in 0..(bound * 2 + 100) {
        oi.observe(&confirmed(&format!("S{i}"), at(0), 1.0), at(0));
    }
    assert!(
        oi.open_count() <= bound,
        "open set {} exceeded bound {}",
        oi.open_count(),
        bound
    );
    assert!(oi.health().capacity_evictions > 0);
}

#[test]
fn k63_saturation_is_distinguishable_from_ordinary_absence() {
    let cfg = OiConfig {
        supported_open_rate_centi: 100,
        inactivity_secs: 100,
        ..OiConfig::default()
    };
    let bound = cfg.max_open_opportunities();
    let mut oi = OpportunityIntelligence::new(cfg);
    let mut evicted = Vec::new();
    for i in 0..(bound + 50) {
        evicted.extend(oi.observe(&confirmed(&format!("S{i}"), at(0), 1.0), at(0)));
    }
    assert!(
        evicted
            .iter()
            .any(|o| o.close_reason == Some(OpportunityCloseReason::CapacityReached)),
        "capacity eviction must be explicitly labelled, never silent"
    );
    assert!(
        !evicted
            .iter()
            .any(|o| o.close_reason == Some(OpportunityCloseReason::Inactivity)),
        "eviction must not masquerade as inactivity"
    );
}

#[test]
fn k64_overload_then_normal_load_recovers() {
    let cfg = OiConfig {
        supported_open_rate_centi: 100,
        inactivity_secs: 100,
        ..OiConfig::default()
    };
    let bound = cfg.max_open_opportunities();
    let mut oi = OpportunityIntelligence::new(cfg);
    for i in 0..(bound + 200) {
        oi.observe(&confirmed(&format!("S{i}"), at(0), 1.0), at(0));
    }
    let during = oi.health().capacity_evictions;
    assert!(during > 0);
    // Quiet period: everything ages out normally.
    oi.observe(&confirmed("CALM", at(1_000), 1.0), at(1_000));
    assert_eq!(oi.open_count(), 1, "backlog drains by inactivity");
    oi.observe(&confirmed("CALM2", at(1_010), 1.0), at(1_010));
    assert_eq!(oi.health().capacity_evictions, during, "no further evictions");
}

#[test]
fn k65_unrelated_symbol_events_do_not_disturb_the_open_set() {
    // Structural guard: `open` is keyed by symbol, so an event for an unrelated
    // symbol is an O(1) miss rather than a scan of every open opportunity.
    let mut oi = engine();
    for i in 0..500 {
        oi.observe(&confirmed(&format!("S{i}"), at(0), 1.0), at(0));
    }
    let before = oi.open_count();
    oi.observe(
        &ScanEvent::BarUpdate {
            symbol: "UNRELATED".into(),
            timestamp: at(1),
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1,
            is_final: true,
            interval_secs: 60,
        },
        at(1),
    );
    assert_eq!(
        oi.open_count(),
        before,
        "unrelated event must not open or close anything"
    );
}

#[test]
fn config_bound_is_derived_and_fingerprint_is_stable() {
    let cfg = OiConfig::default();
    // 10.00/s x 300s x 1.25 = 3,750
    assert_eq!(cfg.max_open_opportunities(), 3_750);
    assert_eq!(cfg.fingerprint(), OiConfig::default().fingerprint());
    let other = OiConfig { inactivity_secs: 301, ..OiConfig::default() };
    assert_ne!(
        cfg.fingerprint(),
        other.fingerprint(),
        "a configuration change must change the fingerprint"
    );
    let v = cfg.versions();
    assert_eq!(v.regime_classifier, REGIME_CLASSIFIER_VERSION);
    assert!(v.config_fingerprint.starts_with("oi-cfg-"));
}

// --- P. Performance validation (§21) ---------------------------------------
//
// Numbers, not assurances. This programme has already shipped a 197x CPU
// regression into review once, and the thing that caught it was a measurement
// rather than a reading of the code.
//
// Reference load: production was observed sustaining ~650 ScanEvents/second
// during market hours (the figure `BROADCAST_CAPACITY` in ws-server is sized
// against). Budgets below are deliberately loose -- they exist to catch an
// order-of-magnitude regression, not to pin a wall-clock number that will
// drift with the machine.

/// Drives `oi` with `events_per_sec` events over `secs` of synthetic time,
/// spread across `symbols` distinct symbols, ranking on the real cadence.
///
/// Returns (events fed, snapshots produced, elapsed).
fn load(
    oi: &mut OpportunityIntelligence,
    events_per_sec: i64,
    secs: i64,
    symbols: i64,
) -> (usize, usize, std::time::Duration) {
    let total = events_per_sec * secs;
    let started = std::time::Instant::now();
    let mut snapshots = 0usize;
    for i in 0..total {
        let t = at(i / events_per_sec);
        let symbol = format!("S{}", i % symbols);
        let price = 10.0 + (i % 97) as f64 * 0.05;
        let event = match i % 4 {
            0 => confirmed(&symbol, t, price),
            1 => momentum(&symbol, t, 0.4 + (i % 5) as f64 * 0.1, 0.5),
            2 => rejected(&symbol, t, price),
            _ => micro(&symbol, t, price),
        };
        oi.observe(&event, t);
        if let Some(rows) = oi.rank(t) {
            snapshots += rows.len();
        }
    }
    (total as usize, snapshots, started.elapsed())
}

#[test]
fn p73_observed_production_load_stays_within_its_derived_bound() {
    let mut oi = engine();
    // 650 events/s for 120s across 400 symbols -- 78,000 events.
    let (fed, snapshots, elapsed) = load(&mut oi, 650, 120, 400);
    let h = oi.health();
    let per_event_ns = elapsed.as_nanos() / fed as u128;

    println!(
        "p73 observed load: {fed} events in {elapsed:?} ({per_event_ns} ns/event),          {snapshots} snapshots, peak_open={} evictions={}",
        h.peak_open_opportunities, h.capacity_evictions
    );

    assert_eq!(h.raw_events_observed, fed as u64);
    assert!(
        h.peak_open_opportunities <= oi.config().max_open_opportunities(),
        "peak {} must not exceed the derived bound {}",
        h.peak_open_opportunities,
        oi.config().max_open_opportunities()
    );
    assert_eq!(h.capacity_evictions, 0, "400 symbols must not reach a 3,750 bound");
    assert!(snapshots > 0, "ranking must actually have run");
    // ~1.5ms/event would be 650 events/s consuming a whole core. Two orders of
    // magnitude below that is the regression alarm.
    assert!(
        per_event_ns < 100_000,
        "{per_event_ns} ns/event is too slow for a research consumer on the dispatch path"
    );
}

#[test]
fn p74_double_observed_load_degrades_proportionally_not_catastrophically() {
    let mut single = engine();
    let (fed1, _, elapsed1) = load(&mut single, 650, 60, 400);
    let mut double = engine();
    let (fed2, _, elapsed2) = load(&mut double, 1_300, 60, 400);

    let ns1 = elapsed1.as_nanos() / fed1 as u128;
    let ns2 = elapsed2.as_nanos() / fed2 as u128;
    println!("p74: 1x = {ns1} ns/event, 2x = {ns2} ns/event");

    assert_eq!(double.health().capacity_evictions, 0);
    // Per-event cost must not grow with the rate. If it does, something is
    // scanning the open set per event, which is how the 197x regression
    // happened: the cost was superlinear and only showed up under real load.
    assert!(
        ns2 < ns1.max(1_000) * 4,
        "per-event cost must stay roughly flat as the rate doubles ({ns1} -> {ns2})"
    );
}

#[test]
fn p75_overload_is_bounded_and_counted_not_unbounded() {
    let mut oi = engine();
    let bound = oi.config().max_open_opportunities();
    // Far more distinct symbols than the bound, each arriving once, so the
    // open set is pushed past capacity rather than merely churned.
    let (fed, _, elapsed) = load(&mut oi, 650, 60, 20_000);
    let h = oi.health();
    println!(
        "p75 overload: {fed} events in {elapsed:?}, peak_open={} bound={bound} evictions={}",
        h.peak_open_opportunities, h.capacity_evictions
    );

    assert!(
        h.peak_open_opportunities <= bound,
        "open set must never exceed the bound, got {}",
        h.peak_open_opportunities
    );
    assert!(
        h.capacity_evictions > 0,
        "and reaching the bound must be counted, not silently absorbed"
    );
}

#[test]
fn p76_the_engine_recovers_after_overload() {
    let mut oi = engine();
    load(&mut oi, 650, 60, 20_000);
    let evictions_after_overload = oi.health().capacity_evictions;
    assert!(evictions_after_overload > 0, "precondition: overload happened");

    // Quiet period long enough for inactivity to retire the open set, then
    // ordinary load again.
    let quiet = at(60 + oi.config().inactivity_secs + 10);
    oi.observe(&confirmed("RECOVER", quiet, 10.0), quiet);
    assert!(
        oi.open_count() < 100,
        "inactivity must clear the overloaded set, {} still open",
        oi.open_count()
    );

    let before = oi.health().scores_emitted;
    for i in 0..600i64 {
        let t = quiet + Duration::seconds(i);
        oi.observe(&confirmed(&format!("R{}", i % 50), t, 10.0 + i as f64 * 0.01), t);
        oi.rank(t);
    }
    assert!(
        oi.health().scores_emitted > before,
        "scoring must resume normally after the overload"
    );
    assert_eq!(
        oi.health().capacity_evictions, evictions_after_overload,
        "and normal load must not keep evicting"
    );
}

#[test]
fn p77_per_opportunity_history_is_capped() {
    // The memory statement rests on two caps, not on typical behaviour: the
    // open set (p75) and whatever each opportunity retains. 50,000 events on
    // ONE symbol is the adversarial case for the second.
    let mut oi = engine();
    for i in 0..50_000i64 {
        let t = at(i / 500);
        oi.observe(&confirmed("AAA", t, 10.0 + (i % 100) as f64 * 0.01), t);
    }
    let op = oi.open_opportunities().next().unwrap();
    assert_eq!(op.raw_event_count, 50_000, "the count is exact");
    assert!(
        op.detectors_seen.len() <= 8,
        "but per-detector state is bounded by the number of detectors, not events"
    );
    // Counters, not collections: nothing here grows with event count.
    assert_eq!(oi.open_count(), 1);
}

#[test]
fn p78_cost_scales_with_the_open_set_and_that_ceiling_is_stated() {
    // The honest characterisation of this engine's cost, and the one p74
    // structurally cannot see: per-event work is flat in the event *rate* but
    // linear in the *open-set size*, because `expire_inactive` scans the open
    // set on every event.
    //
    // Measured in release on the development machine: ~1.5 us/event at 200
    // open, ~70 us/event at the 3,750 bound. At 650 events/second that is
    // ~0.1% of a core normally and ~4.5% saturated -- acceptable for a
    // subscriber that is off the dispatch path, and bounded because the open
    // set is bounded (p75).
    //
    // Pinned as a ratio rather than a wall-clock number so it survives a
    // different machine while still failing if the scan becomes quadratic.
    let cost = |symbols: i64| -> u128 {
        let mut oi = engine();
        let (fed, _, elapsed) = load(&mut oi, 650, 30, symbols);
        elapsed.as_nanos() / fed as u128
    };
    let small = cost(100).max(1);
    let large = cost(4_000).max(1);
    println!("p78: 100 symbols = {small} ns/event, 4000 symbols = {large} ns/event");

    // 40x the symbols. Linear would be ~40x the cost; the assertion allows
    // generous headroom but rules out quadratic blow-up.
    assert!(
        large < small * 400,
        "cost grew {}x for 40x the open set, which is worse than linear",
        large / small
    );
}

// --- M. Score comparability under feature missingness (Correction 3) --------
//
// The v1 policy summed `transformed x weight` over present features, so an
// absent feature contributed 0. That is not neutral: 0 sits at or below the
// floor of every transform in both models -- and *below* the observable floor
// of `continuation.priorMovePct` (0.10). Summing therefore imputed the worst
// observable value for anything unmeasured, and capped a candidate's
// attainable score at its coverage, so feature availability partly determined
// rank. These tests pin the replacement policy and the bias it removes.

/// Builds an opportunity with exactly the evidence a case needs, so coverage
/// classes can be constructed directly instead of coaxed out of an event
/// stream.
fn fixture(
    prior_move: Option<f64>,
    move_from_start: Option<f64>,
    mom: Option<crate::context::MomentumFeatures>,
) -> Opportunity {
    let ctx = SignalContext {
        schema_version: 1,
        symbol: "X".into(),
        session_date: "2026-09-14".into(),
        strategy: Strategy::IgnitionDetector,
        detected_at: at(0),
        captured_at: at(0),
        signal_price: 10.0,
        market: None,
        funnel: None,
        ignition: None,
        momentum: mom,
        consolidation: None,
        halt: None,
        catalyst: None,
        pre_detection: None,
        episode_id: None,
    };
    Opportunity {
        schema_version: OPPORTUNITY_SCHEMA_VERSION,
        id: OpportunityId {
            symbol: "X".into(),
            session_date: "2026-09-14".into(),
            sequence: 1,
        },
        symbol: "X".into(),
        session_date: "2026-09-14".into(),
        first_seen_at: at(0),
        opened_at: at(0),
        last_seen_at: at(0),
        opening_price: 10.0,
        latest_price: 10.0,
        first_detector: Strategy::IgnitionDetector,
        detectors_seen: BTreeMap::new(),
        raw_event_count: 1,
        detector_transitions: 0,
        episode_fragments: 1,
        invalidations_absorbed: 0,
        move_before_detection_pct: prior_move,
        move_from_start_pct: move_from_start,
        max_move_pct: None,
        min_move_pct: None,
        observed_high: 10.0,
        observed_low: 10.0,
        latest_context: Some(ctx),
        detection_context: None,
        detection_context_emitted: false,
        closed_at: None,
        close_reason: None,
    }
}

fn mom_features() -> crate::context::MomentumFeatures {
    crate::context::MomentumFeatures {
        overall: 0.55,
        volume_confirmation: 0.70,
        structure: 0.60,
        ma_slope: 0.42,
        wick_rejection: 0.80,
        qualifies: false,
        observed_at: at(0),
    }
}

/// The earliness transform, duplicated from the model so the test computes the
/// tie point rather than hard-coding a number derived by hand.
fn earliness_transform() -> Transform {
    Transform::Piecewise {
        knots: vec![(-5.0, 0.2), (0.0, 1.0), (2.0, 0.8), (5.0, 0.4), (10.0, 0.1), (20.0, 0.0)],
    }
}

/// m79. An absent feature is never treated as an observed zero.
#[test]
fn m79_an_absent_feature_is_never_treated_as_observed_zero() {
    // Momentum present, earliness absent: the single partial class EarlyQuality
    // can actually reach.
    let op = fixture(None, Some(0.0), Some(mom_features()));
    let eq = early_quality_score(&op, at(1));

    let earliness = eq
        .components
        .iter()
        .find(|c| c.feature == "earliness.priorMovePct")
        .expect("the absent feature is still recorded");
    assert_eq!(earliness.raw, None, "no fabricated input");
    assert_eq!(earliness.transformed, None, "no fabricated transform output");
    assert_eq!(earliness.contribution, 0.0, "it contributes nothing");
    assert!(eq.missing.iter().any(|m| m == "earliness.priorMovePct"));

    // And the comparable score is NOT the sum-with-zero figure, which is what
    // the v1 policy reported.
    let v1_policy_value = eq.raw_weighted;
    let comparable = eq.value.expect("momentum alone is scorable");
    assert!(
        comparable > v1_policy_value,
        "treating the absence as zero understated this candidate: {comparable} vs {v1_policy_value}"
    );
    assert!((eq.coverage - 0.85).abs() < 1e-12, "coverage is recorded, got {}", eq.coverage);
}

/// m80. Two otherwise identical candidates are not ranked differently solely
/// because one lacks a *non-informative* optional feature.
///
/// "Non-informative" is made precise rather than asserted: the optional
/// feature's transformed value is set to exactly the present-weighted mean of
/// the other inputs, so it carries no information the rest do not already
/// carry. Under the v1 policy the candidate lacking it still scored strictly
/// lower; under this policy they tie.
#[test]
fn m80_a_non_informative_absent_optional_feature_does_not_change_rank() {
    let without = fixture(None, Some(0.0), Some(mom_features()));
    let a = early_quality_score(&without, at(1));
    let mean_of_present = a.value.expect("momentum alone is scorable");

    // Invert the earliness piecewise on the (0.0, 1.0) -> (2.0, 0.8) segment to
    // find the prior move whose transform equals that mean exactly.
    let x = 2.0 * (1.0 - mean_of_present) / (1.0 - 0.8);
    let t = earliness_transform();
    assert!(
        (t.apply(x) - mean_of_present).abs() < 1e-9,
        "test setup: earliness({x}) must equal the mean {mean_of_present}"
    );

    let with = fixture(Some(x), Some(0.0), Some(mom_features()));
    let b = early_quality_score(&with, at(1));
    let full = b.value.expect("full coverage is scorable");

    assert!(
        (full - mean_of_present).abs() < 1e-9,
        "a non-informative optional feature must not shift the comparable score: \
         {full} vs {mean_of_present}"
    );
    // The v1 policy would have separated them, which is the bias being removed.
    assert!(
        b.raw_weighted > a.raw_weighted + 1e-9,
        "precondition: under the old sum policy these differed"
    );
    assert!((a.coverage - 0.85).abs() < 1e-12);
    assert!((b.coverage - 1.0).abs() < 1e-12);
}

/// m81. Insufficient evidence stays unranked, with the reason stated.
#[test]
fn m81_insufficient_evidence_remains_unranked_with_an_explicit_reason() {
    // Core missing: no momentum at all, so there is no quality to assess.
    let no_momentum = fixture(Some(1.0), Some(0.0), None);
    let eq = early_quality_score(&no_momentum, at(1));
    assert_eq!(eq.value, None);
    assert_eq!(eq.unrankable_reason, Some(UnrankableReason::CoreFeatureMissing));
    assert_eq!(eq.core_missing.len(), 4, "the whole co-missing momentum block");

    // Insufficient coverage: continuation with its core present but only 0.30
    // of the model observed -- a continuation confidence resting on a move
    // measurement and a derived detector count. Rankable under v1.
    let thin = fixture(None, Some(0.0), None);
    let cc = continuation_confidence(&thin, at(1));
    assert_eq!(cc.value, None, "0.30 coverage must not be ranked against 1.00");
    assert_eq!(cc.unrankable_reason, Some(UnrankableReason::InsufficientCoverage));
    assert!(cc.core_missing.is_empty(), "its core was present; coverage was the problem");
    assert!((cc.coverage - 0.30).abs() < 1e-12, "got {}", cc.coverage);
    assert!(cc.present_inputs >= MIN_PRESENT_INPUTS, "v1 would have scored this");

    // TooFewInputs is a defensive guard the current core sets make unreachable
    // through the public scorers: EarlyQuality's core implies 4 present inputs
    // and Continuation's implies 2. Exercised directly rather than left as an
    // untested branch.
    let starved = finalize_score(
        "test-model",
        Vec::new(),
        Vec::new(),
        &[],
        1,
        0.9,
        1.0,
        0.5,
        3,
    );
    assert_eq!(starved.value, None);
    assert_eq!(starved.unrankable_reason, Some(UnrankableReason::TooFewInputs));
}

/// m82. The comparability semantics are reproducible from the persisted record
/// alone -- including policies this build does not implement.
#[test]
fn m82_score_comparability_is_reconstructible_from_the_record() {
    let op = fixture(None, Some(0.0), Some(mom_features()));
    let eq = early_quality_score(&op, at(1));

    // Everything the policy needs is on the record.
    assert_eq!(eq.policy_version, SCORE_POLICY_VERSION);
    assert!(eq.total_weight > 0.0);
    assert!(eq.present_weight > 0.0);

    // Policy in force: present-weighted mean.
    let recomputed = eq.raw_weighted / eq.present_weight;
    assert!((eq.value.unwrap() - recomputed).abs() < 1e-12);

    // Policy A (v1, sum-with-zero) is recoverable.
    assert!((eq.raw_weighted - eq.components.iter().map(|c| c.contribution).sum::<f64>()).abs() < 1e-12);

    // Coverage ratio is recoverable.
    assert!((eq.coverage - eq.present_weight / eq.total_weight).abs() < 1e-12);

    // Policy C (cohort ranking) is recoverable: the availability cohort is
    // exactly the set of present feature names.
    let cohort: Vec<&str> = eq
        .components
        .iter()
        .filter(|c| c.raw.is_some())
        .map(|c| c.feature.as_str())
        .collect();
    assert_eq!(cohort.len(), eq.present_inputs);

    // And every component carries its own weight, so a re-weighting can be
    // evaluated offline without re-running the engine.
    let total: f64 = eq.components.iter().map(|c| c.weight).sum();
    assert!((total - eq.total_weight).abs() < 1e-12);
}

/// m83. The policy is deterministic and serializes stably.
#[test]
fn m83_the_policy_is_deterministic_and_round_trips() {
    let op = fixture(Some(1.25), Some(0.4), Some(mom_features()));
    let a = early_quality_score(&op, at(1));
    let b = early_quality_score(&op, at(9));
    assert_eq!(a, b, "score must not depend on when it is asked for");

    // Re-serialization rather than value equality: `serde_json` without its
    // `float_roundtrip` feature (not enabled in this workspace) can parse an
    // f64 back one ULP off, so value equality on float-bearing records passes
    // or fails by luck. Comparing the canonical text is the property that
    // actually matters for a persisted research record.
    let json = serde_json::to_string(&a).unwrap();
    let parsed: ShadowScore = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.model_version, a.model_version);
    assert_eq!(parsed.policy_version, a.policy_version);
    assert_eq!(parsed.missing, a.missing);
    assert_eq!(parsed.core_missing, a.core_missing);
    assert_eq!(parsed.present_inputs, a.present_inputs);
    assert_eq!(parsed.unrankable_reason, a.unrankable_reason);
    assert!((parsed.value.unwrap() - a.value.unwrap()).abs() < 1e-12);
    assert!((parsed.coverage - a.coverage).abs() < 1e-12);
    assert!(json.contains("\"policyVersion\""));
    assert!(json.contains("\"rawWeighted\""));
    assert!(json.contains("\"coverage\""));
}

/// m84. A fully-observed candidate's comparable score is unchanged from the v1
/// policy.
///
/// Both models' weights sum to 1.0, so at full coverage the normalization
/// divides by 1.0. This bounds the correction: it moves partially-observed
/// candidates only, and cannot have silently re-scored complete evidence.
/// (Shadow isolation itself is proved in ws-server's group I, unaffected here.)
#[test]
fn m84_full_coverage_scores_are_identical_to_the_previous_policy() {
    let op = fixture(Some(1.25), Some(0.4), Some(mom_features()));

    let eq = early_quality_score(&op, at(1));
    assert!((eq.coverage - 1.0).abs() < 1e-12, "precondition: full coverage");
    assert!((eq.value.unwrap() - eq.raw_weighted).abs() < 1e-12);

    // Continuation at full coverage needs halt evidence too; without it the
    // model is at 0.90, which is the point -- assert the identity holds
    // wherever coverage is 1.0, and the weights-sum invariant separately.
    let cc = continuation_confidence(&op, at(1));
    assert!((cc.total_weight - 1.0).abs() < 1e-12, "continuation weights sum to 1.0");
    if (cc.coverage - 1.0).abs() < 1e-12 {
        assert!((cc.value.unwrap() - cc.raw_weighted).abs() < 1e-12);
    }
    assert!((eq.total_weight - 1.0).abs() < 1e-12, "early-quality weights sum to 1.0");
}
