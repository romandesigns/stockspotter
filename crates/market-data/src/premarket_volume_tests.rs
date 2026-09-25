//! D7b tests for the staleness rule and the premarket minute-bar resolver.
//! Contract numbering: docs/measurement-correctness-contract-2026-09-25.md
//! D7-T1..T14; "brief" names follow the P3 D7 assignment's list. The
//! snapshot-conversion and scan-pipeline halves live in `universe.rs`'s tests,
//! the board half in `movers.rs`'s.

use super::*;
use chrono::TimeZone;

fn z(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
}

fn cand(symbol: &str, gap_pct: f64) -> VolumeCandidate {
    VolumeCandidate {
        symbol: symbol.to_string(),
        gap_pct,
    }
}

// --- D7-T1 / T2 / T14: staleness ------------------------------------------

#[test]
fn d7_t1_staleness_compares_new_york_dates_in_both_encodings() {
    // EDT encoding: New York midnight = 04:00Z. 09-22 premarket sees 09-21.
    let today = market_day(z("2026-09-22T10:54:41Z"));
    assert_eq!(today, day(2026, 9, 22));
    assert_eq!(
        daily_bar_freshness(Some(z("2026-09-21T04:00:00Z")), today),
        DailyBarFreshness::Stale
    );
    assert_eq!(
        daily_bar_freshness(Some(z("2026-09-22T04:00:00Z")), today),
        DailyBarFreshness::Current
    );
    // EST encoding: New York midnight = 05:00Z.
    let today = market_day(z("2026-01-15T12:00:00Z"));
    assert_eq!(
        daily_bar_freshness(Some(z("2026-01-14T05:00:00Z")), today),
        DailyBarFreshness::Stale
    );
    assert_eq!(
        daily_bar_freshness(Some(z("2026-01-15T05:00:00Z")), today),
        DailyBarFreshness::Current
    );
    // The date is the New York date, not the UTC one: an EST bar stamped
    // 05:00Z and an EDT bar stamped 04:00Z are both that calendar day.
    assert_eq!(daily_bar_date(z("2026-01-15T05:00:00Z")), day(2026, 1, 15));
    assert_eq!(daily_bar_date(z("2026-09-22T04:00:00Z")), day(2026, 9, 22));
    assert_eq!(daily_bar_freshness(None, today), DailyBarFreshness::Missing);
    // Never observed, handled fail-closed rather than as current.
    assert_eq!(
        daily_bar_freshness(Some(z("2026-01-16T05:00:00Z")), today),
        DailyBarFreshness::Ahead
    );
}

#[test]
fn d7_t2_monday_and_post_holiday_premarket_are_stale() {
    // Monday 2026-09-21 premarket: dailyBar is Friday 09-18.
    let monday = market_day(z("2026-09-21T09:00:00Z"));
    assert_eq!(
        daily_bar_freshness(Some(z("2026-09-18T04:00:00Z")), monday),
        DailyBarFreshness::Stale
    );
    // Tuesday 2026-09-08, after Labor Day (09-07): dailyBar is Friday 09-04.
    assert_eq!(
        halt_detector::calendar::regular_close_minutes(day(2026, 9, 7)),
        None
    );
    let tuesday = market_day(z("2026-09-08T09:00:00Z"));
    assert_eq!(
        daily_bar_freshness(Some(z("2026-09-04T04:00:00Z")), tuesday),
        DailyBarFreshness::Stale
    );
}

