//! Durable opportunity identity, and the schema-2 snapshot extension.
//!
//! `OpportunityId::sequence` used to come from a per-process `HashMap`
//! counter, so an id was unique within one tracker lifetime and nowhere else.
//! After a restart the map re-seeded empty, numbering restarted at 1, and ids
//! already issued earlier the same day were handed to brand-new opportunities.
//! Every test here pins some part of the replacement: a value derived purely
//! from the opening instant, which no restart can re-seed.

use super::*;
use chrono::TimeZone;

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

fn momentum_ev(symbol: &str, t: DateTime<Utc>) -> ScanEvent {
    ScanEvent::MomentumUpdate {
        symbol: symbol.into(),
        timestamp: t,
        volume_confirmation: 0.7,
        structure: 0.6,
        ma_slope: 0.4,
        wick_rejection: 0.8,
        overall: 0.7,
        qualifies: true,
    }
}

fn engine() -> OpportunityIntelligence {
    OpportunityIntelligence::new(OiConfig::default())
}

// --- identity ---------------------------------------------------------------

/// Two opportunities for one symbol inside one process get distinct ids, and
/// each is the one its own opening instant implies.
#[test]
fn id1_two_opportunities_one_symbol_one_process_are_distinct() {
    let mut oi = engine();
    oi.observe(&confirmed("AAA", at(0), 10.0), at(0));
    oi.observe(&confirmed("ZZZ", at(400), 1.0), at(400));
    oi.observe(&confirmed("AAA", at(500), 12.0), at(500));
    let op = oi.open_opportunities().find(|o| o.symbol == "AAA").unwrap();
    assert_eq!(op.id.sequence, OpportunityId::sequence_for(at(500)));
    assert_ne!(OpportunityId::sequence_for(at(0)), OpportunityId::sequence_for(at(500)));
}

/// Identity is fixed at open and never moves while the opportunity lives.
#[test]
fn id2_identity_is_stable_for_the_whole_lifetime() {
    let mut oi = engine();
    oi.observe(&confirmed("AAA", at(0), 10.0), at(0));
    let first = oi.open_opportunities().next().unwrap().id.as_key();
    for i in 1..60 {
        oi.observe(&confirmed("AAA", at(i), 10.0 + i as f64 * 0.01), at(i));
        oi.observe(&momentum_ev("AAA", at(i)), at(i));
    }
    let last = oi.open_opportunities().next().unwrap().id.as_key();
    assert_eq!(first, last, "identity must not drift as events arrive");
}

/// THE ADVERSARIAL TEST. Reproduces the pattern found in the 2026-09-17
/// artifact, where a tracker was reconstructed mid-session (324 symbols
/// re-issued at 13:30:24 alone) and the same id came back attached to a new
/// opportunity with a fresh `opened_at`.
#[test]
fn id3_tracker_reconstruction_cannot_reissue_an_id() {
    // Lifetime 1: three opportunities for one symbol, so the old counter
    // would have climbed to 3.
    let mut first = engine();
    let mut issued = Vec::new();
    for (i, t) in [at(0), at(400), at(800)].into_iter().enumerate() {
        first.observe(&confirmed("DCX", t, 10.0 + i as f64), t);
        issued.push(first.open_opportunities().find(|o| o.symbol == "DCX").unwrap().id.clone());
    }
    assert_eq!(issued.len(), 3);

    // Lifetime 2: a brand-new tracker, same symbol, same session date, later
    // the same day. Exactly the restart shape.
    let mut second = engine();
    second.observe(&confirmed("DCX", at(1_200), 14.0), at(1_200));
    let after = second.open_opportunities().find(|o| o.symbol == "DCX").unwrap().id.clone();

    assert_eq!(after.session_date, issued[0].session_date, "same session date");
    for prior in &issued {
        assert_ne!(
            after.as_key(),
            prior.as_key(),
            "a reconstructed tracker must never re-issue an id"
        );
    }
    // Not merely different: strictly later, because the derivation is monotone
    // in `opened_at`. That is what makes the guarantee general rather than an
    // accident of these particular timestamps.
    assert!(issued.iter().all(|p| after.sequence > p.sequence));
}

/// The collision proof in one line: strictly increasing in the opening
/// instant, so "opened later" implies "larger sequence", always.
#[test]
fn id4_sequence_is_strictly_increasing_in_the_opening_instant() {
    let mut prev = OpportunityId::sequence_for(at(0));
    for s in [1i64, 2, 59, 60, 3_599, 3_600, 43_200, 86_399] {
        let next = OpportunityId::sequence_for(at(s));
        assert!(next > prev, "sequence must increase at t={s}");
        prev = next;
    }
    // Millisecond resolution, so even a sub-second reopen separates.
    let base = at(10);
    assert!(
        OpportunityId::sequence_for(base + Duration::milliseconds(1))
            > OpportunityId::sequence_for(base)
    );
    // And it stays inside the day.
    assert!(OpportunityId::sequence_for(at(86_399)) < 86_400_000);
}

