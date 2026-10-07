//! Offline opportunity ↔ outcome evaluation join (Correction 1).
//!
//! # Offline, and structurally so
//!
//! Nothing here is reachable from the live shadow path. `OpportunityScoreSnapshot`
//! gains no outcome field; the join produces a *separate* record type that can
//! only be built once the forward observations already exist. A score written
//! during the session and a record written after it are different objects, and
//! the live one stays causally pure — an outcome cannot leak backwards into it
//! because there is nowhere for it to go.
//!
//! # The anchor problem, stated rather than hidden
//!
//! Measurement outcomes are anchored at an **episode's** signal instant. A
//! score snapshot is taken at a **ranking window** instant. These are not the
//! same moment, so "the forward return from this score" is not directly
//! available — what is available is the forward return from the episode this
//! opportunity contained around that time.
//!
//! That gap is carried on every record as [`JoinedOutcome::anchor_offset_secs`]
//! together with the rule that selected the anchor, instead of being quietly
//! absorbed. An analyst can restrict to small offsets, or stratify by it. What
//! they cannot do is mistake one for the other, which is the failure this
//! design exists to prevent.
//!
//! Membership comes from [`crate::membership`], which uses causal fields only —
//! never the outcome being joined.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::episode::OpportunityEpisode;
use crate::horizon::{
    CensorReason, Excursion, HorizonReturn, Observation, TargetTiming, HORIZON_SECS,
};
use crate::membership::{map_memberships, MembershipReport, MembershipStatus};
use crate::opportunity::{
    OiVersions, Opportunity, OpportunityScoreSnapshot, PriceRegime, Regime, ShadowScore,
    ShadowState,
};

pub const EVALUATION_SCHEMA_VERSION: u32 = 1;

/// How an outcome anchor was chosen for a score snapshot.
///
/// Versioned because it is a real analytic choice, not an implementation
/// detail: a different rule would attach different outcomes to the same score.
pub const ANCHOR_RULE_VERSION: &str = "anchor-v1-next-at-or-after-then-latest-before";

/// Which member episode's outcome was attached, and why that one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorSelection {
    /// The first member episode anchored at or after the score instant. The
    /// preferred case: the score precedes the measurement it is judged by.
    NextAtOrAfter,
    /// No member episode was anchored at or after the score, so the most
    /// recent earlier anchor was used. The outcome window therefore *starts
    /// before* the score — a backward-looking join, and the negative
    /// `anchor_offset_secs` says so. Treat these separately.
    LatestBefore,
}

/// Why a score snapshot carries no outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinGap {
    /// The opportunity appears in no membership mapping — it was not supplied.
    OpportunityNotSupplied,
    /// The opportunity resolved to no member episode.
    NoMemberEpisode,
    /// Member episodes exist, but none has a settled outcome yet.
    NoSettledOutcome,
    /// Every candidate anchor was ambiguous, so nothing was attached.
    MembershipAmbiguous,
}

/// The outcome attached to one score snapshot, with its provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinedOutcome {
    /// The episode whose outcome this is.
    pub episode_id: String,
    pub anchor_rule_version: String,
    pub anchor_selection: AnchorSelection,
    /// `episode signal_at − score timestamp`, in seconds. **Signed**: positive
    /// means the measurement starts after the score was taken, negative means
    /// before. Zero is the ideal and is not guaranteed.
    pub anchor_offset_secs: i64,
    pub signal_at: DateTime<Utc>,
    pub signal_price: f64,
    /// Carried whole, censoring intact — 30/60/180/300/600/900/1800s.
    pub returns: Vec<HorizonReturn>,
    /// MFE, MAE, seconds to each, and drawdown before MFE.
    pub excursion: Observation<Excursion>,
    /// +2% / +5% / +10% first-touch timing.
    pub time_to_target: Vec<TargetTiming>,
    pub observed_span_secs: i64,
    pub observation_count: usize,
    /// Horizons that are censored, lifted out so a consumer does not have to
    /// walk `returns` to discover the coverage of the record.
    pub censored_horizons: Vec<i64>,
    /// The distinct censor reasons present anywhere in this outcome.
    /// `PendingCapacityReached` here means *we stopped looking*, never a fact
    /// about the symbol.
    pub censor_reasons: Vec<CensorReason>,
    /// True when every configured horizon resolved.
    pub fully_observed: bool,
}

/// Feature coverage for both scores, lifted to the top level.
///
/// Correction 3 made coverage a first-class property of a score; an evaluation
/// that ignored it would re-introduce exactly the comparability problem that
/// correction removed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationCoverage {
    pub early_quality: f64,
    pub continuation: f64,
    pub early_quality_comparable: bool,
    pub continuation_comparable: bool,
}