#[test]
fn d7_t14_dst_transition_weeks_and_an_early_close() {
    // Spring forward: Monday 2026-03-09 is the first EDT trading day. Friday's
    // bar carries the EST encoding (05:00Z), Monday's the EDT one (04:00Z).
    let pre = utc(2026, 3, 9, 8, 0, 0); // 04:00 EDT
    assert_eq!(market_day(pre), day(2026, 3, 9));
    assert_eq!(market_day(pre - Duration::seconds(1)), day(2026, 3, 8));
    assert_eq!(
        daily_bar_freshness(Some(z("2026-03-06T05:00:00Z")), market_day(pre)),
        DailyBarFreshness::Stale
    );
    let rolled = utc(2026, 3, 9, 13, 31, 1); // 09:31 EDT
    assert_eq!(
        daily_bar_freshness(Some(z("2026-03-09T04:00:00Z")), market_day(rolled)),
        DailyBarFreshness::Current
    );
    assert_eq!(
        crate::classify_session(pre),
        crate::TradingSession::Premarket
    );
    assert_eq!(
        crate::classify_session(rolled),
        crate::TradingSession::Regular
    );

    // Fall back: Monday 2026-11-02 is the first EST trading day. 08:30Z was
    // premarket last week and is overnight now; the market day follows.
    let still_prior = utc(2026, 11, 2, 8, 30, 0); // 03:30 EST
    assert_eq!(market_day(still_prior), day(2026, 11, 1));
    assert_eq!(
        crate::classify_session(still_prior),
        crate::TradingSession::Overnight
    );
    let pre = utc(2026, 11, 2, 9, 0, 0); // 04:00 EST
    assert_eq!(
        daily_bar_freshness(Some(z("2026-10-30T04:00:00Z")), market_day(pre)),
        DailyBarFreshness::Stale
    );
    let rolled = utc(2026, 11, 2, 14, 31, 1); // 09:31 EST
    assert_eq!(
        daily_bar_freshness(Some(z("2026-11-02T05:00:00Z")), market_day(rolled)),
        DailyBarFreshness::Current
    );

    // Early close, Friday 2026-11-27 (13:00 ET): the roll is still at the
    // open, and after the early close the bar stays current for the market day.
    assert_eq!(
        halt_detector::calendar::regular_close_minutes(day(2026, 11, 27)),
        Some(780)
    );
    let after_close = utc(2026, 11, 27, 18, 30, 0); // 13:30 EST
    assert_eq!(
        crate::classify_session(after_close),
        crate::TradingSession::AfterHours
    );
    assert_eq!(
        daily_bar_freshness(Some(z("2026-11-27T05:00:00Z")), market_day(after_close)),
        DailyBarFreshness::Current
    );
    // ... until 04:00 ET the next (Saturday) market day.
    let sat = utc(2026, 11, 28, 9, 0, 0);
    assert_eq!(
        daily_bar_freshness(Some(z("2026-11-27T05:00:00Z")), market_day(sat)),
        DailyBarFreshness::Stale
    );
}

#[test]
fn brief_prior_day_close_to_next_0400_flips_exactly_at_the_open() {
    // Monday 09-21's bar is current through Tuesday 03:59:59 ET and stale
    // from 04:00:00 ET (08:00Z, EDT).
    let t = Some(z("2026-09-21T04:00:00Z"));
    assert_eq!(
        daily_bar_freshness(t, market_day(utc(2026, 9, 22, 7, 59, 59))),
        DailyBarFreshness::Current
    );
    assert_eq!(
        daily_bar_freshness(t, market_day(utc(2026, 9, 22, 8, 0, 0))),
        DailyBarFreshness::Stale
    );

    // The cache's day-scoped state clears on the same boundary.
    let mut cache = PremarketVolumeCache::new();
    cache.ingest_bars_for_test(
        "AAA",
        &[(utc(2026, 9, 21, 23, 0, 0), 500)],
        utc(2026, 9, 21, 23, 30, 0),
    );
    assert!(cache.get("AAA").is_some());
    cache.roll(utc(2026, 9, 22, 7, 59, 59));
    assert!(cache.get("AAA").is_some(), "still market day 09-21");
    cache.roll(utc(2026, 9, 22, 8, 0, 0));
    assert!(cache.get("AAA").is_none(), "a new market day starts empty");
    assert_eq!(cache.health_for_test().market_day, Some(day(2026, 9, 22)));
}

// --- causal summation --------------------------------------------------------

#[test]
fn brief_first_premarket_trade_counts_once_its_minute_completes() {
    let d = day(2026, 9, 22);
    let bars = [(utc(2026, 9, 22, 8, 0, 0), 1_200)];
    assert_eq!(
        sum_completed_bars(&bars, d, utc(2026, 9, 22, 8, 0, 30)),
        0,
        "an open minute is not yet observable"
    );
    assert_eq!(
        sum_completed_bars(&bars, d, utc(2026, 9, 22, 8, 1, 0)),
        1_200
    );
}

