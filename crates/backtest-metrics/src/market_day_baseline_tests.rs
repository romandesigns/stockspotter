//! Market-day scoping of `FeatureCache` (measurement-correctness contract of
//! 2026-09-25, D3 + D7a).
//!
//! Numbered comments (`brief #N`) map each test to the P2 assignment's
//! required D3 list; the rest cover the contract's additional cases. The
//! JAGX/BTTC/PFSA fixtures use values read from the preserved 09-21..09-24
//! evidence; every value that is NOT from the evidence is marked `synthetic`
//! at the point of use.

use super::*;
use crate::episode::EpisodeTracker;
use crate::opportunity::{OiConfig, OpportunityIntelligence};
use chrono::{Duration, TimeZone};
use chrono_tz::America::New_York;

// --- helpers -----------------------------------------------------------------

/// A New York wall-clock instant, DST-aware.
fn et(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
    New_York
        .with_ymd_and_hms(y, mo, d, h, mi, s)
        .single()
        .unwrap()
        .with_timezone(&Utc)
}

fn z(rfc3339: &str) -> DateTime<Utc> {
    rfc3339.parse().unwrap()
}

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn bar(symbol: &str, t: DateTime<Utc>, close: f64) -> ScanEvent {
    ScanEvent::BarUpdate {
        is_final: true,
        symbol: symbol.into(),
        timestamp: t,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1_000,
        interval_secs: 60,
    }
}

fn ignition(symbol: &str, t: DateTime<Utc>, price: f64, kind: IgnitionEventKind) -> ScanEvent {
    ScanEvent::IgnitionEvent {
        symbol: symbol.into(),
        timestamp: t,
        price,
        kind,
    }
}

fn confirmed(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ignition(symbol, t, price, IgnitionEventKind::FollowThroughConfirmed)
}

fn momentum(symbol: &str, t: DateTime<Utc>, qualifies: bool) -> ScanEvent {
    ScanEvent::MomentumUpdate {
        symbol: symbol.into(),
        timestamp: t,
        volume_confirmation: 0.8,
        structure: 0.7,
        ma_slope: 0.6,
        wick_rejection: 0.5,
        overall: if qualifies { 0.9 } else { 0.1 },
        qualifies,
    }
}

fn funnel(
    symbol: &str,
    t: DateTime<Utc>,
    price: f64,
    session_volume: u64,
    gap_pct: f64,
) -> ScanEvent {
    ScanEvent::FunnelSignal {
        symbol: symbol.into(),
        timestamp: t,
        price,
        gap_pct,
        session_volume,
        price_ok: true,
        float_ok: true,
        rel_vol_ok: true,
        gap_ok: true,
        passed: true,
    }
}

fn pre(cache: &FeatureCache, symbol: &str, at: DateTime<Utc>, price: f64) -> PreDetectionContext {
    cache
        .snapshot(symbol, Strategy::IgnitionDetector, at, at, price)
        .pre_detection
        .expect("same-day state must yield a pre-detection context")
}

/// A cache that has been running since the previous afternoon, as the live
/// process normally is: its first event is long before any 04:00 ET open
/// used below, so `baselineTruncated` is false unless a test says otherwise.
fn running_cache() -> FeatureCache {
    let mut cache = FeatureCache::new();
    cache.observe(&bar("ZZZZ", et(2026, 9, 18, 15, 0, 0), 1.0));
    cache
}

fn assert_close(actual: Option<f64>, expected: f64, what: &str) {
    let a = actual.unwrap_or_else(|| panic!("{what}: expected {expected}, got None"));
    assert!(
        (a - expected).abs() < 1e-9,
        "{what}: expected {expected}, got {a}"
    );
}

// --- brief #1-#3: a new market day resets the baseline -----------------------

#[test]
fn brief_1_new_market_day_resets_first_observed_at() {
    let mut cache = running_cache();
    cache.observe(&bar("AAA", et(2026, 9, 21, 9, 29, 0), 2.77));
    cache.observe(&bar("AAA", et(2026, 9, 22, 4, 4, 0), 2.67));
    let p = pre(&cache, "AAA", et(2026, 9, 22, 9, 50, 0), 2.845);
    assert_eq!(p.first_observed_at, Some(et(2026, 9, 22, 4, 4, 0)));
    assert_eq!(p.market_day, Some(day(2026, 9, 22)));
}

#[test]
fn brief_2_new_market_day_resets_first_observed_price() {
    let mut cache = running_cache();
    cache.observe(&bar("AAA", et(2026, 9, 21, 9, 29, 0), 2.77));
    cache.observe(&bar("AAA", et(2026, 9, 22, 4, 4, 0), 2.67));
    let p = pre(&cache, "AAA", et(2026, 9, 22, 9, 50, 0), 2.845);
    assert_eq!(p.first_observed_price, Some(2.67));
    assert_close(
        p.move_before_detection_pct,
        (2.845 - 2.67) / 2.67 * 100.0,
        "move",
    );
}

#[test]
fn brief_3_new_market_day_resets_session_low_observed() {
    let mut cache = running_cache();
    cache.observe(&bar("AAA", et(2026, 9, 21, 9, 29, 0), 2.77));
    cache.observe(&bar("AAA", et(2026, 9, 21, 11, 0, 0), 2.50));
    cache.observe(&bar("AAA", et(2026, 9, 22, 4, 4, 0), 2.67));
    let p = pre(&cache, "AAA", et(2026, 9, 22, 9, 50, 0), 2.845);
    assert_eq!(
        p.session_low_observed,
        Some(2.67),
        "2.50 was yesterday's low"
    );
}

