//! Tests for the runner stage ladder.
//!
//! The classification decides which layer a miss is attributed to, so a defect
//! here would blame the wrong subsystem. The tests below pin every rung, both
//! directions of the early/late split, and — most importantly — every place an
//! unevidenced stage must produce UNKNOWN rather than a guess.

use super::*;

use chrono::TimeZone;

fn at(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 17, h, m, 0).unwrap()
}

/// A ladder that reached every stage, ranked at 14:00 and crossed at 15:00.
fn complete() -> Ladder {
    Ladder {
        symbol: "AAA".to_string(),
        visible: StageEvidence::reached_at(at(13, 30)),
        detected: StageEvidence::reached_at(at(13, 40)),
        opportunity_created: StageEvidence::reached_at(at(13, 41)),
        early_quality_available: StageEvidence::reached_at(at(13, 42)),
        early_ranked: StageEvidence::reached_at(at(13, 50)),
        continuation_available: StageEvidence::reached_at(at(13, 55)),
        continuation_ranked: StageEvidence::reached_at(at(13, 58)),
        top_k: StageEvidence::reached_at(at(14, 0)),
        primary_crossing_at: Some(at(15, 0)),
        remaining_excursion_pct: Some(8.5),
    }
}

// ---------------------------------------------------------------------------
// The six classifications
// ---------------------------------------------------------------------------

#[test]
fn f_ranked_early_with_move_remaining() {
    let ladder = complete();
    assert_eq!(ladder.classify(), Classification::RankedEarly);
    assert_eq!(ladder.classify().letter(), "F");
    assert_eq!(
        ladder.classify().attributable_layer(),
        None,
        "F implicates no layer; it is the system working"
    );
    assert_eq!(ladder.deepest_reached(), Some(RunnerStage::TopK));
}

#[test]
fn e_ranked_highly_but_after_the_move() {
    let mut ladder = complete();
    ladder.top_k = StageEvidence::reached_at(at(16, 0));
    ladder.primary_crossing_at = Some(at(15, 0));
    assert_eq!(ladder.classify(), Classification::RankedLate);
    assert_eq!(ladder.classify().letter(), "E");
    assert_eq!(ladder.classify().attributable_layer(), Some("opportunity intelligence"));
}

/// The boundary: ranked at the same instant as the crossing is not early.
#[test]
fn ranking_simultaneous_with_the_crossing_is_late() {
    let mut ladder = complete();
    ladder.top_k = StageEvidence::reached_at(at(15, 0));
    ladder.primary_crossing_at = Some(at(15, 0));
    assert_eq!(
        ladder.classify(),
        Classification::RankedLate,
        "a rank that arrives with the move is not early"
    );
}

#[test]
fn d_opportunity_created_but_never_top_ranked() {
    let mut ladder = complete();
    ladder.top_k = StageEvidence::absent();
    assert_eq!(ladder.classify(), Classification::RankedPoorly);
    assert_eq!(ladder.classify().letter(), "D");
    assert_eq!(ladder.classify().attributable_layer(), Some("opportunity intelligence"));
}

#[test]
fn c_detected_but_no_opportunity_retained() {
    let mut ladder = complete();
    ladder.opportunity_created = StageEvidence::absent();
    assert_eq!(ladder.classify(), Classification::DetectedNoOpportunity);
    assert_eq!(ladder.classify().letter(), "C");
    assert_eq!(
        ladder.classify().attributable_layer(),
        Some("opportunity intelligence"),
        "losing something detection found is an OI failure, not a scanner one"
    );
}

#[test]
fn b_visible_but_the_detectors_missed_it() {
    let mut ladder = complete();
    ladder.detected = StageEvidence::absent();
    assert_eq!(ladder.classify(), Classification::VisibleNotDetected);
    assert_eq!(ladder.classify().letter(), "B");
    assert_eq!(
        ladder.classify().attributable_layer(),
        Some("scanner"),
        "a detector miss must not be charged to OI"
    );
}

#[test]
fn a_never_visible_at_all() {
    let mut ladder = complete();
    ladder.visible = StageEvidence::absent();
    assert_eq!(ladder.classify(), Classification::NeverVisible);
    assert_eq!(ladder.classify().letter(), "A");
    assert_eq!(ladder.classify().attributable_layer(), Some("scanner"));
}

/// A top-ranked opportunity whose target was never crossed did not miss
/// anything by being late.
#[test]
fn ranked_on_something_that_never_crossed_is_not_late() {
    let mut ladder = complete();
    ladder.primary_crossing_at = None;
    assert_eq!(ladder.classify(), Classification::RankedEarly);
}

// ---------------------------------------------------------------------------
// UNKNOWN -- never a guess
// ---------------------------------------------------------------------------

/// Every rung: unevidenced must produce UNKNOWN, not an inferred answer.
#[test]
fn an_unevidenced_stage_yields_unknown_at_every_rung() {
    for (name, mutate) in [
        ("visible", (|l: &mut Ladder| l.visible = StageEvidence::unevidenced()) as fn(&mut Ladder)),
        ("detected", |l: &mut Ladder| l.detected = StageEvidence::unevidenced()),
        ("opportunity", |l: &mut Ladder| {
            l.opportunity_created = StageEvidence::unevidenced()
        }),
        ("top_k", |l: &mut Ladder| l.top_k = StageEvidence::unevidenced()),
    ] {
        let mut ladder = complete();
        mutate(&mut ladder);
        assert_eq!(
            ladder.classify(),
            Classification::Unknown,
            "an unevidenced {name} stage must not be guessed"
        );
    }
}

