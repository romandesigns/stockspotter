//! D7b (premarket session volume) tests for the universe scan: snapshot
//! conversion, seed preselection, the Stage-2 gate, the 09:31 roll and the
//! quiet watch, plus one end-to-end scan against a mocked Alpaca. Contract
//! numbering: docs/measurement-correctness-contract-2026-09-25.md D7-T1..T14.
//!
//! Fixture values marked "tape" are the real snapshot fields from the
//! preserved 2026-09-22 discovery tape (segments -8 and -9); the rest are
//! stated per test.

use super::*;
use crate::rest::DailySeed;
use chrono::{Duration, TimeZone};
use serde_json::json;

fn z(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn utc(h: u32, mi: u32, s: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 22, h, mi, s).unwrap()
}

fn convert(
    symbol: &str,
    raw: serde_json::Value,
    now: DateTime<Utc>,
) -> (TickerSnapshot, SnapshotMeta) {
    let raw: SnapshotRaw = serde_json::from_value(raw).unwrap();
    snapshot_from_raw(symbol.to_string(), raw, market_day(now), now).expect("convertible snapshot")
}

fn seed(prior_close: f64, avg_daily_volume: u64) -> DailySeed {
    DailySeed {
        prior_close,
        avg_daily_volume,
    }
}

/// The pure half of `scan_shortlist_at` in its production order: convert,
/// seed, fill minute-bar volume, then Stage 2 (float not involved). Returns
/// the snapshots and the sorted price+gap+relative-volume survivors.
fn pipeline(
    raws: &[(&str, serde_json::Value)],
    seeds: &HashMap<String, DailySeed>,
    cache: &PremarketVolumeCache,
    now: DateTime<Utc>,
) -> (HashMap<String, TickerSnapshot>, Vec<String>) {
    let thresholds = FilterThresholds::default();
    let mut snapshots = HashMap::new();
    let mut meta = HashMap::new();
    for (symbol, raw) in raws {
        let (s, m) = convert(symbol, raw.clone(), now);
        snapshots.insert(symbol.to_string(), s);
        meta.insert(symbol.to_string(), m);
    }
    apply_daily_seeds(&mut snapshots, seeds, &thresholds);
    apply_bar_volumes(&mut snapshots, &mut meta, cache);
    let mut survivors: Vec<String> = snapshots
        .values()
        .filter(|s| {
            let v = explain(s, &thresholds);
            v.price_ok && v.gap_ok && v.rel_vol_ok
        })
        .map(|s| s.symbol.clone())
        .collect();
    survivors.sort();
    (snapshots, survivors)
}

// tape: 2026-09-22T10:54:41.984Z
fn bttc_1054() -> serde_json::Value {
    json!({"dailyBar":{"c":0.7948,"t":"2026-09-21T04:00:00Z","v":422876871},
        "latestTrade":{"p":0.595,"t":"2026-09-22T10:54:40.991101315Z"},
        "prevDailyBar":{"c":0.365,"t":"2026-09-18T04:00:00Z","v":48286792}})
}

/// ZEO's real stale snapshot shape (tape, 13:30:45Z) at a chosen price/time.
fn zeo_stale(price: f64, trade_at: &str) -> serde_json::Value {
    json!({"dailyBar":{"c":0.3119,"t":"2026-09-21T04:00:00Z","v":118150019},
        "latestTrade":{"p":price,"t":trade_at},
        "prevDailyBar":{"c":0.246,"t":"2026-09-18T04:00:00Z","v":43988779}})
}

/// ZEO after the roll (tape, 13:31:00Z), with a chosen volume for later minutes.
fn zeo_current(price: f64, trade_at: &str, v: u64) -> serde_json::Value {
    json!({"dailyBar":{"c":0.3647,"t":"2026-09-22T04:00:00Z","v":v},
        "latestTrade":{"p":price,"t":trade_at},
        "prevDailyBar":{"c":0.3119,"t":"2026-09-21T04:00:00Z","v":118150019}})
}

fn tops_stale(price: f64, trade_at: &str) -> serde_json::Value {
    json!({"dailyBar":{"c":0.718,"t":"2026-09-21T04:00:00Z","v":64523364},
        "latestTrade":{"p":price,"t":trade_at},
        "prevDailyBar":{"c":0.712,"t":"2026-09-18T04:00:00Z","v":103018}})
}