#[test]
fn brief_multiple_premarket_trades_accumulate_and_prior_day_bars_are_excluded() {
    let d = day(2026, 9, 22);
    let bars = [
        (utc(2026, 9, 22, 7, 59, 0), 999_999), // 03:59 ET: previous market day
        (utc(2026, 9, 22, 8, 0, 0), 100),
        (utc(2026, 9, 22, 9, 15, 0), 250),
        (utc(2026, 9, 22, 12, 2, 0), 650),
        (utc(2026, 9, 22, 12, 2, 0), 650), // repeated across pages
    ];
    assert_eq!(
        sum_completed_bars(&bars, d, utc(2026, 9, 22, 12, 5, 0)),
        1_000
    );
}

#[test]
fn brief_092959_and_093000_see_exactly_the_completed_minutes() {
    // EDT: 09:29 ET = 13:29Z. At 09:29:59 the 09:29 bar is still open.
    let d = day(2026, 9, 22);
    let bars = [
        (utc(2026, 9, 22, 13, 28, 0), 10),
        (utc(2026, 9, 22, 13, 29, 0), 20),
        (utc(2026, 9, 22, 13, 30, 0), 40),
    ];
    assert_eq!(
        sum_completed_bars(&bars, d, utc(2026, 9, 22, 13, 29, 59)),
        10
    );
    assert_eq!(
        sum_completed_bars(&bars, d, utc(2026, 9, 22, 13, 30, 0)),
        30
    );
    // The opening minute's bar only counts at 09:31:00.
    assert_eq!(
        sum_completed_bars(&bars, d, utc(2026, 9, 22, 13, 30, 59)),
        30
    );
    assert_eq!(
        sum_completed_bars(&bars, d, utc(2026, 9, 22, 13, 31, 0)),
        70
    );
}

#[test]
fn brief_winter_dst_open_is_0900z() {
    // EST: 04:00 ET = 09:00Z. An 08:30Z bar is the previous market day.
    let d = day(2026, 1, 15);
    let bars = [
        (utc(2026, 1, 15, 8, 30, 0), 5_000),
        (utc(2026, 1, 15, 9, 0, 0), 7),
    ];
    assert_eq!(sum_completed_bars(&bars, d, utc(2026, 1, 15, 9, 30, 0)), 7);
}

#[test]
fn brief_summer_dst_open_is_0800z() {
    let d = day(2026, 7, 15);
    let bars = [
        (utc(2026, 7, 15, 7, 59, 0), 5_000),
        (utc(2026, 7, 15, 8, 0, 0), 7),
    ];
    assert_eq!(sum_completed_bars(&bars, d, utc(2026, 7, 15, 8, 30, 0)), 7);
}

// --- planning and budget -----------------------------------------------------

#[test]
fn plan_puts_never_fetched_symbols_first_then_the_largest_gap() {
    let now = utc(2026, 9, 22, 12, 0, 30);
    let mut cache = PremarketVolumeCache::new();
    cache.ingest_bars_for_test("OLD", &[], utc(2026, 9, 22, 11, 58, 10));
    let (planned, deferred) = cache.plan(
        &[
            cand("OLD", 400.0),
            cand("BIG", 90.0),
            cand("SMALL", 12.0),
            cand("NEG", -95.0),
        ],
        now,
    );
    assert_eq!(planned, vec!["NEG", "BIG", "SMALL", "OLD"]);
    assert!(deferred.is_empty());
}

#[test]
fn brief_symbol_first_appearing_premarket_is_fetched_first_and_from_the_open() {
    // A symbol that starts gapping at 11:00 ET was never fetched: it outranks
    // everything already cached, and its first fetch sums every bar since
    // 04:00 ET, not since it appeared.
    let mut cache = PremarketVolumeCache::new();
    let now = utc(2026, 9, 22, 15, 0, 10);
    cache.ingest_bars_for_test("VETERAN", &[], utc(2026, 9, 22, 14, 58, 0));
    let (planned, _) = cache.plan(&[cand("VETERAN", 300.0), cand("NEWCOMER", 11.0)], now);
    assert_eq!(planned.first().map(String::as_str), Some("NEWCOMER"));
    let bars = [
        (utc(2026, 9, 22, 8, 5, 0), 300),
        (utc(2026, 9, 22, 14, 59, 0), 700),
    ];
    cache.ingest_bars_for_test("NEWCOMER", &bars, now);
    assert_eq!(cache.get("NEWCOMER").unwrap().volume, 1_000);
}

