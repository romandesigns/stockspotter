//! Opportunity Intelligence **V2** — shadow research ranking, alongside V1.
//!
//! Implements `OI-V2-PREREGISTRATION-2026-09-19.md`
//! (sha256 6e9bad5b7c461553feea92f91378f7fcb06fbdfc1b7abac6119adcbaad93dc78)
//! exactly. Any divergence from that document is a stop condition, not an
//! invitation to amend the document.
//!
//! # Why V2 exists
//!
//! V1 `early_quality_score` declares all four momentum features as
//! `EARLY_QUALITY_CORE`, and `finalize_score` returns `value: None` when any
//! core feature is missing. Momentum was measured fresh at only **3.5% (Thu)
//! and 4.94% (Fri)** of ranking windows, so V1 EarlyQuality is structurally
//! unrankable at roughly 95% of them. That is an architectural defect, not a
//! weighting problem, and no re-weighting of V1 can repair it.
//!
//! V2 Early therefore has a **minimal, momentum-free core** and treats
//! momentum as bounded supplemental evidence.
//!
//! # Hard boundaries (identical to V1's)
//!
//! * Nothing here is read by a detector, by client ordering, or by
//!   `auto_trader`. It is an independent consumer of already-broadcast state.
//! * No score, regime or rank uses information from after its own timestamp.
//! * **Unknown is `None`, never `0.0`.** Every score carries the list of
//!   features that were missing when it was computed.
//! * V1 is untouched: this module adds types, it does not modify any.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::context::{IgnitionPhase, SignalContext};
use crate::opportunity::{Opportunity, Regime, ScoreComponent, Transform, UnrankableReason};

// ---------------------------------------------------------------------------
// Version identities (preregistration §4)
// ---------------------------------------------------------------------------

pub const V2_SCHEMA_VERSION: u32 = 1;
pub const EARLY_V2_MODEL_VERSION: &str = "early-quality-v2-transparent";
/// Continuation V2 is identity-preserving: the model is V1, unchanged.
/// Minting a new name for unchanged code would be the more misleading choice.
pub const CONTINUATION_V2_MODEL_VERSION: &str = "continuation-v1-transparent";
pub const RISK_QUALITY_MODEL_VERSION: &str = "risk-quality-v1-transparent";
pub const EVIDENCE_CONFIDENCE_VERSION: &str = "evidence-confidence-v1";
pub const PRIORITY_V2_MODEL_VERSION: &str = "opportunity-priority-v2-transparent";
pub const SCORE_POLICY_V2_VERSION: &str = "score-policy-v3-availability-aware";
pub const RANKING_V2_VERSION: &str = "opportunity-rank-v2";

/// Momentum's maximum possible share of Early V2, against V1's 0.85.
///
/// Chosen structurally from the density measurement, never fitted: a family
/// fresh at ~4% of ranking windows must not dominate a score whose job is to
/// rank the other ~96%.
pub const BETA_MOMENTUM_MAX: f64 = 0.25;

/// How old a `SignalContext` may be and still describe an opportunity's
/// current state.
///
/// **Derived structurally, not tuned.** `Opportunity::latest_context` is
/// rewritten in exactly two places -- `open_new` and `record` -- both driven by
/// an inbound event. Ranking never refreshes it. An opportunity with no events
/// is closed by `expire_inactive` once `inactivity_secs` have passed. So the
/// oldest context any *open* opportunity can present is exactly
/// `inactivity_secs`; beyond that the opportunity no longer exists to rank.
/// That bound is a property of the lifecycle, so it is the bound used here.
///
/// The 2026-09-17 artifact agrees to within a rounding tick: the largest
/// observed `scoreTimestamp - features.detectedAt` over 411,366
/// momentum-bearing rows is 299.985s against a 300s `inactivity_secs`.
///
/// Consequence worth stating plainly: for a live, open opportunity this gate
/// can never fire. It is a declared invariant with a test behind it, not an
/// active filter, and it exists so the *policy* is explicit rather than
/// implied. Choosing anything tighter would be a ranking-quality judgement,
/// and that belongs in a V2.1 preregistration, not here.
pub const CONTEXT_FRESHNESS_SECS: i64 = crate::episode::INACTIVITY_TIMEOUT_SECS;

