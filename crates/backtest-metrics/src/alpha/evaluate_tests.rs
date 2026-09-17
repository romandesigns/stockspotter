//! Tests for the evaluation layer.
//!
//! Built on synthetic sessions with known ground truth, because §0 forbids
//! using September 14 or 16 to shape any of this. Two fixtures matter most: one
//! where the score genuinely predicts, and one where it is pure noise. If the
//! second ever reads as enrichment, nothing else here is trustworthy.

use super::*;

use chrono::{DateTime, TimeZone, Utc};

use crate::alpha::dataset::{OpportunityDataset, OpportunityRow};
use crate::alpha::labels::{Crossing, ReferenceOpportunity};
use crate::alpha::ladder::StageEvidence;
use crate::alpha::spec::QualificationSpec;
use crate::horizon::PricePoint;

fn at(secs: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 17, 13, 30, 0).unwrap() + chrono::Duration::seconds(secs)
}

/// A synthetic session.
///
/// `informative` decides whether rank predicts the outcome. Everything else —
/// cohort sizes, windows, symbols, prices — is identical between the two, so a
/// difference in result can only come from the relationship itself.
struct Session {
    dataset: OpportunityDataset,
    discovery: DiscoveryView,
    ladders: Vec<Ladder>,
    reference: Vec<ReferenceOpportunity>,
}

fn build(symbols: usize, windows: usize, informative: bool) -> Session {
    let mut dataset = OpportunityDataset::default();
    let mut discovery = DiscoveryView::default();
    let mut ladders = Vec::new();
    let mut reference = Vec::new();

    for w in 0..windows {
        let window_id = format!("oiw-{w}");
        dataset.window_cohort_sizes.insert(window_id.clone(), symbols);
        for i in 0..symbols {
            let symbol = format!("S{:04}", w * symbols + i);
            let rank = i + 1;
            let entered = at(w as i64 * 30);
            // Informative: the top decile reaches the target, the rest do not.
            // Noise: every tenth symbol reaches it regardless of rank.
            let reaches = if informative {
                rank <= symbols / 10
            } else {
                (w * symbols + i) % 10 == 0
            };

            // Dense out past the longest primary horizon (900s), or the tail
            // gap censors every measurement and the fixture proves nothing.
            let series: Vec<PricePoint> = (0..110)
                .map(|t| {
                    let elapsed = t * 10;
                    let price = if reaches {
                        10.0 + (elapsed as f64 / 300.0) * 0.35
                    } else {
                        10.0 + (elapsed as f64 / 300.0) * 0.05
                    };
                    (entered + chrono::Duration::seconds(elapsed), price)
                })
                .collect();
            discovery.series.insert(symbol.clone(), series);
            discovery.visible.insert(symbol.clone());
            discovery.detected.insert(symbol.clone());

            dataset.rows.push(OpportunityRow {
                opportunity_id: format!("{symbol}:2026-09-17:1"),
                symbol: symbol.clone(),
                session_date: "2026-09-17".to_string(),
                opened_at: entered - chrono::Duration::seconds(60 + i as i64),
                first_ranked_at: entered,
                first_window_id: window_id.clone(),
                first_price: 10.0,
                windows: 3,
                first_window_early_rank: Some(rank),
                first_window_continuation_rank: Some(rank),
                best_early_rank: Some(rank),
                best_continuation_rank: Some(rank),
                first_top_k_early: [(5usize, entered)].into_iter().collect(),
                first_top_k_continuation: [(5usize, entered)].into_iter().collect(),
                early_quality_available: true,
                continuation_available: true,
                regime: if i % 2 == 0 { "early_emerging" } else { "continuation_acceleration" }
                    .to_string(),
                price_band: Some(3),
                detectors: vec!["IgnitionDetector".to_string()],
                confluence_count: 1,
                move_before_detection_pct: Some(i as f64 * 0.01),
                first_window_cohort_size: symbols,
            });

            ladders.push(Ladder {
                symbol: symbol.clone(),
                visible: StageEvidence::reached_at(entered),
                detected: StageEvidence::reached_at(entered),
                opportunity_created: StageEvidence::reached_at(entered),
                early_quality_available: StageEvidence::reached_at(entered),
                early_ranked: StageEvidence::reached_at(entered),
                continuation_available: StageEvidence::reached_at(entered),
                continuation_ranked: StageEvidence::reached_at(entered),
                top_k: if rank <= symbols / 10 {
                    StageEvidence::reached_at(entered)
                } else {
                    StageEvidence::absent()
                },
                primary_crossing_at: reaches
                    .then(|| entered + chrono::Duration::seconds(200)),
                remaining_excursion_pct: Some(3.0),
            });

            reference.push(ReferenceOpportunity {
                symbol,
                session_date: chrono::NaiveDate::from_ymd_opt(2026, 9, 17).unwrap(),
                ineligible: None,
                start_price: Some(10.0),
                start_at: Some(entered),
                session_high: Some(if reaches { 10.4 } else { 10.05 }),
                session_high_at: Some(entered),
                session_low: Some(9.95),
                mfe_pct: Some(if reaches { 4.0 } else { 0.5 }),
                mae_pct: Some(-0.5),
                crossings: vec![
                    if reaches {
                        Crossing::Crossed {
                            at: entered + chrono::Duration::seconds(200),
                            after_secs: 200,
                        }
                    } else {
                        Crossing::NotCrossed
                    },
                    Crossing::NotCrossed,
                    Crossing::NotCrossed,
                ],
                observations: 40,
                largest_gap_secs: 10,
            });
        }
    }
    dataset.windows = dataset.window_cohort_sizes.len();
    dataset.raw_rows = (dataset.rows.len() * 3) as u64;
    Session { dataset, discovery, ladders, reference }
}