#[test]
fn a_symbol_is_refreshed_at_most_once_per_minute() {
    let mut cache = PremarketVolumeCache::new();
    cache.ingest_bars_for_test("AAA", &[], utc(2026, 9, 22, 12, 0, 5));
    let (planned, _) = cache.plan(&[cand("AAA", 20.0)], utc(2026, 9, 22, 12, 0, 50));
    assert!(
        planned.is_empty(),
        "same minute: no new completed bar can exist"
    );
    let (planned, _) = cache.plan(&[cand("AAA", 20.0)], utc(2026, 9, 22, 12, 1, 0));
    assert_eq!(planned, vec!["AAA"]);
}

#[test]
fn per_scan_symbol_cap_and_per_minute_request_budget_defer_the_rest() {
    let cache = PremarketVolumeCache::new();
    let many: Vec<VolumeCandidate> = (0..MAX_SYMBOLS_PER_SCAN + 7)
        .map(|i| cand(&format!("S{i:04}"), 50.0))
        .collect();
    let (planned, deferred) = cache.plan(&many, utc(2026, 9, 22, 12, 0, 0));
    assert_eq!(planned.len(), MAX_SYMBOLS_PER_SCAN);
    assert_eq!(deferred.len(), 7);

    // Spend the minute's request budget: nothing more is planned until the
    // next minute.
    let mut cache = PremarketVolumeCache::new();
    let now = utc(2026, 9, 22, 12, 0, 0);
    for i in 0..MAX_REQUESTS_PER_MINUTE {
        cache.ingest_bars_for_test(&format!("X{i}"), &[], now);
    }
    let (planned, deferred) = cache.plan(&[cand("LATE", 30.0)], now + Duration::seconds(15));
    assert!(planned.is_empty());
    assert_eq!(deferred, vec!["LATE"]);
    cache.roll(now + Duration::seconds(60));
    let (planned, _) = cache.plan(&[cand("LATE", 30.0)], now + Duration::seconds(60));
    assert_eq!(planned, vec!["LATE"]);
}

#[test]
fn brief_reconnect_and_restart_resum_from_the_open_to_the_same_value() {
    // A process that has been fetching all morning, and one that (re)started
    // at 09:00 ET, hold the same number at 09:00: both sum since 04:00 ET.
    let bars: Vec<(DateTime<Utc>, u64)> = (0..300)
        .map(|m| {
            (
                utc(2026, 9, 22, 8, 0, 0) + Duration::minutes(m),
                10 + m as u64,
            )
        })
        .collect();
    let mut continuous = PremarketVolumeCache::new();
    for minute in [60, 120, 180, 240] {
        let now = utc(2026, 9, 22, 8, 0, 0) + Duration::minutes(minute);
        continuous.ingest_bars_for_test("RUN", &bars, now);
    }
    let restart_at = utc(2026, 9, 22, 13, 0, 20);
    continuous.ingest_bars_for_test("RUN", &bars, restart_at);
    let mut restarted = PremarketVolumeCache::new();
    restarted.ingest_bars_for_test("RUN", &bars, restart_at);
    assert_eq!(continuous.get("RUN"), restarted.get("RUN"));
    let expected: u64 = (0..300).map(|m| 10 + m as u64).sum();
    assert_eq!(restarted.get("RUN").unwrap().volume, expected);
    assert_eq!(
        restarted.get("RUN").unwrap().as_of,
        utc(2026, 9, 22, 13, 0, 0)
    );
}