pub const MIN_PRESENT_INPUTS_V2: usize = 3;
pub const MIN_COMPARABLE_COVERAGE_V2: f64 = 0.40;

/// Core inputs for Early V2. Both measured 100% ever / 100% of ranking
/// windows on both development sessions.
///
/// **Momentum is not a member and must never become one.** `momentum_is_never_core`
/// exists to fail if a future change adds it.
pub const EARLY_V2_CORE: [&str; 2] =
    ["earliness.moveBeforeDetectionPct", "maturity.opportunityAge"];

pub const RISK_QUALITY_CORE: [&str; 1] = ["risk.drawdownFromHigh"];

// ---------------------------------------------------------------------------
// Availability state (preregistration §12)
// ---------------------------------------------------------------------------

/// Live momentum availability. Distinct from the offline lifecycle classes,
/// which are derived from a sequence of these, never stored directly.
///
/// `SeenPreviouslyButStale` must never be treated as `CurrentlyAvailable`:
/// `FEATURE_FRESHNESS_SECS = 120` is what separates them, and conflating the
/// two would silently reintroduce stale momentum into Mode B.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MomentumAvailability {
    NeverSeenYet,
    CurrentlyAvailable,
    SeenPreviouslyButStale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoringMode {
    /// Mode A. Momentum absent or stale.
    MomentumIndependent,
    /// Mode B. Complete momentum family currently available.
    MomentumInformed,
}

// ---------------------------------------------------------------------------
// Scores
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct V2Score {
    pub model_version: String,
    pub policy_version: String,
    /// `None` only when genuinely unrankable. Never a fabricated 0.0.
    pub value: Option<f64>,
    pub raw_weighted: f64,
    pub present_weight: f64,
    pub total_weight: f64,
    pub coverage: f64,
    pub components: Vec<ScoreComponent>,
    pub missing: Vec<String>,
    pub core_missing: Vec<String>,
    pub present_inputs: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unrankable_reason: Option<UnrankableReason>,
}

/// Early V2, carrying the full Mode A / Mode B decomposition so an analyst can
/// reconstruct `earlyV2 = modeA·(1−β) + momentum·β` exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EarlyV2 {
    pub score: V2Score,
    pub scoring_mode: ScoringMode,
    pub momentum_availability: MomentumAvailability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub momentum_first_available_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode_a_term: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub momentum_term: Option<f64>,
    /// `finalEarlyV2 − modeATerm`. Bounded by ±`BETA_MOMENTUM_MAX`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub momentum_adjustment: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriorityV2 {
    pub model_version: String,
    pub value: Option<f64>,
    pub regime: Regime,
    pub weight_early: f64,
    pub weight_continuation: f64,
    pub weight_risk: f64,
    pub early_contribution: f64,
    pub continuation_contribution: f64,
    pub risk_contribution: f64,
    pub raw_priority: f64,
    pub evidence_confidence: f64,
    pub confidence_multiplier: f64,
    /// Weights actually used after renormalising over rankable components.
    pub renormalised: bool,
}

// ---------------------------------------------------------------------------
// Config + fingerprint (preregistration §25)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct V2Config {
    pub beta_momentum_max: f64,
    pub min_present_inputs: usize,
    pub min_comparable_coverage: f64,
    pub early_weights: Vec<(String, f64)>,
    pub risk_weights: Vec<(String, f64)>,
    pub regime_weights: Vec<(String, [f64; 3])>,
    pub confidence_core: f64,
    pub confidence_optional: f64,
    pub confidence_momentum: f64,
    pub confidence_floor: f64,
    pub ranking_cadence_secs: i64,
    pub max_rank_cohort: usize,
    pub feature_freshness_secs: i64,
    /// How stale the whole `SignalContext` may be, measured
    /// `scoreTimestamp - features.detectedAt`, before its feature families
    /// stop counting as current. See `momentum_availability` for the
    /// derivation and for why this is NOT `feature_freshness_secs`.
    pub context_freshness_secs: i64,
}

