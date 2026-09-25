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
    // Identity is derived from the OPENING instant, so it must not drift as
    // events arrive. (Was `== 1` while `sequence` was a per-process ordinal;
    // it is now milliseconds-since-midnight of `opened_at`.)
    assert_eq!(op.id.sequence, OpportunityId::sequence_for(at(0)));
    assert_eq!(op.opened_at, at(0));
}

#[test]
fn a2_inactivity_boundary_closes_the_opportunity() {
    // D5: the default lifecycle is `move-v1`, which labels evidence silence
    // `setup_inactivity`; the symbol-activity lifecycle keeps `inactivity`.
    for (config, reason) in [
        (OiConfig::default(), OpportunityCloseReason::SetupInactivity),
        (OiConfig::symbol_activity_v1(), OpportunityCloseReason::Inactivity),
    ] {
        let mut oi = OpportunityIntelligence::new(config);
        oi.observe(&confirmed("AAA", at(0), 10.0), at(0));
        let closed = oi.observe(&confirmed("BBB", at(400), 5.0), at(400));
        assert!(closed.iter().any(|o| o.symbol == "AAA" && o.close_reason == Some(reason)));
    }
}

#[test]
fn a3_a_subsequent_move_creates_sequence_plus_one() {
    let mut oi = engine();
    oi.observe(&confirmed("AAA", at(0), 10.0), at(0));
    oi.observe(&confirmed("ZZZ", at(400), 1.0), at(400));
    oi.observe(&confirmed("AAA", at(500), 12.0), at(500));
    let op = oi.open_opportunities().find(|o| o.symbol == "AAA").unwrap();
    // The point of this test is DISTINCTNESS, not the literal ordinal 2.
    assert_eq!(op.id.sequence, OpportunityId::sequence_for(at(500)));
    assert_ne!(
        op.id.sequence,
        OpportunityId::sequence_for(at(0)),
        "a new move after closure must not reuse the first opportunity's id"
    );
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
    assert_eq!(op.id.sequence, OpportunityId::sequence_for(at(0)));
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
        last_relevant_at: None,
        last_evidence_kind: None,
        opened_phase: None,
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
    // `min(flow, structural) x safety`, where
    //   flow       = 10.00/s x 4,800s  = 48,000
    //   structural = 13,100 symbols
    //   safety     = 5/4
    // so the structural bound binds: 13,100 x 5/4 = 16,375.
    //
    // This asserted 3,750 until the September-16 capacity truncation. The old
    // derivation multiplied the supported rate by `inactivity_secs`, which is a
    // silence timeout rather than a lifetime -- measured mean lifetime that
    // session was 3,752s, 12.5x the 300s the formula assumed. See
    // `OiConfig::max_open_opportunities`.
    assert_eq!(cfg.max_open_opportunities(), 16_375);
    assert_eq!(cfg.required_open_capacity(), 13_100);
    assert!(cfg.capacity_invariant().is_ok());
    assert_eq!(
        cfg.inactivity_secs, 300,
        "the inactivity boundary is explicitly out of scope and must not have moved"
    );
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

/// Opens `symbols` distinct opportunities at one instant, without ranking.
///
/// Ranking scores the whole cohort and dominates the cost, which is the right
/// thing to measure for throughput and the wrong thing for a capacity test:
/// these are about the *open set*, and at a bound of 16,375 a ranking loop
/// would make them minutes long for no extra coverage.
fn open_population(oi: &mut OpportunityIntelligence, symbols: i64, t: DateTime<Utc>) {
    for i in 0..symbols {
        oi.observe(&confirmed(&format!("SYM{i:06}"), t, 10.0 + (i % 97) as f64 * 0.05), t);
    }
}

#[test]
fn p75_overload_is_bounded_and_counted_not_unbounded() {
    let mut oi = engine();
    let bound = oi.config().max_open_opportunities();
    // Past the *repaired* bound, not the old one. Deliberately re-pointed at
    // 16,375 rather than left at a population the new capacity absorbs
    // comfortably -- a bounds test that no longer reaches the bound has
    // silently stopped testing anything.
    open_population(&mut oi, bound as i64 + 2_000, at(1));
    let h = oi.health();
    println!(
        "p75 overload: peak_open={} bound={bound} evictions={}",
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
    let bound = oi.config().max_open_opportunities();
    open_population(&mut oi, bound as i64 + 2_000, at(1));
    let evictions_after_overload = oi.health().capacity_evictions;
    assert!(evictions_after_overload > 0, "precondition: overload happened");

    // Quiet period long enough for inactivity to retire the open set, then
    // ordinary load again.
    let quiet = at(1 + oi.config().inactivity_secs + 10);
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
        last_relevant_at: None,
        last_evidence_kind: None,
        opened_phase: None,
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

// ---------------------------------------------------------------------------
// Engine-capacity load tests (section 8)
//
// The September-16 figures these are built on were reconstructed from the
// preserved artifacts, and from two independent directions that agree:
//
//  * the OI capture's own per-window cohort sizes, and
//  * an uncapped replay of the whole-market ignition stream that the discovery
//    capture preserved.
//
// Where the 3,750 cap was not binding the two reconstructions agree closely
// (p50 3,215 vs 3,211); where it was binding they diverge, which is the
// signature of the truncation. The uncensored figures are the ones used here:
//
//   sustained open rate         0.853/s
//   peak rolling-300s open rate 6.457/s
//   mean opportunity lifetime   3,752s
//   open population p50/p99/max 3,211 / 4,600 / 4,808
// ---------------------------------------------------------------------------

/// The maximum open population the September-16 regular session actually
/// required, uncensored.
const SEPTEMBER_16_PEAK_OPEN: i64 = 4_808;

/// LOAD TEST H -- September-16 open-opportunity pressure.
///
/// The session that failed, replayed against the repaired bound. Requires zero
/// evictions, peak strictly below capacity, and every opportunity preserved.
#[test]
fn h_september_16_open_pressure_evicts_nothing() {
    let mut oi = engine();
    let capacity = oi.config().max_open_opportunities();
    open_population(&mut oi, SEPTEMBER_16_PEAK_OPEN, at(1));

    let h = oi.health();
    println!(
        "H: peak_open={} capacity={capacity} evictions={} opened={}",
        h.peak_open_opportunities, h.capacity_evictions, h.opportunities_opened
    );
    assert_eq!(
        h.capacity_evictions, 0,
        "the September-16 population must not evict at the repaired capacity"
    );
    assert!(
        h.peak_open_opportunities < capacity,
        "peak {} must stay strictly below capacity {capacity}",
        h.peak_open_opportunities
    );
    assert_eq!(
        oi.open_count(),
        SEPTEMBER_16_PEAK_OPEN as usize,
        "every opportunity must still be open -- none silently discarded"
    );
    assert_eq!(h.opportunities_opened, SEPTEMBER_16_PEAK_OPEN as u64);
    // Against the deployed 3,750 this same population evicted 1,058 times.
    let mut old = OpportunityIntelligence::new(OiConfig {
        supported_lifetime_secs: 300,
        supported_symbol_universe: usize::MAX,
        ..OiConfig::default()
    });
    assert_eq!(old.config().max_open_opportunities(), 3_750, "the deployed bound, reconstructed");
    open_population(&mut old, SEPTEMBER_16_PEAK_OPEN, at(1));
    assert!(
        old.health().capacity_evictions > 0,
        "the fixture must actually reproduce the defect against the old bound, \
         or H proves nothing about the repair"
    );
    println!(
        "H: against the deployed bound of 3,750 the same population evicts {} times",
        old.health().capacity_evictions
    );
}

/// LOAD TEST I -- twice the observed pressure.
///
/// Section 3 requires the supported envelope to be at least 2x the observed
/// sustained requirement. This drives exactly that and requires zero eviction.
#[test]
fn i_twice_september_16_pressure_evicts_nothing() {
    let mut oi = engine();
    let capacity = oi.config().max_open_opportunities();
    let population = SEPTEMBER_16_PEAK_OPEN * 2;
    open_population(&mut oi, population, at(1));

    let h = oi.health();
    println!(
        "I: population={population} peak_open={} capacity={capacity} evictions={}",
        h.peak_open_opportunities, h.capacity_evictions
    );
    assert_eq!(h.capacity_evictions, 0, "2x observed pressure is inside the declared envelope");
    assert!(h.peak_open_opportunities < capacity);
    assert_eq!(oi.open_count(), population as usize);
    assert!(
        (capacity as f64) / (SEPTEMBER_16_PEAK_OPEN as f64) >= 2.0,
        "the envelope must be at least 2x the observed requirement"
    );
}

/// LOAD TEST J -- above the envelope.
///
/// Forces capacity pressure and requires that it is bounded, counted exactly,
/// described by a marker, and recovered from.
#[test]
fn j_above_the_envelope_evicts_explicitly_and_recovers() {
    let mut oi = engine();
    let capacity = oi.config().max_open_opportunities();
    let population = capacity as i64 + 1_500;
    open_population(&mut oi, population, at(1));

    // Cloned so the health read does not hold a borrow across the drain below.
    let h = oi.health().clone();
    println!(
        "J: offered={population} capacity={capacity} peak_open={} evictions={}",
        h.peak_open_opportunities, h.capacity_evictions
    );

    // Bounded.
    assert!(
        h.peak_open_opportunities <= capacity,
        "state must stay bounded: peak {} > capacity {capacity}",
        h.peak_open_opportunities
    );
    assert_eq!(oi.open_count(), capacity, "the open set must sit exactly at its bound");
    // Counted, and exactly: every opportunity offered beyond the bound must be
    // accounted for as an eviction rather than quietly not opened.
    assert_eq!(
        h.capacity_evictions,
        (population as usize - capacity) as u64,
        "the eviction counter must be exact, not approximate"
    );
    assert_eq!(h.opportunities_opened, population as u64);
    assert_eq!(h.open_opportunities, capacity);
    assert_eq!(h.opportunity_capacity, capacity);

    // Described. This is the property September 16 lacked entirely: capacity
    // truncation was discoverable only by noticing cohort sizes pinned at 3,750
    // in the records that happened to survive.
    let markers = oi.take_capacity_evictions();
    assert!(!markers.is_empty(), "eviction must emit explicit markers");
    assert_eq!(oi.eviction_markers_dropped(), 0, "no marker may be lost at this scale");
    let m = &markers[0];
    assert_eq!(m.reason, "opportunity_capacity_reached");
    assert_eq!(m.capacity, capacity);
    assert_eq!(m.open_count, capacity, "the marker reports the population that forced it");
    assert!(!m.opportunity_id.is_empty());
    assert!(!m.symbol.is_empty());
    assert_eq!(markers.len() as u64, h.capacity_evictions, "one marker per eviction");
    assert!(
        oi.take_capacity_evictions().is_empty(),
        "draining must not hand the same markers out twice"
    );

    // Recovered: after a quiet period longer than the inactivity boundary the
    // set drains and ordinary load stops evicting.
    let quiet = at(1 + oi.config().inactivity_secs + 10);
    oi.observe(&confirmed("RECOVER", quiet, 10.0), quiet);
    assert!(
        oi.open_count() < 100,
        "inactivity must clear the overloaded set, {} still open",
        oi.open_count()
    );
    let evictions = oi.health().capacity_evictions;
    for i in 0..400i64 {
        let t = quiet + Duration::seconds(i);
        oi.observe(&confirmed(&format!("R{}", i % 50), t, 10.0 + i as f64 * 0.01), t);
    }
    assert_eq!(
        oi.health().capacity_evictions,
        evictions,
        "ordinary load after recovery must not keep evicting"
    );
    assert!(
        oi.take_capacity_evictions().is_empty(),
        "and must not keep emitting markers"
    );
}

/// The eviction-marker buffer is itself bounded, and says so when it overflows.
///
/// An unbounded diagnostic buffer is the exact failure mode this whole
/// assignment is about, so the thing that reports truncation must not become a
/// way to run out of memory.
#[test]
fn the_eviction_marker_buffer_is_bounded_and_reports_its_own_overflow() {
    // A deliberately tiny capacity, so many thousands of evictions happen
    // without needing a large population.
    let mut oi = OpportunityIntelligence::new(OiConfig {
        supported_symbol_universe: 8,
        ..OiConfig::default()
    });
    let capacity = oi.config().max_open_opportunities();
    assert_eq!(capacity, 10);
    open_population(&mut oi, 8_000, at(1));

    assert_eq!(oi.open_count(), capacity);
    assert_eq!(oi.health().capacity_evictions, 8_000 - capacity as u64);
    let markers = oi.take_capacity_evictions();
    assert!(markers.len() <= 4_096, "the marker buffer must stay bounded");
    assert!(
        oi.eviction_markers_dropped() > 0,
        "and overflow must be counted, never silent"
    );
    assert_eq!(
        markers.len() as u64 + oi.eviction_markers_dropped(),
        oi.health().capacity_evictions,
        "markers kept plus markers dropped must account for every eviction exactly"
    );
}

// --- D6. Ranking cohort bound (2026-09-25) ------------------------------------
//
// `docs/measurement-correctness-contract-2026-09-25.md` D6. The cohort levels
// (4,096 / 4,678 / 6,000 / 16,384 and above capacity), determinism under
// reversed insertion, latency and memory live in `tests/d6_rank_load.rs`,
// which is `#[ignore]`d because it is a release-mode measurement.

/// `n` opportunities of which the first `scored_early` carry a momentum
/// surface (so EarlyQuality can score them) and every one has a confirm and a
/// move. Deterministic scores from the index.
fn d6_population(config: OiConfig, n: usize, with_momentum: usize) -> OpportunityIntelligence {
    let mut oi = OpportunityIntelligence::new(config);
    for i in 0..n {
        let s = format!("S{i:05}");
        if i < with_momentum {
            let overall = 0.60 + (i % 37) as f64 / 100.0;
            oi.observe(&momentum(&s, at(0), overall, 0.1 + (i % 11) as f64 / 20.0), at(0));
        }
        oi.observe(&confirmed(&s, at(1), 10.0), at(1));
        oi.observe(&confirmed(&s, at(2), 10.0 * (1.0 + (i % 23) as f64 / 100.0)), at(2));
    }
    oi
}

#[test]
fn d6_the_rank_bound_is_the_open_capacity() {
    let cfg = OiConfig::default();
    assert_eq!(DEFAULT_MAX_RANK_COHORT, DEFAULT_MAX_OPEN_OPPORTUNITIES);
    assert_eq!(cfg.max_rank_cohort, 16_375);
    assert_eq!(cfg.max_rank_cohort, cfg.max_open_opportunities());
    assert!(cfg.capacity_invariant().is_ok());
    // The fingerprint moved with it, and only because of it: this is the value
    // the contract recomputed for "D6 alone" before the change was made.
    //
    // D5 has since added `lifecycle` and `moveInactivitySecs` to the struct,
    // so today's fingerprint differs from both (see
    // `d5_the_fingerprint_moved_deliberately`). The historical values are
    // still reproduced from the pre-D5 field set, which is what an old
    // capture's fingerprint was computed over.
    assert_eq!(pre_d5_fingerprint(&cfg), "oi-cfg-15861d6d0b263f12");
    let old = OiConfig { max_rank_cohort: 4_096, ..OiConfig::default() };
    assert_eq!(
        pre_d5_fingerprint(&old),
        "oi-cfg-b4f21c8b311a1b99",
        "the pre-D6 production fingerprint"
    );
}

/// The fingerprint over the pre-D5 field set: today's canonical JSON with the
/// two D5 fields removed. Same FNV-1a as `OiConfig::fingerprint`.
fn pre_d5_fingerprint(cfg: &OiConfig) -> String {
    let mut v = serde_json::to_value(cfg).unwrap();
    let obj = v.as_object_mut().unwrap();
    obj.remove("lifecycle");
    obj.remove("moveInactivitySecs");
    // `serde_json::Value` would re-order keys; rebuild in struct order.
    let full = serde_json::to_string(cfg).unwrap();
    let cut = full.find(",\"lifecycle\"").expect("lifecycle is serialized last");
    let json = format!("{}}}", &full[..cut]);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&json).unwrap(),
        v,
        "the D5 fields are the struct's last two"
    );
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in json.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("oi-cfg-{hash:016x}")
}

/// D5: the lifecycle selector and `T_move` are in the fingerprint, so the
/// default moved and the two lifecycles are distinguishable by it.
#[test]
fn d5_the_fingerprint_moved_deliberately() {
    let move_v1 = OiConfig::default();
    let legacy = OiConfig::symbol_activity_v1();
    assert_eq!(move_v1.lifecycle, Lifecycle::MoveV1);
    assert_eq!(move_v1.move_inactivity_secs, 300);
    assert_eq!(move_v1.fingerprint(), crate::alpha::spec::EXPECTED_OI_CONFIG_FINGERPRINT);
    assert_ne!(move_v1.fingerprint(), legacy.fingerprint());
    assert_ne!(move_v1.fingerprint(), "oi-cfg-15861d6d0b263f12");
    let other_t = OiConfig { move_inactivity_secs: 301, ..OiConfig::default() };
    assert_ne!(other_t.fingerprint(), move_v1.fingerprint(), "T_move is fingerprinted");
    println!("move-v1 {}  symbol-activity-v1 {}", move_v1.fingerprint(), legacy.fingerprint());
}

#[test]
fn d6_capacity_invariant_refuses_a_rank_bound_below_the_open_set() {
    let below = OiConfig { max_rank_cohort: 16_374, ..OiConfig::default() };
    let error = below.capacity_invariant().expect_err("a bound below capacity must be reported");
    assert!(error.contains("D6"), "{error}");
    let at_capacity = OiConfig { max_rank_cohort: 16_375, ..OiConfig::default() };
    assert!(at_capacity.capacity_invariant().is_ok());
    // A config whose open capacity grows must grow the rank bound with it.
    let wide = OiConfig { supported_symbol_universe: 20_000, ..OiConfig::default() };
    assert!(wide.max_open_opportunities() > wide.max_rank_cohort);
    assert!(wide.capacity_invariant().is_err());
}

/// The definition, at its edges.
#[test]
fn d6_rank_fraction_edges() {
    assert_eq!(rank_fraction(Some(1), 1), Some(0.0), "N = 1 is defined, and best");
    assert_eq!(rank_fraction(Some(1), 4_678), Some(0.0), "best is 0");
    assert_eq!(rank_fraction(Some(10), 10), Some(0.9), "worst is (N - 1) / N");
    assert_eq!(rank_fraction(None, 10), None, "absent iff the rank is absent");
    assert_eq!(rank_fraction(Some(0), 10), None, "ranks are 1-based");
    assert_eq!(rank_fraction(Some(11), 10), None, "a rank outside its cohort is not a fraction");
    assert_eq!(rank_fraction(Some(1), 0), None);
}

/// `fraction < p` selects exactly alpha's `rank <= ceil(N p)` cohort, for
/// every rank, at the preregistered percentages and the named cohort sizes.
/// This is why the fraction is `(rank - 1) / N` and not `rank / N`.
#[test]
fn d6_rank_fraction_matches_the_alpha_top_percent_rule_exactly() {
    for p in [0.05_f64, 0.10, 0.25] {
        for n in [1usize, 7, 10, 4_678] {
            let threshold = (n as f64 * p).ceil() as usize;
            for rank in 1..=n {
                let fraction = rank_fraction(Some(rank), n).unwrap();
                assert_eq!(
                    fraction < p,
                    rank <= threshold,
                    "p={p} N={n} rank={rank}: fraction {fraction} vs ceil threshold {threshold}"
                );
            }
        }
    }
    // The boundary `rank / N` gets wrong: N = 10, p = 0.25 -> ceil = 3.
    assert!(rank_fraction(Some(3), 10).unwrap() < 0.25);
    assert!(3.0 / 10.0 > 0.25, "rank / N would have excluded rank 3");
}

/// On emitted rows the fraction is present exactly when the rank is, and is
/// computed against the same surface's cohort.
#[test]
fn d6_snapshot_fraction_follows_its_own_surface() {
    let mut oi = d6_population(OiConfig::default(), 12, 5);
    let snaps = oi.rank(at(60)).unwrap();
    for s in &snaps {
        assert_eq!(s.early_quality_rank.is_some(), s.early_quality_rank_fraction.is_some());
        assert_eq!(s.continuation_rank.is_some(), s.continuation_rank_fraction.is_some());
        assert_eq!(
            s.early_quality_rank_fraction,
            rank_fraction(s.early_quality_rank, s.early_cohort_size)
        );
        assert_eq!(
            s.continuation_rank_fraction,
            rank_fraction(s.continuation_rank, s.continuation_cohort_size)
        );
    }
    let json = serde_json::to_string(&snaps[0]).unwrap();
    let back: OpportunityScoreSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(back.early_quality_rank_fraction, snaps[0].early_quality_rank_fraction);
    // A pre-D6 row -- no fraction fields at all -- still parses.
    let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
    v.as_object_mut().unwrap().remove("earlyQualityRankFraction");
    v.as_object_mut().unwrap().remove("continuationRankFraction");
    let old: OpportunityScoreSnapshot = serde_json::from_value(v).unwrap();
    assert_eq!(old.early_quality_rank_fraction, None);
}

/// Every scored row is ranked 1..N, contiguous and unique, and `*CohortSize`
/// is the scored count -- above the old 4,096 bound, in a debug build.
#[test]
fn d6_above_the_old_bound_every_scored_row_is_ranked() {
    let n = 4_200;
    let mut oi = d6_population(OiConfig::default(), n, n);
    let snaps = oi.rank(at(60)).unwrap();
    type Surface = (
        fn(&OpportunityScoreSnapshot) -> Option<usize>,
        fn(&OpportunityScoreSnapshot) -> usize,
        fn(&OpportunityScoreSnapshot) -> Option<f64>,
    );
    let surfaces: [Surface; 2] = [
        (|s| s.early_quality_rank, |s| s.early_cohort_size, |s| s.early_quality.value),
        (|s| s.continuation_rank, |s| s.continuation_cohort_size, |s| s.continuation.value),
    ];
    for (rank_of, cohort_of, value_of) in surfaces {
        let scored = snaps.iter().filter(|s| value_of(s).is_some()).count();
        assert!(scored > 4_096, "the fixture must exceed the old bound, scored {scored}");
        let mut ranks: Vec<usize> = snaps.iter().filter_map(|s| rank_of(s)).collect();
        ranks.sort_unstable();
        assert_eq!(ranks, (1..=scored).collect::<Vec<_>>(), "contiguous, unique, 1..N");
        assert!(snaps.iter().all(|s| cohort_of(s) == scored), "cohort size is the true N");
        assert!(
            snaps.iter().all(|s| value_of(s).is_some() == rank_of(s).is_some()),
            "scored iff ranked: no score is left unranked"
        );
    }
    let h = oi.health().clone();
    assert_eq!(h.cohort_truncations, 0);
    assert_eq!(h.early_cohort_truncations + h.continuation_cohort_truncations, 0);
    assert!(oi.take_cohort_truncations().is_empty());
    assert_eq!(h.ranking_windows, 1);
    assert_eq!(h.rank_cohort_capacity, 16_375);
    assert!(h.early_cohort_last > 4_096 && h.early_cohort_peak == h.early_cohort_last);
}

/// Ranks 1..4,096 are identical to what the capped engine produced: the cap
/// was a suffix cut of the same order, and removing it moves no top rank.
#[test]
fn d6_top_ranks_are_identical_to_the_capped_engine() {
    let n = 4_300;
    let rank_map = |cap: usize| {
        let cfg = OiConfig { max_rank_cohort: cap, ..OiConfig::default() };
        let mut oi = d6_population(cfg, n, n);
        let snaps = oi.rank(at(60)).unwrap();
        let mut early: Vec<(usize, String)> = snaps
            .iter()
            .filter_map(|s| s.early_quality_rank.map(|r| (r, s.opportunity_id.clone())))
            .collect();
        early.sort();
        let mut cont: Vec<(usize, String)> = snaps
            .iter()
            .filter_map(|s| s.continuation_rank.map(|r| (r, s.opportunity_id.clone())))
            .collect();
        cont.sort();
        (early, cont, oi.health().cohort_truncations)
    };
    let (capped_e, capped_c, capped_t) = rank_map(4_096);
    let (full_e, full_c, full_t) = rank_map(DEFAULT_MAX_RANK_COHORT);
    assert_eq!(capped_t, 1, "the old bound really did cut this window");
    assert_eq!(full_t, 0);
    assert_eq!(capped_e.len(), 4_096);
    assert_eq!(&full_e[..4_096], &capped_e[..], "EarlyQuality ranks 1..4,096 unchanged");
    assert_eq!(&full_c[..capped_c.len()], &capped_c[..], "Continuation ranks unchanged");
    assert!(full_e.len() > 4_096);
}

/// A deliberately mis-specified bound is still counted -- per surface -- and
/// still self-reports through a marker. The counter must stay reachable, or a
/// zero proves nothing.
#[test]
fn d6_a_violated_bound_is_counted_per_surface_and_marked() {
    // Learn each surface's scored N for this fixture, then put the bound
    // between them so exactly one surface is cut.
    let probe = |cap: usize| {
        let cfg = OiConfig { max_rank_cohort: cap, ..OiConfig::default() };
        let mut oi = d6_population(cfg, 40, 25);
        let snaps = oi.rank(at(60)).unwrap();
        (oi, snaps)
    };
    let (unbounded, _) = probe(DEFAULT_MAX_RANK_COHORT);
    let (e_n, c_n) = (unbounded.health().early_cohort_last, unbounded.health().continuation_cohort_last);
    assert_ne!(e_n, c_n, "the fixture must separate the two surfaces ({e_n} vs {c_n})");
    let cap = e_n.min(c_n);
    let (mut oi, snaps) = probe(cap);
    let h = oi.health().clone();
    let (cut_surface, cut_n) = if c_n > e_n {
        (RankSurface::Continuation, c_n)
    } else {
        (RankSurface::EarlyQuality, e_n)
    };
    assert_eq!(h.cohort_truncations, 1, "the OR is unchanged in meaning");
    match cut_surface {
        RankSurface::Continuation => {
            assert_eq!((h.early_cohort_truncations, h.continuation_cohort_truncations), (0, 1));
        }
        RankSurface::EarlyQuality => {
            assert_eq!((h.early_cohort_truncations, h.continuation_cohort_truncations), (1, 0));
        }
    }
    // The health reports the true N, not the truncated length.
    assert_eq!(h.early_cohort_last, e_n);
    assert_eq!(h.continuation_cohort_last, c_n);
    let markers = oi.take_cohort_truncations();
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].surface, cut_surface);
    assert_eq!(markers[0].scored, cut_n);
    assert_eq!(markers[0].cap, cap);
    assert_eq!(markers[0].reason, "ranking_cohort_truncated");
    assert_eq!(markers[0].window_id, snaps[0].window_id);
    // And the bound was reported as a violation when the engine was built.
    assert!(OiConfig { max_rank_cohort: cap, ..OiConfig::default() }
        .capacity_invariant()
        .is_err());
}

