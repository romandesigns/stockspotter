//! Correction 1 tests, plus the full research-chain fixture the correction
//! batch asks for:
//!
//! ```text
//! market events -> detector episodes -> one collapsed Opportunity
//!   -> ranking windows -> future price path -> episode outcomes
//!   -> offline opportunity/outcome join
//! ```
//!
//! Built from the **real** `EpisodeTracker`, the real
//! `OpportunityIntelligence`, and the real `evaluate_horizons` — not from
//! hand-written stand-ins — so the chain the tests prove is the chain that
//! runs.

use super::*;

use chrono::{Duration, TimeZone};

use crate::episode::EpisodeTracker;
use crate::horizon::{evaluate_horizons, PricePoint, HORIZON_SECS};
use crate::membership::MEMBERSHIP_RULES_VERSION;
use crate::opportunity::{OiConfig, OpportunityIntelligence};
use market_data::events::{IgnitionEventKind, ScanEvent};

fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_789_344_000 + secs, 0).unwrap()
}

fn confirmed(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ScanEvent::IgnitionEvent {
        symbol: symbol.into(),
        timestamp: t,
        price,
        kind: IgnitionEventKind::FollowThroughConfirmed,
    }
}

fn rejected(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ScanEvent::IgnitionEvent {
        symbol: symbol.into(),
        timestamp: t,
        price,
        kind: IgnitionEventKind::FollowThroughRejected,
    }
}

fn momentum(symbol: &str, t: DateTime<Utc>, overall: f64) -> ScanEvent {
    ScanEvent::MomentumUpdate {
        symbol: symbol.into(),
        timestamp: t,
        volume_confirmation: 0.70,
        structure: 0.60,
        ma_slope: 0.42,
        wick_rejection: 0.80,
        overall,
        qualifies: overall >= 0.60,
    }
}

/// The whole chain, assembled once.
struct Chain {
    snapshots: Vec<OpportunityScoreSnapshot>,
    opportunities: Vec<Opportunity>,
    episodes: Vec<OpportunityEpisode>,
    /// The forward path outcomes were measured from.
    path: Vec<PricePoint>,
}

/// One symbol, one continuing move, fragmented into three episodes by two
/// invalidations, scored across several ranking windows, then measured against
/// a real forward path.
///
/// `path_secs` bounds how far the forward path extends, so the censored case
/// is reachable by construction rather than by hoping.
fn build_chain(path_secs: i64) -> Chain {
    let mut oi = OpportunityIntelligence::new(OiConfig::default());
    let mut tracker = EpisodeTracker::new();
    let mut snapshots = Vec::new();

    // --- market events -----------------------------------------------------
    // Momentum first so the momentum block is present and EarlyQuality's core
    // is satisfied; then confirm / invalidate / confirm / invalidate / confirm,
    // which is three episodes and one opportunity.
    let mut events: Vec<(ScanEvent, DateTime<Utc>)> = Vec::new();
    events.push((momentum("AAA", at(0), 0.55), at(0)));
    for (i, t) in [0i64, 40, 80, 120, 160].iter().enumerate() {
        let price = 10.0 + (i as f64) * 0.25;
        if i % 2 == 0 {
            events.push((confirmed("AAA", at(*t), price), at(*t)));
        } else {
            events.push((rejected("AAA", at(*t), price), at(*t)));
        }
    }
    // Keep it alive across several 30s ranking windows.
    for k in 1..=6i64 {
        let t = at(160 + k * 35);
        events.push((momentum("AAA", t, 0.55), t));
        events.push((confirmed("AAA", t, 11.0 + k as f64 * 0.1), t));
    }

    // Episodes closed by invalidation are returned by `observe` at the moment
    // they close -- `finish` only yields what is still open. Collecting just
    // the latter is how the first draft of this fixture saw one episode where
    // the tracker had actually produced three.
    let mut episodes: Vec<OpportunityEpisode> = Vec::new();
    for (event, received_at) in &events {
        oi.observe(event, *received_at);
        episodes.extend(tracker.observe(event, *received_at));
        if let Some(rows) = oi.rank(*received_at) {
            snapshots.extend(rows);
        }
    }

    let end = events.last().unwrap().1 + Duration::seconds(1);
    let mut opportunities = oi.finish(end);
    opportunities.sort_by(|a, b| a.id.as_key().cmp(&b.id.as_key()));
    episodes.extend(tracker.finish(end));
    episodes.sort_by(|a, b| a.id.as_key().cmp(&b.id.as_key()));

    // --- future price path -------------------------------------------------
    // A rising path, sampled every 30s so it is gap-free under MAX_GAP_SECS.
    let base = at(160);
    let path: Vec<PricePoint> = (0..=(path_secs / 30))
        .map(|i| (base + Duration::seconds(i * 30), 11.0 + i as f64 * 0.06))
        .collect();

    // --- episode outcomes --------------------------------------------------
    for ep in &mut episodes {
        ep.outcome = Some(evaluate_horizons(
            ep.opening_price,
            ep.opened_at,
            &path,
            None,
        ));
    }

    Chain { snapshots, opportunities, episodes, path }
}

