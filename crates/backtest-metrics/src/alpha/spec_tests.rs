//! Tests for the pre-registered qualification contract.
//!
//! The contract decides what would count as evidence, so a defect here is not a
//! wrong number — it is a wrong standard. These tests are mostly about the
//! properties that make a pre-registration meaningful: it is stable, it is
//! hashable, a diagnostic result cannot decide it, and every comparison it
//! blocks on has parity with its control.

use super::*;

#[test]
fn the_default_specification_is_internally_valid() {
    let spec = QualificationSpec::default();
    spec.validate().expect("the shipped contract must satisfy its own rules");
}

// ---------------------------------------------------------------------------
// Stability -- a pre-registration that cannot be hashed proves nothing
// ---------------------------------------------------------------------------

#[test]
fn the_hash_is_stable_across_constructions() {
    let first = QualificationSpec::default().sha256();
    for _ in 0..8 {
        assert_eq!(
            QualificationSpec::default().sha256(),
            first,
            "the frozen contract must hash identically every time it is built"
        );
    }
    assert_eq!(first.len(), 64);
    println!("qualification-spec sha256: {first}");
}

/// `frozen_at` is a literal precisely so this holds. A spec stamped with
/// `now()` would hash differently on every run and could not pre-register
/// anything.
#[test]
fn the_freeze_timestamp_is_a_literal_not_a_clock() {
    let a = QualificationSpec::default();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let b = QualificationSpec::default();
    assert_eq!(a.frozen_at, b.frozen_at);
    assert_eq!(a.frozen_at.to_rfc3339(), "2026-09-17T09:00:00+00:00");
}

#[test]
fn the_specification_round_trips_as_json() {
    let spec = QualificationSpec::default();
    let text = spec.canonical_json();
    let back: QualificationSpec = serde_json::from_str(&text).unwrap();
    assert_eq!(spec, back, "the persisted spec must be the spec that was applied");
    assert_eq!(back.sha256(), spec.sha256(), "and must hash to the same value");
}

/// Canonical JSON must not depend on map ordering, or two identical
/// specifications could hash differently on different machines.
#[test]
fn the_canonical_form_has_sorted_keys() {
    let text = QualificationSpec::default().canonical_json();
    let top: serde_json::Value = serde_json::from_str(&text).unwrap();
    let keys: Vec<&String> = top.as_object().unwrap().keys().collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted, "canonical JSON must be key-ordered");
}

/// Any change to the contract must change its hash — that is the entire
/// mechanism by which a reader can tell the criteria were not edited after the
/// result.
#[test]
fn any_change_to_the_contract_changes_its_hash() {
    let base = QualificationSpec::default();
    let baseline_hash = base.sha256();

    let mut loosened = QualificationSpec::default();
    loosened.minimum_evidence.positive_reference_opportunities = 1;
    assert_ne!(loosened.sha256(), baseline_hash, "a weakened evidence floor must be visible");

    let mut retargeted = QualificationSpec::default();
    retargeted.primary_targets_pct = vec![10.0];
    assert_ne!(retargeted.sha256(), baseline_hash, "a changed primary target must be visible");

    let mut unblocked = QualificationSpec::default();
    unblocked.criteria[0].blocking = false;
    assert_ne!(unblocked.sha256(), baseline_hash, "disarming a gate must be visible");

    // The shipped contract is already bound; rebinding it elsewhere must still
    // change the hash, because a contract bound to a different configuration is
    // a different contract.
    let rebound = QualificationSpec::default().bind_to_configuration("oi-cfg-0000000000000000");
    assert_ne!(rebound.sha256(), baseline_hash);
    assert_eq!(
        QualificationSpec::default().expected_oi_config_fingerprint.as_deref(),
        Some(EXPECTED_OI_CONFIG_FINGERPRINT),
        "the contract must ship bound to the deployed configuration"
    );
}

// ---------------------------------------------------------------------------
// The properties that make it a contract rather than a preference
// ---------------------------------------------------------------------------

/// §28: a diagnostic surface must never be able to decide qualification.
#[test]
fn a_secondary_surface_may_not_be_blocking() {
    let mut spec = QualificationSpec::default();
    let index = spec
        .criteria
        .iter()
        .position(|c| c.surface == Surface::Secondary)
        .expect("the contract has diagnostic criteria");
    spec.criteria[index].blocking = true;
    let error = spec.validate().expect_err("promoting a diagnostic to a gate must be rejected");
    assert!(error.contains("secondary surface"), "{error}");
}

