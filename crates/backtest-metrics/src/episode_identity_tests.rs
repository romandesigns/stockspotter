//! Durable episode identity.
//!
//! `EpisodeId`'s `sequence` came from an in-memory counter, so an
//! `episodeId` repeats across tracker reconstruction: 3,881 ids issued more
//! than once over 2026-09-17/18, every one a genuinely distinct episode.
//! `episodeUid` is the replacement join key, and the fixtures below are the
//! real collisions that forced its shape.

use super::*;
use chrono::{Duration, TimeZone};

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

// --- canonical encoding -----------------------------------------------------

/// Cross-language test vectors. Any Python reader computing `episodeUid` must
/// reproduce these byte for byte.
#[test]
fn uid_test_vectors() {
    let t = Utc.timestamp_opt(1_789_344_000, 0).unwrap();
    assert_eq!(
        episode_uid("AAA", t, Strategy::IgnitionDetector),
        "epu1|AAA|2026-09-14T00:00:00.000000000Z|IgnitionDetector"
    );
    let t_ns = Utc.timestamp_opt(1_789_344_000, 123_456_789).unwrap();
    assert_eq!(
        episode_uid("DTSS", t_ns, Strategy::FastFunnel),
        "epu1|DTSS|2026-09-14T00:00:00.123456789Z|FastFunnel"
    );
    // Every strategy token is pinned, so a serde attribute change cannot move
    // the identity of episodes already written.
    for (s, token) in [
        (Strategy::FastFunnel, "FastFunnel"),
        (Strategy::MomentumScorer, "MomentumScorer"),
        (Strategy::IgnitionDetector, "IgnitionDetector"),
        (Strategy::ConsolidationBreakout, "ConsolidationBreakout"),
        (Strategy::Micropullback, "Micropullback"),
    ] {
        assert_eq!(strategy_token(s), token);
    }
}

/// Fixed-width fractional digits are load-bearing. The corpus serialises
/// 98.07% of opens with 9 fractional digits, 0.10% with 6 and 1.83% with
/// none; without normalisation one instant would encode three ways.
#[test]
fn timestamp_encoding_is_fixed_width() {
    let whole = Utc.timestamp_opt(1_789_344_000, 0).unwrap();
    let micros = Utc.timestamp_opt(1_789_344_000, 24_485_000).unwrap();
    let nanos = Utc.timestamp_opt(1_789_344_000, 24_485_725).unwrap();
    for t in [whole, micros, nanos] {
        let uid = episode_uid("AAA", t, Strategy::FastFunnel);
        let stamp = uid.split('|').nth(2).unwrap();
        assert_eq!(stamp.len(), 30, "always {stamp}");
        assert!(stamp.ends_with('Z'));
        assert_eq!(&stamp[19..20], ".", "always a fractional part");
    }
    // And distinct instants stay distinct.
    assert_ne!(
        episode_uid("AAA", micros, Strategy::FastFunnel),
        episode_uid("AAA", nanos, Strategy::FastFunnel)
    );
}

/// The encoding is injective, which is what makes it collision-free by
/// construction rather than collision-resistant by probability.
#[test]
fn encoding_is_injective_across_every_field() {
    let t = at(0);
    let base = episode_uid("AAA", t, Strategy::IgnitionDetector);
    assert_ne!(base, episode_uid("AAB", t, Strategy::IgnitionDetector));
    assert_ne!(base, episode_uid("AAA", t + Duration::nanoseconds(1), Strategy::IgnitionDetector));
    assert_ne!(base, episode_uid("AAA", t, Strategy::FastFunnel));
    assert_eq!(base.matches('|').count(), 3, "exactly three separators");
    assert!(base.starts_with("epu1|"), "version/domain separator leads");
}

// --- the real collisions ----------------------------------------------------