// ---------------------------------------------------------------------------

/// The chain itself: fragmentation collapses, and the join reaches outcomes.
#[test]
fn q01_the_full_research_chain_joins_end_to_end() {
    let chain = build_chain(2400);

    assert_eq!(chain.opportunities.len(), 1, "one collapsed opportunity");
    assert!(chain.episodes.len() >= 3, "several fragmented episodes, got {}", chain.episodes.len());
    assert!(chain.snapshots.len() >= 3, "several ranking windows");

    let set = build_evaluation(&chain.snapshots, &chain.opportunities, &chain.episodes);

    assert_eq!(set.records.len(), chain.snapshots.len(), "one record per scoring window");
    assert_eq!(set.coverage.snapshots_in, chain.snapshots.len());
    assert!(set.coverage.joined > 0, "the join must actually reach outcomes");

    // Membership resolved the fragments onto the single opportunity.
    let op_key = chain.opportunities[0].id.as_key();
    assert!(
        set.membership.members_of(&op_key).len() >= 3,
        "all fragments belong to the one opportunity, got {:?}",
        set.membership.members_of(&op_key)
    );
    for r in &set.records {
        assert_eq!(r.opportunity_id, op_key);
        assert_eq!(r.membership_rules_version, MEMBERSHIP_RULES_VERSION);
    }
}

/// Episode fragmentation must not duplicate the opportunity, and must not
/// multiply its rank slot.
#[test]
fn q02_fragmentation_neither_duplicates_the_opportunity_nor_its_rank_slot() {
    let chain = build_chain(2400);
    let set = build_evaluation(&chain.snapshots, &chain.opportunities, &chain.episodes);

    // One opportunity id across every record, despite 3+ member episodes.
    let distinct: std::collections::BTreeSet<&str> =
        set.records.iter().map(|r| r.opportunity_id.as_str()).collect();
    assert_eq!(distinct.len(), 1, "fragments must not become separate opportunities");
    assert!(set.records[0].member_episode_ids.len() >= 3);

    // Absorption is asserted on the LAST window, not the first. The first
    // window is taken before the invalidations happen, so its count is
    // correctly 0 -- an earlier draft of this test asserted otherwise and was
    // wrong about causality, not about the engine. The counts must also never
    // decrease, which is the stronger statement.
    let last = set.records.last().unwrap();
    assert!(last.invalidations_absorbed >= 2, "the invalidations were absorbed by the end");
    assert!(last.episode_fragments >= 3, "and counted as fragments");
    assert_eq!(set.records[0].invalidations_absorbed, 0, "none had happened yet at the first window");

    let mut prev_absorbed = 0;
    let mut prev_fragments = 0;
    for r in &set.records {
        assert!(r.invalidations_absorbed >= prev_absorbed, "absorption must not go backwards");
        assert!(r.episode_fragments >= prev_fragments, "fragments must not go backwards");
        prev_absorbed = r.invalidations_absorbed;
        prev_fragments = r.episode_fragments;
    }

    // One rank slot per window, not one per fragment.
    for r in &set.records {
        if let Some(rank) = r.early_quality_rank {
            assert!(rank >= 1 && rank <= r.early_cohort_size);
            assert_eq!(r.early_cohort_size, 1, "a single symbol is a cohort of one");
        }
    }
    // And each window appears exactly once.
    let mut windows: Vec<&str> = set.records.iter().map(|r| r.window_id.as_str()).collect();
    let before = windows.len();
    windows.sort_unstable();
    windows.dedup();
    assert_eq!(windows.len(), before, "a window must not be emitted twice");
}