/// One score snapshot joined to what happened next.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpportunityEvaluationRecord {
    pub schema_version: u32,
    pub versions: OiVersions,
    pub membership_rules_version: String,

    // --- identity -------------------------------------------------------
    pub opportunity_id: String,
    pub symbol: String,
    pub session_date: String,
    pub member_episode_ids: Vec<String>,
    pub membership_status: MembershipStatus,
    pub ambiguous_episode_ids: Vec<String>,

    // --- the scoring decision, as it stood -------------------------------
    pub score_timestamp: DateTime<Utc>,
    pub window_id: String,
    pub regime: Regime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_regime: Option<PriceRegime>,
    pub early_quality: ShadowScore,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub early_quality_rank: Option<usize>,
    pub continuation: ShadowScore,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_rank: Option<usize>,
    pub early_cohort_size: usize,
    pub continuation_cohort_size: usize,
    pub shadow_state: ShadowState,
    pub feature_coverage: EvaluationCoverage,
    pub confluence_count: usize,
    pub detectors_seen: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation_span_secs: Option<i64>,
    pub opportunity_age_secs: i64,
    pub current_price: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_before_detection_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_from_start_pct: Option<f64>,
    pub episode_fragments: u32,
    pub invalidations_absorbed: u32,
    pub raw_event_count: u32,

    // --- what happened next ----------------------------------------------
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<JoinedOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join_gap: Option<JoinGap>,
}

/// Completeness of the join itself. Describes coverage of the evidence, not
/// behaviour of the platform.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationCoverageReport {
    pub snapshots_in: usize,
    pub records_out: usize,
    pub joined: usize,
    pub joined_forward: usize,
    pub joined_backward: usize,
    pub fully_observed_outcomes: usize,
    pub gap_opportunity_not_supplied: usize,
    pub gap_no_member_episode: usize,
    pub gap_no_settled_outcome: usize,
    pub gap_membership_ambiguous: usize,
    pub early_quality_comparable: usize,
    pub continuation_comparable: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationSet {
    pub schema_version: u32,
    pub records: Vec<OpportunityEvaluationRecord>,
    pub membership: MembershipReport,
    pub coverage: EvaluationCoverageReport,
}

/// Builds the evaluation set.
///
/// Deterministic: membership is order-independent (see `membership`), anchor
/// selection breaks ties on episode id, and records are sorted by
/// `(session, symbol, score timestamp, window)`.
pub fn build_evaluation(
    snapshots: &[OpportunityScoreSnapshot],
    opportunities: &[Opportunity],
    episodes: &[OpportunityEpisode],
) -> EvaluationSet {
    let membership = map_memberships(opportunities, episodes);
    let mut coverage = EvaluationCoverageReport {
        snapshots_in: snapshots.len(),
        ..Default::default()
    };

    let mut records: Vec<OpportunityEvaluationRecord> = Vec::with_capacity(snapshots.len());

    for snap in snapshots {
        let mapping = membership
            .mappings
            .iter()
            .find(|m| m.opportunity_id == snap.opportunity_id);

        let (member_ids, status, ambiguous_ids) = match mapping {
            Some(m) => (
                m.member_episode_ids.clone(),
                m.status,
                m.ambiguous_episode_ids.clone(),
            ),
            None => (Vec::new(), MembershipStatus::NoEpisodes, Vec::new()),
        };

        let (outcome, join_gap) = if mapping.is_none() {
            (None, Some(JoinGap::OpportunityNotSupplied))
        } else if member_ids.is_empty() {
            if ambiguous_ids.is_empty() {
                (None, Some(JoinGap::NoMemberEpisode))
            } else {
                // Candidates existed but none could be attributed. Reported as
                // ambiguity rather than as absence: those mean different things.
                (None, Some(JoinGap::MembershipAmbiguous))
            }
        } else {
            match select_anchor(&member_ids, episodes, snap.timestamp) {
                Some(joined) => (Some(joined), None),
                None => (None, Some(JoinGap::NoSettledOutcome)),
            }
        };

        match (&outcome, &join_gap) {
            (Some(o), _) => {
                coverage.joined += 1;
                match o.anchor_selection {
                    AnchorSelection::NextAtOrAfter => coverage.joined_forward += 1,
                    AnchorSelection::LatestBefore => coverage.joined_backward += 1,
                }
                if o.fully_observed {
                    coverage.fully_observed_outcomes += 1;
                }
            }
            (None, Some(gap)) => match gap {
                JoinGap::OpportunityNotSupplied => coverage.gap_opportunity_not_supplied += 1,
                JoinGap::NoMemberEpisode => coverage.gap_no_member_episode += 1,
                JoinGap::NoSettledOutcome => coverage.gap_no_settled_outcome += 1,
                JoinGap::MembershipAmbiguous => coverage.gap_membership_ambiguous += 1,
            },
            (None, None) => {}
        }

        if snap.early_quality.value.is_some() {
            coverage.early_quality_comparable += 1;
        }
        if snap.continuation.value.is_some() {
            coverage.continuation_comparable += 1;
        }

        records.push(OpportunityEvaluationRecord {
            schema_version: EVALUATION_SCHEMA_VERSION,
            versions: snap.versions.clone(),
            membership_rules_version: membership.rules_version.clone(),
            opportunity_id: snap.opportunity_id.clone(),
            symbol: snap.symbol.clone(),
            session_date: snap.session_date.clone(),
            member_episode_ids: member_ids,
            membership_status: status,
            ambiguous_episode_ids: ambiguous_ids,
            score_timestamp: snap.timestamp,
            window_id: snap.window_id.clone(),
            regime: snap.regime,
            price_regime: snap.price_regime,
            feature_coverage: EvaluationCoverage {
                early_quality: snap.early_quality.coverage,
                continuation: snap.continuation.coverage,
                early_quality_comparable: snap.early_quality.value.is_some(),
                continuation_comparable: snap.continuation.value.is_some(),
            },
            early_quality: snap.early_quality.clone(),
            early_quality_rank: snap.early_quality_rank,
            continuation: snap.continuation.clone(),
            continuation_rank: snap.continuation_rank,
            early_cohort_size: snap.early_cohort_size,
            continuation_cohort_size: snap.continuation_cohort_size,
            shadow_state: snap.shadow_state,
            confluence_count: snap.confluence_count,
            detectors_seen: snap.detectors_seen.clone(),
            confirmation_span_secs: snap.confirmation_span_secs,
            opportunity_age_secs: snap.opportunity_age_secs,
            current_price: snap.current_price,
            move_before_detection_pct: snap.move_before_detection_pct,
            move_from_start_pct: snap.move_from_start_pct,
            episode_fragments: snap.episode_fragments,
            invalidations_absorbed: snap.invalidations_absorbed,
            raw_event_count: snap.raw_event_count,
            outcome,
            join_gap,
        });
    }

    records.sort_by(|a, b| {
        (
            &a.session_date,
            &a.symbol,
            a.score_timestamp,
            &a.window_id,
            &a.opportunity_id,
        )
            .cmp(&(
                &b.session_date,
                &b.symbol,
                b.score_timestamp,
                &b.window_id,
                &b.opportunity_id,
            ))
    });
    coverage.records_out = records.len();

    EvaluationSet {
        schema_version: EVALUATION_SCHEMA_VERSION,
        records,
        membership,
        coverage,
    }
}