impl Default for V2Config {
    fn default() -> Self {
        Self {
            beta_momentum_max: BETA_MOMENTUM_MAX,
            min_present_inputs: MIN_PRESENT_INPUTS_V2,
            min_comparable_coverage: MIN_COMPARABLE_COVERAGE_V2,
            early_weights: vec![
                ("earliness.moveBeforeDetectionPct".into(), 0.20),
                ("earliness.consumedFromSessionLow".into(), 0.15),
                ("ignition.confirmationRatio".into(), 0.15),
                ("ignition.attemptQuality".into(), 0.10),
                ("ignition.phaseQuality".into(), 0.05),
                ("stability.invalidationBurden".into(), 0.10),
                ("stability.fragmentation".into(), 0.10),
                ("maturity.opportunityAge".into(), 0.15),
            ],
            risk_weights: vec![
                ("risk.drawdownFromHigh".into(), 0.30),
                ("risk.adverseExcursion".into(), 0.20),
                ("risk.instability".into(), 0.20),
                ("risk.fragmentation".into(), 0.15),
                ("risk.ignitionRejectionRate".into(), 0.15),
                ("risk.haltProximity".into(), 0.15),
            ],
            regime_weights: vec![
                ("early_emerging".into(), [0.50, 0.25, 0.25]),
                ("continuation_acceleration".into(), [0.20, 0.55, 0.25]),
                ("reversal_recovery".into(), [0.30, 0.30, 0.40]),
                ("unclassified".into(), [0.30, 0.40, 0.30]),
            ],
            confidence_core: 0.60,
            confidence_optional: 0.25,
            confidence_momentum: 0.15,
            confidence_floor: 0.85,
            ranking_cadence_secs: 30,
            max_rank_cohort: 4_096,
            feature_freshness_secs: crate::context::FEATURE_FRESHNESS_SECS,
            context_freshness_secs: CONTEXT_FRESHNESS_SECS,
        }
    }
}

impl V2Config {
    /// FNV-1a over the canonical JSON encoding — the same algorithm V1 uses,
    /// applied to a separate struct so V1 and V2 fingerprints move
    /// independently.
    pub fn fingerprint(&self) -> String {
        let json = serde_json::to_string(self).unwrap_or_default();
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in json.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
        format!("oi-v2-cfg-{hash:016x}")
    }
}