// --- brief #4-#7: within-day semantics ---------------------------------------

#[test]
fn brief_4_premarket_first_observation_is_retained_into_regular_hours() {
    let mut cache = running_cache();
    cache.observe(&bar("AAA", et(2026, 9, 22, 4, 4, 0), 2.67));
    // 09:30 is a session boundary, not a market-day boundary.
    cache.observe(&bar("AAA", et(2026, 9, 22, 9, 30, 0), 2.80));
    cache.observe(&bar("AAA", et(2026, 9, 22, 10, 15, 0), 2.90));
    let p = pre(&cache, "AAA", et(2026, 9, 22, 10, 15, 0), 2.90);
    assert_eq!(p.first_observed_at, Some(et(2026, 9, 22, 4, 4, 0)));
    assert_eq!(p.first_observed_price, Some(2.67));
}

#[test]
fn brief_5_session_low_can_decrease_causally_during_the_day() {
    let mut cache = running_cache();
    cache.observe(&bar("AAA", et(2026, 9, 22, 4, 4, 0), 2.70));
    let at_first = pre(&cache, "AAA", et(2026, 9, 22, 4, 5, 0), 2.70);
    cache.observe(&bar("AAA", et(2026, 9, 22, 6, 0, 0), 2.60));
    let at_second = pre(&cache, "AAA", et(2026, 9, 22, 6, 0, 0), 2.60);
    assert_eq!(
        at_first.session_low_observed,
        Some(2.70),
        "cut before the lower print"
    );
    assert_eq!(
        at_second.session_low_observed,
        Some(2.60),
        "the new low is today's"
    );
}

#[test]
fn brief_6_session_low_never_increases() {
    let mut cache = running_cache();
    cache.observe(&bar("AAA", et(2026, 9, 22, 4, 4, 0), 2.60));
    for (m, px) in [(10, 2.9), (20, 3.4), (30, 4.1)] {
        cache.observe(&bar("AAA", et(2026, 9, 22, 5, m, 0), px));
        let p = pre(&cache, "AAA", et(2026, 9, 22, 5, m, 0), px);
        assert_eq!(p.session_low_observed, Some(2.60));
    }
}

#[test]
fn brief_7_prior_day_low_does_not_contaminate_next_day() {
    let mut cache = running_cache();
    cache.observe(&bar("AAA", et(2026, 9, 21, 15, 0, 0), 2.50));
    cache.observe(&bar("AAA", et(2026, 9, 22, 4, 4, 0), 2.67));
    cache.observe(&bar("AAA", et(2026, 9, 22, 9, 0, 0), 2.75));
    let p = pre(&cache, "AAA", et(2026, 9, 22, 9, 0, 0), 2.75);
    let low = p.session_low_observed.unwrap();
    assert!(low >= 2.67, "day-2 low {low} is below every day-2 price");
}

// --- brief #8-#11: coverage and process lifetime ------------------------------

#[test]
fn brief_8_a_reconnect_gap_does_not_reset_a_valid_same_day_baseline() {
    // A reconnect (the 600 s idle timeout, or a stream error) is invisible to
    // the cache except as a gap in events: nothing between 04:04 and 04:45.
    let mut cache = running_cache();
    cache.observe(&bar("AAA", et(2026, 9, 22, 4, 4, 0), 2.67));
    cache.observe(&bar("AAA", et(2026, 9, 22, 4, 45, 0), 2.70));
    let p = pre(&cache, "AAA", et(2026, 9, 22, 4, 45, 0), 2.70);
    assert_eq!(p.first_observed_at, Some(et(2026, 9, 22, 4, 4, 0)));
    assert_eq!(p.baseline_truncated, Some(false));
}

#[test]
fn brief_9_process_restart_mid_day_truncates_the_baseline_and_says_so() {
    // A deploy is a restart: a brand-new cache whose first event is 11:00 ET.
    let mut cache = FeatureCache::new();
    cache.observe(&bar("AAA", et(2026, 9, 22, 11, 0, 0), 3.10));
    cache.observe(&bar("AAA", et(2026, 9, 22, 11, 30, 0), 3.40));
    let p = pre(&cache, "AAA", et(2026, 9, 22, 11, 30, 0), 3.40);
    assert_eq!(
        p.first_observed_at,
        Some(et(2026, 9, 22, 11, 0, 0)),
        "first post-restart event"
    );
    assert_eq!(p.first_observed_price, Some(3.10));
    assert_eq!(p.observation_started_at, Some(et(2026, 9, 22, 11, 0, 0)));
    assert_eq!(
        p.baseline_truncated,
        Some(true),
        "the 04:00-11:00 part was never seen"
    );

    // The next market day is complete: the cache was listening at 04:00.
    cache.observe(&bar("AAA", et(2026, 9, 23, 4, 1, 0), 3.00));
    let next = pre(&cache, "AAA", et(2026, 9, 23, 4, 1, 0), 3.00);
    assert_eq!(next.baseline_truncated, Some(false));
    assert_eq!(next.first_observed_price, Some(3.00));
}

