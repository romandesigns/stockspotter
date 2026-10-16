use super::*;
use chrono::TimeZone;

/// 2026-10-15 is a Thursday under EDT: the market day opens 04:00 ET =
/// 08:00Z, the regular session 09:30 ET = 13:30Z.
fn at(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 15, h, m, 0).unwrap()
}

fn funnel(symbol: &str, t: DateTime<Utc>, price: f64, gap_pct: f64, rv: Option<f64>) -> ScanEvent {
    ScanEvent::FunnelSignal {
        symbol: symbol.into(),
        timestamp: t,
        price,
        gap_pct,
        session_volume: 1_000_000,
        float_shares: Some(4_000_000),
        relative_volume: rv,
        price_ok: (0.10..=20.50).contains(&price), // the scanner range
        float_ok: true,
        rel_vol_ok: rv.unwrap_or(0.0) >= 5.0,
        gap_ok: gap_pct >= 10.0,
        passed: true,
    }
}

fn bar(symbol: &str, t: DateTime<Utc>, h: f64, l: f64, c: f64, v: u64) -> ScanEvent {
    ScanEvent::BarUpdate { is_final: true, symbol: symbol.into(), timestamp: t, open: c, high: h, low: l, close: c, volume: v, interval_secs: 60 }
}

fn no_quotes(_: &str) -> Option<LatestQuote> {
    None
}

fn feed(s: &mut Shortlist, events: &[ScanEvent]) {
    for e in events {
        s.on_event(e);
    }
}

#[test]
fn ranks_by_gap_times_relative_volume_best_first() {
    let mut s = Shortlist::default();
    let t = at(13, 40);
    feed(&mut s, &[
        funnel("AAA", t, 3.0, 20.0, Some(5.0)),  // 100
        funnel("BBB", t, 3.0, 10.0, Some(30.0)), // 300
        funnel("CCC", t, 3.0, 50.0, Some(4.0)),  // 200
    ]);
    let body = s.body(t, no_quotes, None);
    let order: Vec<_> = body.candidates.iter().map(|c| (c.rank, c.symbol.as_str())).collect();
    assert_eq!(order, vec![(1, "BBB"), (2, "CCC"), (3, "AAA")]);
    assert_eq!(body.candidates[0].score, 300.0);
    assert_eq!(body.pool_size, 3);
    assert!(body.ranked_by.contains("not an entry signal"));
}

#[test]
fn exclusions_are_counted_not_hidden() {
    let mut s = Shortlist::default();
    let t = at(13, 40);
    feed(&mut s, &[
        funnel("CHEAP", t, 0.05, 30.0, Some(10.0)),
        funnel("DOWN", t, 3.0, -5.0, Some(10.0)),
        funnel("NORV", t, 3.0, 30.0, None),
        funnel("OK", t, 3.0, 30.0, Some(10.0)),
    ]);
    let body = s.body(t, no_quotes, None);
    assert_eq!(body.candidates.len(), 1);
    assert_eq!(body.excluded, Excluded { outside_price_range: 1, not_gapping_up: 1, no_relative_volume: 1 });
    assert_eq!(body.pool_size, 4);
}

#[test]
fn stale_funnel_readings_drop_out() {
    let mut s = Shortlist::default();
    feed(&mut s, &[funnel("OLD", at(13, 30), 3.0, 30.0, Some(10.0))]);
    assert_eq!(s.body(at(13, 40), no_quotes, None).candidates.len(), 1);
    assert_eq!(s.body(at(13, 41), no_quotes, None).candidates.len(), 0, "older than FUNNEL_FRESH_SECS");
}

#[test]
fn only_the_top_n_are_shown() {
    let mut s = Shortlist::default();
    let t = at(13, 40);
    for i in 0..(TOP_N + 5) {
        s.on_event(&funnel(&format!("S{i:02}"), t, 3.0, 10.0 + i as f64, Some(5.0)));
    }
    let body = s.body(t, no_quotes, None);
    assert_eq!(body.candidates.len(), TOP_N);
    assert_eq!(body.candidates[0].symbol, format!("S{:02}", TOP_N + 4));
}