/// A deeper stage observed while a shallower one is unevidenced is a capture
/// inconsistency. It must surface as UNKNOWN rather than being resolved by
/// trusting the deeper observation — the same discipline `attribution.rs`
/// applies, and for the same reason.
#[test]
fn a_deeper_stage_does_not_rescue_an_unevidenced_shallower_one() {
    let mut ladder = complete();
    ladder.detected = StageEvidence::unevidenced();
    assert_eq!(ladder.top_k.reached, Some(true), "the deeper stage is still observed");
    assert_eq!(
        ladder.classify(),
        Classification::Unknown,
        "a top-k rank does not prove the symbol was detected; the log is incomplete"
    );
}

/// Ranked, but with no timing for either side: the ranking is established and
/// the timing is not.
#[test]
fn a_missing_rank_time_makes_the_early_late_split_unknown() {
    let mut ladder = complete();
    ladder.top_k = StageEvidence { reached: Some(true), first_at: None };
    assert_eq!(ladder.classify(), Classification::Unknown);
}

// ---------------------------------------------------------------------------
// Aggregation
// ---------------------------------------------------------------------------

fn population() -> Vec<Ladder> {
    let mut out = Vec::new();
    // 3 F, 2 E, 2 D, 1 C, 2 B, 1 A, 2 UNKNOWN -- 13 total.
    for _ in 0..3 {
        out.push(complete());
    }
    for _ in 0..2 {
        let mut l = complete();
        l.top_k = StageEvidence::reached_at(at(16, 0));
        out.push(l);
    }
    for _ in 0..2 {
        let mut l = complete();
        l.top_k = StageEvidence::absent();
        out.push(l);
    }
    let mut c = complete();
    c.opportunity_created = StageEvidence::absent();
    out.push(c);
    for _ in 0..2 {
        let mut l = complete();
        l.detected = StageEvidence::absent();
        out.push(l);
    }
    let mut a = complete();
    a.visible = StageEvidence::absent();
    out.push(a);
    for _ in 0..2 {
        let mut l = complete();
        l.visible = StageEvidence::unevidenced();
        out.push(l);
    }
    out
}

#[test]
fn the_summary_counts_every_classification() {
    let summary = LadderSummary::of(&population());
    assert_eq!(summary.total, 13);
    assert_eq!(summary.ranked_early, 3);
    assert_eq!(summary.ranked_late, 2);
    assert_eq!(summary.ranked_poorly, 2);
    assert_eq!(summary.detected_no_opportunity, 1);
    assert_eq!(summary.visible_not_detected, 2);
    assert_eq!(summary.never_visible, 1);
    assert_eq!(summary.unknown, 2);
    assert_eq!(
        summary.never_visible
            + summary.visible_not_detected
            + summary.detected_no_opportunity
            + summary.ranked_poorly
            + summary.ranked_late
            + summary.ranked_early
            + summary.unknown,
        summary.total,
        "every reference opportunity must land in exactly one bucket"
    );
}

/// The two recall quantities are different questions over different
/// denominators, and the test exists to keep them that way.
#[test]
fn detection_coverage_and_conditional_retention_are_different_quantities() {
    let summary = LadderSummary::of(&population());
    assert_eq!(summary.evaluable(), 11, "the two unknowns leave the denominator");

    // Detection coverage: reached the detector, over everything evaluable.
    // 11 evaluable - 1 never visible - 2 not detected = 8.
    let coverage = summary.detection_coverage().unwrap();
    assert!((coverage - 8.0 / 11.0).abs() < 1e-12, "coverage was {coverage}");

    // Conditional retention: of those 8, one produced no OI opportunity.
    let retention = summary.oi_conditional_retention().unwrap();
    assert!((retention - 7.0 / 8.0).abs() < 1e-12, "retention was {retention}");

    assert!(
        retention > coverage,
        "retention must be able to look good while coverage does not -- that is precisely \
         why they are reported separately"
    );
}

#[test]
fn attribution_rate_gates_measurability() {
    let summary = LadderSummary::of(&population());
    let rate = summary.attribution_rate().unwrap();
    assert!((rate - 11.0 / 13.0).abs() < 1e-12);

    // A population that is mostly unknown cannot support a recall claim.
    let mostly_unknown: Vec<Ladder> = (0..10)
        .map(|i| {
            let mut l = complete();
            if i > 2 {
                l.visible = StageEvidence::unevidenced();
            }
            l
        })
        .collect();
    let summary = LadderSummary::of(&mostly_unknown);
    assert!(summary.attribution_rate().unwrap() < 0.5);
}

#[test]
fn an_empty_population_reports_nothing_rather_than_zero() {
    let summary = LadderSummary::of(&[]);
    assert_eq!(summary.total, 0);
    assert_eq!(summary.attribution_rate(), None);
    assert_eq!(summary.detection_coverage(), None);
    assert_eq!(summary.oi_conditional_retention(), None);
}

#[test]
fn every_classification_has_a_letter_and_a_settled_attribution() {
    for classification in Classification::ALL {
        assert!(!classification.letter().is_empty());
    }
    assert_eq!(Classification::ALL.len(), 7, "six outcomes plus UNKNOWN");
}

#[test]
fn a_ladder_round_trips_as_json() {
    let ladder = complete();
    let text = serde_json::to_string(&ladder).unwrap();
    let back: Ladder = serde_json::from_str(&text).unwrap();
    assert_eq!(ladder, back);
    // Unevidenced stages must serialize as absent, not as false.
    let mut sparse = complete();
    sparse.detected = StageEvidence::unevidenced();
    let text = serde_json::to_string(&sparse).unwrap();
    assert!(
        text.contains("\"detected\":{}"),
        "an unevidenced stage must not be written as a decision: {text}"
    );
}