#[test]
fn apply_fills_only_unknown_volume_and_never_adds_to_a_current_bar() {
    let mut cache = PremarketVolumeCache::new();
    cache.ingest_bars_for_test(
        "AAA",
        &[(utc(2026, 9, 22, 8, 0, 0), 900)],
        utc(2026, 9, 22, 12, 0, 0),
    );
    let mut current = TickerSnapshot {
        symbol: "AAA".into(),
        price: 1.0,
        float_shares: None,
        avg_daily_volume: 100,
        session_volume: Some(5_000),
        session_volume_source: SessionVolumeSource::SnapshotDailyBarCurrent,
        gap_pct: 0.0,
    };
    assert_eq!(cache.apply(&mut current), None);
    assert_eq!(current.session_volume, Some(5_000), "no double count");
    let mut stale = TickerSnapshot {
        session_volume: None,
        session_volume_source: SessionVolumeSource::Unknown,
        ..current
    };
    assert_eq!(cache.apply(&mut stale), Some(utc(2026, 9, 22, 12, 0, 0)));
    assert_eq!(stale.session_volume, Some(900));
    assert_eq!(
        stale.session_volume_source,
        SessionVolumeSource::MinuteBarsSinceOpen
    );
}

// --- mocked HTTP -------------------------------------------------------------

#[tokio::test]
async fn fetch_is_one_batched_paginated_request_bounded_to_completed_minutes() {
    let (base, log) = crate::test_http::serve(|req| {
        assert_eq!(req.path, "/v2/stocks/bars");
        let body = if req.param("page_token").is_none() {
            r#"{"bars":{"AAA":[{"t":"2026-09-22T08:00:00Z","v":100},{"t":"2026-09-22T11:59:00Z","v":5}],"QUIET":null},"next_page_token":"p2"}"#
        } else {
            // Page two repeats AAA's last bar and carries an open minute.
            r#"{"bars":{"AAA":[{"t":"2026-09-22T11:59:00Z","v":5},{"t":"2026-09-22T12:00:00Z","v":77}],"BBB":[{"t":"2026-09-22T10:00:00Z","v":40}]},"next_page_token":null}"#
        };
        (200, body.to_string())
    });
    let cfg = crate::test_http::config(&base);
    let now = utc(2026, 9, 22, 12, 0, 30);
    let symbols = vec!["AAA".to_string(), "BBB".to_string(), "QUIET".to_string()];
    let (volumes, pages) = fetch_minute_bar_volumes(&cfg, &symbols, day(2026, 9, 22), now)
        .await
        .unwrap();
    assert_eq!(pages, 2);
    assert_eq!(
        volumes.get("AAA"),
        Some(&105),
        "the open 12:00 bar is excluded and the repeat counted once"
    );
    assert_eq!(volumes.get("BBB"), Some(&40));
    assert_eq!(volumes.get("QUIET"), Some(&0));

    let log = log.lock().unwrap();
    let first = &log[0];
    assert_eq!(first.param("symbols"), Some("AAA,BBB,QUIET"));
    assert_eq!(first.param("timeframe"), Some("1Min"));
    assert_eq!(
        first.param("start"),
        Some("2026-09-22T08:00:00+00:00"),
        "04:00 ET under EDT"
    );
    assert_eq!(
        first.param("end"),
        Some("2026-09-22T12:00:00+00:00"),
        "nothing later than the last completed minute"
    );
    assert_eq!(log[1].param("page_token"), Some("p2"));
}

