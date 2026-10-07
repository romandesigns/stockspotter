//! Tests for the qualification matrix and the evidence verdict.
//!
//! The derivation is small and the tests are correspondingly blunt, which is
//! the point: this is the function that decides what the programme concludes,
//! and every path through it should be pinned by name.

use super::*;

use crate::alpha::stats::Estimate;

fn estimate(point: f64, lower: f64, upper: f64) -> Estimate {
    Estimate {
        point: Some(point),
        lower: Some(lower),
        upper: Some(upper),
        confidence: 0.95,
        n_units: 500,
        n_clusters: 120,
        n_raw_rows: 40_000,
        n_censored: 12,
        method: "rate ratio, cluster bootstrap by symbol".to_string(),
    }
}

fn unestimable() -> Estimate {
    Estimate {
        point: Some(1.4),
        lower: None,
        upper: None,
        confidence: 0.95,
        n_units: 3,
        n_clusters: 1,
        n_raw_rows: 90,
        n_censored: 0,
        method: "rate ratio, cluster bootstrap by symbol".to_string(),
    }
}

/// A result for every blocking criterion in the contract, all passing.
fn all_blocking_pass() -> Vec<CriterionResult> {
    let spec = QualificationSpec::default();
    spec.criteria
        .iter()
        .map(|c| CriterionResult {
            criterion_id: c.id.clone(),
            dimension: c.dimension,
            surface: c.surface,
            blocking: c.blocking,
            metric: c.metric.clone(),
            control: c.control.clone(),
            candidate_value: Some(0.6),
            control_value: Some(0.4),
            comparison: Some(1.5),
            estimate: estimate(1.5, 1.2, 1.9),
            criterion: format!("{:?}", c.rule),
            outcome: Outcome::Pass,
            notes: Vec::new(),
        })
        .collect()
}

fn assemble(results: Vec<CriterionResult>, shortfalls: Vec<String>) -> QualificationMatrix {
    QualificationMatrix::assemble(
        &QualificationSpec::default(),
        Verdict::Valid,
        results,
        shortfalls,
        Vec::new(),
    )
}

// ---------------------------------------------------------------------------
// The three evidence outcomes
// ---------------------------------------------------------------------------

#[test]
fn all_blocking_dimensions_passing_qualifies() {
    let matrix = assemble(all_blocking_pass(), Vec::new());
    assert_eq!(matrix.evidence_status, EvidenceStatus::Qualifies);
    assert!(matrix.failed.is_empty());
    assert!(matrix.undecided.is_empty());
    // Every dimension in the contract is represented in the matrix.
    for dimension in Dimension::ALL {
        assert!(matrix.row(dimension).is_some(), "{dimension:?} missing from the matrix");
    }
}

#[test]
fn one_failed_blocking_dimension_does_not_qualify() {
    let mut results = all_blocking_pass();
    let index = results.iter().position(|r| r.blocking).unwrap();
    results[index].outcome = Outcome::Fail;
    results[index].estimate = estimate(0.9, 0.7, 1.1);
    let failed_id = results[index].criterion_id.clone();
    let failed_dimension = results[index].dimension;

    let matrix = assemble(results, Vec::new());
    assert_eq!(matrix.evidence_status, EvidenceStatus::DoesNotQualify);
    assert_eq!(matrix.failed, vec![failed_id]);
    assert_eq!(
        matrix.row(failed_dimension).unwrap().outcome,
        Outcome::Fail,
        "the dimension row must carry the failure, not average it away"
    );
}

#[test]
fn an_undecidable_blocking_criterion_is_insufficient_not_failure() {
    let mut results = all_blocking_pass();
    let index = results.iter().position(|r| r.blocking).unwrap();
    results[index].outcome = Outcome::Insufficient;
    results[index].estimate = unestimable();

    let matrix = assemble(results, Vec::new());
    assert_eq!(
        matrix.evidence_status,
        EvidenceStatus::InsufficientEvidence,
        "an unanswerable criterion must never be reported as V1 failing"
    );
    assert!(matrix.failed.is_empty());
    assert_eq!(matrix.undecided.len(), 1);
}

/// §25: a real failure outranks an undecided one. The session did answer, and
/// the answer was no.
#[test]
fn a_genuine_failure_outranks_an_undecided_criterion() {
    let mut results = all_blocking_pass();
    let blocking: Vec<usize> =
        results.iter().enumerate().filter(|(_, r)| r.blocking).map(|(i, _)| i).collect();
    results[blocking[0]].outcome = Outcome::Fail;
    results[blocking[1]].outcome = Outcome::Insufficient;

    let matrix = assemble(results, Vec::new());
    assert_eq!(matrix.evidence_status, EvidenceStatus::DoesNotQualify);
    assert_eq!(matrix.failed.len(), 1);
    assert_eq!(matrix.undecided.len(), 1, "and the undecided one is still reported");
}

/// §34: a thin session cannot answer the question, and that is not a verdict
/// about V1.
#[test]
fn a_minimum_evidence_shortfall_is_insufficient_evidence() {
    let matrix = assemble(
        all_blocking_pass(),
        vec!["distinct opportunities 640 < 2000 required".to_string()],
    );
    assert_eq!(matrix.evidence_status, EvidenceStatus::InsufficientEvidence);
    assert!(
        matrix.failed.is_empty(),
        "nothing failed; there simply was not enough evidence to ask"
    );
    assert_eq!(matrix.evidence_shortfalls.len(), 1);
}

/// A shortfall wins even when every criterion happens to have passed — the
/// passes were computed on too little evidence to mean anything.
#[test]
fn a_shortfall_outranks_passing_criteria() {
    let matrix = assemble(all_blocking_pass(), vec!["too few ranking windows".to_string()]);
    assert_eq!(matrix.evidence_status, EvidenceStatus::InsufficientEvidence);
}