/// Deterministic and clock-free: same instant, same id, every time.
#[test]
fn id5_derivation_is_deterministic() {
    for s in [0i64, 7, 1_234, 86_399] {
        assert_eq!(OpportunityId::sequence_for(at(s)), OpportunityId::sequence_for(at(s)));
    }
    let mut a = engine();
    let mut b = engine();
    a.observe(&confirmed("AAA", at(42), 10.0), at(42));
    b.observe(&confirmed("AAA", at(42), 10.0), at(42));
    assert_eq!(
        a.open_opportunities().next().unwrap().id.as_key(),
        b.open_opportunities().next().unwrap().id.as_key(),
        "two trackers fed identical input must agree on identity"
    );
}

/// A session boundary changes the date component, so the same time-of-day on
/// two days cannot collide.
#[test]
fn id6_session_boundary_separates_identity() {
    let day2 = at(86_400);
    assert_eq!(
        OpportunityId::sequence_for(at(0)),
        OpportunityId::sequence_for(day2),
        "same seconds into the day"
    );
    let a = OpportunityId {
        symbol: "AAA".into(),
        session_date: at(0).date_naive().to_string(),
        sequence: OpportunityId::sequence_for(at(0)),
    };
    let b = OpportunityId {
        symbol: "AAA".into(),
        session_date: day2.date_naive().to_string(),
        sequence: OpportunityId::sequence_for(day2),
    };
    assert_ne!(a.as_key(), b.as_key(), "the date component separates them");
}

/// `as_key` keeps the historical three-part shape, so existing artifacts and
/// every existing parser keep working.
#[test]
fn id7_key_format_is_unchanged_and_legacy_keys_still_parse() {
    let id = OpportunityId {
        symbol: "AAA".into(),
        session_date: "2026-09-17".into(),
        sequence: OpportunityId::sequence_for(at(0)),
    };
    let key = id.as_key();
    let parts: Vec<&str> = key.split(COLON).collect();
    assert_eq!(parts.len(), 3, "still symbol:date:sequence");
    assert_eq!(parts[0], "AAA");
    assert_eq!(parts[1], "2026-09-17");
    assert!(parts[2].parse::<u32>().is_ok(), "sequence still parses as u32");

    // A legacy id from the counter era remains a valid, readable key.
    let legacy: Vec<&str> = "DCX:2026-09-17:1".split(COLON).collect();
    assert_eq!(legacy.len(), 3);
    assert_eq!(legacy[2].parse::<u32>().unwrap(), 1);
}

const COLON: char = ':';

/// Close-then-reopen produces a new identity, never a recycled one.
#[test]
fn id8_close_and_reopen_does_not_recycle() {
    let mut oi = engine();
    oi.observe(&confirmed("AAA", at(0), 10.0), at(0));
    let before = oi.open_opportunities().next().unwrap().id.as_key();
    // Idle past inactivity_secs so it closes, then reopen the same symbol.
    oi.observe(&confirmed("ZZZ", at(1_000), 1.0), at(1_000));
    oi.observe(&confirmed("AAA", at(1_001), 11.0), at(1_001));
    let after =
        oi.open_opportunities().find(|o| o.symbol == "AAA").unwrap().id.as_key();
    assert_ne!(before, after, "a reopened symbol must get a new identity");
}

/// No identity state survives in the tracker at all -- the map that used to
/// hold it is gone, so there is nothing to grow and nothing to prune.
#[test]
fn id9_identity_needs_no_tracker_state() {
    let mut oi = engine();
    for i in 0..200i64 {
        let sym = format!("S{i}");
        oi.observe(&confirmed(&sym, at(i), 10.0), at(i));
    }
    // Derivation is a pure function: it agrees with the tracker's own ids
    // without consulting the tracker.
    for op in oi.open_opportunities() {
        assert_eq!(op.id.sequence, OpportunityId::sequence_for(op.opened_at));
    }
}

// --- schema-2 snapshot extension --------------------------------------------