#[test]
fn brief_10_symbol_first_appearing_at_1100_gets_an_1100_baseline() {
    let mut cache = running_cache();
    cache.observe(&bar("OTHER", et(2026, 9, 22, 4, 0, 30), 5.0));
    cache.observe(&bar("NEW", et(2026, 9, 22, 11, 0, 0), 1.25));
    let p = pre(&cache, "NEW", et(2026, 9, 22, 11, 5, 0), 1.40);
    assert_eq!(p.first_observed_at, Some(et(2026, 9, 22, 11, 0, 0)));
    assert_eq!(p.first_observed_price, Some(1.25));
    // Not truncated: the process was listening; the symbol simply entered
    // coverage at 11:00. `firstObservedAt` is what says so.
    assert_eq!(p.baseline_truncated, Some(false));
}

#[test]
fn brief_11_disappear_and_reappear_the_same_day_preserves_the_baseline() {
    let mut cache = running_cache();
    cache.observe(&bar("AAA", et(2026, 9, 22, 4, 10, 0), 3.00));
    // Untracked at ~05:00, re-promoted at 14:00: no events in between.
    cache.observe(&bar("AAA", et(2026, 9, 22, 14, 0, 0), 3.50));
    let p = pre(&cache, "AAA", et(2026, 9, 22, 14, 0, 0), 3.50);
    assert_eq!(p.first_observed_at, Some(et(2026, 9, 22, 4, 10, 0)));
    assert_eq!(p.first_observed_price, Some(3.00));

    // Reappearing on a later market day gets a fresh baseline.
    cache.observe(&bar("AAA", et(2026, 9, 23, 13, 0, 0), 4.00));
    let next = pre(&cache, "AAA", et(2026, 9, 23, 13, 0, 0), 4.00);
    assert_eq!(next.first_observed_at, Some(et(2026, 9, 23, 13, 0, 0)));
    assert_eq!(next.session_low_observed, Some(4.00));
}

// --- brief #12-#14: boundaries -------------------------------------------------

#[test]
fn brief_12_dst_summer_boundary_resets_at_0800z() {
    // EDT: 04:00 ET = 08:00Z.
    let mut cache = running_cache();
    cache.observe(&bar("AAA", z("2026-09-22T07:59:50Z"), 9.0)); // 03:59:50 ET, market day 09-21
    cache.observe(&bar("AAA", z("2026-09-22T08:00:10Z"), 2.0)); // 04:00:10 ET, market day 09-22
    let p = pre(&cache, "AAA", z("2026-09-22T08:00:10Z"), 2.0);
    assert_eq!(p.first_observed_price, Some(2.0));
    assert_eq!(p.session_low_observed, Some(2.0));

    // Spring-forward week: Friday 2026-03-06 (EST) -> Monday 2026-03-09 (EDT).
    // Monday's 04:00 is 08:00Z, an hour earlier in UTC than Friday's.
    // (A fresh cache: `running_cache` starts in September.)
    let mut cache = FeatureCache::new();
    cache.observe(&bar("BBB", et(2026, 3, 6, 15, 0, 0), 7.0));
    cache.observe(&bar("BBB", z("2026-03-09T08:00:10Z"), 6.0));
    let p = pre(&cache, "BBB", z("2026-03-09T08:00:10Z"), 6.0);
    assert_eq!(p.market_day, Some(day(2026, 3, 9)));
    assert_eq!(p.first_observed_price, Some(6.0));
}

#[test]
fn brief_13_dst_winter_boundary_resets_at_0900z_not_0800z() {
    // EST: 04:00 ET = 09:00Z. 08:30Z is still the previous market day.
    // (Fresh caches: `running_cache` starts in September.)
    let mut cache = FeatureCache::new();
    cache.observe(&bar("AAA", et(2026, 1, 14, 15, 0, 0), 9.0));
    cache.observe(&bar("AAA", z("2026-01-15T08:30:00Z"), 8.0)); // 03:30 EST
    let still_old = pre(&cache, "AAA", z("2026-01-15T08:30:00Z"), 8.0);
    assert_eq!(still_old.market_day, Some(day(2026, 1, 14)));
    assert_eq!(
        still_old.first_observed_price,
        Some(9.0),
        "no reset before 09:00Z in winter"
    );
    cache.observe(&bar("AAA", z("2026-01-15T09:00:10Z"), 2.0));
    let p = pre(&cache, "AAA", z("2026-01-15T09:00:10Z"), 2.0);
    assert_eq!(p.market_day, Some(day(2026, 1, 15)));
    assert_eq!(p.first_observed_price, Some(2.0));

    // Fall-back week: Friday 2026-10-30 (EDT) -> Monday 2026-11-02 (EST).
    // Monday 08:30Z would have been 04:30 EDT; it is 03:30 EST, not yet the
    // new market day (it belongs to Sunday's, which only resets once more at
    // 09:00Z -- there are no Sunday events in practice).
    let mut cache = FeatureCache::new();
    cache.observe(&bar("BBB", et(2026, 10, 30, 15, 0, 0), 7.0));
    cache.observe(&bar("BBB", z("2026-11-02T09:00:10Z"), 6.0));
    let p = pre(&cache, "BBB", z("2026-11-02T09:00:10Z"), 6.0);
    assert_eq!(p.market_day, Some(day(2026, 11, 2)));
    assert_eq!(p.first_observed_price, Some(6.0));
    assert_eq!(
        market_data::trading_session::market_day_open(day(2026, 11, 2)),
        z("2026-11-02T09:00:00Z")
    );
    assert_eq!(
        market_data::trading_session::market_day_open(day(2026, 3, 9)),
        z("2026-03-09T08:00:00Z")
    );
}