/// Every snapshot's attached outcome is anchored explicitly, and the offset
/// between the score instant and the measurement anchor is on the record.
#[test]
fn q03_each_snapshot_joins_to_a_stated_forward_anchor() {
    let chain = build_chain(2400);
    let set = build_evaluation(&chain.snapshots, &chain.opportunities, &chain.episodes);

    let mut seen_offset = false;
    for r in &set.records {
        let Some(o) = &r.outcome else { continue };
        seen_offset = true;
        assert_eq!(o.anchor_rule_version, ANCHOR_RULE_VERSION);
        assert!(
            o.member_of(&r.member_episode_ids),
            "the anchor must be a resolved member of this opportunity"
        );
        // The offset is exactly what it claims to be.
        assert_eq!(o.anchor_offset_secs, (o.signal_at - r.score_timestamp).num_seconds());
        match o.anchor_selection {
            AnchorSelection::NextAtOrAfter => assert!(o.anchor_offset_secs >= 0),
            AnchorSelection::LatestBefore => assert!(o.anchor_offset_secs < 0),
        }
        // The horizon grid is carried whole.
        assert_eq!(o.returns.len(), HORIZON_SECS.len());
    }
    assert!(seen_offset, "at least one record must carry an outcome");
    assert_eq!(
        set.coverage.joined,
        set.coverage.joined_forward + set.coverage.joined_backward
    );
}

/// Censored observations stay censored, and say why.
#[test]
fn q04_censored_observations_remain_censored() {
    // A path that stops well short of the 1800s horizon.
    let chain = build_chain(300);
    let set = build_evaluation(&chain.snapshots, &chain.opportunities, &chain.episodes);

    let with_outcome: Vec<&OpportunityEvaluationRecord> =
        set.records.iter().filter(|r| r.outcome.is_some()).collect();
    assert!(!with_outcome.is_empty());

    let o = with_outcome[0].outcome.as_ref().unwrap();
    assert!(
        !o.censored_horizons.is_empty(),
        "a 300s path cannot resolve a 1800s horizon"
    );
    assert!(o.censored_horizons.contains(&1800));
    assert!(!o.fully_observed, "and the record must not claim full observation");
    assert!(!o.censor_reasons.is_empty(), "the reason is carried, not just the fact");

    // Nothing censored was silently turned into a number.
    for ret in &o.returns {
        if o.censored_horizons.contains(&ret.horizon_secs) {
            assert!(ret.outcome.is_censored(), "a censored horizon must stay censored");
        }
    }

    // The long path resolves what the short one could not -- so the censoring
    // above is a property of coverage, not a bug in the join.
    let long = build_chain(2400);
    let long_set = build_evaluation(&long.snapshots, &long.opportunities, &long.episodes);
    assert!(
        long_set.coverage.fully_observed_outcomes > 0,
        "with enough forward data, outcomes resolve"
    );
}

/// Future information never influences the historical score.
///
/// The scores are produced with no forward path in existence, then the same
/// snapshots are joined against two *different* futures. The scoring half of
/// every record must be byte-identical across both.
#[test]
fn q05_future_information_never_influences_the_historical_score() {
    let base = build_chain(2400);

    // Same events, same scores -- a wildly different future.
    let mut crashed = build_chain(2400);
    let falling: Vec<PricePoint> = base
        .path
        .iter()
        .enumerate()
        .map(|(i, (t, _))| (*t, 11.0 - i as f64 * 0.08))
        .collect();
    for ep in &mut crashed.episodes {
        ep.outcome = Some(evaluate_horizons(
            ep.opening_price,
            ep.opened_at,
            &falling,
            None,
        ));
    }

    // The snapshots themselves are identical -- they were produced before any
    // of this existed.
    assert_eq!(
        serde_json::to_string(&base.snapshots).unwrap(),
        serde_json::to_string(&crashed.snapshots).unwrap()
    );

    let a = build_evaluation(&base.snapshots, &base.opportunities, &base.episodes);
    let b = build_evaluation(&crashed.snapshots, &crashed.opportunities, &crashed.episodes);

    assert_eq!(a.records.len(), b.records.len());
    for (ra, rb) in a.records.iter().zip(&b.records) {
        assert_eq!(ra.early_quality, rb.early_quality, "score must not move with the future");
        assert_eq!(ra.continuation, rb.continuation);
        assert_eq!(ra.early_quality_rank, rb.early_quality_rank);
        assert_eq!(ra.continuation_rank, rb.continuation_rank);
        assert_eq!(ra.regime, rb.regime);
        assert_eq!(ra.shadow_state, rb.shadow_state);
        assert_eq!(ra.feature_coverage, rb.feature_coverage);
    }
    // Membership is causal too, so it cannot have moved either.
    assert_eq!(
        serde_json::to_string(&a.membership).unwrap(),
        serde_json::to_string(&b.membership).unwrap()
    );
    // But the outcomes genuinely differ, or this test would prove nothing.
    let mfe_a = a.records.iter().filter_map(|r| r.outcome.as_ref()).next().unwrap();
    let mfe_b = b.records.iter().filter_map(|r| r.outcome.as_ref()).next().unwrap();
    assert_ne!(mfe_a.excursion, mfe_b.excursion, "the two futures must differ");
}

