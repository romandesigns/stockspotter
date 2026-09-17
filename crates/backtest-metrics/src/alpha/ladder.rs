//! The runner stage ladder, and where each meaningful opportunity was lost.
//!
//! # The question this answers
//!
//! For an independently identified large runner, the practically useful
//! question is not "did the platform score it well" but *how far did it get,
//! and where did it stop*. Six answers are materially different, and lumping
//! any two together loses the thing a reader needs:
//!
//! | | Meaning | Whose problem |
//! |---|---|---|
//! | **A** | Stockspotter never saw it | universe / data availability |
//! | **B** | Saw it, detectors missed it | the detectors |
//! | **C** | Detected, but no OI opportunity was created or retained | the OI layer |
//! | **D** | OI created it, but ranked it poorly | the OI scores |
//! | **E** | Ranked it highly, but too late to matter | the OI scores, differently |
//! | **F** | Ranked it highly and early, with move remaining | working as intended |
//!
//! A and B are scanner problems; C, D and E are OI problems, and they are
//! distinct ones. Reporting "recall was 40%" without this split would blame the
//! wrong layer roughly half the time.
//!
//! # UNKNOWN is a real answer
//!
//! Every stage is three-valued — observed, observed absent, or unevidenced —
//! and a classification that would require guessing an unevidenced stage
//! returns [`Classification::Unknown`] instead. `attribution.rs` established
//! this convention for the discovery stages and the reasoning carries: writing
//! `false` for "we did not see it" turns a capture gap into a finding about the
//! market.
//!
//! # Relationship to `attribution::Stage`
//!
//! That ladder is an existing, frozen analysis surface that ends at
//! `OpportunityRanked`. This one composes with it rather than replacing it,
//! adding the five OI-internal stages the runner analysis needs. Nothing here
//! changes how `attribution` classifies anything.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A rung on the ladder, in causal order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RunnerStage {
    Visible,
    Detected,
    OpportunityCreated,
    EarlyQualityAvailable,
    EarlyRanked,
    ContinuationAvailable,
    ContinuationRanked,
    TopK,
}

impl RunnerStage {
    pub const ALL: [RunnerStage; 8] = [
        RunnerStage::Visible,
        RunnerStage::Detected,
        RunnerStage::OpportunityCreated,
        RunnerStage::EarlyQualityAvailable,
        RunnerStage::EarlyRanked,
        RunnerStage::ContinuationAvailable,
        RunnerStage::ContinuationRanked,
        RunnerStage::TopK,
    ];
}

/// Where a reference opportunity stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Classification {
    /// A. Never visible to the platform at all.
    NeverVisible,
    /// B. Visible, but no detector produced an event for it.
    VisibleNotDetected,
    /// C. Detected, but no OI opportunity was created or retained.
    DetectedNoOpportunity,
    /// D. An OI opportunity existed, but it never reached a top-k rank.
    RankedPoorly,
    /// E. Reached a top-k rank, but not before the move had happened.
    RankedLate,
    /// F. Reached a top-k rank before the move, with excursion remaining.
    RankedEarly,
    /// The evidence cannot establish where it stopped.
    Unknown,
}

impl Classification {
    /// The single-letter label used in the report's tables.
    pub fn letter(self) -> &'static str {
        match self {
            Classification::NeverVisible => "A",
            Classification::VisibleNotDetected => "B",
            Classification::DetectedNoOpportunity => "C",
            Classification::RankedPoorly => "D",
            Classification::RankedLate => "E",
            Classification::RankedEarly => "F",
            Classification::Unknown => "UNKNOWN",
        }
    }

    /// Which layer the outcome implicates. `None` for F and UNKNOWN.
    ///
    /// The whole point of the split: a scanner miss and an OI miss are
    /// different findings, and the report must not average them.
    pub fn attributable_layer(self) -> Option<&'static str> {
        match self {
            Classification::NeverVisible | Classification::VisibleNotDetected => Some("scanner"),
            Classification::DetectedNoOpportunity
            | Classification::RankedPoorly
            | Classification::RankedLate => Some("opportunity intelligence"),
            Classification::RankedEarly | Classification::Unknown => None,
        }
    }

    pub const ALL: [Classification; 7] = [
        Classification::NeverVisible,
        Classification::VisibleNotDetected,
        Classification::DetectedNoOpportunity,
        Classification::RankedPoorly,
        Classification::RankedLate,
        Classification::RankedEarly,
        Classification::Unknown,
    ];
}

/// Three-valued evidence for one stage, plus when it happened where that is
/// known.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageEvidence {
    /// `None` means unevidenced — never "did not happen".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reached: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_at: Option<DateTime<Utc>>,
}

impl StageEvidence {
    pub fn reached_at(at: DateTime<Utc>) -> Self {
        Self { reached: Some(true), first_at: Some(at) }
    }
    pub fn absent() -> Self {
        Self { reached: Some(false), first_at: None }
    }
    pub fn unevidenced() -> Self {
        Self { reached: None, first_at: None }
    }
}