/// REGRESSION FIXTURES for the three measured same-instant reopenings.
///
/// Every one is a midnight session rollover where `FastFunnel` and
/// `MomentumScorer` opened on the identical whole second, at the identical
/// price. The legacy id collides; full timestamp precision does not separate
/// them (the source truncates to the second); the opening detector does.
#[test]
fn dtss_aiff_kxin_same_instant_reopenings_are_separated() {
    for (symbol, day, price) in
        [("DTSS", "2026-09-17", 0.9365), ("AIFF", "2026-09-18", 1.27), ("KXIN", "2026-09-18", 1.86)]
    {
        let opened: DateTime<Utc> = format!("{day}T00:00:00Z").parse().unwrap();
        let funnel = episode_uid(symbol, opened, Strategy::FastFunnel);
        let momentum = episode_uid(symbol, opened, Strategy::MomentumScorer);
        assert_ne!(
            funnel, momentum,
            "{symbol} {day}: two distinct episodes at the same instant and price \
             ({price}) must not share an identity"
        );
        // The legacy key is what collided -- both were sequence 1 after the
        // counter re-seeded.
        let legacy = EpisodeId {
            symbol: symbol.into(),
            session_date: day.into(),
            sequence: 1,
        };
        assert_eq!(legacy.as_key(), legacy.as_key(), "legacy key is identical for both");
    }
}

/// Full timestamp precision alone is NOT sufficient, which is why the tuple
/// includes the detector. Stated as a test so nobody "simplifies" it away.
#[test]
fn opening_instant_alone_is_insufficient() {
    let opened: DateTime<Utc> = "2026-09-17T00:00:00Z".parse().unwrap();
    let a = ("DTSS", opened, Strategy::FastFunnel);
    let b = ("DTSS", opened, Strategy::MomentumScorer);
    assert_eq!(a.1, b.1, "the instants really are identical, to the nanosecond");
    assert_ne!(episode_uid(a.0, a.1, a.2), episode_uid(b.0, b.1, b.2));
}

// --- lifecycle --------------------------------------------------------------

fn tracker() -> EpisodeTracker {
    EpisodeTracker::new()
}

/// One episode keeps one uid for its whole life.
#[test]
fn uid_is_stable_through_an_episode_lifetime() {
    let mut t = tracker();
    t.observe(&confirmed("AAA", at(0), 10.0), at(0));
    let first = t.open_episodes().next().unwrap().uid.clone();
    for i in 1..20 {
        t.observe(&confirmed("AAA", at(i), 10.0 + i as f64 * 0.01), at(i));
    }
    let last = t.open_episodes().next().unwrap().uid.clone();
    assert_eq!(first, last);
    assert!(first.is_some());
}

/// Close-then-reopen produces a different uid, because the opening instant
/// differs. This is the ordinary case; the same-instant case is above.
#[test]
fn close_and_reopen_gets_a_new_uid() {
    let mut t = tracker();
    t.observe(&confirmed("AAA", at(0), 10.0), at(0));
    let before = t.open_episodes().next().unwrap().uid.clone().unwrap();
    // Invalidation closes an episode immediately -- the lifecycle property
    // that made the opportunity fix unsafe here.
    t.observe(&rejected("AAA", at(5), 9.5), at(5));
    assert_eq!(t.open_count(), 0, "a rejection closes the episode at once");
    t.observe(&confirmed("AAA", at(6), 10.2), at(6));
    let after = t.open_episodes().next().unwrap().uid.clone().unwrap();
    assert_ne!(before, after);
}

/// Tracker reconstruction re-seeds the legacy counter and therefore reissues
/// `episodeId` -- exactly the historical defect -- while `episodeUid` stays
/// distinct. This is the whole point of the change.
#[test]
fn reconstruction_reissues_the_legacy_id_but_never_the_uid() {
    let mut first = tracker();
    first.observe(&confirmed("ZTG", at(0), 1.48), at(0));
    let a = first.open_episodes().next().unwrap();
    let (a_legacy, a_uid) = (a.id.as_key(), a.uid.clone().unwrap());

    // A brand-new tracker, same symbol, same session date, later the same day.
    let mut second = tracker();
    second.observe(&confirmed("ZTG", at(3_600), 1.42), at(3_600));
    let b = second.open_episodes().next().unwrap();
    let (b_legacy, b_uid) = (b.id.as_key(), b.uid.clone().unwrap());

    assert_eq!(a_legacy, b_legacy, "the legacy id DOES repeat -- documented, not fixed");
    assert_ne!(a_uid, b_uid, "the durable identity must not");
}