/// `total_cmp` gives a NaN a fixed place, so ordering is total and the result
/// does not depend on input order -- the property `partial_cmp(..)
/// .unwrap_or(Equal)` could not guarantee. Finite scores keep their order.
#[test]
fn d6_a_nan_score_cannot_make_the_order_input_dependent() {
    let id = |s: &str| OpportunityId { symbol: s.into(), session_date: "d".into(), sequence: 1 };
    let scored = vec![
        (id("AAA"), 0.5),
        (id("BBB"), f64::NAN),
        (id("CCC"), 0.9),
        (id("DDD"), -f64::NAN),
        (id("EEE"), 0.1),
    ];
    let mut reversed = scored.clone();
    reversed.reverse();
    let a = rank_cohort(scored, vec![], "w".into(), at(0), 100);
    let b = rank_cohort(reversed, vec![], "w".into(), at(0), 100);
    let order = |r: &Ranking| r.entries.iter().map(|e| e.symbol.clone()).collect::<Vec<_>>();
    assert_eq!(order(&a), order(&b), "input order must not matter");
    // Positive NaN sorts ahead of every finite score (descending), negative
    // NaN behind; the finite ones keep score order.
    assert_eq!(order(&a), vec!["BBB", "CCC", "AAA", "EEE", "DDD"]);
    assert_eq!(a.entries.iter().map(|e| e.rank).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5]);
}

