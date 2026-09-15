//! Correction 2 tests — the eight membership invariants, each named after the
//! way an identity join would have gone wrong.

use super::*;

use chrono::TimeZone;
use std::collections::BTreeMap;

use crate::context::SignalContext;
use crate::episode::{EpisodeCloseReason, EpisodeId, TraderLinkage};
use crate::opportunity::{OpportunityCloseReason, OpportunityId, OPPORTUNITY_SCHEMA_VERSION};
use crate::signals::Strategy;

fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_789_344_000 + secs, 0).unwrap()
}

const SESSION: &str = "2026-09-14";

fn ctx(symbol: &str, t: DateTime<Utc>) -> SignalContext {
    SignalContext {
        schema_version: 1,
        symbol: symbol.into(),
        session_date: SESSION.into(),
        strategy: Strategy::IgnitionDetector,
        detected_at: t,
        captured_at: t,
        signal_price: 10.0,
        market: None,
        funnel: None,
        ignition: None,
        momentum: None,
        consolidation: None,
        halt: None,
        catalyst: None,
        pre_detection: None,
        episode_id: None,
    }
}

/// An opportunity with an explicit window. `closed` = `None` leaves it open, so
/// the `window_end == last_seen_at` path is reachable.
fn op(
    symbol: &str,
    sequence: u32,
    opened: DateTime<Utc>,
    last_seen: DateTime<Utc>,
    closed: Option<DateTime<Utc>>,
    fragments: u32,
) -> Opportunity {
    op_on(symbol, SESSION, sequence, opened, last_seen, closed, fragments)
}

fn op_on(
    symbol: &str,
    session: &str,
    sequence: u32,
    opened: DateTime<Utc>,
    last_seen: DateTime<Utc>,
    closed: Option<DateTime<Utc>>,
    fragments: u32,
) -> Opportunity {
    Opportunity {
        schema_version: OPPORTUNITY_SCHEMA_VERSION,
        id: OpportunityId {
            symbol: symbol.into(),
            session_date: session.into(),
            sequence,
        },
        symbol: symbol.into(),
        session_date: session.into(),
        first_seen_at: opened,
        opened_at: opened,
        last_seen_at: last_seen,
        opening_price: 10.0,
        latest_price: 10.0,
        first_detector: Strategy::IgnitionDetector,
        detectors_seen: BTreeMap::new(),
        raw_event_count: 1,
        detector_transitions: 0,
        episode_fragments: fragments,
        invalidations_absorbed: fragments.saturating_sub(1),
        move_before_detection_pct: None,
        move_from_start_pct: None,
        max_move_pct: None,
        min_move_pct: None,
        observed_high: 10.0,
        observed_low: 10.0,
        latest_context: None,
        detection_context: None,
        detection_context_emitted: false,
        closed_at: closed,
        close_reason: closed.map(|_| OpportunityCloseReason::Inactivity),
    }
}

fn ep(
    symbol: &str,
    sequence: u32,
    opened: DateTime<Utc>,
    closed: Option<DateTime<Utc>>,
) -> OpportunityEpisode {
    ep_on(symbol, SESSION, sequence, opened, closed)
}

fn ep_on(
    symbol: &str,
    session: &str,
    sequence: u32,
    opened: DateTime<Utc>,
    closed: Option<DateTime<Utc>>,
) -> OpportunityEpisode {
    OpportunityEpisode {
        schema_version: 1,
        id: EpisodeId {
            symbol: symbol.into(),
            session_date: session.into(),
            sequence,
        },
        opened_at: opened,
        opened_by: Strategy::IgnitionDetector,
        opening_price: 10.0,
        opening_context: ctx(symbol, opened),
        confirmations: Vec::new(),
        momentum_track: Vec::new(),
        observed_high: 10.0,
        observed_low: 10.0,
        last_observed_at: closed.unwrap_or(opened),
        last_price: 10.0,
        event_count: 1,
        closed_at: closed,
        close_reason: closed.map(|_| EpisodeCloseReason::Invalidated),
        trader: TraderLinkage::default(),
        research_rank: None,
        outcome: None,
    }
}

// ---------------------------------------------------------------------------

/// 1. One episode, one opportunity.
#[test]
fn n01_a_single_episode_resolves_to_its_opportunity() {
    let report = map_memberships(
        &[op("AAA", 1, at(0), at(100), Some(at(400)), 1)],
        &[ep("AAA", 1, at(10), Some(at(90)))],
    );
    assert_eq!(report.mappings.len(), 1);
    let m = &report.mappings[0];
    assert_eq!(m.member_episode_ids, vec!["AAA:2026-09-14:1".to_string()]);
    assert_eq!(m.status, MembershipStatus::Resolved);
    assert!(report.ambiguous_episodes.is_empty());
    assert!(report.unassigned_episodes.is_empty());
    assert_eq!(report.opportunity_of("AAA:2026-09-14:1"), Some("AAA:2026-09-14:1"));
}

