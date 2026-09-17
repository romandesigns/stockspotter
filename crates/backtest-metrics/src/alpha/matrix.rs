//! The qualification matrix and the evidence verdict (§25, §26).
//!
//! # Two decisions, never one
//!
//! **Session status** — is this session analysable at all — is decided first,
//! from the capture's own completeness evidence, and can only stop the
//! pipeline. **Evidence status** — did V1 satisfy the pre-registered contract —
//! is reached only when the session is VALID.
//!
//! Conflating them is how an instrument failure becomes a model conclusion. On
//! 2026-09-16 the capture discarded 89% of the session; scoring the survivors
//! would have produced a confident, precise and meaningless answer about V1.
//!
//! # Three values on each side, and they do not collapse into two
//!
//! `INSUFFICIENT_EVIDENCE` is not a failure, and `DOES_NOT_QUALIFY` is not a
//! data problem. A session that was too thin to answer the question has not
//! told us V1 is bad; a session that answered it and said no has. Reporting
//! either as the other would be the single easiest way to mislead a reader of
//! this programme, which is why they are separate variants rather than a
//! boolean with a caveat.
//!
//! # No single winner score
//!
//! §26 forbids reducing this to one number. The matrix stays decomposable to
//! the criterion, each row carrying its metric, its control, its sample size
//! and its own PASS / FAIL / INSUFFICIENT — so a failed dimension can never be
//! averaged away behind a passing one.

use serde::{Deserialize, Serialize};

use crate::alpha::spec::{Criterion, Dimension, QualificationSpec, Rule, Surface};
use crate::alpha::stats::Estimate;
use crate::completeness::Verdict;

/// How one criterion came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Outcome {
    Pass,
    Fail,
    /// The evidence could not decide it. Never a failure.
    Insufficient,
}

/// The programme-level answer about V1 (§25).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EvidenceStatus {
    Qualifies,
    DoesNotQualify,
    InsufficientEvidence,
    /// The session was not VALID, so the question was never asked.
    NotEvaluated,
}

impl std::fmt::Display for EvidenceStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            EvidenceStatus::Qualifies => "QUALIFIES",
            EvidenceStatus::DoesNotQualify => "DOES_NOT_QUALIFY",
            EvidenceStatus::InsufficientEvidence => "INSUFFICIENT_EVIDENCE",
            EvidenceStatus::NotEvaluated => "NOT_EVALUATED",
        })
    }
}

/// One measured criterion, with everything §26 requires exposed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CriterionResult {
    pub criterion_id: String,
    pub dimension: Dimension,
    pub surface: Surface,
    pub blocking: bool,
    pub metric: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control: Option<String>,
    /// The candidate cohort's value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_value: Option<f64>,
    /// The control's value on the same population.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control_value: Option<f64>,
    /// Ratio or difference, whichever the rule uses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comparison: Option<f64>,
    pub estimate: Estimate,
    /// The pre-registered criterion, in words, quoted from the contract.
    pub criterion: String,
    pub outcome: Outcome,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl CriterionResult {
    /// Decides one criterion from its estimate and the contract's rule.
    ///
    /// Separated from measurement so the decision is inspectable on its own and
    /// cannot quietly depend on how a number was produced.
    pub fn decide(criterion: &Criterion, estimate: Estimate, comparison: Option<f64>) -> Outcome {
        match &criterion.rule {
            // Diagnostic surfaces are reported, never decisive.
            Rule::Descriptive => Outcome::Pass,
            Rule::EnrichmentLowerBoundAbove { value }
            | Rule::UpliftLowerBoundAbove { value }
            | Rule::MedianAbove { value } => match estimate.lower_bound_above(*value) {
                Some(true) => Outcome::Pass,
                Some(false) => Outcome::Fail,
                None => Outcome::Insufficient,
            },
            Rule::NoWorseThanControlBy { fraction } => {
                // "Not materially worse" is a one-sided question, so it is the
                // lower bound that has to clear the tolerance -- an interval
                // that merely *includes* parity is not evidence of parity.
                match estimate.lower_bound_above(1.0 - fraction) {
                    Some(true) => Outcome::Pass,
                    Some(false) => Outcome::Fail,
                    None => Outcome::Insufficient,
                }
            }
            Rule::MonotoneAcrossRanks => match comparison {
                // The caller supplies 1.0 for monotone, 0.0 for not, and
                // `None` when there were too few populated buckets to say.
                Some(v) if v >= 1.0 => Outcome::Pass,
                Some(_) => Outcome::Fail,
                None => Outcome::Insufficient,
            },
        }
    }
}

/// One dimension's row in the matrix.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DimensionRow {
    pub dimension: Dimension,
    /// The dimension's own outcome, derived from its **blocking** criteria
    /// only. A dimension with no blocking criterion is diagnostic.
    pub outcome: Outcome,
    pub blocking: bool,
    pub criteria: Vec<CriterionResult>,
}