#[tokio::test]
async fn brief_no_premarket_trades_is_a_known_zero_not_unknown() {
    let (base, _) =
        crate::test_http::serve(|_| (200, r#"{"bars":null,"next_page_token":null}"#.to_string()));
    let cfg = crate::test_http::config(&base);
    let mut cache = PremarketVolumeCache::new();
    let now = utc(2026, 9, 22, 9, 30, 0);
    let report = cache.resolve(&cfg, &[cand("NOTHING", 40.0)], now).await;
    assert_eq!(report.requested, vec!["NOTHING"]);
    assert_eq!(
        cache.get("NOTHING"),
        Some(BarVolume {
            volume: 0,
            as_of: now
        })
    );
}

#[tokio::test]
async fn a_failed_fetch_is_counted_and_fails_closed_then_keeps_an_earlier_value() {
    let fail = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let flag = std::sync::Arc::clone(&fail);
    let (base, _) = crate::test_http::serve(move |_| {
        if flag.load(std::sync::atomic::Ordering::SeqCst) {
            (500, "{}".to_string())
        } else {
            (
                200,
                r#"{"bars":{"AAA":[{"t":"2026-09-22T08:00:00Z","v":300}]},"next_page_token":null}"#
                    .to_string(),
            )
        }
    });
    let cfg = crate::test_http::config(&base);
    let mut cache = PremarketVolumeCache::new();

    let t0 = utc(2026, 9, 22, 9, 0, 0);
    let report = cache
        .resolve(&cfg, &[cand("AAA", 40.0), cand("BBB", 30.0)], t0)
        .await;
    assert_eq!(report.failed.len(), 2);
    assert_eq!(cache.get("AAA"), None, "unknown stays unknown");
    assert_eq!(cache.health_for_test().fetch_failures, 1);
    assert_eq!(cache.health_for_test().init_failures, 2);
    assert!(cache.health_for_test().initialized_at.is_none());

    fail.store(false, std::sync::atomic::Ordering::SeqCst);
    let t1 = t0 + Duration::minutes(1);
    cache.resolve(&cfg, &[cand("AAA", 40.0)], t1).await;
    assert_eq!(
        cache.get("AAA"),
        Some(BarVolume {
            volume: 300,
            as_of: t1
        })
    );
    assert_eq!(cache.health_for_test().initialized_at, Some(t1));
    assert_eq!(cache.health_for_test().last_successful_fetch_at, Some(t1));

    fail.store(true, std::sync::atomic::Ordering::SeqCst);
    let t2 = t1 + Duration::minutes(1);
    cache.resolve(&cfg, &[cand("AAA", 40.0)], t2).await;
    assert_eq!(
        cache.get("AAA"),
        Some(BarVolume {
            volume: 300,
            as_of: t1
        }),
        "an earlier causal value survives a failure"
    );
    assert_eq!(cache.health_for_test().fetch_failures, 2);
    assert_eq!(
        cache.health_for_test().init_failures,
        2,
        "AAA was already initialised"
    );
}

#[test]
fn health_block_is_bounded_and_uses_the_documented_names() {
    let mut cache = PremarketVolumeCache::new();
    let now = utc(2026, 9, 22, 12, 0, 0);
    cache.roll(now);
    cache.publish_scan(
        ScanVolumeCounts {
            by_source: VolumeSourceCounts {
                snapshot_daily_bar_current: 1,
                minute_bars_since_open: 2,
                unknown: 13_000,
            },
            survivors_needing_volume: 5,
            survivors_resolved: 3,
            survivors_deferred: 1,
            survivors_no_trade_since_open: 1,
        },
        now,
    );
    // Read the cache's own stamped block rather than the process-global
    // slot, which other tests running in parallel also publish to.
    let json = serde_json::to_value(cache.stamped()).unwrap();
    assert!(health().is_some(), "publish_scan fills the global slot");
    for key in [
        "marketDay",
        "initializedAt",
        "lastScanAt",
        "lastSuccessfulFetchAt",
        "bySource",
        "survivorsNeedingVolume",
        "survivorsResolved",
        "survivorsDeferred",
        "survivorsNoTradeSinceOpen",
        "fetchFailures",
        "initFailures",
        "lastFetchError",
        "requestsThisMinute",
        "requestsThisMarketDay",
        "cachedSymbols",
        "maxSymbolsPerScan",
        "symbolsPerRequest",
        "maxRequestsPerMinute",
    ] {
        assert!(json.get(key).is_some(), "missing {key}");
    }
    assert_eq!(json["marketDay"], "2026-09-22");
    assert_eq!(json["bySource"]["snapshotDailyBarCurrent"], 1);
    assert_eq!(json["bySource"]["minuteBarsSinceOpen"], 2);
    assert_eq!(json["bySource"]["unknown"], 13_000);
    assert!(json["fetchFailures"].is_u64() && json["initFailures"].is_u64());
    assert_eq!(json["maxSymbolsPerScan"], MAX_SYMBOLS_PER_SCAN);
}
