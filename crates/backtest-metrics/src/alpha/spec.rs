//! **Frozen.** The pre-registered Alpha qualification contract (§24, §31).
//!
//! # What this is, and why it is a source file
//!
//! This is the document that decides, in advance, what would count as evidence
//! that Opportunity Intelligence V1 generalises. It is frozen and hashed
//! *before* the first untouched prospective session is evaluated, and the
//! qualification report quotes its hash — so a reader can tell whether the
//! criteria were chosen before or after the result.
//!
//! It lives in source, versioned with the code that applies it, because a
//! specification that can be edited between the session and the analysis is not
//! a pre-registration. `frozen_at` is a literal, not `now()`: the default spec
//! must hash to the same value on every machine and every run, or the hash
//! proves nothing.
//!
//! # Why so few numeric thresholds
//!
//! §24 is explicit: *"Define numerical gates only where the development
//! evidence supports a defensible pre-registration … Do not manufacture
//! arbitrary thresholds merely to obtain a binary answer."*
//!
//! There is no prior prospective evidence about V1. September 14 is the
//! development dataset that *informed* the scores, and September 16 is an
//! instrument-validation session whose capture was 11% complete. Neither can
//! justify a statement like "enrichment must exceed 1.5×". Inventing one would
//! not make the answer more rigorous; it would make it arbitrary, and it would
//! be indistinguishable from choosing the number that V1 happens to clear.
//!
//! So nearly every criterion here is **directional and comparative**: V1 must
//! beat a stated control on the same population, same target, same horizon,
//! same censoring, with a clustered confidence bound that excludes "no
//! difference". Those are falsifiable without a magic number.
//!
//! The one place numbers *are* pre-registered is [`MinimumEvidence`], and §34
//! explicitly permits estimating those from expected session volume — they
//! decide whether the question is answerable, not whether the answer is good.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::alpha::labels::{ReferenceLabelSpec, REFERENCE_LABEL_VERSION};
use crate::alpha::sha256;
use crate::horizon::{HORIZON_SECS, TARGET_PCTS};
use crate::opportunity::{
    CONTINUATION_MODEL_VERSION, EARLY_QUALITY_MODEL_VERSION, OI_FEATURE_SCHEMA_VERSION,
    OPPORTUNITY_SCHEMA_VERSION, PRICE_REGIME_VERSION, RANKING_VERSION, REGIME_CLASSIFIER_VERSION,
    SCORE_POLICY_VERSION,
};

pub const QUALIFICATION_SCHEMA_VERSION: u32 = 1;
/// Bumped by the pre-evaluation contract review of 2026-09-17, which split
/// recall into three distinct questions, added the secondary-direction
/// reporting requirement, and bound the contract to a configuration.
///
/// v3 additionally splits the control that was named `first-detection`. It was
/// doing two jobs -- a cohort comparison for A2 and a ladder comparison for C1
/// -- and as written A2's control measured the same opportunities from a
/// different anchor instant than the candidate, which would have confounded
/// selection with timing. Neither v1 nor v2 was ever used to evaluate a
/// session.
///
/// # v4 is a re-binding of v3, not a new contract (2026-09-25)
///
/// Every criterion, control, target, horizon, minimum-evidence figure and
/// reporting requirement is v3's, unchanged, and `FROZEN_AT` still names the
/// instant those were frozen. What moved is what the contract is **bound
/// to**, because the measurement-correctness assignment
/// (`docs/measurement-correctness-contract-2026-09-25.md`) changed two pinned
/// identities:
///
/// * D6 raised `max_rank_cohort` from 4,096 to the open capacity, so the OI
///   config fingerprint moved `oi-cfg-b4f21c8b311a1b99` ->
///   `oi-cfg-15861d6d0b263f12`;
/// * D4 made outcome disposition a measurement, so
///   `expectedOutcomeMeasurementVersion` moved `opportunity-outcome-v1` ->
///   `opportunity-outcome-v2`.
///
/// Either changes the canonical JSON and therefore the SHA, and a frozen
/// contract whose hash changes under the same name is two contracts wearing
/// one label -- which is exactly what a version exists to prevent. So the
/// name moves with the binding. v3 (`a4106f3a...c317`) remains the contract
/// for sessions captured under `oi-cfg-b4f21c8b311a1b99`; none was evaluated
/// under it prospectively, and those captures are INVALID wherever
/// `cohortTruncations > 0`.
///
/// **Not final at this commit.** The parallel D3/D7a change bumps the
/// feature/context schema versions, which this spec also pins, so the SHA
/// moves again when both land; `ops/qualify/session.sh`'s
/// `EXPECTED_SPEC_SHA` is enforced equal to `sha256()` by the build and must
/// be recomputed at that merge, before any prospective session.
pub const SPEC_VERSION: &str = "alpha-qualification-v4";