// ---------------------------------------------------------------------------
// The session gate
// ---------------------------------------------------------------------------

/// §37: an invalid session stops before any Alpha claim exists.
#[test]
fn an_invalid_session_is_not_evaluated() {
    for verdict in [Verdict::Invalid, Verdict::Indeterminate] {
        let matrix = QualificationMatrix::assemble(
            &QualificationSpec::default(),
            verdict,
            all_blocking_pass(),
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(
            matrix.evidence_status,
            EvidenceStatus::NotEvaluated,
            "{verdict:?} must not yield an Alpha verdict"
        );
    }
}

#[test]
fn the_not_evaluated_matrix_makes_no_claim_at_all() {
    let matrix = QualificationMatrix::not_evaluated();
    assert_eq!(matrix.evidence_status, EvidenceStatus::NotEvaluated);
    assert!(matrix.rows.is_empty(), "no dimension may carry a number");
    assert!(matrix.notes[0].contains("not about Opportunity Intelligence V1"));
}

// ---------------------------------------------------------------------------
// The properties that stop a result being gamed
// ---------------------------------------------------------------------------

/// §28: a diagnostic result cannot rescue or sink the verdict.
#[test]
fn a_diagnostic_criterion_cannot_override_a_primary_gate() {
    // Every diagnostic fails; every blocking one passes.
    let mut results = all_blocking_pass();
    for result in results.iter_mut().filter(|r| !r.blocking) {
        result.outcome = Outcome::Fail;
    }
    assert_eq!(assemble(results, Vec::new()).evidence_status, EvidenceStatus::Qualifies);

    // And the converse: diagnostics passing cannot rescue a failed gate.
    let mut results = all_blocking_pass();
    let index = results.iter().position(|r| r.blocking).unwrap();
    results[index].outcome = Outcome::Fail;
    for result in results.iter_mut().filter(|r| !r.blocking) {
        result.outcome = Outcome::Pass;
    }
    assert_eq!(assemble(results, Vec::new()).evidence_status, EvidenceStatus::DoesNotQualify);
}

/// A contract term that produced no result must not pass by omission.
#[test]
fn a_blocking_criterion_with_no_result_is_insufficient() {
    let mut results = all_blocking_pass();
    let index = results.iter().position(|r| r.blocking).unwrap();
    results.remove(index);
    assert_eq!(
        assemble(results, Vec::new()).evidence_status,
        EvidenceStatus::InsufficientEvidence,
        "silently dropping a gate must not read as passing it"
    );
}

// ---------------------------------------------------------------------------
// Rule decisions
// ---------------------------------------------------------------------------

#[test]
fn an_enrichment_rule_is_decided_on_the_lower_bound() {
    let spec = QualificationSpec::default();
    let criterion = spec.criteria.iter().find(|c| c.id == "A1-early-quality-enrichment").unwrap();

    // Clears parity.
    assert_eq!(
        CriterionResult::decide(criterion, estimate(1.6, 1.15, 2.2), Some(1.6)),
        Outcome::Pass
    );
    // A large point estimate whose interval still spans 1 is not evidence.
    assert_eq!(
        CriterionResult::decide(criterion, estimate(1.6, 0.85, 2.9), Some(1.6)),
        Outcome::Fail,
        "a wide interval spanning parity must not pass on its point estimate"
    );
    // No interval at all.
    assert_eq!(
        CriterionResult::decide(criterion, unestimable(), Some(1.4)),
        Outcome::Insufficient
    );
}

#[test]
fn a_tolerance_rule_requires_the_lower_bound_to_clear_the_tolerance() {
    let spec = QualificationSpec::default();
    let criterion =
        spec.criteria.iter().find(|c| c.id == "D1-excursion-ratio-not-degraded").unwrap();
    // 10% tolerance: the lower bound must exceed 0.90.
    assert_eq!(CriterionResult::decide(criterion, estimate(1.0, 0.95, 1.1), Some(1.0)), Outcome::Pass);
    assert_eq!(
        CriterionResult::decide(criterion, estimate(0.95, 0.80, 1.1), Some(0.95)),
        Outcome::Fail,
        "an interval reaching below the tolerance is not evidence of parity"
    );
}

#[test]
fn a_monotonicity_rule_is_decided_on_the_supplied_shape() {
    let spec = QualificationSpec::default();
    let criterion = spec.criteria.iter().find(|c| c.id == "R1-ranking-monotonicity").unwrap();
    assert_eq!(CriterionResult::decide(criterion, estimate(1.0, 0.9, 1.1), Some(1.0)), Outcome::Pass);
    assert_eq!(CriterionResult::decide(criterion, estimate(1.0, 0.9, 1.1), Some(0.0)), Outcome::Fail);
    assert_eq!(
        CriterionResult::decide(criterion, estimate(1.0, 0.9, 1.1), None),
        Outcome::Insufficient,
        "too few populated buckets is undecidable, not a failure"
    );
}

#[test]
fn a_descriptive_rule_never_fails() {
    let spec = QualificationSpec::default();
    let criterion = spec.criteria.iter().find(|c| !c.blocking).unwrap();
    assert_eq!(CriterionResult::decide(criterion, unestimable(), None), Outcome::Pass);
}

#[test]
fn the_matrix_round_trips_as_json() {
    let matrix = assemble(all_blocking_pass(), Vec::new());
    let text = serde_json::to_string(&matrix).unwrap();
    assert!(text.contains("\"QUALIFIES\""));
    let back: QualificationMatrix = serde_json::from_str(&text).unwrap();
    assert_eq!(matrix, back);
}
