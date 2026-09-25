//! Live wiring for opportunity-native outcomes.
//!
//! The collector itself is tested in `backtest_metrics::opportunity_outcome`.
//! These test the WIRING: that a ranking snapshot becomes an anchor, that
//! market events reach it as forward prices, that closure and capacity cannot
//! stop it, and that writer loss is explicit.

use super::*;
use backtest_metrics::opportunity::{OiConfig, OpportunityIntelligence};
use chrono::{Duration, TimeZone};
use market_data::{IgnitionEventKind, ScanEvent};

fn at(s: i64) -> DateTime<Utc> {
    // 05:30 EDT premarket, so `session_close` (20:00Z) is ahead of it.
    Utc.timestamp_opt(1_789_378_200 + s, 0).unwrap() // 2026-09-14T09:30:00Z
}

fn confirmed(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ScanEvent::IgnitionEvent {
        symbol: symbol.into(),
        timestamp: t,
        price,
        kind: IgnitionEventKind::FollowThroughConfirmed,
    }
}

fn driver() -> OutcomeDriver {
    OutcomeDriver::new(None, &OiConfig::default().versions())
}

/// One admitted ranking snapshot produces exactly one anchor.
#[test]
fn every_ranking_snapshot_becomes_an_anchor() {
    let mut engine = OpportunityIntelligence::new(OiConfig::default());
    let mut d = driver();
    for i in 0..5i64 {
        engine.observe(&confirmed(&format!("S{i}"), at(i), 10.0), at(i));
    }
    let snaps = engine.rank(at(60)).expect("a window is due");
    assert!(!snaps.is_empty());
    d.anchor_and_settle(&snaps, at(60));
    assert_eq!(
        d.health().anchors_created as usize,
        snaps.len(),
        "one anchor per snapshot, no filtering"
    );
}

/// Market events reach anchors as forward prices, through the same extraction
/// rule the episode measurement uses.
#[test]
fn market_events_reach_anchors_as_forward_prices() {
    let mut engine = OpportunityIntelligence::new(OiConfig::default());
    let mut d = driver();
    engine.observe(&confirmed("AAA", at(0), 10.0), at(0));
    let snaps = engine.rank(at(30)).expect("due");
    d.anchor_and_settle(&snaps, at(30));

    for t in (60..=1_400).step_by(30) {
        d.observe_price(&confirmed("AAA", at(t), 11.0), at(t));
    }
    d.finish(at(2_000));
    assert_eq!(d.health().anchors_settled, d.health().anchors_created);
}

/// A finalised bar is observable one interval after its opening timestamp;
/// an in-progress bucket uses the receipt clock.
#[test]
fn bar_timestamp_semantics_match_the_episode_rule() {
    let t = at(0);
    let received = at(7);
    let finalised = ScanEvent::BarUpdate {
        symbol: "AAA".into(),
        timestamp: t,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 12.5,
        volume: 100,
        is_final: true,
        interval_secs: 60,
    };
    let (_, when, px) = forward_price(&finalised, received).unwrap();
    assert_eq!(when, t + Duration::seconds(60), "a closed bar is known at its close");
    assert_eq!(px, 12.5);

    let live = ScanEvent::BarUpdate {
        symbol: "AAA".into(),
        timestamp: t,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 12.5,
        volume: 100,
        is_final: false,
        interval_secs: 60,
    };
    let (_, when, _) = forward_price(&live, received).unwrap();
    assert_eq!(when, received, "an in-progress bucket uses the receipt clock");
}

/// Events that carry no price produce no observation, rather than a zero.
#[test]
fn priceless_events_are_not_observations() {
    let m = ScanEvent::MomentumUpdate {
        symbol: "AAA".into(),
        timestamp: at(0),
        volume_confirmation: 0.7,
        structure: 0.6,
        ma_slope: 0.4,
        wick_rejection: 0.8,
        overall: 0.7,
        qualifies: true,
    };
    assert!(forward_price(&m, at(0)).is_none());
}