fn tops_current(price: f64, trade_at: &str, v: u64) -> serde_json::Value {
    json!({"dailyBar":{"c":1.15,"t":"2026-09-22T04:00:00Z","v":v},
        "latestTrade":{"p":price,"t":trade_at},
        "prevDailyBar":{"c":0.718,"t":"2026-09-21T04:00:00Z","v":64523364}})
}

/// LHSW's real stale bars (tape, 10:54:43Z); the +184% premarket price and
/// time are from the P2 trace (12:30Z).
fn lhsw_stale(price: f64, trade_at: &str) -> serde_json::Value {
    json!({"dailyBar":{"c":0.433,"t":"2026-09-21T04:00:00Z","v":452453},
        "latestTrade":{"p":price,"t":trade_at},
        "prevDailyBar":{"c":0.4301,"t":"2026-09-18T04:00:00Z","v":584008}})
}

// --- D7-T3 / T4 ---------------------------------------------------------------

#[test]
fn d7_t3_bttc_stale_gap_is_against_daily_bar_close() {
    let (s, m) = convert("BTTC", bttc_1054(), z("2026-09-22T10:54:41.984Z"));
    assert_eq!(m.freshness, DailyBarFreshness::Stale);
    assert_eq!(
        m.daily_bar_date,
        Some(NaiveDate::from_ymd_opt(2026, 9, 21).unwrap())
    );
    // 0.595 / 0.7948 - 1 = -25.1%. The old reference, prevDailyBar.c 0.365
    // (09-18), gave +63.0%.
    assert!((s.gap_pct - (-25.14)).abs() < 0.01, "gap {}", s.gap_pct);
    assert!((0.595 / 0.365 - 1.0) * 100.0 > 63.0);
    // Average proxy is the last session's volume, not the one before.
    assert_eq!(s.avg_daily_volume, 422_876_871);
}

#[test]
fn d7_t4_stale_session_volume_is_unknown_and_stage2_fails_closed_without_bars() {
    let now = z("2026-09-22T10:54:41.984Z");
    let (s, m) = convert("BTTC", bttc_1054(), now);
    assert_eq!(
        s.session_volume, None,
        "not dailyBar.v = 422,876,871 (09-21's whole day)"
    );
    assert_eq!(s.session_volume_source, SessionVolumeSource::Unknown);
    assert_eq!(m.session_volume_as_of, None);

    // Even a gapper with a real seed fails relative volume closed until bars
    // provide today's volume.
    let now = utc(12, 30, 5);
    let (_, survivors) = pipeline(
        &[("LHSW", lhsw_stale(1.23, "2026-09-22T12:29:58Z"))],
        &HashMap::from([("LHSW".to_string(), seed(0.433, 500_000))]),
        &PremarketVolumeCache::new(),
        now,
    );
    assert!(survivors.is_empty());
}

// --- D7-T5 / T6 / T7 ------------------------------------------------------------

#[test]
fn d7_t5_premarket_survivor_with_bar_volume_passes_relative_volume() {
    // LHSW-style: +184% on 09-21's close, 4.0M shares since 04:00 ET against
    // a 500k 20-day average = 8x. Before D7 its rel-vol read 452,453 (09-21's
    // day) / 500k = 0.9 and it could not qualify premarket.
    let now = utc(12, 30, 5);
    let mut cache = PremarketVolumeCache::new();
    cache.ingest_bars_for_test(
        "LHSW",
        &[(utc(8, 0, 0), 1_000_000), (utc(12, 0, 0), 3_000_000)],
        now,
    );
    let (snaps, survivors) = pipeline(
        &[("LHSW", lhsw_stale(1.23, "2026-09-22T12:29:58Z"))],
        &HashMap::from([("LHSW".to_string(), seed(0.433, 500_000))]),
        &cache,
        now,
    );
    let s = &snaps["LHSW"];
    assert!((s.gap_pct - 184.06).abs() < 0.01, "gap {}", s.gap_pct);
    assert_eq!(s.session_volume, Some(4_000_000));
    assert_eq!(
        s.session_volume_source,
        SessionVolumeSource::MinuteBarsSinceOpen
    );
    assert_eq!(survivors, vec!["LHSW"]);
}

