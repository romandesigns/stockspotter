//! Tests for the clustered statistics.
//!
//! These are the tests that stop the qualification manufacturing precision. The
//! two that matter most are `repeated_ranking_snapshots_do_not_inflate_n` and
//! `clustering_widens_the_interval_relative_to_naive_resampling` — the first
//! because it is the difference between 2.5 million rows and a few thousand
//! opportunities, the second because it is the whole reason to cluster at all.

use super::*;

fn obs(cluster: &str, unit: &str, candidate: bool, outcome: Option<bool>) -> Observation {
    Observation {
        cluster: cluster.to_string(),
        unit: unit.to_string(),
        in_candidate: candidate,
        in_control: true,
        outcome,
        value: None,
    }
}

fn rng() -> Rng {
    Rng::from_hex_seed("c4afe217e7df3e1bf8508afef5605001d5081ba57b42c43bcc143263470ed77d")
}

// ---------------------------------------------------------------------------
// Collapsing -- the analytical unit is the opportunity
// ---------------------------------------------------------------------------

/// The single most important property here. One opportunity ranked in forty
/// windows is one observation, not forty.
#[test]
fn repeated_ranking_snapshots_do_not_inflate_n() {
    let mut rows = Vec::new();
    for window in 0..40 {
        rows.push(obs("AAA", "AAA:2026-09-17:1", true, Some(true)));
        rows.push(obs("BBB", "BBB:2026-09-17:1", false, Some(false)));
        let _ = window;
    }
    assert_eq!(rows.len(), 80, "the fixture must actually repeat");

    let (units, clusters) = counts(&rows);
    assert_eq!(units, 2, "two opportunities, whatever the row count");
    assert_eq!(clusters, 2, "two symbols");

    let estimate = cluster_bootstrap(&rows, Quantity::Rate, 200, 0.95, &mut rng());
    assert_eq!(estimate.n_units, 2);
    assert_eq!(estimate.n_raw_rows, 80, "the raw count is still reported, not hidden");
    assert!(
        estimate.n_units < estimate.n_raw_rows,
        "inflation must be visible as the gap between these two"
    );
}

/// An opportunity enters a cohort at its *first* qualifying window, and the
/// outcome recorded is the one from that instant. Taking the best window would
/// let the analysis pick, after the fact, where each opportunity looked
/// strongest.
#[test]
fn a_unit_enters_a_cohort_at_its_first_qualifying_window_not_its_best() {
    let rows = vec![
        // Window 1: not yet top-ranked, and the forward measurement fails.
        obs("AAA", "AAA:1", false, Some(false)),
        // Window 2: enters the candidate cohort, still failing.
        obs("AAA", "AAA:1", true, Some(false)),
        // Window 3: would have looked much better, and must not be the one used.
        obs("AAA", "AAA:1", true, Some(true)),
    ];
    let units = collapse(&rows);
    assert_eq!(units.len(), 1);
    assert!(units[0].in_candidate, "it did enter the cohort");
    assert_eq!(
        units[0].outcome,
        Some(false),
        "the outcome must be the one from first entry, not the flattering later one"
    );
}

#[test]
fn cohort_membership_is_monotone() {
    let rows = vec![
        obs("AAA", "AAA:1", false, Some(false)),
        obs("AAA", "AAA:1", true, Some(true)),
        obs("AAA", "AAA:1", false, Some(false)),
    ];
    let units = collapse(&rows);
    assert!(units[0].in_candidate, "once entered, an opportunity does not leave the cohort");
}

#[test]
fn collapsing_preserves_first_seen_order() {
    let rows = vec![
        obs("CCC", "CCC:1", true, Some(true)),
        obs("AAA", "AAA:1", true, Some(true)),
        obs("BBB", "BBB:1", true, Some(true)),
        obs("AAA", "AAA:1", true, Some(true)),
    ];
    let units = collapse(&rows);
    let order: Vec<&str> = units.iter().map(|u| u.unit.as_str()).collect();
    assert_eq!(order, vec!["CCC:1", "AAA:1", "BBB:1"]);
}

// ---------------------------------------------------------------------------
// Censoring -- never a failure
// ---------------------------------------------------------------------------

/// §33 / the label's own rule: a censored measurement is excluded from both
/// cohorts and reported, never counted as a negative.
#[test]
fn censored_outcomes_are_excluded_not_counted_as_failures() {
    let rows = vec![
        obs("AAA", "AAA:1", true, Some(true)),
        obs("BBB", "BBB:1", true, Some(true)),
        obs("CCC", "CCC:1", true, None), // censored
        obs("DDD", "DDD:1", true, None), // censored
    ];
    let estimate = cluster_bootstrap(&rows, Quantity::Rate, 500, 0.95, &mut rng());
    assert_eq!(
        estimate.point,
        Some(1.0),
        "two of two *known* outcomes succeeded; the censored pair must not drag it to 0.5"
    );
    assert_eq!(estimate.n_censored, 2, "and the exclusion must be reported");
    assert_eq!(estimate.n_units, 4);
}