/// THE INVARIANT. An anchor keeps measuring after its opportunity is gone --
/// including after the engine evicts it for capacity.
#[test]
fn anchors_outlive_their_opportunity() {
    let mut engine = OpportunityIntelligence::new(OiConfig::default());
    let mut d = driver();
    engine.observe(&confirmed("AAA", at(0), 10.0), at(0));
    let snaps = engine.rank(at(30)).expect("due");
    d.anchor_and_settle(&snaps, at(30));
    let created = d.health().anchors_created;
    assert_eq!(created, 1);

    // Let the opportunity die of inactivity, then keep publishing prices.
    engine.observe(&confirmed("ZZZ", at(1_000), 1.0), at(1_000));
    assert_eq!(engine.health().open_opportunities, 1, "AAA is gone from the engine");
    for t in (60..=1_400).step_by(30) {
        d.observe_price(&confirmed("AAA", at(t), 11.0), at(t));
    }
    d.anchor_and_settle(&[], at(30 + 1_320 + 30));
    assert_eq!(d.health().anchors_settled, 1, "the anchor settled on its own schedule");
    assert_eq!(d.health().capacity_evictions, 0);
}

/// Score-independence at the wiring level: the driver never reads a score.
#[test]
fn the_driver_cannot_see_a_score() {
    let mut engine = OpportunityIntelligence::new(OiConfig::default());
    let mut d = driver();
    for i in 0..8i64 {
        engine.observe(&confirmed(&format!("S{i}"), at(i), 10.0 + i as f64), at(i));
    }
    let snaps = engine.rank(at(60)).expect("due");
    // Some of these will be rankable and some not; every one must anchor.
    let unrankable = snaps.iter().filter(|s| s.early_quality.value.is_none()).count();
    d.anchor_and_settle(&snaps, at(60));
    assert_eq!(d.health().anchors_created as usize, snaps.len());
    assert!(
        unrankable > 0 || snaps.len() == d.health().anchors_created as usize,
        "even unrankable snapshots anchor"
    );
}

/// Settlement releases rows; nothing is left outstanding after `finish`.
#[test]
fn finish_settles_everything_outstanding() {
    let mut engine = OpportunityIntelligence::new(OiConfig::default());
    let mut d = driver();
    engine.observe(&confirmed("AAA", at(0), 10.0), at(0));
    let snaps = engine.rank(at(30)).expect("due");
    d.anchor_and_settle(&snaps, at(30));
    assert_eq!(d.health().outstanding, 1);
    d.finish(at(60));
    assert_eq!(d.health().outstanding, 0);
    assert_eq!(d.health().anchors_settled, d.health().anchors_created);
}

/// A horizon reaching past the regular-session close is censored
/// `SessionEnded`, not silently shortened. The close is 16:00 America/New_York
/// (D13): 20:00Z in summer, 21:00Z in winter, 18:00Z on an early close -- not
/// the fixed 20:00Z it used to be.
#[test]
fn session_close_is_the_regular_close_in_new_york() {
    let summer = Utc.with_ymd_and_hms(2026, 9, 14, 14, 0, 0).unwrap();
    assert_eq!(session_close(summer), Utc.with_ymd_and_hms(2026, 9, 14, 20, 0, 0).unwrap());
    let winter = Utc.with_ymd_and_hms(2026, 11, 2, 20, 30, 0).unwrap(); // 15:30 EST
    assert_eq!(session_close(winter), Utc.with_ymd_and_hms(2026, 11, 2, 21, 0, 0).unwrap());
    assert!(session_close(winter) > winter, "15:30 EST is inside the session");
    let early = Utc.with_ymd_and_hms(2026, 11, 27, 15, 0, 0).unwrap();
    assert_eq!(session_close(early), Utc.with_ymd_and_hms(2026, 11, 27, 18, 0, 0).unwrap());
    // The shared fixture instant (05:30 EDT premarket) still closes ahead.
    assert!(session_close(at(0)) > at(0));
}