#[test]
fn d7_t6_yesterdays_runner_without_volume_today_does_not_pass_premarket() {
    // ZEO-style (tape 13:30:45Z shape): +15.5% on 09-21's close, but only
    // 20M today against a ~9.9M average = 2x. Before D7 it passed all
    // premarket on 09-21's 118,150,019 (11.9x).
    let now = utc(12, 30, 5);
    let seeds = HashMap::from([("ZEO".to_string(), seed(0.3119, 9_906_940))]);
    assert!(118_150_019.0 / 9_906_940.0 >= 5.0, "what the old rule saw");

    let mut cache = PremarketVolumeCache::new();
    cache.ingest_bars_for_test("ZEO", &[(utc(8, 0, 0), 20_000_000)], now);
    let (snaps, survivors) = pipeline(
        &[("ZEO", zeo_stale(0.3602, "2026-09-22T12:29:50Z"))],
        &seeds,
        &cache,
        now,
    );
    assert!(explain(&snaps["ZEO"], &FilterThresholds::default()).gap_ok);
    assert!(survivors.is_empty());

    // With no bars at all it is unknown and fails closed too.
    let (_, survivors) = pipeline(
        &[("ZEO", zeo_stale(0.3602, "2026-09-22T12:29:50Z"))],
        &seeds,
        &PremarketVolumeCache::new(),
        now,
    );
    assert!(survivors.is_empty());
}

#[test]
fn d7_t7_seed_preselection_uses_the_corrected_gap() {
    // Down 50% yesterday (2.00 -> 1.00), up 20% today premarket. Against the
    // close two sessions back it read -40% and was never seeded.
    let now = utc(11, 0, 0);
    let raw = json!({"dailyBar":{"c":1.00,"t":"2026-09-21T04:00:00Z","v":3000000},
        "latestTrade":{"p":1.20,"t":"2026-09-22T10:59:00Z"},
        "prevDailyBar":{"c":2.00,"t":"2026-09-18T04:00:00Z","v":1000000}});
    let (s, _) = convert("BOUNCE", raw, now);
    assert!((s.gap_pct - 20.0).abs() < 1e-9);
    let snapshots = HashMap::from([("BOUNCE".to_string(), s)]);
    assert_eq!(
        seed_candidates(&snapshots, &HashMap::new(), &FilterThresholds::default()),
        vec!["BOUNCE"]
    );

    // And the inverse: up yesterday, flat today -- no longer seeded on
    // yesterday's move.
    let raw = json!({"dailyBar":{"c":2.00,"t":"2026-09-21T04:00:00Z","v":3000000},
        "latestTrade":{"p":2.01,"t":"2026-09-22T10:59:00Z"},
        "prevDailyBar":{"c":1.00,"t":"2026-09-18T04:00:00Z","v":1000000}});
    let (s, _) = convert("FADED", raw, now);
    let snapshots = HashMap::from([("FADED".to_string(), s)]);
    assert!(seed_candidates(&snapshots, &HashMap::new(), &FilterThresholds::default()).is_empty());
}

// --- D7-T8 / T9: the roll -----------------------------------------------------------

/// TOPS minute bars: 50,000,000 through 09:29 ET and 3,418,201 in the 09:30
/// minute, so the sum through 09:30 is the tape's post-roll dailyBar.v.
fn tops_bars() -> Vec<(DateTime<Utc>, u64)> {
    vec![
        (utc(8, 0, 0), 20_000_000),
        (utc(12, 0, 0), 30_000_000),
        (utc(13, 30, 0), 3_418_201),
    ]
}

fn zeo_bars() -> Vec<(DateTime<Utc>, u64)> {
    vec![(utc(8, 0, 0), 20_000_000), (utc(13, 30, 0), 1_727_454)]
}