#[test]
fn levels_split_premarket_from_regular_and_anchor_vwap_at_the_open() {
    let mut s = Shortlist::default();
    feed(&mut s, &[
        funnel("AAA", at(13, 35), 3.0, 30.0, Some(10.0)),
        bar("AAA", at(8, 0), 2.5, 2.0, 2.2, 1000),    // 04:00 ET premarket
        bar("AAA", at(12, 0), 3.4, 2.6, 3.0, 1000),   // 08:00 ET premarket high 3.4
        bar("AAA", at(13, 30), 3.6, 3.0, 3.3, 2000),  // 09:30 ET regular
        bar("AAA", at(13, 31), 3.3, 3.0, 3.0, 2000),
    ]);
    // Before the open, VWAP is anchored at 04:00 over premarket bars.
    let pre = s.body(at(13, 0), no_quotes, None);
    assert!(pre.candidates.is_empty() || pre.candidates[0].levels.vwap_anchor == Some("premarket"));

    let body = s.body(at(13, 35), no_quotes, None);
    let l = &body.candidates[0].levels;
    assert_eq!(l.premarket_high, Some(3.4));
    assert_eq!(l.premarket_low, Some(2.0));
    assert_eq!(l.day_high, Some(3.6));
    assert_eq!(l.vwap_anchor, Some("regular"));
    let expected_vwap = ((3.6 + 3.0 + 3.3) / 3.0 * 2000.0 + (3.3 + 3.0 + 3.0) / 3.0 * 2000.0) / 4000.0;
    assert!((l.vwap.unwrap() - expected_vwap).abs() < 1e-9);
    assert!(l.bars_complete, "first bar at 04:00 ET covers the premarket");
    assert!(l.dollar_volume.unwrap() > 0.0);
}

#[test]
fn late_bars_are_marked_incomplete_until_backfilled() {
    let mut s = Shortlist::default();
    let t = at(13, 40);
    feed(&mut s, &[funnel("AAA", t, 3.0, 30.0, Some(10.0)), bar("AAA", at(13, 39), 3.1, 3.0, 3.05, 500)]);
    assert!(!s.body(t, no_quotes, None).candidates[0].levels.bars_complete);

    assert_eq!(s.take_backfill_requests(t), vec!["AAA".to_string()]);
    assert!(s.take_backfill_requests(t).is_empty(), "pending is not requested twice");

    s.merge_backfill("AAA", vec![(at(8, 1).timestamp(), MinuteBar { open: 2.0, high: 2.4, low: 1.9, close: 2.1, volume: 300 })], true, t);
    let l = s.body(t, no_quotes, None).candidates[0].levels.clone();
    assert!(l.bars_complete);
    assert_eq!(l.premarket_high, Some(2.4));
}

#[test]
fn a_failed_backfill_is_retried_after_the_backoff() {
    let mut s = Shortlist::default();
    let t = at(13, 40);
    feed(&mut s, &[funnel("AAA", t, 3.0, 30.0, Some(10.0))]);
    assert_eq!(s.take_backfill_requests(t).len(), 1);
    s.merge_backfill("AAA", std::iter::empty(), false, t);
    assert!(s.take_backfill_requests(t + chrono::Duration::seconds(BACKFILL_RETRY_SECS - 1)).is_empty());
    let later = t + chrono::Duration::seconds(BACKFILL_RETRY_SECS);
    s.on_event(&funnel("AAA", later, 3.0, 30.0, Some(10.0)));
    assert_eq!(s.take_backfill_requests(later).len(), 1);
}

#[test]
fn live_bars_win_over_backfilled_bars_for_the_same_minute() {
    let mut s = Shortlist::default();
    let t = at(13, 40);
    feed(&mut s, &[funnel("AAA", t, 3.0, 30.0, Some(10.0)), bar("AAA", at(8, 0), 9.0, 1.0, 5.0, 100)]);
    s.merge_backfill("AAA", vec![(at(8, 0).timestamp(), MinuteBar { open: 2.0, high: 2.1, low: 1.9, close: 2.0, volume: 50 })], true, t);
    assert_eq!(s.body(t, no_quotes, None).candidates[0].levels.premarket_high, Some(9.0));
}

#[test]
fn quote_and_halt_band_are_stated_in_prices_and_percent() {
    let mut s = Shortlist::default();
    let t = at(13, 40);
    feed(&mut s, &[
        funnel("AAA", t, 3.0, 30.0, Some(10.0)),
        ScanEvent::HaltWarning {
            estimated_bands: false,
            symbol: "AAA".into(),
            timestamp: t,
            reference_price: 3.0,
            current_price: 3.0,
            band_width_dollars: 0.30,
            band_doubled: false,
            proximity_ratio: 0.0,
            relative_volume: None,
            level: HaltAlertLevel::Calm,
            luld_in_effect: true,
        },
    ]);
    let quote = |sym: &str| (sym == "AAA").then_some(LatestQuote { bid: 2.99, ask: 3.01, bid_size: 1, ask_size: 1, at: t - chrono::Duration::seconds(2) });
    let c = s.body(t, quote, None).candidates[0].clone();
    let q = c.quote.unwrap();
    assert!((q.spread_pct - 0.02 / 3.0 * 100.0).abs() < 1e-9);
    assert_eq!(q.age_secs, 2);
    let h = c.halt.unwrap();
    assert!((h.upper_band - 3.30).abs() < 1e-12 && (h.lower_band - 2.70).abs() < 1e-12);
    assert!((h.distance_to_upper_pct - 10.0).abs() < 1e-9);
}