/// Reconstructs opportunity windows from persisted score snapshots.
///
/// # Why this exists
///
/// The shadow log persists *scoring decisions*, not `Opportunity` records, and
/// the measurement capture persists *episodes*. So an offline join has no
/// opportunity windows to work from unless they are derived — and deriving
/// them from data already on disk is preferable to adding a second live write
/// path for something recoverable.
///
/// # What it can and cannot recover
///
/// Every snapshot carries `opportunityAgeSecs`, so `opened_at = timestamp −
/// age` recovers the open instant exactly. The *close* is not recoverable:
/// scoring stops at the last ranking window, which precedes the real close by
/// up to the inactivity boundary. The reconstructed window therefore ends at
/// the last observed ranking window and is marked open, so membership
/// under-assigns the tail rather than guessing it. Episodes opening after the
/// final ranking window are reported `OutsideEveryWindow`, not attached.
///
/// Fields other than identity and the window are placeholders and must not be
/// read: only `map_memberships` consumes this, and it reads symbol, session,
/// id, `opened_at`, `last_seen_at`, `closed_at` and `episode_fragments`.
pub fn reconstruct_opportunities_from_snapshots(
    snapshots: &[OpportunityScoreSnapshot],
) -> Vec<Opportunity> {
    use std::collections::BTreeMap;

    // key -> (opened_at, last_window, fragments)
    let mut seen: BTreeMap<String, (DateTime<Utc>, DateTime<Utc>, u32, String, String)> =
        BTreeMap::new();
    for snap in snapshots {
        let opened = snap.timestamp - chrono::Duration::seconds(snap.opportunity_age_secs.max(0));
        let entry = seen.entry(snap.opportunity_id.clone()).or_insert((
            opened,
            snap.timestamp,
            snap.episode_fragments,
            snap.symbol.clone(),
            snap.session_date.clone(),
        ));
        // The earliest derived open and the latest window seen, so out-of-order
        // input cannot shrink the window.
        if opened < entry.0 {
            entry.0 = opened;
        }
        if snap.timestamp > entry.1 {
            entry.1 = snap.timestamp;
        }
        entry.2 = entry.2.max(snap.episode_fragments);
    }

    seen.into_iter()
        .map(|(key, (opened, last_window, fragments, symbol, session_date))| {
            let sequence = key
                .rsplit(':')
                .next()
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(1);
            Opportunity {
                schema_version: crate::opportunity::OPPORTUNITY_SCHEMA_VERSION,
                id: crate::opportunity::OpportunityId {
                    symbol: symbol.clone(),
                    session_date: session_date.clone(),
                    sequence,
                },
                symbol,
                session_date,
                first_seen_at: opened,
                opened_at: opened,
                last_seen_at: last_window,
                opening_price: 0.0,
                latest_price: 0.0,
                first_detector: crate::signals::Strategy::IgnitionDetector,
                detectors_seen: std::collections::BTreeMap::new(),
                raw_event_count: 0,
                detector_transitions: 0,
                episode_fragments: fragments,
                invalidations_absorbed: 0,
                move_before_detection_pct: None,
                move_from_start_pct: None,
                max_move_pct: None,
                min_move_pct: None,
                observed_high: 0.0,
                observed_low: 0.0,
                latest_context: None,
                detection_context: None,
                detection_context_emitted: false,
                // Deliberately open: the real close is not recoverable, and
                // claiming one would silently over-assign the tail.
                closed_at: None,
                close_reason: None,
                last_relevant_at: None,
                last_evidence_kind: None,
                opened_phase: None,
            }
        })
        .collect()
}