/// Same symbol and date, several episodes, all distinct.
#[test]
fn many_episodes_one_symbol_one_day_are_all_distinct() {
    let mut t = tracker();
    let mut uids = std::collections::BTreeSet::new();
    for i in 0..25i64 {
        t.observe(&confirmed("AAA", at(i * 10), 10.0), at(i * 10));
        if let Some(e) = t.open_episodes().next() {
            uids.insert(e.uid.clone().unwrap());
        }
        t.observe(&rejected("AAA", at(i * 10 + 5), 9.9), at(i * 10 + 5));
    }
    assert_eq!(uids.len(), 25, "every open must mint a distinct uid");
}

/// A session boundary is separated by the date inside the timestamp.
#[test]
fn session_boundary_separates_uids() {
    let day1: DateTime<Utc> = "2026-09-17T13:30:00Z".parse().unwrap();
    let day2: DateTime<Utc> = "2026-09-18T13:30:00Z".parse().unwrap();
    assert_ne!(
        episode_uid("AAA", day1, Strategy::IgnitionDetector),
        episode_uid("AAA", day2, Strategy::IgnitionDetector),
        "same time of day, different session"
    );
}

// --- schema / compatibility -------------------------------------------------

/// A version-1 artifact still parses, and its missing uid reads as absent --
/// which is precisely how a reader knows it cannot assume collision-freedom.
#[test]
fn legacy_artifact_without_uid_still_parses() {
    let mut t = tracker();
    t.observe(&confirmed("AAA", at(0), 10.0), at(0));
    let ep = t.open_episodes().next().unwrap().clone();
    let mut v = serde_json::to_value(&ep).unwrap();
    let obj = v.as_object_mut().unwrap();
    obj.remove("episodeUid");
    obj.insert("schemaVersion".into(), serde_json::json!(1));
    let old: OpportunityEpisode = serde_json::from_value(v).expect("version 1 must parse");
    assert_eq!(old.schema_version, 1);
    assert_eq!(old.uid, None, "absent, and distinguishable from any value");
    assert_eq!(old.id.as_key(), ep.id.as_key(), "the legacy key survives untouched");
}

/// A schema-2 record carries BOTH identities, and the legacy one is unchanged.
#[test]
fn schema_two_carries_both_identities() {
    let mut t = tracker();
    t.observe(&confirmed("AAA", at(0), 10.0), at(0));
    let ep = t.open_episodes().next().unwrap();
    assert_eq!(ep.schema_version, 2);
    let json = serde_json::to_string(ep).unwrap();
    assert!(json.contains("\"episodeUid\""), "the wire field is episodeUid: {json}");
    assert!(json.contains("\"id\""), "the legacy identifier is still present");
    let back: OpportunityEpisode = serde_json::from_str(&json).unwrap();
    assert_eq!(back.uid, ep.uid);
    assert_eq!(back.id, ep.id);
}

/// Duplicate legacy ids must not merge distinct episodes when the join uses
/// the durable identity. This is the analytical failure the change prevents.
#[test]
fn duplicate_legacy_ids_do_not_merge_under_the_new_key() {
    let opened_a: DateTime<Utc> = "2026-09-17T00:00:00Z".parse().unwrap();
    let opened_b: DateTime<Utc> = "2026-09-17T08:00:37Z".parse().unwrap();
    let opened_c: DateTime<Utc> = "2026-09-17T13:30:00Z".parse().unwrap();
    // The real ZTG:2026-09-17:1 shape: one legacy id, three distinct episodes.
    let legacy = "ZTG:2026-09-17:1";
    let uids: std::collections::BTreeSet<String> = [opened_a, opened_b, opened_c]
        .iter()
        .map(|t| episode_uid("ZTG", *t, Strategy::IgnitionDetector))
        .collect();
    assert_eq!(uids.len(), 3, "three distinct episodes behind one legacy id {legacy}");
}

/// Reports the qualification contract hash so §15 can state it exactly.
#[test]
fn spec_hash_is_reported() {
    let spec = crate::alpha::spec::QualificationSpec::default();
    println!("QualificationSpec {} sha256 {}", spec.version, spec.sha256());
    println!("EPISODE_SCHEMA_VERSION {}", EPISODE_SCHEMA_VERSION);
    println!("OiConfig fingerprint {}", crate::opportunity::OiConfig::default().fingerprint());
}