/// New fields come from live state, and `openedAt` agrees with the age the
/// same row reports -- which is what makes any future id re-issue visible.
#[test]
fn snap1_extension_fields_are_present_and_consistent() {
    let mut oi = engine();
    oi.observe(&confirmed("AAA", at(0), 10.0), at(0));
    oi.observe(&confirmed("AAA", at(30), 11.0), at(30));
    let snaps = oi.rank(at(60)).expect("ranking window is due");
    let s = snaps.first().expect("one ranked opportunity");
    assert_eq!(s.schema_version, 2, "schema 2 identifies the extended shape");
    assert_eq!(s.opened_at, Some(at(0)));
    assert_eq!(s.opening_price, Some(10.0));
    assert!(s.observed_high.is_some() && s.observed_low.is_some());
    assert!(s.observed_high.unwrap() >= s.observed_low.unwrap());
    assert_eq!(
        (s.timestamp - s.opened_at.unwrap()).num_seconds(),
        s.opportunity_age_secs,
        "openedAt and opportunityAgeSecs must describe the same instant"
    );
}

/// A version-1 artifact still deserializes, and absence reads as absent --
/// never as a real zero.
#[test]
fn snap2_legacy_snapshot_still_parses_and_absence_is_not_zero() {
    let mut oi = engine();
    oi.observe(&confirmed("AAA", at(0), 10.0), at(0));
    let snaps = oi.rank(at(60)).expect("ranking window is due");
    let mut v: serde_json::Value = serde_json::to_value(&snaps[0]).unwrap();
    let obj = v.as_object_mut().unwrap();
    for f in
        ["observedHigh", "observedLow", "maxMovePct", "minMovePct", "openingPrice", "openedAt"]
    {
        obj.remove(f);
    }
    obj.insert("schemaVersion".into(), serde_json::json!(1));
    let old: OpportunityScoreSnapshot =
        serde_json::from_value(v).expect("a version-1 row must still deserialize");
    assert_eq!(old.schema_version, 1);
    assert_eq!(old.observed_high, None, "absent");
    assert_eq!(old.opened_at, None);
    assert_ne!(old.observed_high, Some(0.0), "absence is distinguishable from zero");
}

/// Round-trip preserves every new value exactly, on the camelCase wire shape.
#[test]
fn snap3_round_trip_preserves_the_new_fields() {
    let mut oi = engine();
    oi.observe(&confirmed("AAA", at(0), 10.0), at(0));
    oi.observe(&confirmed("AAA", at(30), 12.5), at(30));
    let snaps = oi.rank(at(60)).expect("ranking window is due");
    let json = serde_json::to_string(&snaps[0]).unwrap();
    let back: OpportunityScoreSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(back.observed_high, snaps[0].observed_high);
    assert_eq!(back.observed_low, snaps[0].observed_low);
    assert_eq!(back.max_move_pct, snaps[0].max_move_pct);
    assert_eq!(back.min_move_pct, snaps[0].min_move_pct);
    assert_eq!(back.opening_price, snaps[0].opening_price);
    assert_eq!(back.opened_at, snaps[0].opened_at);
    assert!(json.contains("observedHigh"), "camelCase on the wire");
    assert!(json.contains("openedAt"));
}

// --- cost -------------------------------------------------------------------

/// Measured serialized cost of the schema-2 extension, and a bound on it.
///
/// Printed with `--nocapture` so the numbers in the report are measured rather
/// than estimated; asserted so the extension cannot quietly grow.
#[test]
fn snap4_extension_storage_cost_is_bounded_and_measured() {
    let mut oi = engine();
    for i in 0..40i64 {
        oi.observe(&confirmed(&format!("S{i:05}"), at(i), 10.0 + i as f64 * 0.1), at(i));
    }
    let snaps = oi.rank(at(60)).expect("ranking window is due");
    assert!(snaps.len() >= 40);

    let mut with_ext = 0usize;
    let mut without_ext = 0usize;
    for s in &snaps {
        let full = serde_json::to_string(s).unwrap();
        with_ext += full.len();
        let mut v: serde_json::Value = serde_json::from_str(&full).unwrap();
        for f in
            ["observedHigh", "observedLow", "maxMovePct", "minMovePct", "openingPrice", "openedAt"]
        {
            v.as_object_mut().unwrap().remove(f);
        }
        without_ext += serde_json::to_string(&v).unwrap().len();
    }
    let n = snaps.len();
    let delta = with_ext - without_ext;
    let per_record = delta as f64 / n as f64;
    println!(
        "schema-2 extension: {n} records, {without_ext} -> {with_ext} bytes,          +{delta} total, +{per_record:.1} B/record ({:.2}%)",
        100.0 * delta as f64 / without_ext as f64
    );
    assert!(
        per_record < 200.0,
        "six scalar fields must not cost more than 200 B/record, got {per_record:.1}"
    );
}