#[test]
fn brief_14_crossing_utc_midnight_after_hours_is_not_a_new_market_day() {
    let mut cache = running_cache();
    cache.observe(&bar("AAA", et(2026, 9, 22, 16, 30, 0), 3.0));
    // 20:30 ET = 00:30Z on 09-23: new UTC date, same market day.
    let late = z("2026-09-23T00:30:00Z");
    cache.observe(&bar("AAA", late, 3.3));
    let snap = cache.snapshot("AAA", Strategy::IgnitionDetector, late, late, 3.3);
    let p = snap.pre_detection.unwrap();
    assert_eq!(p.market_day, Some(day(2026, 9, 22)));
    assert_eq!(p.first_observed_at, Some(et(2026, 9, 22, 16, 30, 0)));
    assert_eq!(p.first_observed_price, Some(3.0));
    // UTC `sessionDate` is deliberately unchanged -- it is the identity and
    // file partition key, and a separate quantity from the market day.
    assert_eq!(snap.session_date, "2026-09-23");
}

// --- brief #15 + regression witnesses: JAGX -----------------------------------

/// JAGX over 09-21, 09-22 and 09-24, as the deployed process saw it.
///
/// From the preserved evidence:
/// * 09-21 first observation `2026-09-21T13:29:32.112819945Z` @ 2.77 and a
///   session low of 2.50 (OI rows of 09-22 and 09-24 carry both); day-1
///   `dailyBar.c` 2.67 and last trade 2.706 at 22:38:29Z (discovery tape).
/// * 09-22 first trade 04:45:56 ET @ 2.70 (discovery tape `latestTrade`);
///   confirmation `2026-09-22T13:50:57.220233887Z` @ 2.845 (OI row, which
///   reported `moveBeforeDetectionPct` 2.7076 against the 09-21 2.77).
/// * 09-24 ranking row `2026-09-24T08:02:06.242157608Z` @ 8.29 with
///   `price1mBefore` 8.0, reporting +201.08% against the same 09-21 2.77.
///
/// From the assignment brief, not re-read from the tape in bounded greps:
/// the 09-22 first receipt at 04:04 ET @ 2.67 and the 09:05:45 ET ignition
/// candidate. Synthetic: the candidate's price (2.75) and the 09-24 first
/// bar's exact time (08:01:00Z, the minute before the row that reports
/// `price1mBefore` 8.0).
fn jagx_events() -> Vec<ScanEvent> {
    vec![
        // day 1: 2026-09-21
        confirmed("JAGX", z("2026-09-21T13:29:32.112819945Z"), 2.77),
        bar("JAGX", z("2026-09-21T15:10:00Z"), 2.50),
        bar("JAGX", z("2026-09-21T19:59:00Z"), 2.67),
        bar("JAGX", z("2026-09-21T22:38:00Z"), 2.706),
        // day 2: 2026-09-22
        bar("JAGX", et(2026, 9, 22, 4, 4, 0), 2.67),
        bar("JAGX", z("2026-09-22T08:45:56.080346208Z"), 2.70),
        ignition(
            "JAGX",
            et(2026, 9, 22, 9, 5, 45),
            2.75,
            IgnitionEventKind::CandidateOpened,
        ),
        confirmed("JAGX", z("2026-09-22T13:50:57.220233887Z"), 2.845),
        // day 3: 2026-09-24
        bar("JAGX", z("2026-09-24T08:01:00Z"), 8.0),
        confirmed("JAGX", z("2026-09-24T08:02:06.242157608Z"), 8.29),
    ]
}

#[test]
fn brief_15_jagx_sequence_measures_each_day_against_that_day() {
    let events = jagx_events();
    let mut cache = running_cache();
    // Fold through the 09-22 confirmation.
    for e in &events[..8] {
        cache.observe(e);
    }
    let confirm_at = z("2026-09-22T13:50:57.220233887Z");
    let snap = cache.snapshot(
        "JAGX",
        Strategy::IgnitionDetector,
        confirm_at,
        confirm_at,
        2.845,
    );
    let p = snap.pre_detection.unwrap();
    assert_eq!(p.market_day, Some(day(2026, 9, 22)));
    assert_eq!(
        p.first_observed_at,
        Some(z("2026-09-22T08:04:00Z")),
        "04:04 ET, 09-22"
    );
    assert_eq!(p.first_observed_price, Some(2.67));
    assert_eq!(
        p.session_low_observed,
        Some(2.67),
        "a 09-22 low, not 09-21's 2.50"
    );
    assert_close(
        p.move_before_detection_pct,
        (2.845 - 2.67) / 2.67 * 100.0,
        "moveBeforeDetectionPct from 09-22 data only (old contract: 2.7076 vs 2.77)",
    );
    let ig = snap.ignition.expect("the confirmation itself is fresh");
    assert_eq!(
        ig.candidates_opened, 1,
        "the deployed row said 19, counted since 09-21"
    );
    assert_eq!(ig.confirmations, 1);

    // Day 3: must not read +201% from the 09-21 price.
    for e in &events[8..] {
        cache.observe(e);
    }
    let d3 = z("2026-09-24T08:02:06.242157608Z");
    let p3 = pre(&cache, "JAGX", d3, 8.29);
    assert_eq!(p3.market_day, Some(day(2026, 9, 24)));
    assert_eq!(p3.first_observed_price, Some(8.0));
    assert_eq!(p3.session_low_observed, Some(8.0));
    let moved = p3.move_before_detection_pct.unwrap();
    assert!(
        (moved - 3.625).abs() < 1e-9,
        "09-24 move is +3.6%, not +201%: got {moved}"
    );
}