/// One reference opportunity's full ladder.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ladder {
    pub symbol: String,
    pub visible: StageEvidence,
    pub detected: StageEvidence,
    pub opportunity_created: StageEvidence,
    pub early_quality_available: StageEvidence,
    pub early_ranked: StageEvidence,
    pub continuation_available: StageEvidence,
    pub continuation_ranked: StageEvidence,
    pub top_k: StageEvidence,
    /// First crossing of the primary target, where it happened.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_crossing_at: Option<DateTime<Utc>>,
    /// Favourable excursion still available at the first top-k rank, percent.
    /// `None` when it cannot be computed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_excursion_pct: Option<f64>,
}

impl Ladder {
    pub fn stage(&self, stage: RunnerStage) -> StageEvidence {
        match stage {
            RunnerStage::Visible => self.visible,
            RunnerStage::Detected => self.detected,
            RunnerStage::OpportunityCreated => self.opportunity_created,
            RunnerStage::EarlyQualityAvailable => self.early_quality_available,
            RunnerStage::EarlyRanked => self.early_ranked,
            RunnerStage::ContinuationAvailable => self.continuation_available,
            RunnerStage::ContinuationRanked => self.continuation_ranked,
            RunnerStage::TopK => self.top_k,
        }
    }

    /// The deepest stage positively observed, if any.
    pub fn deepest_reached(&self) -> Option<RunnerStage> {
        RunnerStage::ALL
            .iter()
            .rev()
            .copied()
            .find(|stage| self.stage(*stage).reached == Some(true))
    }

    /// Where this opportunity stopped, or `Unknown`.
    ///
    /// Walks the ladder from the bottom and stops at the first stage that is
    /// either absent (a classification) or unevidenced (an Unknown). Never
    /// infers a shallower stage from a deeper one: a deeper stage observed
    /// while a shallower one is unevidenced is a *capture* inconsistency, and
    /// resolving it silently would hide an incomplete log.
    pub fn classify(&self) -> Classification {
        match self.visible.reached {
            None => return Classification::Unknown,
            Some(false) => return Classification::NeverVisible,
            Some(true) => {}
        }
        match self.detected.reached {
            None => return Classification::Unknown,
            Some(false) => return Classification::VisibleNotDetected,
            Some(true) => {}
        }
        match self.opportunity_created.reached {
            None => return Classification::Unknown,
            Some(false) => return Classification::DetectedNoOpportunity,
            Some(true) => {}
        }
        match self.top_k.reached {
            None => return Classification::Unknown,
            Some(false) => return Classification::RankedPoorly,
            Some(true) => {}
        }
        // Reached a top-k rank. Early or late is decided against the crossing,
        // and needs both instants; without them the timing is unknown even
        // though the ranking is not.
        match (self.top_k.first_at, self.primary_crossing_at) {
            (Some(ranked_at), Some(crossed_at)) => {
                if ranked_at < crossed_at {
                    Classification::RankedEarly
                } else {
                    Classification::RankedLate
                }
            }
            // Ranked, and the target was never crossed at all: nothing was
            // missed by being late, so the ranking was not late.
            (Some(_), None) if self.primary_crossing_at.is_none() => Classification::RankedEarly,
            _ => Classification::Unknown,
        }
    }
}

/// Counts by classification, for the report's aggregate table.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LadderSummary {
    pub total: usize,
    pub never_visible: usize,
    pub visible_not_detected: usize,
    pub detected_no_opportunity: usize,
    pub ranked_poorly: usize,
    pub ranked_late: usize,
    pub ranked_early: usize,
    pub unknown: usize,
}

impl LadderSummary {
    pub fn of(ladders: &[Ladder]) -> Self {
        let mut summary = Self { total: ladders.len(), ..Self::default() };
        for ladder in ladders {
            match ladder.classify() {
                Classification::NeverVisible => summary.never_visible += 1,
                Classification::VisibleNotDetected => summary.visible_not_detected += 1,
                Classification::DetectedNoOpportunity => summary.detected_no_opportunity += 1,
                Classification::RankedPoorly => summary.ranked_poorly += 1,
                Classification::RankedLate => summary.ranked_late += 1,
                Classification::RankedEarly => summary.ranked_early += 1,
                Classification::Unknown => summary.unknown += 1,
            }
        }
        summary
    }

    /// Reference opportunities whose classification is established.
    pub fn evaluable(&self) -> usize {
        self.total - self.unknown
    }

    /// Fraction whose stage is causally established. The measurability gate:
    /// a recall analysis over a mostly-unknown population is not one.
    pub fn attribution_rate(&self) -> Option<f64> {
        if self.total == 0 {
            None
        } else {
            Some(self.evaluable() as f64 / self.total as f64)
        }
    }

    /// Independent detection coverage: reached the detector, over everything
    /// causally evaluable. Characterises the scanner, not OI.
    pub fn detection_coverage(&self) -> Option<f64> {
        let evaluable = self.evaluable();
        if evaluable == 0 {
            return None;
        }
        let reached_detector = evaluable - self.never_visible - self.visible_not_detected;
        Some(reached_detector as f64 / evaluable as f64)
    }

    /// Among those detection reached, the fraction OI retained as an
    /// opportunity. The conditional-retention quantity, which grades OI
    /// without charging it for upstream misses.
    pub fn oi_conditional_retention(&self) -> Option<f64> {
        let detected =
            self.evaluable() - self.never_visible - self.visible_not_detected;
        if detected == 0 {
            return None;
        }
        Some((detected - self.detected_no_opportunity) as f64 / detected as f64)
    }
}

#[cfg(test)]
#[path = "ladder_tests.rs"]
mod tests;