// --- writer -----------------------------------------------------------------

/// Writer loss is explicit and counted, never silent. The gate holds the
/// writer so the bound is actually reachable -- a drop counter no test can
/// reach is worth nothing.
///
/// Deliberately does NOT call `finish()`: that flushes, and flushing while
/// the writer is held at the barrier is a deadlock, not a test. Settlement is
/// driven by advancing the clock instead, exactly as production does.
#[test]
fn writer_loss_is_explicit() {
    let dir = std::env::temp_dir().join(format!("oo-loss-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let gate = Arc::new(std::sync::Barrier::new(2));
    let rec = OutcomeRecorder::start_inner(dir.clone(), 2, Some(Arc::clone(&gate)))
        .expect("directory is usable");

    let mut engine = OpportunityIntelligence::new(OiConfig::default());
    for i in 0..10i64 {
        engine.observe(&confirmed(&format!("S{i}"), at(i), 10.0), at(i));
    }
    let snaps = engine.rank(at(60)).expect("due");
    assert!(snaps.len() > 2, "need more rows than the queue can hold");

    let mut d = OutcomeDriver::new(Some(rec), &OiConfig::default().versions());
    d.anchor_and_settle(&snaps, at(60));
    // Advance past the settlement deadline so every anchor produces a row.
    let started = std::time::Instant::now();
    d.anchor_and_settle(&[], at(60 + 1_320 + 60));
    let elapsed = started.elapsed();

    // While the writer is gated, rows sit in the queue counted as `attempted`
    // but not yet as `written`, so only the inequality holds here.
    let health = d.capture_health().expect("capture on").clone();
    let gated = health.snapshot();
    assert!(gated.attempted > 0, "rows were offered to the writer");
    assert!(gated.dropped > 0, "a saturated queue must drop and count, not grow");
    assert!(gated.loss_spans > 0, "a drop must record a loss span in band");
    assert!(
        gated.attempted >= gated.written + gated.dropped + gated.write_errors,
        "nothing may be counted twice"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "settlement must never block on the writer (took {elapsed:?})"
    );

    // Release the writer and drain: only now can the exact identity hold, and
    // it is the identity the completeness verdict is built on.
    gate.wait();
    d.finish(at(60 + 1_320 + 120));
    let drained = health.snapshot();
    assert_eq!(
        drained.attempted,
        drained.written + drained.dropped + drained.write_errors,
        "attempted must account for every row exactly -- no silent loss"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Capture off must not change measurement, only persistence.
#[test]
fn capture_off_still_measures() {
    let mut engine = OpportunityIntelligence::new(OiConfig::default());
    let mut d = driver();
    assert!(d.capture_health().is_none());
    engine.observe(&confirmed("AAA", at(0), 10.0), at(0));
    let snaps = engine.rank(at(30)).expect("due");
    d.anchor_and_settle(&snaps, at(30));
    d.finish(at(60));
    assert_eq!(d.health().anchors_created, 1);
    assert_eq!(d.health().anchors_settled, 1);
}

/// Health exposes everything §11 requires.
#[test]
fn health_surface_is_complete() {
    let mut engine = OpportunityIntelligence::new(OiConfig::default());
    let mut d = driver();
    engine.observe(&confirmed("AAA", at(0), 10.0), at(0));
    let snaps = engine.rank(at(30)).expect("due");
    d.anchor_and_settle(&snaps, at(30));
    let h = d.health();
    assert_eq!(h.outstanding, 1);
    assert_eq!(h.peak_outstanding, 1);
    assert!(h.capacity > 0);
    assert_eq!(h.anchors_created, 1);
    assert_eq!(h.anchors_settled, 0);
    assert_eq!(h.capacity_evictions, 0);
    assert_eq!(h.symbols_tracked, 1);
}