#[test]
fn jagx_through_opportunity_intelligence_opens_with_same_day_prior_move() {
    let mut oi = OpportunityIntelligence::new(OiConfig::default());
    for e in &jagx_events()[..8] {
        let at = event_symbol_and_time(e).unwrap().1;
        oi.observe(e, at);
    }
    let op = oi
        .open_opportunities()
        .find(|o| o.symbol == "JAGX" && o.session_date == "2026-09-22")
        .expect("the 09-22 confirmation opens an opportunity");
    assert_close(
        op.move_before_detection_pct,
        (2.845 - 2.67) / 2.67 * 100.0,
        "OI prior move",
    );
    let p = op
        .detection_context
        .as_ref()
        .unwrap()
        .pre_detection
        .unwrap();
    assert_eq!(p.first_observed_price, Some(2.67));
    assert_eq!(p.market_day, Some(day(2026, 9, 22)));
    assert_eq!(
        OiConfig::default().versions().baseline_policy.as_deref(),
        Some(BASELINE_POLICY)
    );
}

#[test]
fn the_episode_trackers_own_cache_resets_too() {
    // `EpisodeTracker` holds a second, independent `FeatureCache`
    // (ws-server's measurement driver creates it once at start); it must
    // carry the same fix through the shared type.
    let mut tracker = EpisodeTracker::new();
    let mut opened = Vec::new();
    for e in &jagx_events() {
        let at = event_symbol_and_time(e).unwrap().1;
        opened.extend(tracker.observe(e, at));
    }
    opened.extend(tracker.open_episodes().cloned());
    let day2 = opened
        .iter()
        .find(|ep| ep.id.session_date == "2026-09-22")
        .expect("a 09-22 episode");
    let p = day2.opening_context.pre_detection.unwrap();
    assert_eq!(p.first_observed_price, Some(2.67));
    assert_eq!(p.session_low_observed, Some(2.67));
    assert_close(
        p.move_before_detection_pct,
        (2.845 - 2.67) / 2.67 * 100.0,
        "episode prior move",
    );

    let day3 = opened
        .iter()
        .find(|ep| ep.id.session_date == "2026-09-24")
        .expect("a 09-24 episode");
    let p3 = day3.opening_context.pre_detection.unwrap();
    assert_eq!(p3.first_observed_price, Some(8.0));
    assert!(p3.move_before_detection_pct.unwrap() < 10.0, "not +201%");
}

// --- additional contract cases -------------------------------------------------

#[test]
fn a_weekend_resets_exactly_once_and_so_does_a_holiday() {
    let mut cache = running_cache();
    cache.observe(&bar("AAA", et(2026, 9, 18, 15, 0, 0), 5.0)); // Friday
    cache.observe(&bar("AAA", et(2026, 9, 21, 4, 10, 0), 6.0)); // Monday: reset
    cache.observe(&bar("AAA", et(2026, 9, 21, 4, 20, 0), 5.5)); // same day: no reset
    let p = pre(&cache, "AAA", et(2026, 9, 21, 4, 20, 0), 5.5);
    assert_eq!(
        p.first_observed_price,
        Some(6.0),
        "Monday's first print, reset once only"
    );
    assert_eq!(p.session_low_observed, Some(5.5), "Friday's 5.0 is gone");

    // Thanksgiving 2026-11-26: Wednesday -> Friday resets once.
    cache.observe(&bar("AAA", et(2026, 11, 25, 15, 0, 0), 4.0));
    cache.observe(&bar("AAA", et(2026, 11, 27, 9, 0, 0), 4.4));
    cache.observe(&bar("AAA", et(2026, 11, 27, 9, 30, 0), 4.2));
    let p = pre(&cache, "AAA", et(2026, 11, 27, 9, 30, 0), 4.2);
    assert_eq!(p.first_observed_price, Some(4.4));
    assert_eq!(p.session_low_observed, Some(4.2));
}

#[test]
fn a_late_earlier_day_event_is_ignored_for_day_scoped_state() {
    let mut cache = running_cache();
    cache.observe(&bar("AAA", et(2026, 9, 22, 4, 4, 0), 2.67));
    // A late UpdatedBar correction for yesterday arrives after the roll.
    cache.observe(&bar("AAA", et(2026, 9, 21, 19, 59, 0), 2.40));
    let p = pre(&cache, "AAA", et(2026, 9, 22, 4, 5, 0), 2.70);
    assert_eq!(p.first_observed_price, Some(2.67), "not reseeded");
    assert_eq!(p.session_low_observed, Some(2.67), "not lowered");
    assert_eq!(cache.last_price("AAA"), Some(2.67));
    assert_eq!(
        cache.last_price_at("AAA", et(2026, 9, 22, 4, 5, 0)),
        Some(2.67)
    );
}