#[test]
fn d7_t8_after_the_roll_daily_bar_volume_is_used_as_is_and_equals_the_session_tracker() {
    let now = z("2026-09-22T13:31:01.197770735Z");
    let (s, m) = convert(
        "TOPS",
        tops_current(1.17, "2026-09-22T13:31:00.945942125Z", 53_418_201),
        now,
    );
    assert_eq!(m.freshness, DailyBarFreshness::Current);
    assert_eq!(s.session_volume, Some(53_418_201));
    assert_eq!(
        s.session_volume_source,
        SessionVolumeSource::SnapshotDailyBarCurrent
    );
    assert!(
        (s.gap_pct - (1.17 / 0.718 - 1.0) * 100.0).abs() < 1e-9,
        "reference is prevDailyBar.c = 09-21 once current"
    );

    // The stream tracker, backfilled from 04:00 ET, holds the same number: the
    // two sources describe one quantity, so nothing needs adding.
    let mut tracker = crate::SessionTracker::new("TOPS".into(), 0.718, 5_000_000, None);
    for (t, v) in tops_bars() {
        tracker.on_bar(&crate::Bar {
            symbol: "TOPS".into(),
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: v,
            timestamp: t,
        });
    }
    assert_eq!(tracker.session_volume(), 53_418_201);
}

#[test]
fn d7_t9_brief_0931_roll_is_continuous_with_no_double_count() {
    // 09:29 -> 09:33 ET. Before the roll the snapshots are stale and TOPS/ZEO
    // volumes come from bars; from 13:31:01Z they are current. Membership may
    // only change because of real volume, never because of the roll: TOPS
    // (10x+) is in throughout and ZEO (~2x) is out throughout. Before D7, ZEO
    // was IN until 13:31:02Z and OUT after -- purely the roll (P2 trace).
    let seeds = HashMap::from([
        ("TOPS".to_string(), seed(0.718, 5_000_000)),
        ("ZEO".to_string(), seed(0.3119, 9_906_940)),
    ]);
    let steps: Vec<(DateTime<Utc>, Vec<(&str, serde_json::Value)>)> = vec![
        (
            utc(13, 29, 30),
            vec![
                ("TOPS", tops_stale(1.21, "2026-09-22T13:29:29Z")),
                ("ZEO", zeo_stale(0.36, "2026-09-22T13:29:29Z")),
            ],
        ),
        (
            utc(13, 29, 59),
            vec![
                ("TOPS", tops_stale(1.20, "2026-09-22T13:29:58Z")),
                ("ZEO", zeo_stale(0.36, "2026-09-22T13:29:58Z")),
            ],
        ),
        (
            utc(13, 30, 0),
            vec![
                ("TOPS", tops_stale(1.20, "2026-09-22T13:29:59Z")),
                ("ZEO", zeo_stale(0.36, "2026-09-22T13:29:59Z")),
            ],
        ),
        (
            z("2026-09-22T13:30:45.853Z"),
            vec![
                ("TOPS", tops_stale(1.2, "2026-09-22T13:30:45.637824538Z")),
                ("ZEO", zeo_stale(0.3602, "2026-09-22T13:30:45.096208701Z")),
            ],
        ),
        (
            z("2026-09-22T13:31:01.197Z"),
            vec![
                (
                    "TOPS",
                    tops_current(1.17, "2026-09-22T13:31:00.945942125Z", 53_418_201),
                ),
                (
                    "ZEO",
                    zeo_current(0.3629, "2026-09-22T13:31:00.646902536Z", 21_727_454),
                ),
            ],
        ),
        (
            utc(13, 32, 0),
            vec![
                (
                    "TOPS",
                    tops_current(1.15, "2026-09-22T13:31:59Z", 55_000_000),
                ),
                ("ZEO", zeo_current(0.37, "2026-09-22T13:31:59Z", 22_500_000)),
            ],
        ),
        (
            utc(13, 33, 0),
            vec![
                (
                    "TOPS",
                    tops_current(1.16, "2026-09-22T13:32:59Z", 56_000_000),
                ),
                ("ZEO", zeo_current(0.37, "2026-09-22T13:32:59Z", 23_000_000)),
            ],
        ),
    ];
    let mut cache = PremarketVolumeCache::new();
    let mut last_tops = 0u64;
    for (now, raws) in steps {
        cache.ingest_bars_for_test("TOPS", &tops_bars(), now);
        cache.ingest_bars_for_test("ZEO", &zeo_bars(), now);
        let (snaps, survivors) = pipeline(&raws, &seeds, &cache, now);
        assert_eq!(survivors, vec!["TOPS"], "membership at {now}");
        let tops = snaps["TOPS"].session_volume.unwrap();
        assert!(tops >= last_tops, "volume never steps backwards at {now}");
        // No double count: never bars + dailyBar.v.
        assert!(tops <= 56_000_000, "{tops} at {now}");
        last_tops = tops;
        let expect_source = if now < z("2026-09-22T13:31:00Z") {
            SessionVolumeSource::MinuteBarsSinceOpen
        } else {
            SessionVolumeSource::SnapshotDailyBarCurrent
        };
        assert_eq!(
            snaps["TOPS"].session_volume_source, expect_source,
            "at {now}"
        );
    }
    // At the roll the snapshot's value equals the bar sum through 09:30 ET.
    assert_eq!(
        sum_completed_for_test(&tops_bars(), z("2026-09-22T13:31:01Z")),
        53_418_201
    );
    assert_eq!(
        sum_completed_for_test(&zeo_bars(), z("2026-09-22T13:31:01Z")),
        21_727_454
    );
}