/// §33: a blocking comparison whose control lacks parity must be descriptive.
#[test]
fn a_blocking_criterion_requires_a_parity_control() {
    let mut spec = QualificationSpec::default();
    spec.controls[0].same_horizon = false;
    let error = spec.validate().expect_err("a non-parity control must not gate qualification");
    assert!(error.contains("parity"), "{error}");
}

#[test]
fn a_criterion_may_not_name_an_undefined_control() {
    let mut spec = QualificationSpec::default();
    spec.criteria[0].control = Some("a-control-that-does-not-exist".to_string());
    assert!(spec.validate().is_err());
}

/// §24 names four dimensions that must carry blocking evidence.
#[test]
fn every_required_dimension_has_a_blocking_criterion() {
    let spec = QualificationSpec::default();
    for required in [
        Dimension::EarlyQuality,
        Dimension::ContinuationQuality,
        Dimension::Recall,
        Dimension::RiskExcursion,
    ] {
        assert!(
            spec.criteria.iter().any(|c| c.dimension == required && c.blocking),
            "{required:?} must have a blocking criterion"
        );
    }
    // And removing one must be caught.
    let mut stripped = QualificationSpec::default();
    stripped.criteria.retain(|c| c.dimension != Dimension::Recall);
    assert!(stripped.validate().is_err(), "a missing required dimension must be rejected");
}

/// §26: the matrix must cover all seven dimensions, so nothing is silently
/// absent from the report.
#[test]
fn the_matrix_covers_every_dimension() {
    let spec = QualificationSpec::default();
    for dimension in Dimension::ALL {
        assert!(
            spec.criteria.iter().any(|c| c.dimension == dimension),
            "{dimension:?} has no criterion at all"
        );
    }
}

/// §24: numeric gates only where defensible. Every blocking criterion must be
/// comparative or structural — never a bare threshold on V1's own rate.
#[test]
fn no_blocking_criterion_is_an_invented_performance_threshold() {
    let spec = QualificationSpec::default();
    for criterion in spec.blocking() {
        match &criterion.rule {
            // Comparative: needs a control, and the bound is "no difference".
            Rule::EnrichmentLowerBoundAbove { value } => {
                assert_eq!(*value, 1.0, "{}: enrichment gates compare to parity, not to a chosen level", criterion.id);
                assert!(criterion.control.is_some(), "{}: comparative rules need a control", criterion.id);
            }
            Rule::UpliftLowerBoundAbove { value } => {
                assert_eq!(*value, 0.0, "{}", criterion.id);
                assert!(criterion.control.is_some());
            }
            // Shape constraints carry no magnitude at all.
            Rule::MonotoneAcrossRanks => {}
            // Tolerances are for boundary effects, and must stay small.
            Rule::NoWorseThanControlBy { fraction } => {
                assert!(
                    *fraction <= 0.10,
                    "{}: a tolerance above 10% stops being a boundary allowance",
                    criterion.id
                );
                assert!(criterion.control.is_some());
            }
            // Only structural values: zero lead time, or a half that decides
            // whether the unknowns dominate.
            Rule::MedianAbove { value } => {
                assert!(
                    *value == 0.0 || *value == 0.5,
                    "{}: {value} is not a structural value",
                    criterion.id
                );
            }
            Rule::Descriptive => {
                panic!("{}: a descriptive rule cannot be blocking", criterion.id)
            }
        }
        assert!(
            criterion.rationale.len() > 80,
            "{}: a pre-registered criterion needs its reasoning recorded",
            criterion.id
        );
    }
}

/// §34: minimum evidence decides answerability, not quality, and must be well
/// below a normal session so it excludes only genuinely thin ones.
#[test]
fn minimum_evidence_is_below_a_normal_session() {
    let m = MinimumEvidence::default();
    // Reconstructed September-16 regular session, for scale.
    assert!(m.distinct_opportunities < 19_963 / 5, "{} is not well below a normal session", m.distinct_opportunities);
    assert!(m.distinct_symbols < 8_886 / 5);
    assert!(m.ranking_windows < 780 / 2);
    assert!(m.positive_reference_opportunities >= 30, "too low to estimate a rate at all");
    assert!(m.observations_per_primary_surface >= 10);
    assert!(
        m.regimes_required.is_empty(),
        "no regime-specific claim is blocking in v1, so none may be required"
    );
}