/// The instant this contract was frozen. A literal, deliberately: a spec whose
/// hash changes every time it is constructed cannot pre-register anything.
pub const FROZEN_AT: &str = "2026-09-17T09:00:00Z";

/// The Opportunity Intelligence configuration this contract is bound to.
///
/// `oi-cfg-15861d6d0b263f12` = the capture repair's configuration with D6's
/// `maxRankCohort: 16375` (was 4,096). Previously `oi-cfg-b4f21c8b311a1b99`,
/// produced by the capture repair (see the Stage A report). Must equal
/// `OiConfig::default().fingerprint()`; `spec_tests` enforces it.
pub const EXPECTED_OI_CONFIG_FINGERPRINT: &str = "oi-cfg-15861d6d0b263f12";

// ---------------------------------------------------------------------------
// Dimensions and surfaces
// ---------------------------------------------------------------------------

/// The qualification matrix's dimensions (§26). The overall evidence status is
/// derived from these; it is never an average, and a failed dimension is never
/// absorbed into one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Dimension {
    EarlyQuality,
    ContinuationQuality,
    RankingQuality,
    Recall,
    Earliness,
    RiskExcursion,
    RobustnessSegmentation,
}

impl Dimension {
    pub const ALL: [Dimension; 7] = [
        Dimension::EarlyQuality,
        Dimension::ContinuationQuality,
        Dimension::RankingQuality,
        Dimension::Recall,
        Dimension::Earliness,
        Dimension::RiskExcursion,
        Dimension::RobustnessSegmentation,
    ];
}

/// §28. A primary surface can decide qualification; a secondary one is
/// diagnostic and **cannot override a primary gate** in either direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Surface {
    Primary,
    Secondary,
}

/// What each of the words "baseline" and "control" means, stated separately so
/// no table can be ambiguous about it (§32).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BaselineKind {
    /// `ba722698af4fa2b387339eb017a2d58734f969c7` — the frozen strategy commit.
    StrategyBaseline,
    /// Analysis Baseline 001 / September 14. Informed the V1 scores, so it is
    /// development evidence and never validation.
    DevelopmentDataset,
    /// September 16. Instrument validation only; its capture was 11% complete.
    InstrumentValidationDataset,
    /// OI V1 on the next untouched VALID session.
    ProspectiveCandidate,
    /// The exact comparison used for one qualification metric.
    Control,
}

/// One named control, with the parity conditions it is compared under (§33).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlDefinition {
    pub id: String,
    pub kind: BaselineKind,
    pub description: String,
    /// Stated so a reader can check parity rather than assume it.
    pub population: String,
    pub same_target: bool,
    pub same_horizon: bool,
    pub same_censoring: bool,
    pub same_session_window: bool,
    pub same_eligibility: bool,
}

impl ControlDefinition {
    /// §33: parity on all five axes, or the comparison is descriptive rather
    /// than a qualification gate.
    pub fn has_parity(&self) -> bool {
        self.same_target
            && self.same_horizon
            && self.same_censoring
            && self.same_session_window
            && self.same_eligibility
    }
}

// ---------------------------------------------------------------------------
// Criteria
// ---------------------------------------------------------------------------

/// How a criterion is decided.
///
/// Every variant is either a comparison against a named control or a shape
/// constraint. None of them is a bare threshold on V1's own number, because
/// there is no prospective evidence that would justify one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "rule")]
pub enum Rule {
    /// The clustered lower confidence bound on (candidate / control) must
    /// exceed 1. "Better than its control, by more than sampling noise."
    EnrichmentLowerBoundAbove { value: f64 },
    /// The clustered lower bound on (candidate − control) must exceed 0.
    UpliftLowerBoundAbove { value: f64 },
    /// Ranked buckets must not get *worse* as rank improves, allowing for
    /// overlap in their confidence intervals.
    MonotoneAcrossRanks,
    /// The candidate must be no worse than its control by more than `fraction`
    /// of the control's value — for quantities where being *equal* is the
    /// requirement and being better is a bonus.
    NoWorseThanControlBy { fraction: f64 },
    /// A median must exceed a value. Used only where the value is structural
    /// (a lead time of zero means "ranked after the move", which is not a
    /// threshold anyone chose).
    MedianAbove { value: f64 },
    /// Reported, never decisive. Diagnostic surfaces use this.
    Descriptive,
}