fn sum_completed_for_test(bars: &[(DateTime<Utc>, u64)], now: DateTime<Utc>) -> u64 {
    crate::premarket_volume::sum_completed_bars(bars, market_day(now), now)
}

// --- candidates ---------------------------------------------------------------

#[test]
fn brief_no_trade_since_the_open_is_never_fetched_and_stays_unknown() {
    // A gapper whose latest trade is yesterday's after-hours print: nothing
    // to sum today, no request spent, relative volume fails closed.
    let now = utc(9, 0, 0);
    let thresholds = FilterThresholds::default();
    let (s, m) = convert("SLEEPY", lhsw_stale(0.55, "2026-09-21T23:59:00Z"), now);
    let mut snaps = HashMap::from([("SLEEPY".to_string(), s)]);
    let meta = HashMap::from([("SLEEPY".to_string(), m)]);
    apply_daily_seeds(
        &mut snaps,
        &HashMap::from([("SLEEPY".to_string(), seed(0.433, 500_000))]),
        &thresholds,
    );
    let (candidates, no_trade) = volume_candidates(&snaps, &meta, &thresholds, now);
    assert!(candidates.is_empty());
    assert_eq!(no_trade, 1);
    assert!(!explain(&snaps["SLEEPY"], &thresholds).rel_vol_ok);
}

#[test]
fn brief_symbol_first_appearing_in_the_regular_session_needs_no_premarket_fetch() {
    // After the roll the snapshot carries today's volume (premarket included),
    // so a symbol that first gaps at 11:00 ET is not a bar-volume candidate;
    // once qualified, live tracking backfills its SessionTracker from 04:00 ET
    // (`rest::fetch_session_bars`), as before.
    let now = utc(15, 0, 0);
    let thresholds = FilterThresholds::default();
    let (s, m) = convert(
        "LATE",
        tops_current(1.60, "2026-09-22T14:59:59Z", 90_000_000),
        now,
    );
    assert_eq!(s.session_volume, Some(90_000_000));
    let mut snaps = HashMap::from([("LATE".to_string(), s)]);
    let meta = HashMap::from([("LATE".to_string(), m)]);
    apply_daily_seeds(
        &mut snaps,
        &HashMap::from([("LATE".to_string(), seed(0.718, 5_000_000))]),
        &thresholds,
    );
    let (candidates, no_trade) = volume_candidates(&snaps, &meta, &thresholds, now);
    assert!(candidates.is_empty() && no_trade == 0);
    assert!(explain(&snaps["LATE"], &thresholds).rel_vol_ok);
}

// --- D7-T11: quiet watch ---------------------------------------------------------