/// 2. Three invalidation-fragmented episodes resolve to ONE opportunity.
///
/// The case the identity join would have got wrong: the opportunity is
/// sequence 1, the episodes are sequences 1, 2 and 3, and `AAA:…:2` as an
/// episode has no opportunity counterpart at all.
#[test]
fn n02_three_fragmented_episodes_resolve_to_one_opportunity() {
    let report = map_memberships(
        &[op("AAA", 1, at(0), at(240), Some(at(540)), 3)],
        &[
            ep("AAA", 1, at(0), Some(at(60))),
            ep("AAA", 2, at(80), Some(at(140))),
            ep("AAA", 3, at(160), Some(at(240))),
        ],
    );
    assert_eq!(report.mappings.len(), 1, "one opportunity, not three");
    let m = &report.mappings[0];
    assert_eq!(
        m.member_episode_ids,
        vec![
            "AAA:2026-09-14:1".to_string(),
            "AAA:2026-09-14:2".to_string(),
            "AAA:2026-09-14:3".to_string()
        ]
    );
    assert_eq!(m.status, MembershipStatus::Resolved);
    assert_eq!(m.episode_fragments_claimed, 3, "matches the opportunity's own count");
    assert_eq!(m.member_episode_ids.len(), m.episode_fragments_claimed as usize);

    // The identity assumption this replaces: episode sequence 2 and 3 exist
    // with no opportunity of the same key.
    assert_eq!(report.opportunity_of("AAA:2026-09-14:2"), Some("AAA:2026-09-14:1"));
    assert_eq!(report.opportunity_of("AAA:2026-09-14:3"), Some("AAA:2026-09-14:1"));
}

/// 3. Two distinct opportunities on one symbol and day stay separate.
#[test]
fn n03_two_opportunities_on_one_symbol_day_remain_separate() {
    let report = map_memberships(
        &[
            op("AAA", 1, at(0), at(100), Some(at(400)), 1),
            op("AAA", 2, at(500), at(600), Some(at(900)), 1),
        ],
        &[
            ep("AAA", 1, at(10), Some(at(90))),
            ep("AAA", 2, at(510), Some(at(590))),
        ],
    );
    assert_eq!(report.mappings.len(), 2);
    assert_eq!(report.members_of("AAA:2026-09-14:1"), ["AAA:2026-09-14:1".to_string()]);
    assert_eq!(report.members_of("AAA:2026-09-14:2"), ["AAA:2026-09-14:2".to_string()]);
    assert!(report.ambiguous_episodes.is_empty());
}

/// 4. An episode straddling a boundary is NOT silently assigned.
#[test]
fn n04_an_episode_spanning_a_boundary_is_not_silently_assigned() {
    // Opens inside opportunity 1's window, closes after that window ended.
    let report = map_memberships(
        &[op("AAA", 1, at(0), at(100), Some(at(400)), 1)],
        &[ep("AAA", 1, at(50), Some(at(500)))],
    );
    assert!(report.members_of("AAA:2026-09-14:1").is_empty(), "must not be claimed");
    assert_eq!(report.ambiguous_episodes.len(), 1);
    assert!(matches!(
        report.ambiguous_episodes[0].ambiguity,
        AmbiguityReason::EpisodeOutlivesOpportunity { .. }
    ));
    assert_eq!(report.mappings[0].status, MembershipStatus::PartiallyAmbiguous);
    assert_eq!(report.mappings[0].ambiguous_episode_ids, vec!["AAA:2026-09-14:1".to_string()]);

    // And the other boundary shape: an instant two windows both claim.
    let touching = map_memberships(
        &[
            op("AAA", 1, at(0), at(100), Some(at(400)), 1),
            op("AAA", 2, at(400), at(500), Some(at(800)), 1),
        ],
        &[ep("AAA", 7, at(400), Some(at(450)))],
    );
    assert_eq!(touching.ambiguous_episodes.len(), 1);
    match &touching.ambiguous_episodes[0].ambiguity {
        AmbiguityReason::MultipleCandidates { candidates } => {
            assert_eq!(candidates.len(), 2, "both windows claim the instant");
        }
        other => panic!("expected MultipleCandidates, got {other:?}"),
    }
    assert!(touching.members_of("AAA:2026-09-14:1").is_empty());
    assert!(touching.members_of("AAA:2026-09-14:2").is_empty());
}

/// 5. Different symbols can never join, even with identical timing and
/// identical sequence numbers.
#[test]
fn n05_different_symbols_can_never_join() {
    let report = map_memberships(
        &[op("AAA", 1, at(0), at(100), Some(at(400)), 1)],
        &[ep("BBB", 1, at(10), Some(at(90)))],
    );
    assert!(report.members_of("AAA:2026-09-14:1").is_empty());
    assert_eq!(report.unassigned_episodes.len(), 1);
    assert_eq!(
        report.unassigned_episodes[0].unassigned,
        UnassignedReason::NoOpportunityForSymbolSession
    );
    assert_eq!(report.mappings[0].status, MembershipStatus::NoEpisodes);
}