// ---------------------------------------------------------------------------
// Model identity
// ---------------------------------------------------------------------------

/// The contract is written against a specific model. If any of these move, the
/// spec is evaluating something it was not written for.
#[test]
fn the_contract_pins_the_model_identity_it_was_written_against() {
    let spec = QualificationSpec::default();
    assert_eq!(spec.expected_early_quality_model, crate::opportunity::EARLY_QUALITY_MODEL_VERSION);
    assert_eq!(spec.expected_continuation_model, crate::opportunity::CONTINUATION_MODEL_VERSION);
    assert_eq!(spec.expected_ranking, crate::opportunity::RANKING_VERSION);
    assert_eq!(spec.expected_score_policy, crate::opportunity::SCORE_POLICY_VERSION);
    assert_eq!(spec.expected_regime_classifier, crate::opportunity::REGIME_CLASSIFIER_VERSION);
    assert_eq!(spec.reference_label_version, crate::alpha::labels::REFERENCE_LABEL_VERSION);
    assert_eq!(spec.reference_label, crate::alpha::labels::ReferenceLabelSpec::default());
}

/// §28: primary and secondary surfaces must be disjoint, or "primary" means
/// nothing.
#[test]
fn primary_and_secondary_surfaces_are_disjoint() {
    let spec = QualificationSpec::default();
    for target in &spec.primary_targets_pct {
        assert!(!spec.secondary_targets_pct.contains(target), "{target} is both primary and secondary");
    }
    for horizon in &spec.primary_horizons_secs {
        assert!(!spec.secondary_horizons_secs.contains(horizon));
    }
    assert!(!spec.primary_ranking_surfaces.is_empty());
    // The primary target is the most estimable of the frozen family, not the
    // most flattering: +2% is the most frequent.
    assert_eq!(spec.primary_targets_pct, vec![crate::horizon::TARGET_PCTS[0]]);
}

/// Censoring is stated in the contract, not decided during analysis.
#[test]
fn censoring_and_clustering_are_pre_registered() {
    let spec = QualificationSpec::default();
    assert!(spec.censoring_treatment.contains("never converted to failures"));
    assert_eq!(spec.uncertainty.cluster_unit, "symbol");
    assert!(spec.uncertainty.resamples >= 1_000);
    assert!(spec.analytical_unit.starts_with("opportunity"));
    assert!(!spec.required_completeness.is_empty());
}

// ---------------------------------------------------------------------------
// The 2026-09-17 contract review
// ---------------------------------------------------------------------------

/// Recall is three distinct questions, and only two of them grade OI.
#[test]
fn recall_is_split_into_three_questions_with_the_right_subjects() {
    let spec = QualificationSpec::default();
    let by_id = |id: &str| spec.criteria.iter().find(|c| c.id == id).cloned();

    // B. Conditional retention -- grades OI, blocking, conditional on detection.
    let c1 = by_id("C1-oi-conditional-retention").expect("conditional retention criterion");
    assert!(c1.blocking);
    assert_eq!(c1.surface, Surface::Primary);
    assert!(
        c1.metric.contains("DETECTION reached"),
        "the population must be explicitly conditional on detection: {}",
        c1.metric
    );

    // A. Detection coverage -- grades the scanner, reported, never a gate.
    let c3 = by_id("C3-independent-detection-coverage").expect("detection coverage criterion");
    assert!(!c3.blocking, "coverage characterises the scanner, not OI, so it must not gate OI");
    assert_eq!(c3.rule, Rule::Descriptive);
    assert!(c3.rationale.contains("never had the opportunity to perform"));

    // C. Attribution measurability -- still blocking.
    let c2 = by_id("C2-recall-attribution-established").expect("attribution criterion");
    assert!(c2.blocking, "an unmeasurable recall analysis is not a recall analysis");
}