#[test]
fn d6_closes_are_counted_by_reason() {
    let mut oi = engine();
    oi.observe(&confirmed("AAA", at(0), 10.0), at(0));
    oi.observe(&confirmed("BBB", at(400), 5.0), at(400)); // AAA: setup inactivity
    let _ = oi.finish(at(500)); // BBB: capture end
    let c = oi.health().closed_by_reason;
    // D5: `move-v1` (the default) writes `setupInactivity`, never `inactivity`.
    assert_eq!(c.setup_inactivity, 1);
    assert_eq!(c.inactivity, 0);
    assert_eq!(c.capture_ended, 1);
    assert_eq!(c.total(), oi.health().opportunities_closed);
}

/// D4-5: a session boundary found by expiry used to be dated `last_seen_at`,
/// before anchors the opportunity had already been ranked into. `closed_at` is
/// now floored at the last ranking instant the opportunity took part in.
#[test]
fn d4_5_a_close_is_never_dated_before_a_window_the_opportunity_was_ranked_in() {
    // Pinned to the symbol-activity lifecycle: 23:58Z -> 00:04Z is a new UTC
    // date but the SAME market day (19:58 -> 20:04 EDT), so under `move-v1`
    // this is correctly a setup-inactivity close and no boundary at all. The
    // `move-v1` form of this regression crosses 04:00 ET instead; see
    // `opportunity_lifecycle_tests::d4_5_move_v1_session_boundary_floor`.
    let mut oi = OpportunityIntelligence::new(OiConfig::symbol_activity_v1());
    let last_seen = Utc.with_ymd_and_hms(2026, 9, 14, 23, 58, 0).unwrap();
    oi.observe(&momentum("AAA", last_seen, 0.7, 0.4), last_seen);
    oi.observe(&confirmed("AAA", last_seen, 10.0), last_seen);
    // Ranked 2 minutes later while still open: an anchor exists at `ranked`.
    let ranked = last_seen + Duration::seconds(120);
    oi.observe(&confirmed("OTHER", ranked, 1.0), ranked);
    let snaps = oi.rank(ranked).unwrap();
    assert!(snaps.iter().any(|s| s.symbol == "AAA"));
    // Expiry, found across midnight by an unrelated event.
    let found = last_seen + Duration::seconds(400);
    let closed = oi.observe(&confirmed("ZZZ", found, 1.0), found);
    let aaa = closed.iter().find(|o| o.symbol == "AAA").unwrap();
    assert_eq!(aaa.close_reason, Some(OpportunityCloseReason::SessionBoundary));
    assert!(
        aaa.closed_at.unwrap() >= ranked,
        "closedAt {:?} precedes the anchor issued at {ranked:?}",
        aaa.closed_at
    );
}