// ---------------------------------------------------------------------------
// Clustering -- the reason the interval is wide
// ---------------------------------------------------------------------------

/// Forty opportunities from one symbol are one independent draw, not forty.
/// This is the property that stops a single lucky symbol from looking decisive.
#[test]
fn clustering_counts_symbols_not_opportunities() {
    let mut rows = Vec::new();
    for i in 0..40 {
        rows.push(obs("SAME", &format!("SAME:{i}"), true, Some(true)));
    }
    let estimate = cluster_bootstrap(&rows, Quantity::Rate, 500, 0.95, &mut rng());
    assert_eq!(estimate.n_units, 40, "forty opportunities");
    assert_eq!(estimate.n_clusters, 1, "but one independent symbol");
    assert!(
        !estimate.is_estimable(),
        "and one cluster cannot support an interval at all"
    );
    assert_eq!(estimate.point, Some(1.0), "the point estimate is still reported");
}

/// The same total evidence spread over more symbols yields a tighter interval.
#[test]
fn more_independent_symbols_tighten_the_interval() {
    let build = |symbols: usize, per_symbol: usize| -> Vec<Observation> {
        let mut rows = Vec::new();
        for s in 0..symbols {
            for o in 0..per_symbol {
                // A stable 50% rate however it is divided up.
                let hit = (s * per_symbol + o) % 2 == 0;
                rows.push(obs(&format!("S{s}"), &format!("S{s}:{o}"), true, Some(hit)));
            }
        }
        rows
    };
    let few = cluster_bootstrap(&build(4, 25), Quantity::Rate, 1_000, 0.95, &mut rng());
    let many = cluster_bootstrap(&build(50, 2), Quantity::Rate, 1_000, 0.95, &mut rng());

    assert_eq!(few.n_units, 100);
    assert_eq!(many.n_units, 100, "the same number of opportunities either way");
    let few_width = few.upper.unwrap() - few.lower.unwrap();
    let many_width = many.upper.unwrap() - many.lower.unwrap();
    assert!(
        many_width < few_width,
        "50 symbols must give a tighter interval than 4: {many_width:.3} vs {few_width:.3}"
    );
}

// ---------------------------------------------------------------------------
// The comparative quantities the criteria are decided on
// ---------------------------------------------------------------------------

#[test]
fn a_real_enrichment_produces_a_lower_bound_above_one() {
    // Candidates hit 80%, the surrounding cohort 40%, across many symbols.
    let mut rows = Vec::new();
    for s in 0..60 {
        let candidate_hit = s % 5 != 0; // 80%
        rows.push(Observation {
            cluster: format!("S{s}"),
            unit: format!("S{s}:cand"),
            in_candidate: true,
            in_control: true,
            outcome: Some(candidate_hit),
            value: None,
        });
        for o in 0..4 {
            let control_hit = (s + o) % 5 < 2; // 40%
            rows.push(Observation {
                cluster: format!("S{s}"),
                unit: format!("S{s}:ctrl{o}"),
                in_candidate: false,
                in_control: true,
                outcome: Some(control_hit),
                value: None,
            });
        }
    }
    let estimate = cluster_bootstrap(&rows, Quantity::RateRatio, 2_000, 0.95, &mut rng());
    assert!(estimate.is_estimable());
    assert!(estimate.point.unwrap() > 1.3, "point ratio {:?}", estimate.point);
    assert_eq!(
        estimate.lower_bound_above(1.0),
        Some(true),
        "a real 2x enrichment across 60 symbols must clear parity: {estimate:?}"
    );
}

/// The converse, and the one that actually protects the verdict: when the
/// candidate is no better than its control, the interval must span 1.
#[test]
fn no_real_difference_does_not_clear_parity() {
    let mut rows = Vec::new();
    for s in 0..60 {
        for o in 0..5 {
            let hit = (s * 5 + o) % 2 == 0; // 50% for everyone
            rows.push(Observation {
                cluster: format!("S{s}"),
                unit: format!("S{s}:{o}"),
                in_candidate: o == 0,
                in_control: true,
                outcome: Some(hit),
                value: None,
            });
        }
    }
    let estimate = cluster_bootstrap(&rows, Quantity::RateRatio, 2_000, 0.95, &mut rng());
    assert!(estimate.is_estimable());
    assert_eq!(
        estimate.lower_bound_above(1.0),
        Some(false),
        "an equal-rate cohort must not read as enriched: {estimate:?}"
    );
}