/// One pre-registered criterion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Criterion {
    pub id: String,
    pub dimension: Dimension,
    pub surface: Surface,
    /// Blocking criteria decide QUALIFIES / DOES_NOT_QUALIFY. Non-blocking
    /// ones are reported and cannot change the verdict.
    pub blocking: bool,
    /// The quantity measured, in words a reader can check against the tables.
    pub metric: String,
    /// The `ControlDefinition::id` it is compared against, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control: Option<String>,
    pub rule: Rule,
    /// Why this criterion, and why in this form. Written before the result.
    pub rationale: String,
}

// ---------------------------------------------------------------------------
// Minimum evidence (§34)
// ---------------------------------------------------------------------------

/// Whether the session can answer the question at all.
///
/// A shortfall here yields **INSUFFICIENT_EVIDENCE**, never a failure. These
/// are the only numbers in the contract, and §34 explicitly permits estimating
/// them from expected session volume. They are set well below what a normal
/// regular session produces, so that they exclude only genuinely thin sessions
/// rather than quietly becoming a second performance gate.
///
/// Scale reference, from the reconstructed September-16 regular session:
/// 19,963 opportunities opened, 8,886 distinct symbols with ignition activity,
/// 6,086 of them with a confirmed ignition, and 780 ranking windows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MinimumEvidence {
    pub distinct_opportunities: usize,
    pub distinct_symbols: usize,
    /// Positive reference opportunities at the primary target.
    pub positive_reference_opportunities: usize,
    pub ranking_windows: usize,
    /// Observations required in each primary top-k / percentile surface.
    pub observations_per_primary_surface: usize,
    /// Regimes that must each be populated for a regime-specific blocking
    /// claim. Empty because no regime-specific claim is blocking in v1.
    pub regimes_required: Vec<String>,
}