/// The evaluation output is consumable without reconstructing hidden
/// assumptions.
#[test]
fn q06_the_record_is_self_describing() {
    let chain = build_chain(2400);
    let set = build_evaluation(&chain.snapshots, &chain.opportunities, &chain.episodes);
    let r = &set.records[0];

    // Round-trips. Split deliberately: everything the *scoring* half of the
    // record contains must survive exactly, while the measured outcome floats
    // are compared within tolerance.
    //
    // That is not laxity, it is a real property of the dependency. `serde_json`
    // without its `float_roundtrip` feature -- which this workspace does not
    // enable -- writes an f64 correctly but can parse it back one ULP off
    // (reproduced: 10.909090909090903 -> 10.909090909090905). Asserting exact
    // equality here would be asserting a dependency behaviour we do not have.
    // See the correction report; enabling that feature changes workspace-wide
    // parsing and is not this batch's to authorise.
    let json = serde_json::to_string(r).unwrap();
    let parsed: OpportunityEvaluationRecord = serde_json::from_str(&json).unwrap();

    let mut a = parsed.clone();
    let mut b = r.clone();
    let (oa, ob) = (a.outcome.take(), b.outcome.take());
    assert_eq!(a, b, "the scoring half of the record round-trips exactly");
    match (oa, ob) {
        (Some(oa), Some(ob)) => {
            assert_eq!(oa.episode_id, ob.episode_id);
            assert_eq!(oa.anchor_selection, ob.anchor_selection);
            assert_eq!(oa.anchor_offset_secs, ob.anchor_offset_secs);
            assert_eq!(oa.censored_horizons, ob.censored_horizons);
            assert_eq!(oa.censor_reasons, ob.censor_reasons);
            assert_eq!(oa.fully_observed, ob.fully_observed);
            assert_eq!(oa.returns.len(), ob.returns.len());
            for (x, y) in oa.returns.iter().zip(&ob.returns) {
                assert_eq!(x.horizon_secs, y.horizon_secs);
                match (x.outcome, y.outcome) {
                    (Observation::Observed(p), Observation::Observed(q)) => {
                        assert!((p - q).abs() < 1e-9, "{p} vs {q}")
                    }
                    (Observation::Censored(p), Observation::Censored(q)) => assert_eq!(p, q),
                    _ => panic!("censoring must not change across a round trip"),
                }
            }
        }
        (None, None) => {}
        _ => panic!("outcome presence must not change across a round trip"),
    }

    // Every version needed to interpret it travels with it.
    assert_eq!(r.schema_version, EVALUATION_SCHEMA_VERSION);
    assert!(!r.versions.config_fingerprint.is_empty());
    assert!(!r.versions.score_policy.is_empty(), "the comparability policy is named");
    assert!(!r.versions.early_quality_model.is_empty());
    assert!(!r.versions.regime_classifier.is_empty());
    assert_eq!(r.membership_rules_version, MEMBERSHIP_RULES_VERSION);
    if let Some(o) = &r.outcome {
        assert!(!o.anchor_rule_version.is_empty(), "the anchor rule is named");
    }

    // Comparability is reconstructible from the record, not assumed.
    assert!((r.feature_coverage.early_quality - r.early_quality.coverage).abs() < 1e-12);
    assert_eq!(
        r.feature_coverage.early_quality_comparable,
        r.early_quality.value.is_some()
    );
    if let Some(v) = r.early_quality.value {
        let recomputed = r.early_quality.raw_weighted / r.early_quality.present_weight;
        assert!((v - recomputed).abs() < 1e-12);
    }

    // A gap is always explained, never merely absent.
    for rec in &set.records {
        assert!(
            rec.outcome.is_some() != rec.join_gap.is_some(),
            "exactly one of outcome / join_gap must be present"
        );
    }
}

/// Deterministic, and never derived from an identity assumption on ids.
#[test]
fn q07_the_join_is_deterministic_and_not_id_based() {
    let chain = build_chain(2400);
    let a = build_evaluation(&chain.snapshots, &chain.opportunities, &chain.episodes);

    let mut eps = chain.episodes.clone();
    eps.reverse();
    let mut snaps = chain.snapshots.clone();
    snaps.reverse();
    let b = build_evaluation(&snaps, &chain.opportunities, &eps);

    assert_eq!(
        serde_json::to_string(&a.records).unwrap(),
        serde_json::to_string(&b.records).unwrap(),
        "input order must not change the join"
    );

    // The opportunity is sequence 1; the episodes include sequences that have
    // no same-key opportunity. An identity join would have dropped them.
    let members = a.records[0].member_episode_ids.clone();
    assert!(members.len() >= 3);
    assert!(
        members.iter().any(|m| m.ends_with(":2")),
        "episode sequence 2 must be a member of opportunity sequence 1, got {members:?}"
    );
    assert!(a.records[0].opportunity_id.ends_with(":1"));
}