/// Picks the outcome anchor for one score instant.
///
/// Prefers the first member episode anchored at or after the score, because a
/// score should be judged by what happened after it. Falls back to the most
/// recent earlier anchor, flagged, rather than dropping the record silently.
/// Ties break on episode id so the choice is deterministic.
fn select_anchor(
    member_ids: &[String],
    episodes: &[OpportunityEpisode],
    score_at: DateTime<Utc>,
) -> Option<JoinedOutcome> {
    let mut forward: Option<(&OpportunityEpisode, i64)> = None;
    let mut backward: Option<(&OpportunityEpisode, i64)> = None;

    for ep in episodes {
        let key = ep.id.as_key();
        if !member_ids.iter().any(|m| *m == key) {
            continue;
        }
        let Some(outcome) = ep.outcome.as_ref() else { continue };
        let offset = (outcome.signal_at - score_at).num_seconds();
        if offset >= 0 {
            let better = match forward {
                None => true,
                Some((cur, cur_off)) => {
                    (offset, key.as_str()) < (cur_off, cur.id.as_key().as_str())
                }
            };
            if better {
                forward = Some((ep, offset));
            }
        } else {
            let better = match backward {
                None => true,
                Some((cur, cur_off)) => {
                    // Closest earlier anchor: the largest (least negative).
                    (-offset, key.as_str()) < (-cur_off, cur.id.as_key().as_str())
                }
            };
            if better {
                backward = Some((ep, offset));
            }
        }
    }

    let (ep, offset, selection) = match (forward, backward) {
        (Some((ep, off)), _) => (ep, off, AnchorSelection::NextAtOrAfter),
        (None, Some((ep, off))) => (ep, off, AnchorSelection::LatestBefore),
        (None, None) => return None,
    };
    let outcome = ep.outcome.as_ref()?;

    let mut censored_horizons: Vec<i64> = outcome
        .returns
        .iter()
        .filter(|r| r.outcome.is_censored())
        .map(|r| r.horizon_secs)
        .collect();
    censored_horizons.sort_unstable();

    let mut censor_reasons: Vec<CensorReason> = Vec::new();
    let mut note = |o: &Observation<f64>| {
        if let Observation::Censored(reason) = o {
            if !censor_reasons.contains(reason) {
                censor_reasons.push(*reason);
            }
        }
    };
    for r in &outcome.returns {
        note(&r.outcome);
    }
    if let Observation::Censored(reason) = &outcome.excursion {
        if !censor_reasons.contains(reason) {
            censor_reasons.push(*reason);
        }
    }
    for t in &outcome.time_to_target {
        if let Observation::Censored(reason) = &t.outcome {
            if !censor_reasons.contains(reason) {
                censor_reasons.push(*reason);
            }
        }
    }

    Some(JoinedOutcome {
        episode_id: ep.id.as_key(),
        anchor_rule_version: ANCHOR_RULE_VERSION.to_string(),
        anchor_selection: selection,
        anchor_offset_secs: offset,
        signal_at: outcome.signal_at,
        signal_price: outcome.signal_price,
        returns: outcome.returns.clone(),
        excursion: outcome.excursion,
        time_to_target: outcome.time_to_target.clone(),
        observed_span_secs: outcome.observed_span_secs,
        observation_count: outcome.observation_count,
        fully_observed: censored_horizons.is_empty()
            && outcome.returns.len() == HORIZON_SECS.len(),
        censored_horizons,
        censor_reasons,
    })
}

#[cfg(test)]
#[path = "evaluation_tests.rs"]
mod tests;