#[test]
fn a_price_less_opening_event_on_a_new_day_has_no_price_to_open_at() {
    let t = et(2026, 9, 22, 4, 0, 30);
    let mut cache = running_cache();
    cache.observe(&bar("AAA", et(2026, 9, 21, 15, 59, 0), 3.0));
    cache.observe(&momentum("AAA", t, true));
    assert_eq!(
        cache.last_price("AAA"),
        None,
        "yesterday's close is not today's price"
    );
    assert_eq!(cache.last_price_at("AAA", t), None);
    let p = pre(&cache, "AAA", t, 3.0);
    assert_eq!(p.price_1m_before, None);
    assert_eq!(p.price_5m_before, None);
    assert_eq!(p.first_observed_at, None, "absent, never detectedAt");
    assert_eq!(p.move_before_detection_pct, None);

    // Nothing opens at yesterday's price in either consumer.
    let mut oi = OpportunityIntelligence::new(OiConfig::default());
    oi.observe(
        &bar("AAA", et(2026, 9, 21, 15, 59, 0), 3.0),
        et(2026, 9, 21, 15, 59, 0),
    );
    oi.observe(&momentum("AAA", t, true), t);
    assert_eq!(oi.open_opportunities().count(), 0);
    let mut tracker = EpisodeTracker::new();
    tracker.observe(
        &bar("AAA", et(2026, 9, 21, 15, 59, 0), 3.0),
        et(2026, 9, 21, 15, 59, 0),
    );
    tracker.observe(&momentum("AAA", t, true), t);
    assert_eq!(tracker.open_count(), 0);
}

#[test]
fn a_query_on_a_later_day_than_the_state_sees_nothing() {
    let mut cache = running_cache();
    let t = et(2026, 9, 21, 15, 0, 0);
    cache.observe(&bar("AAA", t, 3.0));
    cache.observe(&momentum("AAA", t, true));
    cache.observe(&funnel("AAA", t, 3.0, 1_000_000, 12.0));
    let next = et(2026, 9, 22, 4, 1, 0);
    let snap = cache.snapshot("AAA", Strategy::FastFunnel, next, next, 3.1);
    assert!(snap.pre_detection.is_none());
    assert!(snap.funnel.is_none());
    assert!(snap.market.is_none());
    assert!(snap.momentum.is_none());
    assert_eq!(cache.last_price_at("AAA", next), None);
    // The time-less form still answers for the state's own day; that is why
    // consumers fold the event before asking.
    assert_eq!(cache.last_price("AAA"), Some(3.0));
}

#[test]
fn detector_counters_and_catalyst_reset_per_market_day() {
    let mut cache = running_cache();
    let d1 = et(2026, 9, 21, 10, 0, 0);
    cache.observe(&ignition(
        "AAA",
        d1,
        2.0,
        IgnitionEventKind::CandidateOpened,
    ));
    cache.observe(&ignition(
        "AAA",
        d1,
        2.0,
        IgnitionEventKind::FollowThroughRejected,
    ));
    cache.observe(&ScanEvent::ConsolidationEvent {
        symbol: "AAA".into(),
        timestamp: d1,
        price: 2.0,
        kind: ConsolidationEventKind::SurgeDetected,
        strategy: ConsolidationStrategy::Micropullback,
    });
    cache.observe(&ScanEvent::CatalystUpdate {
        symbol: "AAA".into(),
        timestamp: d1,
        catalyst_tags: vec!["earnings".into()],
        headline_count: 1,
        most_recent_headline: None,
        most_recent_published_at: None,
    });
    let d2 = et(2026, 9, 22, 9, 40, 0);
    cache.observe(&ignition(
        "AAA",
        d2,
        2.5,
        IgnitionEventKind::CandidateOpened,
    ));
    cache.observe(&ScanEvent::ConsolidationEvent {
        symbol: "AAA".into(),
        timestamp: d2,
        price: 2.5,
        kind: ConsolidationEventKind::SurgeDetected,
        strategy: ConsolidationStrategy::Micropullback,
    });
    let snap = cache.snapshot("AAA", Strategy::IgnitionDetector, d2, d2, 2.5);
    let ig = snap.ignition.unwrap();
    assert_eq!((ig.candidates_opened, ig.rejections), (1, 0));
    assert_eq!(snap.consolidation.unwrap().surges, 1);
    assert!(
        snap.catalyst.is_none(),
        "yesterday's lookup is not today's (see FeatureCache docs)"
    );
}

#[test]
fn replay_from_the_day_file_matches_live_once_the_day_has_reset() {
    // Live: a cache that saw 09-21. Replay: a fresh cache fed only 09-22.
    let events = jagx_events();
    let mut live = running_cache();
    for e in &events[..8] {
        live.observe(e);
    }
    let mut replay = FeatureCache::new();
    for e in &events[4..8] {
        replay.observe(e);
    }
    let at = z("2026-09-22T13:50:57.220233887Z");
    let a = pre(&live, "JAGX", at, 2.845);
    let b = pre(&replay, "JAGX", at, 2.845);
    assert_eq!(
        (
            a.first_observed_at,
            a.first_observed_price,
            a.session_low_observed,
            a.move_before_detection_pct,
            a.price_1m_before,
            a.price_5m_before,
            a.market_day
        ),
        (
            b.first_observed_at,
            b.first_observed_price,
            b.session_low_observed,
            b.move_before_detection_pct,
            b.price_1m_before,
            b.price_5m_before,
            b.market_day
        ),
    );
    // They differ only in the provenance fields, and honestly: a replay that
    // starts after 04:00 ET cannot claim a complete day.
    assert_eq!(a.baseline_truncated, Some(false));
    assert_eq!(b.baseline_truncated, Some(true));
}