/// Why the evidence status is what it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QualificationMatrix {
    pub rows: Vec<DimensionRow>,
    pub evidence_status: EvidenceStatus,
    /// Blocking criteria that failed, named.
    pub failed: Vec<String>,
    /// Blocking criteria that could not be decided, named.
    pub undecided: Vec<String>,
    /// Minimum-evidence shortfalls, if any. Non-empty forces
    /// INSUFFICIENT_EVIDENCE regardless of the criteria.
    pub evidence_shortfalls: Vec<String>,
    /// Diagnostic observations worth a reader's attention, including any
    /// primary result that reverses in a segment.
    pub notes: Vec<String>,
}

impl QualificationMatrix {
    /// Assembles the matrix and derives the verdict.
    ///
    /// `session` gates everything: a session that is not VALID yields
    /// `NotEvaluated` and no Alpha claim is computed at all (§37).
    pub fn assemble(
        spec: &QualificationSpec,
        session: Verdict,
        results: Vec<CriterionResult>,
        evidence_shortfalls: Vec<String>,
        notes: Vec<String>,
    ) -> Self {
        let mut rows: Vec<DimensionRow> = Vec::new();
        for dimension in Dimension::ALL {
            let criteria: Vec<CriterionResult> =
                results.iter().filter(|r| r.dimension == dimension).cloned().collect();
            if criteria.is_empty() {
                continue;
            }
            let blocking: Vec<&CriterionResult> = criteria.iter().filter(|c| c.blocking).collect();
            let outcome = if blocking.is_empty() {
                Outcome::Pass
            } else if blocking.iter().any(|c| c.outcome == Outcome::Fail) {
                Outcome::Fail
            } else if blocking.iter().any(|c| c.outcome == Outcome::Insufficient) {
                Outcome::Insufficient
            } else {
                Outcome::Pass
            };
            rows.push(DimensionRow {
                dimension,
                outcome,
                blocking: !blocking.is_empty(),
                criteria,
            });
        }

        let failed: Vec<String> = results
            .iter()
            .filter(|r| r.blocking && r.outcome == Outcome::Fail)
            .map(|r| r.criterion_id.clone())
            .collect();
        let undecided: Vec<String> = results
            .iter()
            .filter(|r| r.blocking && r.outcome == Outcome::Insufficient)
            .map(|r| r.criterion_id.clone())
            .collect();

        let evidence_status = Self::derive(
            session,
            spec,
            &results,
            &failed,
            &undecided,
            &evidence_shortfalls,
        );

        Self { rows, evidence_status, failed, undecided, evidence_shortfalls, notes }
    }

    /// The derivation, kept separate and total so it can be reasoned about and
    /// tested without building a whole matrix.
    fn derive(
        session: Verdict,
        spec: &QualificationSpec,
        results: &[CriterionResult],
        failed: &[String],
        undecided: &[String],
        shortfalls: &[String],
    ) -> EvidenceStatus {
        // §37: the session gate is absolute and comes first.
        if session != Verdict::Valid {
            return EvidenceStatus::NotEvaluated;
        }
        // §34: not enough evidence to ask the question. Never a failure.
        if !shortfalls.is_empty() {
            return EvidenceStatus::InsufficientEvidence;
        }
        // Every blocking criterion in the contract must actually have been
        // measured. A contract term that silently produced no result would
        // otherwise pass by omission.
        for criterion in spec.blocking() {
            if !results.iter().any(|r| r.criterion_id == criterion.id) {
                return EvidenceStatus::InsufficientEvidence;
            }
        }
        // A genuine blocking failure is decisive, and outranks an undecided
        // one: the session did answer, and the answer was no.
        if !failed.is_empty() {
            return EvidenceStatus::DoesNotQualify;
        }
        if !undecided.is_empty() {
            return EvidenceStatus::InsufficientEvidence;
        }
        EvidenceStatus::Qualifies
    }

    pub fn row(&self, dimension: Dimension) -> Option<&DimensionRow> {
        self.rows.iter().find(|r| r.dimension == dimension)
    }

    /// A matrix with no Alpha claim at all, for a session that did not pass its
    /// completeness gate.
    pub fn not_evaluated() -> Self {
        Self {
            rows: Vec::new(),
            evidence_status: EvidenceStatus::NotEvaluated,
            failed: Vec::new(),
            undecided: Vec::new(),
            evidence_shortfalls: Vec::new(),
            notes: vec![
                "The session did not pass its completeness gate, so no Alpha evaluation was \
                 performed. This is a statement about the capture, not about Opportunity \
                 Intelligence V1."
                    .to_string(),
            ],
        }
    }
}

#[cfg(test)]
#[path = "matrix_tests.rs"]
mod tests;