#[test]
fn mean_ratios_work_for_continuous_measurements() {
    let mut rows = Vec::new();
    for s in 0..40 {
        rows.push(Observation {
            cluster: format!("S{s}"),
            unit: format!("S{s}:cand"),
            in_candidate: true,
            in_control: true,
            outcome: None,
            value: Some(3.0),
        });
        rows.push(Observation {
            cluster: format!("S{s}"),
            unit: format!("S{s}:ctrl"),
            in_candidate: false,
            in_control: true,
            outcome: None,
            value: Some(1.5),
        });
    }
    let estimate = cluster_bootstrap(&rows, Quantity::MeanRatio, 1_000, 0.95, &mut rng());
    assert!(estimate.point.unwrap() > 1.0);
    assert_eq!(estimate.lower_bound_above(1.0), Some(true));
}

// ---------------------------------------------------------------------------
// Determinism and degenerate input
// ---------------------------------------------------------------------------

#[test]
fn the_bootstrap_is_deterministic_for_a_given_seed() {
    let rows: Vec<Observation> = (0..40)
        .map(|s| obs(&format!("S{s}"), &format!("S{s}:1"), s % 3 == 0, Some(s % 2 == 0)))
        .collect();
    let first = cluster_bootstrap(&rows, Quantity::Rate, 500, 0.95, &mut rng());
    for _ in 0..5 {
        assert_eq!(
            cluster_bootstrap(&rows, Quantity::Rate, 500, 0.95, &mut rng()),
            first,
            "identical input and seed must give an identical estimate"
        );
    }
}

#[test]
fn a_different_seed_gives_a_similar_but_not_identical_interval() {
    let rows: Vec<Observation> = (0..60)
        .map(|s| obs(&format!("S{s}"), &format!("S{s}:1"), true, Some(s % 3 != 0)))
        .collect();
    let a = cluster_bootstrap(&rows, Quantity::Rate, 1_000, 0.95, &mut Rng::from_hex_seed("aa"));
    let b = cluster_bootstrap(&rows, Quantity::Rate, 1_000, 0.95, &mut Rng::from_hex_seed("bb"));
    assert_eq!(a.point, b.point, "the point estimate does not depend on the seed");
    assert!(
        (a.lower.unwrap() - b.lower.unwrap()).abs() < 0.15,
        "and the interval should be close: {a:?} vs {b:?}"
    );
}

#[test]
fn an_empty_or_degenerate_sample_reports_nothing_rather_than_guessing() {
    let empty = cluster_bootstrap(&[], Quantity::Rate, 500, 0.95, &mut rng());
    assert_eq!(empty.point, None);
    assert!(!empty.is_estimable());
    assert_eq!(empty.n_units, 0);

    // Every outcome censored: nothing is estimable, and nothing is invented.
    let censored: Vec<Observation> =
        (0..10).map(|s| obs(&format!("S{s}"), &format!("S{s}:1"), true, None)).collect();
    let estimate = cluster_bootstrap(&censored, Quantity::Rate, 500, 0.95, &mut rng());
    assert_eq!(estimate.point, None);
    assert_eq!(estimate.n_censored, 10);

    // A control rate of zero makes a ratio undefined rather than infinite.
    let rows = vec![
        Observation { cluster: "A".into(), unit: "A:1".into(), in_candidate: true, in_control: false, outcome: Some(true), value: None },
        Observation { cluster: "B".into(), unit: "B:1".into(), in_candidate: false, in_control: true, outcome: Some(false), value: None },
    ];
    let ratio = cluster_bootstrap(&rows, Quantity::RateRatio, 500, 0.95, &mut rng());
    assert_eq!(ratio.point, None, "division by a zero control rate must not produce infinity");
}

#[test]
fn the_seed_is_derived_from_the_specification_hash() {
    let spec = crate::alpha::spec::QualificationSpec::default();
    let from_spec = Rng::from_hex_seed(&spec.sha256());
    let same = Rng::from_hex_seed(&spec.sha256());
    let rows: Vec<Observation> =
        (0..30).map(|s| obs(&format!("S{s}"), &format!("S{s}:1"), true, Some(s % 2 == 0))).collect();
    let a = cluster_bootstrap(&rows, Quantity::Rate, 400, 0.95, &mut from_spec.clone());
    let b = cluster_bootstrap(&rows, Quantity::Rate, 400, 0.95, &mut same.clone());
    assert_eq!(a, b, "the same contract must always seed the same resampling");
}