/// A contract whose evidence floors the fixtures can actually meet. The gates
/// themselves are untouched — only the "is this session big enough to ask"
/// numbers, which would otherwise dominate every test.
fn test_spec() -> QualificationSpec {
    let mut spec = QualificationSpec::default();
    spec.minimum_evidence.distinct_opportunities = 100;
    spec.minimum_evidence.distinct_symbols = 50;
    spec.minimum_evidence.positive_reference_opportunities = 10;
    spec.minimum_evidence.ranking_windows = 2;
    spec.uncertainty.resamples = 300;
    spec
}

fn run(session: &Session, spec: &QualificationSpec) -> Evaluation {
    evaluate(&Inputs {
        spec,
        dataset: &session.dataset,
        discovery: &session.discovery,
        ladders: &session.ladders,
        reference: &session.reference,
    })
}

// ---------------------------------------------------------------------------
// The two fixtures that matter
// ---------------------------------------------------------------------------

/// A score that genuinely predicts must clear parity.
#[test]
fn an_informative_score_produces_enrichment_above_parity() {
    let session = build(40, 6, true);
    let evaluation = run(&session, &test_spec());
    let a1 = evaluation
        .results
        .iter()
        .find(|r| r.criterion_id == "A1-early-quality-enrichment")
        .unwrap();
    assert!(
        a1.comparison.unwrap() > 1.0,
        "a score that predicts must enrich: {:?}",
        a1.estimate
    );
    assert_eq!(a1.outcome, Outcome::Pass, "{:?}", a1.estimate);
}

/// **The test that protects the verdict.** A score unrelated to the outcome
/// must not read as enrichment, whatever the sample size.
#[test]
fn a_noise_score_does_not_clear_parity() {
    let session = build(40, 6, false);
    let evaluation = run(&session, &test_spec());
    let a1 = evaluation
        .results
        .iter()
        .find(|r| r.criterion_id == "A1-early-quality-enrichment")
        .unwrap();
    assert_ne!(
        a1.outcome,
        Outcome::Pass,
        "rank unrelated to outcome must not pass A1: point {:?}, interval {:?}..{:?}",
        a1.comparison,
        a1.estimate.lower,
        a1.estimate.upper
    );
}

// ---------------------------------------------------------------------------
// Cohort construction
// ---------------------------------------------------------------------------

#[test]
fn a_percentile_selector_scales_with_the_windows_cohort() {
    assert_eq!(Selector::TopPercent(0.10).threshold(40), 4);
    assert_eq!(Selector::TopPercent(0.10).threshold(100), 10);
    assert_eq!(Selector::TopK(5).threshold(1000), 5, "a top-k surface does not scale");
    assert_eq!(
        Selector::TopPercent(0.10).threshold(3),
        1,
        "a percentile surface in a tiny window must still name somebody"
    );
}