impl Default for MinimumEvidence {
    fn default() -> Self {
        Self {
            // ~10% of the September-16 count. A session producing fewer than
            // 2,000 opportunities is materially unlike a normal one.
            distinct_opportunities: 2_000,
            // ~10% of the symbols that showed ignition activity.
            distinct_symbols: 800,
            // The binding one in practice: the qualification compares rates
            // between cohorts, and 100 positives is roughly where a clustered
            // interval stops spanning everything.
            positive_reference_opportunities: 100,
            // 780 windows in a full session; 200 is a materially short session
            // but still a real cross-section.
            ranking_windows: 200,
            // Below this a top-k cohort's rate is not estimable.
            observations_per_primary_surface: 30,
            regimes_required: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// The specification
// ---------------------------------------------------------------------------

/// A conclusion the final report must state, whatever the numbers say.
///
/// Part of the contract rather than the report generator's own convention: a
/// reporting obligation that can be dropped during writing is not an
/// obligation. `FINAL-ALPHA-QUALIFICATION.md` is checked against this list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportingRequirement {
    pub id: String,
    pub description: String,
    /// Breakdowns that must each appear.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub breakdowns: Vec<String>,
    /// True when the requirement is purely descriptive and must never be
    /// allowed to move the verdict.
    pub descriptive_only: bool,
}

fn default_reporting() -> Vec<ReportingRequirement> {
    vec![
        ReportingRequirement {
            id: "R-detection-coverage".to_string(),
            description:
                "Independent detection coverage: meaningful reference opportunities reaching the \
                 DETECTOR stage, over all causally evaluable meaningful reference opportunities. \
                 A primary reported *system* metric characterising the scanner beneath OI. No \
                 absolute pass threshold is pre-registered, because none is derivable from \
                 development data."
                    .to_string(),
            breakdowns: vec![
                "overall".to_string(),
                "+2%".to_string(),
                "+5%".to_string(),
                "+10%".to_string(),
                "by price regime".to_string(),
                "by time of day".to_string(),
                "by causal miss stage".to_string(),
            ],
            descriptive_only: true,
        },
        ReportingRequirement {
            id: "R-two-interpretations".to_string(),
            description:
                "The report must state two conclusions separately and prominently: OI LAYER \
                 QUALIFICATION (given what Stockspotter detected, did OI V1 prioritise usefully, \
                 early, without unacceptable risk degradation) and END-TO-END SCANNER COVERAGE \
                 (how much of the independent population Stockspotter detected at all). Strong \
                 conditional OI performance must not hide poor scanner coverage, and poor \
                 upstream recall must not make OI appear to fail a task it never had the chance \
                 to perform."
                    .to_string(),
            breakdowns: Vec::new(),
            descriptive_only: false,
        },
        ReportingRequirement {
            id: "R-secondary-direction".to_string(),
            description:
                "Whether the direction of the primary +2% relationship is also present at +5% and \
                 +10% wherever evidence suffices. Distinguishes short-continuation identification \
                 from information that also points toward larger runner-type outcomes. \
                 Descriptive: it may not become a qualification gate."
                    .to_string(),
            breakdowns: vec!["+5%".to_string(), "+10%".to_string()],
            descriptive_only: true,
        },
        ReportingRequirement {
            id: "R-runner-stage-ladder".to_string(),
            description:
                "For every independent reference opportunity, the deepest stage causally \
                 established along the full ladder, classified A-F: never seen / seen but not \
                 detected / detected but no OI opportunity / OI opportunity but ranked poorly / \
                 ranked highly but too late / ranked highly and early with move remaining. \
                 UNKNOWN remains valid wherever evidence cannot establish a stage."
                    .to_string(),
            breakdowns: vec![
                "A never visible".to_string(),
                "B visible, detector missed".to_string(),
                "C detected, no OI opportunity".to_string(),
                "D OI opportunity, ranked poorly".to_string(),
                "E ranked highly, too late".to_string(),
                "F ranked highly and early".to_string(),
                "UNKNOWN".to_string(),
            ],
            descriptive_only: true,
        },
    ]
}

/// Uncertainty method (§27), stated in the contract so it cannot be chosen
/// after the fact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UncertaintyMethod {
    pub method: String,
    pub cluster_unit: String,
    pub resamples: usize,
    pub confidence: f64,
    /// Seeded from the spec hash, so a run is reproducible and the seed cannot
    /// be shopped for a better interval.
    pub seed_source: String,
}

impl Default for UncertaintyMethod {
    fn default() -> Self {
        Self {
            method: "cluster bootstrap, percentile interval".to_string(),
            cluster_unit: "symbol".to_string(),
            resamples: 2_000,
            confidence: 0.95,
            seed_source: "sha256 of the canonical specification".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QualificationSpec {
    pub qualification_schema_version: u32,
    pub version: String,
    pub frozen_at: DateTime<Utc>,

    /// The model identity this contract is written against. A session produced
    /// by different model versions is not evaluable under it.
    pub expected_opportunity_schema: u32,
    /// The episode artifact shape. Version 2 is the first that carries
    /// `episodeUid`; a version-1 artifact cannot be joined on a collision-free
    /// key, and 3,881 ids were reissued over 2026-09-17/18 alone.
    pub expected_episode_schema: u32,
    /// The opportunity-native outcome contract a session is expected to have
    /// written. A session captured without it cannot answer whether outcome
    /// coverage is score-independent, because the episode-attached model's
    /// coverage is not.
    pub expected_outcome_measurement_version: String,
    pub expected_feature_schema: u32,
    pub expected_regime_classifier: String,
    pub expected_price_regime: String,
    pub expected_early_quality_model: String,
    pub expected_continuation_model: String,
    pub expected_ranking: String,
    pub expected_score_policy: String,
    /// Filled when the prospective session's configuration is pinned. `None`
    /// here means the spec has not yet been bound to a deployment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_oi_config_fingerprint: Option<String>,

    pub reference_label_version: String,
    pub reference_label: ReferenceLabelSpec,

    /// §28. Qualification is decided on these and only these.
    pub primary_targets_pct: Vec<f64>,
    pub primary_horizons_secs: Vec<i64>,
    pub primary_ranking_surfaces: Vec<String>,
    /// Reported, diagnostic, and explicitly non-decisive.
    pub secondary_targets_pct: Vec<f64>,
    pub secondary_horizons_secs: Vec<i64>,
    pub secondary_ranking_surfaces: Vec<String>,

    pub controls: Vec<ControlDefinition>,
    pub criteria: Vec<Criterion>,
    /// Conclusions the final report owes a reader regardless of the verdict.
    pub reporting: Vec<ReportingRequirement>,
    pub minimum_evidence: MinimumEvidence,

    pub censoring_treatment: String,
    pub uncertainty: UncertaintyMethod,
    pub session_window_utc: String,
    pub required_completeness: Vec<String>,
    /// The analytical unit. Stated because using the wrong one is the single
    /// easiest way to manufacture significance here.
    pub analytical_unit: String,
}

impl Default for QualificationSpec {
    fn default() -> Self {
        Self {
            qualification_schema_version: QUALIFICATION_SCHEMA_VERSION,
            version: SPEC_VERSION.to_string(),
            frozen_at: FROZEN_AT.parse().expect("FROZEN_AT is a literal RFC 3339 timestamp"),

            expected_opportunity_schema: OPPORTUNITY_SCHEMA_VERSION,
            expected_episode_schema: crate::episode::EPISODE_SCHEMA_VERSION,
            expected_outcome_measurement_version:
                crate::opportunity_outcome::OPPORTUNITY_OUTCOME_VERSION.to_string(),
            expected_feature_schema: OI_FEATURE_SCHEMA_VERSION,
            expected_regime_classifier: REGIME_CLASSIFIER_VERSION.to_string(),
            expected_price_regime: PRICE_REGIME_VERSION.to_string(),
            expected_early_quality_model: EARLY_QUALITY_MODEL_VERSION.to_string(),
            expected_continuation_model: CONTINUATION_MODEL_VERSION.to_string(),
            expected_ranking: RANKING_VERSION.to_string(),
            expected_score_policy: SCORE_POLICY_VERSION.to_string(),
            // Bound by the contract review of 2026-09-17. A contract bound to
            // a configuration is a different contract, and this is the one the
            // prospective session will be evaluated under.
            expected_oi_config_fingerprint: Some(EXPECTED_OI_CONFIG_FINGERPRINT.to_string()),

            reference_label_version: REFERENCE_LABEL_VERSION.to_string(),
            reference_label: ReferenceLabelSpec::default(),

            // +2% is primary. It is the most frequent of the three, so it is
            // the one a single session can actually estimate; +5 and +10 are
            // reported and diagnostic. Choosing the *most estimable* target in
            // advance is not the same as choosing the most flattering one.
            primary_targets_pct: vec![TARGET_PCTS[0]],
            // 300s and 900s: long enough for a move to develop, short enough
            // to be observed within the session for most opportunities.
            primary_horizons_secs: vec![300, 900],
            primary_ranking_surfaces: vec![
                "earlyQualityRank top-5".to_string(),
                "earlyQualityRank top-10%".to_string(),
                "continuationRank top-5".to_string(),
                "continuationRank top-10%".to_string(),
            ],
            secondary_targets_pct: vec![TARGET_PCTS[1], TARGET_PCTS[2]],
            secondary_horizons_secs: HORIZON_SECS
                .iter()
                .copied()
                .filter(|h| *h != 300 && *h != 900)
                .collect(),
            secondary_ranking_surfaces: vec![
                "top-1".to_string(),
                "top-3".to_string(),
                "top-1%".to_string(),
                "top-5%".to_string(),
                "deciles".to_string(),
            ],

            controls: default_controls(),
            criteria: default_criteria(),
            reporting: default_reporting(),
            minimum_evidence: MinimumEvidence::default(),

            censoring_treatment:
                "censored outcomes are excluded from both candidate and control and reported as \
                 coverage; they are never converted to failures"
                    .to_string(),
            uncertainty: UncertaintyMethod::default(),
            session_window_utc: "13:30:00Z-20:00:00Z".to_string(),
            required_completeness: vec![
                "sessionStatus == VALID".to_string(),
                "opportunityIntelligence.dropped == 0".to_string(),
                "measurement.dropped == 0".to_string(),
                "discovery.queueLost == 0".to_string(),
                "all writerErrors == 0".to_string(),
                "opportunityEngine.capacityEvictions == 0".to_string(),
                "opportunityEngine.cohortTruncations == 0".to_string(),
                "settlement.unsettled == 0".to_string(),
                "commit and oiConfigFingerprint match the expected values".to_string(),
            ],
            analytical_unit:
                "opportunity; repeated ranking snapshots of one opportunity are not independent \
                 observations and never inflate N"
                    .to_string(),
        }
    }
}

fn default_controls() -> Vec<ControlDefinition> {
    let parity = |id: &str, description: &str, population: &str| ControlDefinition {
        id: id.to_string(),
        kind: BaselineKind::Control,
        description: description.to_string(),
        population: population.to_string(),
        same_target: true,
        same_horizon: true,
        same_censoring: true,
        same_session_window: true,
        same_eligibility: true,
    };
    vec![
        parity(
            "contemporaneous-cohort",
            "every opportunity open in the same ranking window, ranked or not",
            "the candidate's own window, so the comparison is within one market moment",
        ),
        parity(
            "detection-recency",
            "the k most recently opened opportunities in the same window -- what selecting on \
             detection recency alone would have chosen",
            "the same window's cohort, same anchor instant, same cohort size as the candidate",
        ),
        parity(
            "detector-stage",
            "the DETECTOR rung of the same stage ladder, over the same reference population",
            "independent reference opportunities, classified once and read at two depths",
        ),
        parity(
            "move-magnitude",
            "ranking by move-from-start alone, ignoring every OI score",
            "the same window's cohort, ordered by a trivially available quantity",
        ),
        parity(
            "random-within-window",
            "a uniformly drawn subset of the same window's cohort, of the same size",
            "the same window's cohort",
        ),
    ]
}

fn default_criteria() -> Vec<Criterion> {
    vec![
        // --- A. Early quality (blocking) -----------------------------------
        Criterion {
            id: "A1-early-quality-enrichment".to_string(),
            dimension: Dimension::EarlyQuality,
            surface: Surface::Primary,
            blocking: true,
            metric: "P(reaches +2% within horizon) for earlyQualityRank top-5 and top-10%"
                .to_string(),
            control: Some("contemporaneous-cohort".to_string()),
            rule: Rule::EnrichmentLowerBoundAbove { value: 1.0 },
            rationale:
                "The minimum honest claim: candidates the early-quality score ranks highest reach \
                 the target more often than the cohort they were ranked within, by more than \
                 clustered sampling noise. Compared inside the window so a busy market cannot be \
                 mistaken for skill. No magnitude is pre-registered because no prospective \
                 evidence exists to justify one."
                    .to_string(),
        },
        Criterion {
            id: "A2-early-quality-beats-first-detection".to_string(),
            dimension: Dimension::EarlyQuality,
            surface: Surface::Primary,
            blocking: true,
            metric: "same rate, against a cohort of the same size selected by detection recency"
                .to_string(),
            control: Some("detection-recency".to_string()),
            rule: Rule::EnrichmentLowerBoundAbove { value: 1.0 },
            rationale:
                "Enrichment over the whole window could be produced by any ordering, so A1 alone \
                 does not show the *score* is doing the work. This compares against the cheapest \
                 alternative selection rule -- take the newest detections -- at the same cohort \
                 size, same window and same anchor instant, so the only thing that differs is how \
                 the k were chosen. Anchoring the control anywhere other than the candidate's own \
                 instant would confound selection with timing and make the result unreadable."
                    .to_string(),
        },
        Criterion {
            id: "A3-direction-at-secondary-targets".to_string(),
            dimension: Dimension::EarlyQuality,
            surface: Surface::Secondary,
            blocking: false,
            metric: "sign and magnitude of the same enrichment at +5% and +10%, wherever evidence \
                     suffices"
                .to_string(),
            control: Some("contemporaneous-cohort".to_string()),
            rule: Rule::Descriptive,
            rationale:
                "Distinguishes a model that identifies short continuation from one whose \
                 information also points toward larger runner-type outcomes. Descriptive by \
                 construction: +5% and +10% are secondary surfaces and may not move the verdict, \
                 but their direction is exactly what a reader needs in order to know which of \
                 those two things V1 is."
                    .to_string(),
        },
        // --- B. Continuation (blocking) ------------------------------------
        Criterion {
            id: "B1-continuation-enrichment-within-movement-band".to_string(),
            dimension: Dimension::ContinuationQuality,
            surface: Surface::Primary,
            blocking: true,
            metric: "additional forward return after ranking, within bands of movement already \
                     completed"
                .to_string(),
            control: Some("contemporaneous-cohort".to_string()),
            rule: Rule::EnrichmentLowerBoundAbove { value: 1.0 },
            rationale:
                "Continuation must be judged after controlling for how much of the move has \
                 already happened, or it will simply rediscover that things which have moved tend \
                 to keep moving. Banding first is what makes it a claim about the score."
                    .to_string(),
        },
        Criterion {
            id: "B2-continuation-monotonicity".to_string(),
            dimension: Dimension::ContinuationQuality,
            surface: Surface::Primary,
            blocking: true,
            metric: "continuation rate across continuationRank buckets".to_string(),
            control: None,
            rule: Rule::MonotoneAcrossRanks,
            rationale:
                "A score that enriches only at its very top and is otherwise unordered is a \
                 threshold, not a ranking. Monotonicity is what distinguishes the two."
                    .to_string(),
        },
        // --- Ranking quality (blocking) ------------------------------------
        Criterion {
            id: "R1-ranking-monotonicity".to_string(),
            dimension: Dimension::RankingQuality,
            surface: Surface::Primary,
            blocking: true,
            metric: "target rate across earlyQualityRank buckets within the same window"
                .to_string(),
            control: None,
            rule: Rule::MonotoneAcrossRanks,
            rationale:
                "Both A and B are expressed through a rank, so a non-monotone ranking would make \
                 them accidents of where the cut happened to fall."
                    .to_string(),
        },
        Criterion {
            id: "R2-ranking-beats-move-magnitude".to_string(),
            dimension: Dimension::RankingQuality,
            surface: Surface::Primary,
            blocking: true,
            metric: "top-5 target rate, ranked by OI score vs ranked by move-from-start"
                .to_string(),
            control: Some("move-magnitude".to_string()),
            rule: Rule::EnrichmentLowerBoundAbove { value: 1.0 },
            rationale:
                "Move magnitude is free. If the model cannot beat sorting by it, the model is not \
                 what is doing the work."
                    .to_string(),
        },
        // --- C. Recall: three distinct questions ---------------------------
        //
        // The contract review of 2026-09-17 separated what had been one
        // criterion. "Does OI preserve what detection found" and "did the
        // scanner detect enough of the market" are different questions with
        // different subjects, and answering only the first would have let
        // strong conditional OI performance conceal weak upstream coverage.
        Criterion {
            id: "C1-oi-conditional-retention".to_string(),
            dimension: Dimension::Recall,
            surface: Surface::Primary,
            blocking: true,
            metric: "among meaningful reference opportunities that DETECTION reached, the \
                     fraction that OI retained as an opportunity"
                .to_string(),
            control: Some("detector-stage".to_string()),
            rule: Rule::NoWorseThanControlBy { fraction: 0.05 },
            rationale:
                "Conditional on detection, deliberately. OI is an intelligence layer sitting \
                 downstream of the detectors, so it must not lose what detection already found -- \
                 but it must not be graded for failures upstream of it either. The population is \
                 therefore restricted to reference opportunities detection actually reached. \
                 Stated as a comparison rather than an absolute rate because no defensible \
                 absolute target exists yet; the 5% tolerance is for session-edge boundary \
                 effects, not for real loss."
                    .to_string(),
        },
        Criterion {
            id: "C3-independent-detection-coverage".to_string(),
            dimension: Dimension::Recall,
            surface: Surface::Secondary,
            blocking: false,
            metric: "meaningful reference opportunities reaching DETECTOR, over all causally \
                     evaluable meaningful reference opportunities"
                .to_string(),
            control: None,
            rule: Rule::Descriptive,
            rationale:
                "A primary reported *system* metric that characterises the scanner beneath OI, \
                 not OI itself -- which is why it is reported rather than gated. No absolute pass \
                 threshold is pre-registered because none is derivable from development data, and \
                 inventing one would grade OI V1 for a task it never had the opportunity to \
                 perform. Reported overall, at each target, by price regime, by time of day and \
                 by causal miss stage."
                    .to_string(),
        },
        Criterion {
            id: "C2-recall-attribution-established".to_string(),
            dimension: Dimension::Recall,
            surface: Surface::Primary,
            blocking: true,
            metric: "fraction of reference opportunities whose deepest reached stage is \
                     causally established rather than UNKNOWN"
                .to_string(),
            control: None,
            rule: Rule::MedianAbove { value: 0.5 },
            rationale:
                "A recall figure computed over a population that is mostly UNKNOWN is not a recall \
                 figure. This gates the *measurability* of C1, not the platform's performance. \
                 0.5 is structural: below it, the unknown majority determines the answer."
                    .to_string(),
        },
        // --- Earliness (blocking, paired with A so the trade-off cannot hide)
        Criterion {
            id: "E1-ranked-before-the-move".to_string(),
            dimension: Dimension::Earliness,
            surface: Surface::Primary,
            blocking: true,
            metric: "median seconds between first top-5 early-quality rank and the +2% crossing, \
                     over reference opportunities that both ranked and crossed"
                .to_string(),
            control: None,
            rule: Rule::MedianAbove { value: 0.0 },
            rationale:
                "§18 exists so that 'early but noisy' and 'late but precise' cannot be traded \
                 against each other silently. A median lead of zero or less means the system ranks \
                 after the move, which has no early value whatever its precision. Zero is \
                 structural, not a chosen threshold."
                    .to_string(),
        },
        Criterion {
            id: "E2-move-already-completed".to_string(),
            dimension: Dimension::Earliness,
            surface: Surface::Secondary,
            blocking: false,
            metric: "distribution of percentage of the eventual move already completed at first \
                     top-5 rank"
                .to_string(),
            control: None,
            rule: Rule::Descriptive,
            rationale:
                "Reported alongside E1 so a positive lead time cannot conceal that most of the \
                 move had already happened."
                    .to_string(),
        },
        // --- D. Risk (blocking) --------------------------------------------
        Criterion {
            id: "D1-excursion-ratio-not-degraded".to_string(),
            dimension: Dimension::RiskExcursion,
            surface: Surface::Primary,
            blocking: true,
            metric: "MFE / |MAE| for the top-ranked cohort, against the contemporaneous cohort"
                .to_string(),
            control: Some("contemporaneous-cohort".to_string()),
            rule: Rule::NoWorseThanControlBy { fraction: 0.10 },
            rationale:
                "§24D: higher-ranked candidates must not buy apparent precision with \
                 disproportionately adverse excursion. Stated as 'not materially worse than the \
                 cohort' rather than an absolute ratio, because the absolute level is a property \
                 of the market that day, not of the model."
                    .to_string(),
        },
        Criterion {
            id: "D2-drawdown-before-mfe".to_string(),
            dimension: Dimension::RiskExcursion,
            surface: Surface::Secondary,
            blocking: false,
            metric: "drawdown before maximum favourable excursion, by rank bucket".to_string(),
            control: Some("contemporaneous-cohort".to_string()),
            rule: Rule::Descriptive,
            rationale:
                "The shape of the path matters to anything that would eventually trade it, and is \
                 not captured by the ratio alone."
                    .to_string(),
        },
        // --- Robustness (diagnostic) ---------------------------------------
        Criterion {
            id: "S1-no-primary-result-reverses-in-a-major-segment".to_string(),
            dimension: Dimension::RobustnessSegmentation,
            surface: Surface::Secondary,
            blocking: false,
            metric: "sign of each primary enrichment within regime, price band, time band, \
                     opening detector and confluence count"
                .to_string(),
            control: Some("contemporaneous-cohort".to_string()),
            rule: Rule::Descriptive,
            rationale:
                "§21: a global result must not hide a segment where the relationship reverses. \
                 Diagnostic rather than blocking because a single session will not populate every \
                 segment well enough to make a reversal decisive — but a reversal is reported \
                 prominently and is a reason for human review."
                    .to_string(),
        },
    ]
}

impl QualificationSpec {
    /// Canonical JSON: `serde_json` with sorted keys, which `serde_json::Value`
    /// gives by default because its map is a `BTreeMap`. Round-tripping through
    /// `Value` is what makes the hash independent of struct field order.
    pub fn canonical_json(&self) -> String {
        let value: serde_json::Value =
            serde_json::to_value(self).expect("the specification is plain data");
        serde_json::to_string_pretty(&value).expect("a Value always serializes")
    }

    /// The hash the qualification report must quote.
    pub fn sha256(&self) -> String {
        sha256::hex(self.canonical_json().as_bytes())
    }

    /// Criteria that can decide the verdict.
    pub fn blocking(&self) -> impl Iterator<Item = &Criterion> {
        self.criteria.iter().filter(|c| c.blocking)
    }

    /// §28: a secondary surface is diagnostic and can never be blocking. A spec
    /// that violated this would let a cherry-picked cell decide qualification.
    pub fn validate(&self) -> Result<(), String> {
        for criterion in &self.criteria {
            if criterion.blocking && criterion.surface != Surface::Primary {
                return Err(format!(
                    "criterion {} is blocking but sits on a secondary surface; a diagnostic \
                     result must never decide qualification",
                    criterion.id
                ));
            }
            if let Some(control) = &criterion.control {
                let Some(definition) = self.controls.iter().find(|c| &c.id == control) else {
                    return Err(format!(
                        "criterion {} names control {control}, which is not defined",
                        criterion.id
                    ));
                };
                if criterion.blocking && !definition.has_parity() {
                    return Err(format!(
                        "criterion {} is blocking but its control {control} lacks parity; \
                         §33 requires such a comparison be descriptive only",
                        criterion.id
                    ));
                }
            }
        }
        // Every blocking dimension of §24 must actually be covered.
        for required in [
            Dimension::EarlyQuality,
            Dimension::ContinuationQuality,
            Dimension::Recall,
            Dimension::RiskExcursion,
        ] {
            if !self.criteria.iter().any(|c| c.dimension == required && c.blocking) {
                return Err(format!("{required:?} has no blocking criterion"));
            }
        }
        if self.primary_targets_pct.is_empty() || self.primary_horizons_secs.is_empty() {
            return Err("a primary target and horizon must be pre-registered".to_string());
        }
        // The two-interpretation obligation is the one a report is most likely
        // to quietly drop, because it is the one that can make a good result
        // look worse.
        for required in ["R-detection-coverage", "R-two-interpretations", "R-runner-stage-ladder"] {
            if !self.reporting.iter().any(|r| r.id == required) {
                return Err(format!("reporting requirement {required} is missing"));
            }
        }
        if self.expected_oi_config_fingerprint.is_none() {
            return Err(
                "the contract must be bound to an OI configuration before it can evaluate a \
                 prospective session"
                    .to_string(),
            );
        }
        Ok(())
    }

    /// Binds the contract to a specific deployment. Done **before** the
    /// session, and it changes the hash — which is the point: a spec bound to a
    /// different configuration is a different spec.
    pub fn bind_to_configuration(mut self, oi_config_fingerprint: &str) -> Self {
        self.expected_oi_config_fingerprint = Some(oi_config_fingerprint.to_string());
        self
    }
}

#[cfg(test)]
#[path = "spec_tests.rs"]
mod tests;