/// A snapshot whose opportunity was not supplied is reported, not dropped.
#[test]
fn q08_a_missing_opportunity_is_an_explicit_gap() {
    let chain = build_chain(2400);
    let set = build_evaluation(&chain.snapshots, &[], &chain.episodes);
    assert_eq!(set.records.len(), chain.snapshots.len(), "nothing is dropped");
    assert!(set.coverage.gap_opportunity_not_supplied > 0);
    for r in &set.records {
        assert_eq!(r.join_gap, Some(JoinGap::OpportunityNotSupplied));
        assert!(r.outcome.is_none());
        assert!(r.member_episode_ids.is_empty());
    }
}

/// Member episodes with no settled outcome are a stated gap, not a zero.
#[test]
fn q09_an_unsettled_outcome_is_a_stated_gap() {
    let chain = build_chain(2400);
    let mut bare = chain.episodes.clone();
    for ep in &mut bare {
        ep.outcome = None;
    }
    let set = build_evaluation(&chain.snapshots, &chain.opportunities, &bare);
    assert!(set.coverage.gap_no_settled_outcome > 0);
    for r in &set.records {
        assert!(r.outcome.is_none());
        assert_eq!(r.join_gap, Some(JoinGap::NoSettledOutcome));
        assert!(
            !r.member_episode_ids.is_empty(),
            "membership still resolved -- only the outcome is missing"
        );
    }
}

impl JoinedOutcome {
    fn member_of(&self, members: &[String]) -> bool {
        members.iter().any(|m| *m == self.episode_id)
    }
}

/// Opportunity windows are recoverable from persisted snapshots alone, and the
/// unrecoverable part is marked rather than guessed.
#[test]
fn q10_opportunity_windows_reconstruct_from_snapshots_alone() {
    let chain = build_chain(2400);
    let rebuilt = reconstruct_opportunities_from_snapshots(&chain.snapshots);

    assert_eq!(rebuilt.len(), 1, "one opportunity, recovered");
    let real = &chain.opportunities[0];
    let got = &rebuilt[0];
    assert_eq!(got.id.as_key(), real.id.as_key(), "identity is exact");
    assert_eq!(got.opened_at, real.opened_at, "the open instant is exact");
    assert!(got.closed_at.is_none(), "the close is not recoverable and is not invented");

    // The join works from the reconstruction, which is what an offline run
    // actually has available.
    let set = build_evaluation(&chain.snapshots, &rebuilt, &chain.episodes);
    assert_eq!(set.records.len(), chain.snapshots.len());
    assert!(set.coverage.joined > 0, "outcomes still reachable from the reconstruction");
    assert!(
        set.membership.mappings[0].opportunity_open,
        "the window is flagged as bounded by the last ranking window"
    );

    // The documented cost of reconstruction, asserted rather than glossed:
    // scoring stops at the last ranking window, so a fragment opening after it
    // falls outside the recoverable window. It is reported, never attached to
    // a window that was not proven to contain it.
    let rebuilt_members = set.membership.members_of(&real.id.as_key()).len();
    let true_members = map_memberships(&chain.opportunities, &chain.episodes)
        .members_of(&real.id.as_key())
        .len();
    assert!(rebuilt_members >= 2, "most fragments still resolve, got {rebuilt_members}");
    assert!(
        rebuilt_members <= true_members,
        "reconstruction must never over-assign: {rebuilt_members} vs {true_members}"
    );
    if rebuilt_members < true_members {
        // Reported in one of the two explicit lists. Which one depends on the
        // shape: a fragment opening after the last ranking window is
        // `OutsideEveryWindow`, while one that opened inside it but closed
        // after it straddles the reconstructed boundary and is
        // `EpisodeOutlivesOpportunity`. Both are surfaced; neither is silent.
        let reported = set.membership.unassigned_episodes.len()
            + set.membership.ambiguous_episodes.len();
        assert!(reported > 0, "the shortfall must be reported, not silent");
        assert_eq!(
            reported,
            true_members - rebuilt_members,
            "every unresolved fragment is accounted for exactly once"
        );
    }
}