/// 6. Different sessions can never join.
#[test]
fn n06_different_sessions_can_never_join() {
    let report = map_memberships(
        &[op_on("AAA", "2026-09-14", 1, at(0), at(100), Some(at(400)), 1)],
        &[ep_on("AAA", "2026-09-15", 1, at(10), Some(at(90)))],
    );
    assert!(report.members_of("AAA:2026-09-14:1").is_empty());
    assert_eq!(report.unassigned_episodes.len(), 1);
    assert_eq!(
        report.unassigned_episodes[0].unassigned,
        UnassignedReason::NoOpportunityForSymbolSession
    );
}

/// 7. The mapping is deterministic — input order cannot change it.
#[test]
fn n07_mapping_is_deterministic() {
    let ops = vec![
        op("AAA", 1, at(0), at(100), Some(at(400)), 2),
        op("BBB", 1, at(0), at(100), Some(at(400)), 1),
        op("AAA", 2, at(500), at(600), Some(at(900)), 1),
    ];
    let eps = vec![
        ep("AAA", 1, at(0), Some(at(40))),
        ep("AAA", 2, at(60), Some(at(100))),
        ep("BBB", 1, at(10), Some(at(90))),
        ep("AAA", 3, at(510), Some(at(560))),
    ];

    let forward = map_memberships(&ops, &eps);

    let mut ops_rev = ops.clone();
    ops_rev.reverse();
    let mut eps_rev = eps.clone();
    eps_rev.reverse();
    let backward = map_memberships(&ops_rev, &eps_rev);

    assert_eq!(
        serde_json::to_string(&forward).unwrap(),
        serde_json::to_string(&backward).unwrap(),
        "input order must not change the mapping"
    );
    assert_eq!(forward.members_of("AAA:2026-09-14:1").len(), 2);
    assert_eq!(forward.members_of("AAA:2026-09-14:2").len(), 1);
    assert_eq!(forward.members_of("BBB:2026-09-14:1").len(), 1);
}

/// 8. Future outcome information is never used to establish membership.
///
/// The same two inputs are mapped twice: once with both episodes' outcomes
/// absent, once with sharply different outcomes attached. Membership must be
/// byte-identical, because an outcome is an observation about what happened
/// *after* the episode and cannot be allowed to decide what it belonged to.
#[test]
fn n08_future_outcomes_never_influence_membership() {
    use crate::horizon::{Excursion, HorizonOutcome, HorizonReturn, Observation};

    let ops = [op("AAA", 1, at(0), at(240), Some(at(540)), 2)];
    let bare = [
        ep("AAA", 1, at(0), Some(at(60))),
        ep("AAA", 2, at(80), Some(at(140))),
    ];

    let outcome = |mfe: f64| HorizonOutcome {
        schema_version: 1,
        signal_price: 10.0,
        signal_at: at(0),
        returns: vec![HorizonReturn {
            horizon_secs: 30,
            outcome: Observation::Observed(mfe),
        }],
        excursion: Observation::Observed(Excursion {
            mfe_pct: mfe,
            mae_pct: -1.0,
            seconds_to_mfe: 30,
            seconds_to_mae: 10,
            drawdown_before_mfe_pct: -1.0,
        }),
        time_to_target: Vec::new(),
        observed_span_secs: 1800,
        observation_count: 10,
    };

    let mut enriched = bare.clone();
    enriched[0].outcome = Some(outcome(25.0));
    enriched[1].outcome = Some(outcome(-8.0));

    let without = map_memberships(&ops, &bare);
    let with = map_memberships(&ops, &enriched);

    assert_eq!(
        serde_json::to_string(&without).unwrap(),
        serde_json::to_string(&with).unwrap(),
        "attaching outcomes must not change membership"
    );
    assert_eq!(without.members_of("AAA:2026-09-14:1").len(), 2);
}

/// An open opportunity bounds its window at `last_seen_at` and says so, rather
/// than claiming an unknown tail.
#[test]
fn n09_an_open_opportunity_under_assigns_rather_than_guessing() {
    let report = map_memberships(
        &[op("AAA", 1, at(0), at(100), None, 1)],
        &[
            ep("AAA", 1, at(50), Some(at(80))),
            // After last_seen_at: inside the true window or not, unknowable.
            ep("AAA", 2, at(200), Some(at(260))),
        ],
    );
    let m = &report.mappings[0];
    assert!(m.opportunity_open, "the window end is not a real close");
    assert_eq!(m.window_end, at(100));
    assert_eq!(m.member_episode_ids, vec!["AAA:2026-09-14:1".to_string()]);
    assert_eq!(report.unassigned_episodes.len(), 1, "the later one is reported, not claimed");
    assert_eq!(
        report.unassigned_episodes[0].unassigned,
        UnassignedReason::OutsideEveryWindow
    );
}

/// An episode that opened before its opportunity existed is not a member.
#[test]
fn n10_an_episode_predating_the_opportunity_is_not_a_member() {
    let report = map_memberships(
        &[op("AAA", 1, at(100), at(200), Some(at(500)), 1)],
        &[ep("AAA", 1, at(10), Some(at(50)))],
    );
    assert!(report.members_of("AAA:2026-09-14:1").is_empty());
    assert_eq!(report.unassigned_episodes.len(), 1);
    assert_eq!(
        report.unassigned_episodes[0].unassigned,
        UnassignedReason::OutsideEveryWindow
    );
}