/// Controls are drawn from the candidate's own window, which is what makes
/// them contemporaneous.
#[test]
fn every_control_is_drawn_from_the_same_window() {
    let session = build(20, 4, true);
    let spec = test_spec();
    let inputs = Inputs {
        spec: &spec,
        dataset: &session.dataset,
        discovery: &session.discovery,
        ladders: &session.ladders,
        reference: &session.reference,
    };
    for control in [Control::Contemporaneous, Control::DetectionRecency, Control::MoveMagnitude] {
        let rows = observations(&inputs, Score::EarlyQuality, Selector::TopK(5), control, 2.0, 300, None);
        assert!(!rows.is_empty(), "{control:?} produced no observations");
        // Every observation belongs to an opportunity that exists in exactly
        // one window, so membership cannot have crossed a window boundary.
        let ids: std::collections::BTreeSet<&str> =
            rows.iter().map(|r| r.unit.as_str()).collect();
        assert_eq!(ids.len(), rows.len(), "{control:?} emitted a duplicate unit");
    }
}

/// The recency control selects the newest openings, and the magnitude control
/// the largest prior moves — both at the candidate's cohort size.
#[test]
fn the_alternative_controls_select_what_they_claim_to() {
    let session = build(20, 2, true);
    let spec = test_spec();
    let inputs = Inputs {
        spec: &spec,
        dataset: &session.dataset,
        discovery: &session.discovery,
        ladders: &session.ladders,
        reference: &session.reference,
    };
    let recency =
        observations(&inputs, Score::EarlyQuality, Selector::TopK(5), Control::DetectionRecency, 2.0, 300, None);
    let control_count = recency.iter().filter(|o| o.in_control).count();
    assert_eq!(control_count, 5 * 2, "five per window, two windows");

    let magnitude =
        observations(&inputs, Score::EarlyQuality, Selector::TopK(5), Control::MoveMagnitude, 2.0, 300, None);
    assert_eq!(magnitude.iter().filter(|o| o.in_control).count(), 5 * 2);

    // The contemporaneous control is everyone, which is a different size.
    let everyone =
        observations(&inputs, Score::EarlyQuality, Selector::TopK(5), Control::Contemporaneous, 2.0, 300, None);
    assert_eq!(everyone.iter().filter(|o| o.in_control).count(), 40);
}

/// Membership uses the first window's rank, never the best rank ever held.
#[test]
fn membership_uses_the_first_window_rank_not_the_best() {
    let mut session = build(20, 2, true);
    // An opportunity that ranked 19th on entry and 1st later must not be in
    // the candidate cohort: using the later rank would be look-ahead.
    session.dataset.rows[18].first_window_early_rank = Some(19);
    session.dataset.rows[18].best_early_rank = Some(1);
    let spec = test_spec();
    let inputs = Inputs {
        spec: &spec,
        dataset: &session.dataset,
        discovery: &session.discovery,
        ladders: &session.ladders,
        reference: &session.reference,
    };
    let rows = observations(&inputs, Score::EarlyQuality, Selector::TopK(5), Control::Contemporaneous, 2.0, 300, None);
    let target = rows.iter().find(|o| o.unit.starts_with(&session.dataset.rows[18].symbol)).unwrap();
    assert!(!target.in_candidate, "a later good rank must not backdate membership");
}

// ---------------------------------------------------------------------------
// Minimum evidence and censoring
// ---------------------------------------------------------------------------

#[test]
fn a_thin_session_reports_shortfalls_rather_than_failures() {
    let session = build(5, 2, true);
    let evaluation = run(&session, &QualificationSpec::default());
    assert!(!evaluation.shortfalls.is_empty(), "a tiny session must not silently qualify");
    assert!(
        evaluation.shortfalls.iter().any(|s| s.contains("distinct opportunities")),
        "{:?}",
        evaluation.shortfalls
    );
}