// ---------------------------------------------------------------------------
// Scoring helpers -- V2's own, because V2's gate semantics differ from V1's
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn push(
    components: &mut Vec<ScoreComponent>,
    missing: &mut Vec<String>,
    present: &mut usize,
    present_weight: &mut f64,
    total_weight: &mut f64,
    feature: &str,
    raw: Option<f64>,
    transform: &Transform,
    weight: f64,
) -> f64 {
    *total_weight += weight;
    match raw {
        Some(v) if v.is_finite() => {
            let t = transform.apply(v);
            let contribution = t * weight;
            *present += 1;
            *present_weight += weight;
            components.push(ScoreComponent {
                feature: feature.to_string(),
                raw: Some(v),
                transformed: Some(t),
                weight,
                contribution,
            });
            contribution
        }
        // Missing stays missing. It is never coerced to 0.0, which would be
        // indistinguishable from an observed worst-case value.
        _ => {
            missing.push(feature.to_string());
            components.push(ScoreComponent {
                feature: feature.to_string(),
                raw: None,
                transformed: None,
                weight,
                contribution: 0.0,
            });
            0.0
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn finalize(
    model_version: &str,
    components: Vec<ScoreComponent>,
    missing: Vec<String>,
    core: &[&str],
    present: usize,
    present_weight: f64,
    total_weight: f64,
    raw_weighted: f64,
    cfg: &V2Config,
) -> V2Score {
    let core_missing: Vec<String> = core
        .iter()
        .filter(|c| missing.iter().any(|m| m == *c))
        .map(|c| c.to_string())
        .collect();
    let coverage = if total_weight > 0.0 { present_weight / total_weight } else { 0.0 };

    let unrankable_reason = if !core_missing.is_empty() {
        Some(UnrankableReason::CoreFeatureMissing)
    } else if present < cfg.min_present_inputs {
        Some(UnrankableReason::TooFewInputs)
    } else if coverage < cfg.min_comparable_coverage || present_weight <= 0.0 {
        Some(UnrankableReason::InsufficientCoverage)
    } else {
        None
    };

    let value = if unrankable_reason.is_none() {
        Some((raw_weighted / present_weight).clamp(0.0, 1.0))
    } else {
        None
    };

    V2Score {
        model_version: model_version.to_string(),
        policy_version: SCORE_POLICY_V2_VERSION.to_string(),
        value,
        raw_weighted,
        present_weight,
        total_weight,
        coverage,
        components,
        missing,
        core_missing,
        present_inputs: present,
        unrankable_reason,
    }
}

fn ctx_of(op: &Opportunity) -> Option<&SignalContext> {
    op.latest_context.as_ref()
}

/// Complete momentum family, in a context that is still current at `at`.
///
/// # The F2 correction
///
/// This used to re-derive momentum freshness as
/// `scoreTimestamp - momentum.observed_at <= FEATURE_FRESHNESS_SECS`, which
/// double-gated and was wrong twice over.
///
/// `momentum.observed_at` is **bar-close data time**: `live.rs` emits
/// `bar.timestamp + 1 minute`, and Alpaca bar timestamps are bar-open, so the
/// value is the earliest instant that bar could be known. All 411,366 momentum
/// blocks in the 2026-09-17 artifact are minute-aligned, confirming it.
///
/// `FeatureCache::snapshot` already gates that data time against
/// `detected_at` -- a momentum block is only ever attached when
/// `detected_at - observed_at <= FEATURE_FRESHNESS_SECS`. Measured maximum in
/// the artifact: 120.998s against a 120s bound, i.e. the gate holds exactly
/// (`num_seconds()` truncates toward zero). **So the mere presence of a
/// momentum block is already proof that it passed the data-freshness gate.**
/// Re-checking it here proves nothing new.
///
/// What the old check actually measured was the *sum* of two ages:
/// ```text
///   A = detectedAt     - momentum.observedAt   data age    (gated, <= 120s)
///   B = scoreTimestamp - detectedAt            context age (was ungated)
///   old test:  A + B <= 120
/// ```
/// `latest_context` refreshes on events, not on ranking windows, so B is
/// routinely non-zero and reaches `inactivity_secs`. Comparing `A + B` to a
/// bound written for `A` alone marked live momentum stale: 40,674 rows on
/// 2026-09-17 (2.27% of the stale stratum) that V1 ranked and V2 did not.
/// Worked example from the artifact -- scoreTimestamp 00:00:30.013,
/// detectedAt 23:57:27.756, observedAt 23:57:00 -- gives A = 27.8s (fresh,
/// correctly attached) but A + B = 210s, and the old code called it stale.
///
/// The two ages are now judged separately: data freshness by the block's
/// presence, context freshness by `CONTEXT_FRESHNESS_SECS`.
pub fn momentum_availability(
    op: &Opportunity,
    at: DateTime<Utc>,
    ever_seen: bool,
    cfg: &V2Config,
) -> MomentumAvailability {
    let fresh = ctx_of(op).is_some_and(|c| {
        // 1. Context age. `at` before `detected_at` would mean the context came
        //    from this opportunity's future: not a staleness question but a
        //    causality violation, and the range check fails it closed.
        let context_age = (at - c.detected_at).num_seconds();
        if !(0..=cfg.context_freshness_secs).contains(&context_age) {
            return false;
        }
        c.momentum.as_ref().is_some_and(|m| {
            // 2. Data freshness is NOT re-derived -- presence is the proof that
            //    `FeatureCache::snapshot` already applied FEATURE_FRESHNESS_SECS.
            //    What IS re-asserted is the *ordering* half of that contract,
            //    which is a causality invariant rather than a staleness bound
            //    and costs one comparison: a momentum bar may not post-date the
            //    context that carries it. In a well-formed artifact this is
            //    always true, so it only ever fires on a malformed one.
            m.observed_at <= c.detected_at
                // 3. A non-finite component cannot be blended in Mode B.
                && m.overall.is_finite()
                && m.ma_slope.is_finite()
                && m.structure.is_finite()
                && m.volume_confirmation.is_finite()
                && m.wick_rejection.is_finite()
        })
    });
    if fresh {
        MomentumAvailability::CurrentlyAvailable
    } else if ever_seen {
        MomentumAvailability::SeenPreviouslyButStale
    } else {
        MomentumAvailability::NeverSeenYet
    }
}

// ---------------------------------------------------------------------------
// Early V2 -- Mode A
// ---------------------------------------------------------------------------

fn mode_a(op: &Opportunity, at: DateTime<Utc>, cfg: &V2Config) -> V2Score {
    let ctx = ctx_of(op);
    let ign = ctx.and_then(|c| c.ignition.as_ref());
    let pre = ctx.and_then(|c| c.pre_detection.as_ref());

    let mut components = Vec::new();
    let mut missing = Vec::new();
    let mut present = 0usize;
    let mut pw = 0.0;
    let mut tw = 0.0;
    let mut total = 0.0;

    // A1: earliness at detection. V1's only non-momentum component, knots
    // carried over unchanged.
    total += push(
        &mut components, &mut missing, &mut present, &mut pw, &mut tw,
        "earliness.moveBeforeDetectionPct",
        op.move_before_detection_pct,
        &Transform::Piecewise {
            knots: vec![(-5.0, 0.2), (0.0, 1.0), (2.0, 0.8), (5.0, 0.4), (10.0, 0.1), (20.0, 0.0)],
        },
        0.20,
    );

    // A2: move consumed measured from the SESSION LOW, which A1 cannot see --
    // A1 is anchored at detection, so a move already far from its base but
    // detected early looks identical to a genuinely early one.
    let consumed = pre
        .and_then(|p| p.session_low_observed)
        .filter(|low| *low > 0.0)
        .map(|low| (op.latest_price / low - 1.0) * 100.0);
    total += push(
        &mut components, &mut missing, &mut present, &mut pw, &mut tw,
        "earliness.consumedFromSessionLow",
        consumed,
        &Transform::Piecewise {
            knots: vec![(0.0, 1.0), (10.0, 0.8), (25.0, 0.5), (50.0, 0.2), (100.0, 0.0)],
        },
        0.15,
    );

    // B1: constructive progression vs repeated failure. Denominator zero is
    // MISSING, not 0.0 -- "no attempts yet" is not "all attempts failed".
    let conf_ratio = ign.and_then(|i| {
        let denom = i.confirmations + i.rejections;
        (denom > 0).then(|| f64::from(i.confirmations) / f64::from(denom))
    });
    total += push(
        &mut components, &mut missing, &mut present, &mut pw, &mut tw,
        "ignition.confirmationRatio", conf_ratio, &Transform::Raw, 0.15,
    );

    // B2: "is this the first attempt or the fourth" -- the source comment on
    // candidates_opened names this as a real feature.
    total += push(
        &mut components, &mut missing, &mut present, &mut pw, &mut tw,
        "ignition.attemptQuality",
        ign.map(|i| f64::from(i.candidates_opened)),
        &Transform::Bins { bounds: vec![1.0, 2.0, 4.0], weights: vec![1.0, 1.0, 0.6, 0.3, 0.1] },
        0.10,
    );

    // B3: current phase, on the only sensible ordering of the enum.
    let phase = ign.map(|i| match i.phase {
        IgnitionPhase::FollowThroughConfirmed => 1.0,
        IgnitionPhase::CandidateOpened => 0.5,
        IgnitionPhase::FollowThroughRejected => 0.0,
    });
    total += push(
        &mut components, &mut missing, &mut present, &mut pw, &mut tw,
        "ignition.phaseQuality", phase, &Transform::Raw, 0.05,
    );

    // C1/C2: repeated fragmentation and absorbed invalidation must not be
    // mistaken for conviction. rawEventCount is deliberately NOT a positive
    // component for exactly this reason.
    total += push(
        &mut components, &mut missing, &mut present, &mut pw, &mut tw,
        "stability.invalidationBurden",
        Some(f64::from(op.invalidations_absorbed)),
        &Transform::Bins {
            bounds: vec![0.0, 1.0, 3.0, 8.0],
            weights: vec![1.0, 0.8, 0.55, 0.25, 0.05],
        },
        0.10,
    );
    total += push(
        &mut components, &mut missing, &mut present, &mut pw, &mut tw,
        "stability.fragmentation",
        Some(f64::from(op.episode_fragments)),
        &Transform::Bins { bounds: vec![1.0, 2.0, 4.0], weights: vec![1.0, 0.7, 0.4, 0.15] },
        0.10,
    );

    // D1: age is context, not merit. Young is unproven, 60-900s is prime,
    // stale decays. Bounded at both ends.
    let age = (at - op.opened_at).num_seconds().max(0) as f64;
    total += push(
        &mut components, &mut missing, &mut present, &mut pw, &mut tw,
        "maturity.opportunityAge",
        Some(age),
        &Transform::Piecewise {
            knots: vec![
                (0.0, 0.35), (60.0, 0.85), (300.0, 1.0), (900.0, 0.9),
                (1800.0, 0.6), (3600.0, 0.3), (7200.0, 0.1),
            ],
        },
        0.15,
    );

    finalize(EARLY_V2_MODEL_VERSION, components, missing, &EARLY_V2_CORE,
             present, pw, tw, total, cfg)
}

/// The momentum term, normalised over present weight. V1's four transforms,
/// unchanged.
fn momentum_term(op: &Opportunity) -> Option<f64> {
    let m = ctx_of(op)?.momentum.as_ref()?;
    let parts: [(f64, f64); 4] = [
        (Transform::Presence { threshold: 0.0 }.apply(m.ma_slope), 0.35),
        (Transform::Raw.apply(m.structure), 0.25),
        (
            Transform::Bins {
                bounds: vec![0.40, 0.60, 0.80, 1.00],
                weights: vec![0.0, 0.35, 0.80, 1.00, 0.70],
            }
            .apply(m.volume_confirmation),
            0.25,
        ),
        (
            Transform::Bins { bounds: vec![0.50, 0.75, 1.00], weights: vec![0.0, 0.50, 1.00, 0.75] }
                .apply(m.wick_rejection),
            0.15,
        ),
    ];
    let mut num = 0.0;
    let mut den = 0.0;
    for (t, w) in parts {
        if t.is_finite() {
            num += t * w;
            den += w;
        }
    }
    (den > 0.0).then(|| (num / den).clamp(0.0, 1.0))
}

/// Early V2. Mode A always runs; Mode B adds a bounded momentum adjustment.
pub fn early_quality_v2(
    op: &Opportunity,
    at: DateTime<Utc>,
    availability: MomentumAvailability,
    momentum_first_available_at: Option<DateTime<Utc>>,
    cfg: &V2Config,
) -> EarlyV2 {
    let mut score = mode_a(op, at, cfg);
    let mode_a_value = score.value;

    let use_momentum = availability == MomentumAvailability::CurrentlyAvailable;
    let mterm = if use_momentum { momentum_term(op) } else { None };

    let (mode, adjustment) = match (mode_a_value, mterm) {
        (Some(a), Some(m)) => {
            let beta = cfg.beta_momentum_max;
            let blended = (a * (1.0 - beta) + m * beta).clamp(0.0, 1.0);
            score.value = Some(blended);
            (ScoringMode::MomentumInformed, Some(blended - a))
        }
        // Momentum cannot rescue an unrankable Mode A: the core is what makes
        // an opportunity rankable, and momentum is not in it.
        _ => (ScoringMode::MomentumIndependent, None),
    };

    EarlyV2 {
        score,
        scoring_mode: mode,
        momentum_availability: availability,
        momentum_first_available_at,
        mode_a_term: mode_a_value,
        momentum_term: mterm,
        momentum_adjustment: adjustment,
    }
}

// ---------------------------------------------------------------------------
// RiskQuality -- higher is MORE FAVOURABLE (better controlled)
// ---------------------------------------------------------------------------

pub fn risk_quality(op: &Opportunity, cfg: &V2Config) -> V2Score {
    let ctx = ctx_of(op);
    let ign = ctx.and_then(|c| c.ignition.as_ref());
    let halt = ctx.and_then(|c| c.halt.as_ref());

    let mut components = Vec::new();
    let mut missing = Vec::new();
    let mut present = 0usize;
    let mut pw = 0.0;
    let mut tw = 0.0;
    let mut total = 0.0;

    let drawdown = (op.observed_high > 0.0)
        .then(|| ((op.observed_high - op.latest_price) / op.observed_high * 100.0).max(0.0));
    total += push(
        &mut components, &mut missing, &mut present, &mut pw, &mut tw,
        "risk.drawdownFromHigh", drawdown,
        &Transform::Piecewise {
            knots: vec![(0.0, 1.0), (2.0, 0.85), (5.0, 0.6), (10.0, 0.3), (20.0, 0.1), (40.0, 0.0)],
        },
        0.30,
    );
    total += push(
        &mut components, &mut missing, &mut present, &mut pw, &mut tw,
        "risk.adverseExcursion", op.min_move_pct,
        &Transform::Piecewise {
            knots: vec![
                (-40.0, 0.0), (-20.0, 0.1), (-10.0, 0.3), (-5.0, 0.6), (-2.0, 0.85), (0.0, 1.0),
            ],
        },
        0.20,
    );
    total += push(
        &mut components, &mut missing, &mut present, &mut pw, &mut tw,
        "risk.instability", Some(f64::from(op.invalidations_absorbed)),
        &Transform::Bins {
            bounds: vec![0.0, 1.0, 3.0, 8.0], weights: vec![1.0, 0.8, 0.5, 0.2, 0.0],
        },
        0.20,
    );
    total += push(
        &mut components, &mut missing, &mut present, &mut pw, &mut tw,
        "risk.fragmentation", Some(f64::from(op.episode_fragments)),
        &Transform::Bins { bounds: vec![1.0, 2.0, 4.0], weights: vec![1.0, 0.7, 0.35, 0.1] },
        0.15,
    );
    let rej_rate = ign.and_then(|i| {
        let denom = i.confirmations + i.rejections;
        (denom > 0).then(|| f64::from(i.rejections) / f64::from(denom))
    });
    total += push(
        &mut components, &mut missing, &mut present, &mut pw, &mut tw,
        "risk.ignitionRejectionRate", rej_rate,
        &Transform::Piecewise {
            knots: vec![(0.0, 1.0), (0.25, 0.8), (0.5, 0.5), (0.75, 0.2), (1.0, 0.0)],
        },
        0.15,
    );
    // OPTIONAL: halt was measured at 37.7-38.0% of ranking windows, so
    // RiskQuality must stay scoreable without it.
    total += push(
        &mut components, &mut missing, &mut present, &mut pw, &mut tw,
        "risk.haltProximity", halt.map(|h| h.proximity_ratio),
        &Transform::Piecewise {
            knots: vec![(0.0, 1.0), (0.25, 0.9), (0.5, 0.6), (0.75, 0.3), (1.0, 0.0)],
        },
        0.15,
    );

    finalize(RISK_QUALITY_MODEL_VERSION, components, missing, &RISK_QUALITY_CORE,
             present, pw, tw, total, cfg)
}

// ---------------------------------------------------------------------------
// Evidence confidence
// ---------------------------------------------------------------------------

/// Full universal evidence with NO momentum reaches 0.85. Absence of momentum
/// must not by itself imply low confidence -- operating before momentum is
/// precisely Mode A's purpose.
pub fn evidence_confidence(
    early: &V2Score,
    availability: MomentumAvailability,
    cfg: &V2Config,
) -> f64 {
    let core_total = EARLY_V2_CORE.len() as f64;
    let core_present = core_total - early.core_missing.len() as f64;
    let core_frac = if core_total > 0.0 { core_present / core_total } else { 0.0 };
    let momentum_conf = match availability {
        MomentumAvailability::CurrentlyAvailable => 1.0,
        MomentumAvailability::SeenPreviouslyButStale => 0.5,
        MomentumAvailability::NeverSeenYet => 0.0,
    };
    (cfg.confidence_core * core_frac
        + cfg.confidence_optional * early.coverage
        + cfg.confidence_momentum * momentum_conf)
        .clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Priority V2
// ---------------------------------------------------------------------------

fn regime_key(r: Regime) -> &'static str {
    match r {
        Regime::EarlyEmerging => "early_emerging",
        Regime::ContinuationAcceleration => "continuation_acceleration",
        Regime::ReversalRecovery => "reversal_recovery",
        Regime::Unclassified => "unclassified",
    }
}

pub fn opportunity_priority_v2(
    early: &EarlyV2,
    continuation: Option<f64>,
    risk: &V2Score,
    regime: Regime,
    cfg: &V2Config,
) -> PriorityV2 {
    let key = regime_key(regime);
    let w = cfg
        .regime_weights
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, w)| *w)
        .unwrap_or([0.30, 0.40, 0.30]);

    let e = early.score.value;
    let c = continuation;
    let r = risk.value;

    // Renormalise over whatever is actually rankable, so a missing component
    // removes its weight rather than silently scoring as zero.
    let mut num = 0.0;
    let mut den = 0.0;
    let ec = e.map(|v| v * w[0]).unwrap_or(0.0);
    let cc = c.map(|v| v * w[1]).unwrap_or(0.0);
    let rc = r.map(|v| v * w[2]).unwrap_or(0.0);
    if e.is_some() { num += ec; den += w[0]; }
    if c.is_some() { num += cc; den += w[1]; }
    if r.is_some() { num += rc; den += w[2]; }

    let raw = if den > 0.0 { num / den } else { 0.0 };
    let conf = evidence_confidence(&early.score, early.momentum_availability, cfg);
    let mult = cfg.confidence_floor + (1.0 - cfg.confidence_floor) * conf;
    let value = (den > 0.0).then(|| (raw * mult).clamp(0.0, 1.0));

    PriorityV2 {
        model_version: PRIORITY_V2_MODEL_VERSION.to_string(),
        value,
        regime,
        weight_early: w[0],
        weight_continuation: w[1],
        weight_risk: w[2],
        early_contribution: ec,
        continuation_contribution: cc,
        risk_contribution: rc,
        raw_priority: raw,
        evidence_confidence: conf,
        confidence_multiplier: mult,
        renormalised: den < (w[0] + w[1] + w[2]) - 1e-12,
    }
}

// ---------------------------------------------------------------------------
// Bounded rank persistence (§39/§22) -- fixed size, no history vector
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RankPersistence {
    pub first_score_at: Option<DateTime<Utc>>,
    pub first_rank_at: Option<DateTime<Utc>>,
    pub best_rank: Option<usize>,
    pub best_percentile: Option<f64>,
    pub current_percentile: Option<f64>,
    pub consecutive_top25: u32,
    pub consecutive_top10: u32,
    pub consecutive_top5: u32,
    pub total_top25_windows: u32,
    pub total_top10_windows: u32,
    pub total_top5_windows: u32,
    pub previous_score: Option<f64>,
    pub score_delta: Option<f64>,
    pub previous_rank: Option<usize>,
    pub rank_delta: Option<i64>,
    pub best_rank_at: Option<DateTime<Utc>>,
    pub time_since_best_rank_secs: Option<i64>,
}

impl RankPersistence {
    /// Fold one ranking window in. Bounded: every field is a scalar, so state
    /// is O(1) per opportunity regardless of session length.
    ///
    /// Reset happens only with the opportunity lifecycle. A detector
    /// invalidation that does not close the opportunity must not reset this,
    /// which is why the caller owns the lifetime, not this type.
    pub fn observe(&mut self, at: DateTime<Utc>, score: Option<f64>, rank: Option<usize>, cohort: usize) {
        if score.is_some() && self.first_score_at.is_none() {
            self.first_score_at = Some(at);
        }
        if let Some(s) = score {
            self.score_delta = self.previous_score.map(|p| s - p);
            self.previous_score = Some(s);
        }
        let Some(rk) = rank else {
            self.consecutive_top25 = 0;
            self.consecutive_top10 = 0;
            self.consecutive_top5 = 0;
            self.current_percentile = None;
            return;
        };
        if self.first_rank_at.is_none() {
            self.first_rank_at = Some(at);
        }
        self.rank_delta = self.previous_rank.map(|p| rk as i64 - p as i64);
        self.previous_rank = Some(rk);

        let pct = if cohort > 0 { 100.0 * rk as f64 / cohort as f64 } else { 100.0 };
        self.current_percentile = Some(pct);
        if self.best_rank.is_none_or(|b| rk < b) {
            self.best_rank = Some(rk);
            self.best_rank_at = Some(at);
        }
        if self.best_percentile.is_none_or(|b| pct < b) {
            self.best_percentile = Some(pct);
        }
        self.time_since_best_rank_secs = self.best_rank_at.map(|b| (at - b).num_seconds());

        if pct <= 25.0 { self.total_top25_windows += 1; self.consecutive_top25 += 1; }
        else { self.consecutive_top25 = 0; }
        if pct <= 10.0 { self.total_top10_windows += 1; self.consecutive_top10 += 1; }
        else { self.consecutive_top10 = 0; }
        if pct <= 5.0 { self.total_top5_windows += 1; self.consecutive_top5 += 1; }
        else { self.consecutive_top5 = 0; }
    }
}

#[cfg(test)]
#[path = "opportunity_v2_tests.rs"]
mod tests;