/// Decision 2: +5% and +10% stay diagnostic, but their direction must be
/// reported. A reporting duty, never a gate.
#[test]
fn the_secondary_direction_is_reported_but_cannot_gate() {
    let spec = QualificationSpec::default();
    let a3 = spec
        .criteria
        .iter()
        .find(|c| c.id == "A3-direction-at-secondary-targets")
        .expect("secondary-direction criterion");
    assert!(!a3.blocking);
    assert_eq!(a3.surface, Surface::Secondary);
    assert_eq!(a3.rule, Rule::Descriptive);

    let requirement = spec
        .reporting
        .iter()
        .find(|r| r.id == "R-secondary-direction")
        .expect("secondary-direction reporting requirement");
    assert!(requirement.descriptive_only);
    assert_eq!(requirement.breakdowns, vec!["+5%".to_string(), "+10%".to_string()]);
}

/// Decision 4: the report owes a reader both conclusions, separately.
#[test]
fn the_contract_requires_both_interpretations_to_be_reported() {
    let spec = QualificationSpec::default();
    let two = spec
        .reporting
        .iter()
        .find(|r| r.id == "R-two-interpretations")
        .expect("two-interpretation requirement");
    assert!(two.description.contains("OI LAYER QUALIFICATION"));
    assert!(two.description.contains("END-TO-END SCANNER COVERAGE"));
    assert!(
        two.description.contains("must not hide") || two.description.contains("must not make"),
        "the requirement must state the failure it prevents"
    );
    assert!(!two.descriptive_only, "stating both conclusions is an obligation, not a flourish");

    // And dropping it must fail validation.
    let mut stripped = QualificationSpec::default();
    stripped.reporting.retain(|r| r.id != "R-two-interpretations");
    assert!(stripped.validate().is_err());
}

/// Decision 3A: the coverage metric's required breakdowns are part of the
/// contract, not the report generator's discretion.
#[test]
fn detection_coverage_breakdowns_are_pre_registered() {
    let spec = QualificationSpec::default();
    let coverage = spec
        .reporting
        .iter()
        .find(|r| r.id == "R-detection-coverage")
        .expect("coverage reporting requirement");
    for required in ["overall", "+2%", "+5%", "+10%", "by price regime", "by time of day", "by causal miss stage"] {
        assert!(
            coverage.breakdowns.iter().any(|b| b == required),
            "coverage must be broken down {required}"
        );
    }
    assert!(coverage.descriptive_only, "no absolute pass threshold is pre-registered for it");
}

/// Decision 5: the runner ladder must distinguish all six outcomes, and keep
/// UNKNOWN.
#[test]
fn the_runner_ladder_distinguishes_every_miss_stage() {
    let spec = QualificationSpec::default();
    let ladder = spec
        .reporting
        .iter()
        .find(|r| r.id == "R-runner-stage-ladder")
        .expect("runner ladder requirement");
    assert_eq!(ladder.breakdowns.len(), 7, "six classifications plus UNKNOWN");
    for expected in ["A never visible", "B visible, detector missed", "C detected, no OI opportunity",
                     "D OI opportunity, ranked poorly", "E ranked highly, too late",
                     "F ranked highly and early", "UNKNOWN"] {
        assert!(ladder.breakdowns.iter().any(|b| b == expected), "missing {expected}");
    }
}

/// Decision 6: the contract is bound, and an unbound one is refused.
#[test]
fn the_contract_is_bound_to_the_deployed_configuration() {
    let spec = QualificationSpec::default();
    assert_eq!(
        spec.expected_oi_config_fingerprint.as_deref(),
        Some("oi-cfg-b4f21c8b311a1b99")
    );
    spec.validate().unwrap();

    let mut unbound = QualificationSpec::default();
    unbound.expected_oi_config_fingerprint = None;
    let error = unbound.validate().expect_err("an unbound contract cannot evaluate a session");
    assert!(error.contains("bound to an OI configuration"), "{error}");
}

/// Decision 1: the two additional blocking dimensions are kept.
#[test]
fn ranking_quality_and_earliness_remain_blocking() {
    let spec = QualificationSpec::default();
    for dimension in [Dimension::RankingQuality, Dimension::Earliness] {
        assert!(
            spec.criteria.iter().any(|c| c.dimension == dimension && c.blocking),
            "{dimension:?} must remain blocking per the contract review"
        );
    }
}

/// The version was bumped, so a v1 result can never be mistaken for a v2 one.
#[test]
fn the_reviewed_contract_is_a_new_version() {
    assert_eq!(QualificationSpec::default().version, "alpha-qualification-v3");
}