// --- D7a: funnel / market freshness -------------------------------------------

#[test]
fn d7a_bttc_previous_market_day_funnel_is_omitted() {
    // BTTC's final 09-21 reading, which the deployed process still attached to
    // rows on 09-22 and 09-24: sessionVolume 421,689,047 at minuteOfDayUtc 0,
    // i.e. 2026-09-22T00:00Z = 20:00 ET on 09-21 (market day 09-21).
    let mut cache = running_cache();
    cache.observe(&funnel(
        "BTTC",
        z("2026-09-22T00:00:00Z"),
        0.6932,
        421_689_047,
        89.9178082191781,
    ));
    for at in [
        z("2026-09-22T10:54:28Z"),
        z("2026-09-24T09:04:10.531105816Z"),
    ] {
        let snap = cache.snapshot("BTTC", Strategy::FastFunnel, at, at, 0.70);
        assert!(
            snap.funnel.is_none(),
            "a 09-21 funnel must not appear at {at}"
        );
        assert!(snap.market.is_none());
    }
    // Still omitted once BTTC has a same-day price but no same-day funnel.
    let d2 = z("2026-09-22T10:55:00Z");
    cache.observe(&bar("BTTC", d2, 0.52));
    let snap = cache.snapshot("BTTC", Strategy::IgnitionDetector, d2, d2, 0.52);
    assert!(snap.funnel.is_none());
    assert!(snap.pre_detection.is_some());
}

#[test]
fn d7a_pfsa_premarket_funnel_carries_todays_premarket_volume() {
    // PFSA's 09-24 07:30 ET row carried a 09-23 funnel group (sessionVolume
    // 3,048,880 at minuteOfDayUtc 1218 = 16:18 ET on 09-23). Today's value
    // below (412,300 at 07:29 ET) is synthetic: any 09-24 premarket
    // FunnelSignal must replace it, and before one arrives the group is absent.
    let mut cache = running_cache();
    cache.observe(&funnel(
        "PFSA",
        z("2026-09-23T20:18:00Z"),
        2.14,
        3_048_880,
        10.309278350515473,
    ));
    let early = et(2026, 9, 24, 5, 0, 0);
    cache.observe(&bar("PFSA", early, 2.20));
    assert!(cache
        .snapshot("PFSA", Strategy::FastFunnel, early, early, 2.20)
        .funnel
        .is_none());

    let signal_at = et(2026, 9, 24, 7, 29, 0);
    cache.observe(&funnel("PFSA", signal_at, 2.31, 412_300, 18.2));
    let at = z("2026-09-24T11:30:21.114757688Z");
    let snap = cache.snapshot("PFSA", Strategy::FastFunnel, at, at, 2.31);
    let f = snap
        .funnel
        .expect("a same-day premarket funnel is included");
    assert_eq!(
        f.session_volume, 412_300,
        "today's premarket stream, not 09-23's 3,048,880"
    );
    assert_eq!(f.observed_at, Some(signal_at));
    assert_eq!(f.market_day, Some(day(2026, 9, 24)));
    let m = snap.market.expect("market group");
    assert_eq!(m.session_volume, Some(412_300));
    assert_eq!(m.session.as_deref(), Some("premarket"));
    assert_eq!(m.observed_at, Some(signal_at));
    assert_eq!(m.market_day, Some(day(2026, 9, 24)));
}

#[test]
fn d7a_funnel_observed_after_detected_at_is_omitted() {
    let mut cache = running_cache();
    let signal_at = et(2026, 9, 22, 10, 1, 0); // bar end
    cache.observe(&funnel("AAA", signal_at, 3.0, 5_000_000, 20.0));
    let before = et(2026, 9, 22, 10, 0, 30);
    let snap = cache.snapshot("AAA", Strategy::IgnitionDetector, before, before, 3.0);
    assert!(snap.funnel.is_none());
    assert!(snap.market.is_none());
    let after = et(2026, 9, 22, 10, 1, 0);
    let snap = cache.snapshot("AAA", Strategy::IgnitionDetector, after, after, 3.0);
    let f = snap.funnel.unwrap();
    assert!(f.observed_at.unwrap() <= after);
    assert_eq!(snap.market.unwrap().session.as_deref(), Some("regular"));
}

// --- schema compatibility -------------------------------------------------------

/// The `features` object of the deployed 09-22 JAGX confirmation row (OI row
/// schema 2, signal-context schema 1), verbatim from the preserved file.
const JAGX_0922_FEATURES_V1: &str = r#"{"schemaVersion":1,"symbol":"JAGX","sessionDate":"2026-09-22","strategy":"IgnitionDetector","detectedAt":"2026-09-22T13:50:57.220233887Z","capturedAt":"2026-09-22T13:50:57.236541921Z","signalPrice":2.845,"ignition":{"phase":"follow_through_confirmed","candidatesOpened":19,"confirmations":2,"rejections":5,"priceAtPhase":2.845,"phaseAt":"2026-09-22T13:50:57.220233887Z"},"preDetection":{"sessionLowObserved":2.5,"firstObservedPrice":2.77,"firstObservedAt":"2026-09-21T13:29:32.112819945Z","moveBeforeDetectionPct":2.7075812274368296}}"#;