#[test]
fn d7_t11_quiet_watch_premarket_never_selects_on_yesterdays_volume() {
    let now = utc(10, 0, 0);
    let cfg = QuietWatchConfig::default();
    // Busy yesterday (5M vs 1M the day before: rel-vol 5.0 under the old
    // rule, so excluded), flat today. Now: unknown volume is not read as
    // yesterday's, the gap is ~0 against yesterday's close -> quiet.
    let busy_yesterday = json!({"dailyBar":{"c":1.00,"t":"2026-09-21T04:00:00Z","v":5000000},
        "latestTrade":{"p":1.01,"t":"2026-09-22T09:59:00Z"},
        "prevDailyBar":{"c":0.80,"t":"2026-09-18T04:00:00Z","v":1000000}});
    // Flat against two sessions back (old gap +2%, old rel-vol 0.1: quiet),
    // but up 30% against yesterday's close today: not quiet.
    let gapping_today = json!({"dailyBar":{"c":0.80,"t":"2026-09-21T04:00:00Z","v":200000},
        "latestTrade":{"p":1.04,"t":"2026-09-22T09:59:00Z"},
        "prevDailyBar":{"c":1.02,"t":"2026-09-18T04:00:00Z","v":2000000}});
    let mut snaps = HashMap::new();
    for (sym, raw) in [("BUSYYDAY", busy_yesterday), ("GAPTODAY", gapping_today)] {
        let (s, _) = convert(sym, raw, now);
        assert_eq!(s.session_volume, None);
        snaps.insert(sym.to_string(), s);
    }
    assert_eq!(select_quiet_watch(&snaps, &cfg), vec!["BUSYYDAY"]);

    // A known volume still has to be quiet.
    snaps.get_mut("BUSYYDAY").unwrap().session_volume = Some(6_000_000);
    snaps.get_mut("BUSYYDAY").unwrap().session_volume_source =
        SessionVolumeSource::MinuteBarsSinceOpen;
    assert!(select_quiet_watch(&snaps, &cfg).is_empty());
}

// --- selection_inputs self-describe ---------------------------------------------

#[test]
fn selection_inputs_carry_source_daily_bar_date_and_as_of() {
    let now = utc(12, 30, 5);
    let mut cache = PremarketVolumeCache::new();
    cache.ingest_bars_for_test("LHSW", &[(utc(8, 0, 0), 4_000_000)], now);
    let (mut s, mut m) = convert("LHSW", lhsw_stale(1.23, "2026-09-22T12:29:58Z"), now);
    let as_of = cache.apply(&mut s);
    m.session_volume_as_of = as_of;
    let row = serde_json::to_value(SelectionInput {
        snapshot: &s,
        daily_bar_date: m.daily_bar_date,
        daily_bar_freshness: Some(m.freshness),
        session_volume_as_of: m.session_volume_as_of,
    })
    .unwrap();
    assert_eq!(row["symbol"], "LHSW");
    assert_eq!(row["session_volume"], 4_000_000);
    assert_eq!(row["sessionVolumeSource"], "minute_bars_since_open");
    assert_eq!(row["dailyBarDate"], "2026-09-21");
    assert_eq!(row["dailyBarFreshness"], "stale");
    assert_eq!(row["sessionVolumeAsOf"], "2026-09-22T12:30:00Z");

    let (s, _) = convert("BTTC", bttc_1054(), z("2026-09-22T10:54:41.984Z"));
    let row = serde_json::to_value(&s).unwrap();
    assert!(row["session_volume"].is_null());
    assert_eq!(row["sessionVolumeSource"], "unknown");
}

// --- end to end, mocked Alpaca ------------------------------------------------------

fn daily_history(symbol: &str) -> serde_json::Value {
    // 20 sessions before 09-22: 18 quiet ones plus the two real ones on tape.
    let (quiet, d18, d21) = match symbol {
        "LHSW" => (520_000u64, (0.4301, 584_008u64), (0.433, 452_453u64)),
        "ZEO" => (
            2_000_000u64,
            (0.246, 43_988_779u64),
            (0.3119, 118_150_019u64),
        ),
        _ => (
            1_000_000u64,
            (0.365, 48_286_792u64),
            (0.7948, 422_876_871u64),
        ),
    };
    let mut bars = Vec::new();
    let mut d = NaiveDate::from_ymd_opt(2026, 8, 21).unwrap();
    while bars.len() < 18 {
        if !matches!(
            chrono::Datelike::weekday(&d),
            chrono::Weekday::Sat | chrono::Weekday::Sun
        ) {
            bars.push(json!({"t":format!("{d}T04:00:00Z"),"c":1.0,"v":quiet}));
        }
        d = d.succ_opt().unwrap();
    }
    bars.push(json!({"t":"2026-09-18T04:00:00Z","c":d18.0,"v":d18.1}));
    bars.push(json!({"t":"2026-09-21T04:00:00Z","c":d21.0,"v":d21.1}));
    json!(bars)
}