#[test]
fn censored_opportunities_are_excluded_and_counted() {
    let mut session = build(40, 4, true);
    // Strip the price series from a quarter of the symbols: their outcome
    // becomes unknowable.
    let victims: Vec<String> =
        session.dataset.rows.iter().take(40).map(|r| r.symbol.clone()).collect();
    for symbol in victims {
        session.discovery.series.remove(&symbol);
    }
    let evaluation = run(&session, &test_spec());
    let a1 = evaluation
        .results
        .iter()
        .find(|r| r.criterion_id == "A1-early-quality-enrichment")
        .unwrap();
    assert!(
        a1.estimate.n_censored > 0,
        "opportunities with no price series must be counted as censored, not as failures"
    );
}

// ---------------------------------------------------------------------------
// The whole matrix
// ---------------------------------------------------------------------------

#[test]
fn every_contract_criterion_receives_a_result() {
    let session = build(40, 6, true);
    let spec = test_spec();
    let evaluation = run(&session, &spec);
    for criterion in &spec.criteria {
        assert!(
            evaluation.results.iter().any(|r| r.criterion_id == criterion.id),
            "{} produced no result; a silently missing gate must not pass by omission",
            criterion.id
        );
    }
}

#[test]
fn the_population_counts_distinguish_rows_from_opportunities() {
    let session = build(30, 5, true);
    let evaluation = run(&session, &test_spec());
    let p = &evaluation.population;
    assert_eq!(p.distinct_opportunities, 150);
    assert_eq!(p.distinct_symbols, 150);
    assert_eq!(p.ranking_windows, 5);
    assert!(
        p.raw_snapshot_rows > p.distinct_opportunities as u64,
        "the collapse must remain visible in the reported population"
    );
}

#[test]
fn the_secondary_direction_is_measured_but_produces_no_gate() {
    let session = build(40, 6, true);
    let spec = test_spec();
    let evaluation = run(&session, &spec);
    assert!(!evaluation.secondary_direction.is_empty(), "the +5/+10 direction must be measured");
    for surface in &evaluation.secondary_direction {
        assert!(
            spec.secondary_targets_pct.contains(&surface.target_pct),
            "only secondary targets belong here"
        );
    }
    // And no blocking result may reference a secondary target.
    let a3 = evaluation
        .results
        .iter()
        .find(|r| r.criterion_id == "A3-direction-at-secondary-targets")
        .unwrap();
    assert!(!a3.blocking);
}

#[test]
fn segmentation_covers_every_declared_dimension() {
    let session = build(40, 6, true);
    let evaluation = run(&session, &test_spec());
    let dimensions: std::collections::BTreeSet<&str> =
        evaluation.segments.iter().map(|s| s.dimension.as_str()).collect();
    for expected in ["regime", "price band", "time of day", "opening detector", "confluence"] {
        assert!(dimensions.contains(expected), "no {expected} segmentation; got {dimensions:?}");
    }
}

#[test]
fn the_evaluation_is_deterministic() {
    let session = build(30, 4, true);
    let spec = test_spec();
    let first = run(&session, &spec);
    for _ in 0..3 {
        let again = run(&session, &spec);
        assert_eq!(first.results, again.results, "identical input must give an identical result");
        assert_eq!(first.segments, again.segments);
    }
}

/// A criterion named on several primary surfaces is decided by the worst of
/// them, so it cannot pass on whichever one happened to look best.
#[test]
fn a_criterion_is_decided_by_its_worst_primary_surface() {
    assert_eq!(worst(&[Outcome::Pass, Outcome::Fail]), Outcome::Fail);
    assert_eq!(worst(&[Outcome::Pass, Outcome::Insufficient]), Outcome::Insufficient);
    assert_eq!(worst(&[Outcome::Fail, Outcome::Insufficient]), Outcome::Fail);
    assert_eq!(worst(&[Outcome::Pass, Outcome::Pass]), Outcome::Pass);
    assert_eq!(worst(&[]), Outcome::Insufficient, "no surface measured is not a pass");
}

#[test]
fn the_contracts_primary_surfaces_parse_into_selectors() {
    let spec = QualificationSpec::default();
    let early = primary_selectors(&spec, Score::EarlyQuality);
    assert!(early.contains(&Selector::TopK(5)), "{early:?}");
    assert!(early.contains(&Selector::TopPercent(0.10)), "{early:?}");
    let continuation = primary_selectors(&spec, Score::Continuation);
    assert_eq!(continuation.len(), 2);
}