#[test]
fn halt_warnings_for_unknown_symbols_are_not_kept() {
    let mut s = Shortlist::default();
    s.on_event(&ScanEvent::HaltWarning {
        estimated_bands: true,
        symbol: "ZZZ".into(),
        timestamp: at(13, 40),
        reference_price: 1.0,
        current_price: 1.0,
        band_width_dollars: 0.1,
        band_doubled: false,
        proximity_ratio: 0.0,
        relative_volume: None,
        level: HaltAlertLevel::Calm,
        luld_in_effect: true,
    });
    assert!(s.symbols.is_empty());
}

#[test]
fn the_market_day_boundary_clears_yesterday() {
    let mut s = Shortlist::default();
    feed(&mut s, &[funnel("AAA", at(13, 40), 3.0, 30.0, Some(10.0))]);
    let tomorrow = Utc.with_ymd_and_hms(2026, 10, 16, 8, 1, 0).unwrap(); // 04:01 ET next day
    s.on_event(&funnel("BBB", tomorrow, 3.0, 20.0, Some(10.0)));
    assert!(!s.symbols.contains_key("AAA"));
    assert_eq!(s.body(tomorrow, no_quotes, None).candidates[0].symbol, "BBB");
}

#[test]
fn the_body_serializes_in_camel_case() {
    let mut s = Shortlist::default();
    let t = at(13, 40);
    feed(&mut s, &[funnel("AAA", t, 3.0, 30.0, Some(10.0))]);
    let json = serde_json::to_value(s.body(t, no_quotes, None)).unwrap();
    for key in ["generatedAt", "marketDay", "rankedBy", "poolSize", "excluded", "candidates"] {
        assert!(json.get(key).is_some(), "missing {key}");
    }
    let c = &json["candidates"][0];
    for key in ["gapPct", "relativeVolume", "floatShares", "funnelPassed", "levels"] {
        assert!(c.get(key).is_some(), "missing candidate.{key}");
    }
    assert_eq!(json["session"], "regular");
}

#[test]
fn candidates_carry_the_track_record_of_their_profile() {
    use backtest_metrics::base_rates::{aggregate, Observation, Outcome, Skipped};
    let o = |outcome| Observation {
        symbol: "X".into(),
        day: NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
        gap_pct: 30.0,
        relative_volume: 5.0,
        entry: 1.0,
        entry_spread_pct: 1.0,
        outcome,
        close_return_net_pct: -1.0,
        max_adverse_pct: -3.0,
    };
    let mut obs: Vec<_> = (0..12).map(|_| o(Outcome::Target)).collect();
    obs.extend((0..18).map(|_| o(Outcome::Stop)));
    let rates = aggregate(&obs, 30, &Skipped::default(), "fixture", at(21, 0));

    let mut s = Shortlist::default();
    let t = at(13, 40);
    feed(&mut s, &[funnel("IN", t, 3.0, 30.0, Some(5.0)), funnel("THIN", t, 3.0, 60.0, Some(20.0))]);
    let body = s.body(t, no_quotes, Some(&rates));
    let by = |sym: &str| body.candidates.iter().find(|c| c.symbol == sym).unwrap().track_record.clone().unwrap();

    let tr = by("IN");
    assert_eq!(tr.profile, "25-50% day change, 3-10x relative volume");
    assert!(tr.sufficient);
    assert_eq!(tr.n, 30);
    assert!((tr.target_rate.unwrap() - 0.4).abs() < 1e-12);
    assert_eq!(tr.decision_time_et, "09:35");

    let thin = by("THIN");
    assert_eq!(thin.n, 0);
    assert!(!thin.sufficient);
    assert_eq!(thin.target_rate, None, "no rate without a sample");

    assert!(s.body(t, no_quotes, None).candidates.iter().all(|c| c.track_record.is_none()));
}