#[tokio::test]
async fn end_to_end_premarket_scan_qualifies_todays_runner_not_yesterdays() {
    let now = utc(12, 30, 5);
    let (base, log) = crate::test_http::serve(move |req| {
        let body = match req.path.as_str() {
            "/v2/assets" => json!([
                {"symbol":"LHSW","name":"L Corp","tradable":true,"status":"active"},
                {"symbol":"ZEO","name":"Z Corp","tradable":true,"status":"active"},
                {"symbol":"BTTC","name":"B Corp","tradable":true,"status":"active"}]),
            "/v2/stocks/snapshots" => json!({
                "LHSW": lhsw_stale(1.23, "2026-09-22T12:29:58Z"),
                "ZEO": zeo_stale(0.3602, "2026-09-22T12:29:50Z"),
                "BTTC": bttc_1054()}),
            "/v2/stocks/bars" if req.param("timeframe") == Some("1Day") => {
                let bars: serde_json::Map<String, serde_json::Value> = req
                    .param("symbols")
                    .unwrap()
                    .split(',')
                    .map(|s| (s.to_string(), daily_history(s)))
                    .collect();
                json!({"bars":bars,"next_page_token":null})
            }
            "/v2/stocks/bars" if req.param("timeframe") == Some("1Min") => {
                let mut bars = serde_json::Map::new();
                for s in req.param("symbols").unwrap().split(',') {
                    let v = match s {
                        "LHSW" => {
                            json!([{"t":"2026-09-22T08:00:00Z","v":1000000},{"t":"2026-09-22T12:00:00Z","v":3000000}])
                        }
                        "ZEO" => json!([{"t":"2026-09-22T08:00:00Z","v":20000000}]),
                        _ => serde_json::Value::Null,
                    };
                    bars.insert(s.to_string(), v);
                }
                json!({"bars":bars,"next_page_token":null})
            }
            other => panic!("unexpected request {other}"),
        };
        (200, body.to_string())
    });
    let cfg = crate::test_http::config(&base);
    let thresholds = FilterThresholds::default();
    let mut float_cache = FloatCache::new(250);
    float_cache.roll_day(
        now.with_timezone(&chrono_tz::America::New_York)
            .date_naive(),
    );
    float_cache.known.insert("LHSW".into(), Some(5_000_000));
    float_cache.known.insert("ZEO".into(), Some(5_000_000));
    let mut volume_cache = PremarketVolumeCache::new();

    let outcome = scan_shortlist_at(&cfg, &thresholds, &mut float_cache, &mut volume_cache, now)
        .await
        .unwrap();
    let qualified: Vec<&str> = outcome
        .qualified
        .iter()
        .map(|q| q.symbol.as_str())
        .collect();
    assert_eq!(
        qualified,
        vec!["LHSW"],
        "today's runner in, yesterday's (ZEO, 118M on 09-21) out"
    );

    let minute_requests = |log: &std::sync::Mutex<Vec<crate::test_http::Request>>| {
        log.lock()
            .unwrap()
            .iter()
            .filter(|r| r.param("timeframe") == Some("1Min"))
            .cloned()
            .collect::<Vec<_>>()
    };
    let first = minute_requests(&log);
    assert_eq!(first.len(), 1, "one batched request");
    assert_eq!(
        first[0].param("symbols"),
        Some("LHSW,ZEO"),
        "BTTC (-25% today) is not a candidate; biggest gap first"
    );
    let seeded = log
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.param("timeframe") == Some("1Day"))
        .count();
    assert_eq!(seeded, 1);

    // Same minute: cached, no new request. Next minute: refreshed.
    scan_shortlist_at(
        &cfg,
        &thresholds,
        &mut float_cache,
        &mut volume_cache,
        now + Duration::seconds(15),
    )
    .await
    .unwrap();
    assert_eq!(minute_requests(&log).len(), 1);
    scan_shortlist_at(
        &cfg,
        &thresholds,
        &mut float_cache,
        &mut volume_cache,
        now + Duration::seconds(60),
    )
    .await
    .unwrap();
    assert_eq!(minute_requests(&log).len(), 2);

    let health = volume_cache.stamped();
    assert_eq!(
        health.market_day,
        Some(NaiveDate::from_ymd_opt(2026, 9, 22).unwrap())
    );
    assert_eq!(health.fetch_failures, 0);
    assert_eq!(health.survivors_needing_volume, 2);
    assert_eq!(health.survivors_resolved, 2);
    assert_eq!(health.by_source.minute_bars_since_open, 2);
    assert_eq!(health.by_source.unknown, 1);
    assert_eq!(health.requests_this_market_day, 2);
}