#[test]
fn a_deployed_schema_1_context_still_deserializes_as_the_old_contract() {
    let ctx: SignalContext = serde_json::from_str(JAGX_0922_FEATURES_V1).unwrap();
    assert_eq!(ctx.schema_version, 1);
    let p = ctx.pre_detection.unwrap();
    assert_eq!(
        p.market_day, None,
        "absent marketDay = old contract, not sessionDate"
    );
    assert_eq!(p.baseline_truncated, None, "unknown, not 'complete'");
    assert_eq!(p.observation_started_at, None);
    assert_eq!(
        p.first_observed_at,
        Some(z("2026-09-21T13:29:32.112819945Z"))
    );

    // The deployed OI `versions` object: no baselinePolicy.
    let v: crate::opportunity::OiVersions = serde_json::from_str(
        r#"{"opportunitySchema":2,"featureSchema":2,"regimeClassifier":"regime-v1","priceRegime":"price-regime-v1","earlyQualityModel":"early-quality-v1-transparent","continuationModel":"continuation-v1-transparent","ranking":"opportunity-rank-v1","scorePolicy":"score-policy-v2-core-gated-coverage-normalized","configFingerprint":"oi-cfg-b4f21c8b311a1b99"}"#,
    )
    .unwrap();
    assert_eq!(v.baseline_policy, None);
}

#[test]
fn an_absent_first_observed_at_is_never_rendered_as_detected_at() {
    let raw = r#"{"schemaVersion":2,"symbol":"AAA","sessionDate":"2026-09-22","strategy":"MomentumScorer","detectedAt":"2026-09-22T08:00:30Z","capturedAt":"2026-09-22T08:00:30Z","signalPrice":3.0,"preDetection":{"marketDay":"2026-09-22","observationStartedAt":"2026-09-18T19:00:00Z","baselineTruncated":false}}"#;
    let ctx: SignalContext = serde_json::from_str(raw).unwrap();
    let p = ctx.pre_detection.unwrap();
    assert_eq!(p.first_observed_at, None);
    assert_eq!(p.market_day, Some(day(2026, 9, 22)));
    let back = serde_json::to_string(&ctx).unwrap();
    assert!(!back.contains("firstObservedAt"), "{back}");

    // And a freshly cut snapshot round-trips exactly, new fields included.
    let mut cache = running_cache();
    let t = et(2026, 9, 22, 7, 29, 0);
    cache.observe(&funnel("AAA", t, 3.0, 10_000, 5.0));
    let snap = cache.snapshot("AAA", Strategy::FastFunnel, t, t, 3.0);
    assert_eq!(snap.schema_version, 2);
    let json = serde_json::to_string(&snap).unwrap();
    for key in [
        "\"marketDay\":\"2026-09-22\"",
        "\"observationStartedAt\"",
        "\"baselineTruncated\":false",
        "\"observedAt\"",
        "\"session\":\"premarket\"",
    ] {
        assert!(json.contains(key), "missing {key} in {json}");
    }
    let back: SignalContext = serde_json::from_str(&json).unwrap();
    assert_eq!(back, snap);
}

// --- load (brief section 16) ------------------------------------------------------

/// Cost of the daily reset across a full universe. Ignored by default; run
/// with `cargo test --release -p backtest-metrics daily_reset_cost -- --ignored --nocapture`.
#[test]
#[ignore]
fn daily_reset_cost_over_a_full_universe() {
    use std::time::Instant;
    const SYMBOLS: usize = 13_000;
    const PRICES_PER_SYMBOL: i64 = 64;
    let names: Vec<String> = (0..SYMBOLS).map(|i| format!("S{i:05}")).collect();
    let mut cache = FeatureCache::new();
    let d1 = et(2026, 9, 21, 15, 0, 0);
    for k in 0..PRICES_PER_SYMBOL {
        for n in &names {
            cache.observe(&bar(
                n,
                d1 + Duration::seconds(k * 5),
                10.0 + k as f64 * 0.01,
            ));
        }
    }
    let heap = |c: &FeatureCache| -> usize {
        c.symbols
            .iter()
            .map(|(k, s)| {
                k.capacity()
                    + std::mem::size_of::<SymbolState>()
                    + s.price_trail.capacity() * std::mem::size_of::<(DateTime<Utc>, f64)>()
            })
            .sum()
    };
    let before = heap(&cache);

    // One same-day event per symbol: the steady-state per-event cost.
    let same = d1 + Duration::seconds(PRICES_PER_SYMBOL * 5);
    let t0 = Instant::now();
    for n in &names {
        cache.observe(&bar(n, same, 11.0));
    }
    let same_day = t0.elapsed();

    // The first event of the next market day for every symbol: 13,000 resets.
    let d2 = et(2026, 9, 22, 4, 0, 5);
    let t0 = Instant::now();
    for n in &names {
        cache.observe(&bar(n, d2, 12.0));
    }
    let reset = t0.elapsed();
    let after = heap(&cache);

    for n in names.iter().take(10) {
        assert_eq!(pre(&cache, n, d2, 12.0).first_observed_price, Some(12.0));
    }
    println!(
        "{SYMBOLS} symbols: same-day pass {:?} ({:.0} ns/event); reset pass {:?} ({:.0} ns/event); \
         approx heap {:.1} MiB before reset -> {:.1} MiB after",
        same_day,
        same_day.as_nanos() as f64 / SYMBOLS as f64,
        reset,
        reset.as_nanos() as f64 / SYMBOLS as f64,
        before as f64 / (1024.0 * 1024.0),
        after as f64 / (1024.0 * 1024.0),
    );
}
