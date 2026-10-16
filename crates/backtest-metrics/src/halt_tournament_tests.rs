use chrono::TimeZone;

use super::*;

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 8, 28).unwrap()
}

/// Unix seconds of hh:mm:ss Eastern (EDT, UTC-4) on `day()`.
fn et(h: u32, m: u32, s: u32) -> i64 {
    Utc.with_ymd_and_hms(2026, 8, 28, h + 4, m, s).unwrap().timestamp()
}

fn reading(at_secs: i64, ratio: f64, up: bool) -> Reading {
    Reading { at_ms: at_secs * 1000, price: if up { 1.2 } else { 0.8 }, reference: 1.0, ratio, relative_volume: Some(2.0), vwap: 1.0, luld_in_effect: at_secs >= et(9, 30, 0) }
}

fn fire(t: &mut Tracker, rule: &Rule, at: i64, ratio: f64) -> bool {
    t.observe(rule, &reading(at, ratio, true)).is_some()
}

fn rule(id: &str) -> Rule {
    candidates().into_iter().find(|c| c.id == id).unwrap().rule
}

#[test]
fn eight_candidates_in_preregistered_order() {
    let ids: Vec<&str> = candidates().iter().map(|c| c.id).collect();
    assert_eq!(ids, ["G70", "G60", "G80", "G90", "G70-FF", "G70-REG", "G70-2ND", "G50-RV-VWAP"]);
    assert!(candidates().iter().enumerate().all(|(i, c)| c.index == i));
}

#[test]
fn gauge_pct_mirrors_speed_alert_rs() {
    assert_eq!(gauge_pct(0.696), 70);
    assert_eq!(gauge_pct(0.694), 69);
    assert_eq!(gauge_pct(0.005), 1);
    assert_eq!(gauge_pct(1.7), 100);
    assert_eq!(gauge_pct(-0.2), 0);
    assert_eq!(gauge_pct(f64::NAN), 0);
}

#[test]
fn live_rule_baseline_crossing_up_only_and_staying_above() {
    let g70 = rule("G70");
    let t0 = et(10, 0, 0);
    let mut t = Tracker::default();
    assert!(!fire(&mut t, &g70, t0, 0.95), "first reading is a baseline");
    let mut t = Tracker::default();
    fire(&mut t, &g70, t0, 0.10);
    assert!(!fire(&mut t, &g70, t0 + 1, 0.69));
    assert!(fire(&mut t, &g70, t0 + 2, 0.70), "exactly 70 counts");
    assert!(!fire(&mut t, &g70, t0 + 3, 0.90), "staying above does not re-fire");
    // A down move through the threshold is skipped and still becomes the previous pct.
    let mut t = Tracker::default();
    t.observe(&g70, &reading(t0, 0.5, false));
    assert!(t.observe(&g70, &reading(t0 + 1, 0.75, false)).is_none());
    assert!(!fire(&mut t, &g70, t0 + 2, 0.80));
}

#[test]
fn cooldown_is_ten_minutes_and_stale_readings_are_ignored() {
    let g70 = rule("G70");
    let t0 = et(10, 0, 0);
    let mut t = Tracker::default();
    fire(&mut t, &g70, t0, 0.1);
    assert!(fire(&mut t, &g70, t0 + 1, 0.75));
    fire(&mut t, &g70, t0 + 1 + 9 * 60 - 1, 0.1);
    assert!(!fire(&mut t, &g70, t0 + 1 + 9 * 60, 0.75));
    fire(&mut t, &g70, t0 + 1 + 10 * 60 - 1, 0.1);
    assert!(fire(&mut t, &g70, t0 + 1 + 10 * 60, 0.75));
    let mut t = Tracker::default();
    fire(&mut t, &g70, t0 + 10, 0.1);
    assert!(!fire(&mut t, &g70, t0 + 5, 0.9), "older reading ignored");
    assert!(!fire(&mut t, &g70, t0 + 10, 0.9), "same millisecond ignored");
    assert!(fire(&mut t, &g70, t0 + 11, 0.9));
}

#[test]
fn filling_fast_gate_compares_with_the_newest_point_a_minute_older() {
    let ff = rule("G70-FF");
    let t0 = et(10, 0, 0);
    let mut t = Tracker::default();
    fire(&mut t, &ff, t0, 0.50);
    fire(&mut t, &ff, t0 + 30, 0.60);
    assert!(fire(&mut t, &ff, t0 + 60, 0.70), "+20 points in 60 s");
    let mut t = Tracker::default();
    fire(&mut t, &ff, t0, 0.30);
    fire(&mut t, &ff, t0 + 20, 0.60);
    assert!(!fire(&mut t, &ff, t0 + 80, 0.72), "newest point >= 60 s older is 60, delta 12");
    // Without a minute of history it cannot be filling fast.
    let mut t = Tracker::default();
    fire(&mut t, &ff, t0, 0.10);
    assert!(!fire(&mut t, &ff, t0 + 5, 0.90));
}

#[test]
fn regular_only_skips_premarket_but_still_tracks_its_readings() {
    let reg = rule("G70-REG");
    let g70 = rule("G70");
    let pre = et(8, 0, 0);
    let (mut a, mut b) = (Tracker::default(), Tracker::default());
    for (t, r) in [(&mut a, &reg), (&mut b, &g70)] {
        fire(t, r, pre, 0.1);
    }
    assert!(!fire(&mut a, &reg, pre + 1, 0.8));
    assert!(fire(&mut b, &g70, pre + 1, 0.8));
    // Still above 70 at the open: no crossing, so no regular alert either.
    assert!(!fire(&mut a, &reg, et(9, 30, 5), 0.85));
    fire(&mut a, &reg, et(9, 31, 0), 0.2);
    assert!(fire(&mut a, &reg, et(9, 32, 0), 0.75));
}

#[test]
fn second_push_needs_a_dip_below_40_and_a_recross_within_15_minutes() {
    let sp = rule("G70-2ND");
    let t0 = et(10, 0, 0);
    let mut t = Tracker::default();
    fire(&mut t, &sp, t0, 0.1);
    assert!(!fire(&mut t, &sp, t0 + 60, 0.75), "the first push never fires");
    fire(&mut t, &sp, t0 + 120, 0.55);
    assert!(!fire(&mut t, &sp, t0 + 180, 0.72), "re-cross without a dip below 40");
    fire(&mut t, &sp, t0 + 240, 0.35);
    assert!(fire(&mut t, &sp, t0 + 300, 0.71), "dip then re-cross 2 min after the previous push");
    // Too slow: dip, then re-cross more than 15 minutes after the last push.
    let mut t = Tracker::default();
    fire(&mut t, &sp, t0, 0.1);
    fire(&mut t, &sp, t0 + 60, 0.75);
    fire(&mut t, &sp, t0 + 120, 0.2);
    assert!(!fire(&mut t, &sp, t0 + 60 + 15 * 60 + 1, 0.75));
    // That late crossing is itself a push: a quick second one now fires.
    fire(&mut t, &sp, t0 + 60 + 16 * 60, 0.3);
    assert!(fire(&mut t, &sp, t0 + 60 + 17 * 60, 0.75));
}

#[test]
fn confluence_needs_relative_volume_and_price_over_vwap() {
    let cf = rule("G50-RV-VWAP");
    let t0 = et(10, 0, 0);
    let mk = |at: i64, ratio: f64, rvol: Option<f64>, vwap: f64| Reading { at_ms: at * 1000, price: 1.2, reference: 1.0, ratio, relative_volume: rvol, vwap, luld_in_effect: true };
    for (rvol, vwap, expect) in [(Some(1.0), 1.1, true), (Some(0.99), 1.1, false), (None, 1.1, false), (Some(3.0), 1.21, false)] {
        let mut t = Tracker::default();
        t.observe(&cf, &mk(t0, 0.3, rvol, vwap));
        assert_eq!(t.observe(&cf, &mk(t0 + 1, 0.5, rvol, vwap)).is_some(), expect, "{rvol:?} {vwap}");
    }
}

#[test]
fn decisions_never_depend_on_later_readings() {
    // Alerts on a prefix equal the full run's alerts inside that prefix.
    let g = rule("G70-2ND");
    let t0 = et(9, 40, 0);
    let rs: Vec<Reading> = (0..400).map(|i| reading(t0 + i * 7, ((i as f64) * 0.37).sin().abs(), i % 5 != 0)).collect();
    let run = |rs: &[Reading]| {
        let mut t = Tracker::default();
        rs.iter().filter(|r| t.observe(&g, r).is_some()).map(|r| r.at_ms).collect::<Vec<_>>()
    };
    let full = run(&rs);
    assert!(!full.is_empty());
    let cut = rs[200].at_ms;
    assert_eq!(run(&rs[..=200]), full.into_iter().filter(|t| *t <= cut).collect::<Vec<_>>());
}

#[test]
fn readings_use_the_real_monitor_and_a_point_in_time_vwap() {
    let t0 = et(10, 0, 0);
    let mk = |s: i64, p: f64, size: u64| TradeIn { price: p, size, timestamp: DateTime::<Utc>::from_timestamp(s, 0).unwrap(), conditions: vec![] };
    let mut trades: Vec<TradeIn> = (0..10).map(|i| mk(t0 + i, 5.0, 100)).collect();
    trades.push(mk(t0 + 10, 6.0, 100));
    trades.push(mk(t0 + 11, -1.0, 100)); // dropped
    trades.reverse(); // order must not matter
    let rs = readings(&trades, 1_000);
    assert_eq!(rs.len(), 11);
    assert!(rs.windows(2).all(|w| w[0].at_ms <= w[1].at_ms));
    let last = rs.last().unwrap();
    assert!(last.ratio > 1.0, "the monitor test's own Red scenario");
    assert!((last.vwap - (5.0 * 10.0 + 6.0) / 11.0).abs() < 1e-12);
    assert!((rs[0].vwap - 5.0).abs() < 1e-12);
    assert_eq!(last.relative_volume, Some(1.1));
    assert!(last.luld_in_effect);
}

#[test]
fn entry_instant_is_the_next_minute_boundary() {
    let at = et(9, 31, 10) * 1000 + 500;
    assert_eq!(entry_instant(at), et(9, 32, 0));
    assert_eq!(entry_instant(et(9, 32, 0) * 1000), et(9, 33, 0), "a print at :00.000 still enters a minute later");
}

fn bar(start: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
    Bar { symbol: "AAA".into(), open: o, high: h, low: l, close: c, volume: 1000, timestamp: DateTime::<Utc>::from_timestamp(start, 0).unwrap() }
}

fn quote(at: i64, bid: f64, ask: f64) -> QuoteIn {
    QuoteIn { bid, ask, timestamp: DateTime::<Utc>::from_timestamp(at, 0).unwrap() }
}

/// Flat $10 bars every minute 04:00-16:00, closes stepping +0.01 per minute
/// from 10:00 so horizon values are distinguishable.
fn market(quotes: Vec<QuoteIn>) -> Market {
    let clock = Clock::of(day()).unwrap();
    let mut bars = Vec::new();
    let mut s = clock.day_open;
    while s < clock.close {
        let c = if s >= et(10, 0, 0) { 10.0 + (s - et(10, 0, 0)) as f64 / 60.0 * 0.01 } else { 10.0 };
        bars.push(bar(s, c, c + 0.005, c - 0.005, c));
        s += 60;
    }
    Market::new(clock, day(), &bars, &quotes)
}

#[test]
fn clock_windows_and_close() {
    let c = Clock::of(day()).unwrap();
    assert_eq!(c.day_open, et(4, 0, 0));
    assert_eq!(c.regular_open, et(9, 30, 0));
    assert_eq!(c.close, et(16, 0, 0));
    assert_eq!(c.window(et(9, 29, 59)), Some(Window::Premarket));
    assert_eq!(c.window(et(9, 30, 0)), Some(Window::Regular));
    assert_eq!(c.window(et(16, 0, 0)), None);
    assert_eq!(c.window(et(3, 59, 59)), None);
}

#[test]
fn quote_entry_at_the_ask_with_half_spread_exits() {
    let m = market(vec![quote(et(10, 0, 0) - 30, 9.99, 10.01), quote(et(10, 0, 0), 5.0, 50.0)]);
    let x = measure(&m, et(10, 0, 0)).unwrap();
    assert_eq!(x.cost_model, CostModel::Quote);
    assert_eq!(x.entry, 10.01, "the quote AT the instant is not known yet");
    let spread = 0.02 / 10.0 * 100.0;
    assert!((x.spread_pct - spread).abs() < 1e-12);
    // 30 min: last bar ended by 10:30 is the 10:29 bar, close 10.29.
    let want = (10.29 * (1.0 - spread / 200.0) - 10.01) / 10.01 * 100.0;
    assert!((x.net[PRIMARY].unwrap() - want).abs() < 1e-9);
    assert_eq!(x.entry_at, et(10, 0, 0));
}

#[test]
fn stale_or_missing_quote_falls_back_to_the_proxy() {
    let m = market(vec![quote(et(10, 0, 0) - 121, 9.99, 10.01)]);
    let x = measure(&m, et(10, 0, 0)).unwrap();
    assert_eq!(x.cost_model, CostModel::Proxy);
    assert!((x.entry - 10.0 * 1.0025).abs() < 1e-12);
    assert_eq!(x.spread_pct, PROXY_ROUND_TRIP_PCT);
    // A proxy needs a bar within 300 s.
    let clock = Clock::of(day()).unwrap();
    let sparse = Market::new(clock, day(), &[bar(et(10, 10, 0), 10.0, 10.0, 10.0, 10.0)], &[]);
    assert!(measure(&sparse, et(10, 4, 59)).is_none());
    assert_eq!(measure(&sparse, et(10, 5, 0)).unwrap().entry_at, et(10, 10, 0));
}

#[test]
fn horizons_past_the_close_are_censored_not_truncated() {
    let m = market(vec![]);
    let x = measure(&m, et(15, 40, 0)).unwrap();
    assert!(x.net[0].is_some() && x.net[1].is_some());
    assert!(x.net[PRIMARY].is_none() && x.net[3].is_none());
    assert!(x.net[4].is_some());
}

#[test]
fn session_alerts_enter_after_the_print_with_a_deterministic_matched_baseline() {
    let m = market(vec![]);
    let clock = m.clock;
    let t0 = et(10, 0, 0);
    let rs = vec![reading(et(8, 0, 0), 0.1, true), reading(t0 + 5, 0.1, true), reading(t0 + 10, 0.75, true), reading(et(15, 45, 0), 0.1, true), reading(et(15, 46, 0), 0.9, true)];
    let cands = candidates();
    let a = evaluate_session(&cands, "AAA", day(), clock, &m, &rs, K_RANDOM, RANDOM_SEED);
    let b = evaluate_session(&cands, "AAA", day(), clock, &m, &rs, K_RANDOM, RANDOM_SEED);
    assert_eq!(a, b);
    let (rows, counts) = &a[0];
    assert_eq!(counts.raised, 2);
    assert_eq!(counts.late, 1, "15:46 enters 15:47; 30 min would pass the close");
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!(r.window, Window::Regular);
    assert!(r.measured.entry_at * 1000 > r.alert_at.timestamp_millis());
    assert_eq!(r.measured.entry_at, et(10, 1, 0));
    assert!(r.baseline[PRIMARY].is_some());
    // A different seed draws different random minutes.
    let c = evaluate_session(&cands, "AAA", day(), clock, &m, &rs, K_RANDOM, RANDOM_SEED + 1);
    assert_ne!(c[0].0[0].baseline, r.baseline);
}

#[test]
fn random_pool_respects_windows_and_the_primary_horizon() {
    let m = market(vec![]);
    let pool = random_pool(&m);
    assert!(pool.iter().any(|p| p.0 == Window::Premarket));
    assert!(pool.iter().all(|p| p.1.entry_at + PRIMARY_SECS <= m.clock.close));
    // Last eligible start is 15:30.
    assert_eq!(pool.iter().map(|p| p.1.entry_at).max(), Some(et(15, 30, 0)));
}

#[test]
fn diff_replicates_match_the_shared_bootstrap() {
    let trades: Vec<Paired<u32>> = (0..60).map(|i| Paired { cluster: i % 11, ret: (i as f64 * 0.91).sin() * 3.0, random_mean: (i as f64 * 0.13).cos() }).collect();
    let shared = cluster_bootstrap(&trades, 3000, 7).unwrap().1;
    let mine = percentile_interval(diff_replicates(&trades, 3000, 7).unwrap(), 0.05);
    assert_eq!(shared, mine);
    let adj = percentile_interval(diff_replicates(&trades, 3000, 7).unwrap(), 0.05 / 8.0);
    assert!(adj.lo <= mine.lo && adj.hi >= mine.hi, "the adjusted interval is wider");
    let one: Vec<Paired<u32>> = trades.iter().map(|t| Paired { cluster: 0, ..t.clone() }).collect();
    assert!(diff_replicates(&one, 100, 1).is_none());
}

fn row(day_n: u32, sym: &str, ret: f64, base: f64) -> AlertRow {
    let d = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap() + chrono::Duration::days(i64::from(day_n));
    let mut net = [Some(ret); N_H];
    net[3] = None;
    AlertRow {
        symbol: sym.into(),
        day: d,
        alert_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
        window: Window::Regular,
        price: 5.0,
        pct: 70,
        filling_fast: false,
        measured: Measured { cost_model: CostModel::Quote, entry: 5.0, spread_pct: 0.4, entry_at: 0, net, outcome: Outcome::Neither },
        baseline: [Some(base); N_H],
        baseline_target_rate: Some(0.0),
    }
}

#[test]
fn selection_floors_winner_and_bonferroni_qualification() {
    // 60 alerts over 12 dates, each +1 point over random: clearly positive.
    let good: Vec<AlertRow> = (0..60).map(|i| row(i % 12, if i % 2 == 0 { "A" } else { "B" }, 1.0 + (i as f64 * 0.7).sin() * 0.3, 0.0)).collect();
    let s_good = summarize("good", &good, Counts::default(), 8, 2000, 1);
    assert_eq!(s_good.n, 60);
    assert_eq!(s_good.dates, 12);
    assert!((s_good.adjusted_level - (1.0 - 0.05 / 8.0)).abs() < 1e-12);
    assert!(eligible(&s_good) && qualifies(&s_good));
    assert!(s_good.diff_ci_adjusted.unwrap().lo <= s_good.diff_ci.unwrap().lo);
    assert_eq!(s_good.horizons[3].n, 0, "censored horizon has no values");
    // Too few dates: not eligible however good.
    let few: Vec<AlertRow> = (0..60).map(|i| row(i % 9, "A", 5.0 + (i as f64).sin(), 0.0)).collect();
    let s_few = summarize("few", &few, Counts::default(), 8, 2000, 1);
    assert!(!eligible(&s_few));
    // Noise around zero: eligible, does not qualify.
    let noise: Vec<AlertRow> = (0..80).map(|i| row(i % 16, "A", (i as f64 * 1.3).sin() * 4.0, 0.0)).collect();
    let s_noise = summarize("noise", &noise, Counts::default(), 8, 2000, 1);
    assert!(eligible(&s_noise) && !qualifies(&s_noise));
    let all = vec![s_noise.clone(), s_few.clone(), s_good.clone()];
    assert_eq!(winner(&all).unwrap().id, "good");
    assert!(winner(&[s_few]).is_none());
    assert_eq!(winner(&[s_noise.clone()]).unwrap().id, "noise");
    assert!(!confirms(&s_noise));
    assert!(confirms(&s_good));
}

#[test]
fn file_names_and_the_split_cut() {
    assert_eq!(date_from_file_name("AAPL-2026-04-20-sip-v1.json"), NaiveDate::from_ymd_opt(2026, 4, 20));
    assert_eq!(date_from_file_name("BRK-B-2026-04-20-sip-v1.json"), NaiveDate::from_ymd_opt(2026, 4, 20));
    assert_eq!(date_from_file_name("AAPL-2026-04-20.json"), None);
    let d = |n: u32| NaiveDate::from_ymd_opt(2026, 3, n).unwrap();
    assert_eq!(split_cut(&[d(1), d(2), d(2), d(3)]), Some(d(2)));
    assert_eq!(split_cut(&[d(1), d(2), d(3), d(4)]), Some(d(2)));
    assert_eq!(split_cut(&[]), None);
}

#[test]
fn day_file_parses_the_cache_shape() {
    let json = r#"{"session_seeds":{},"symbol":"AAA","prior_close":2.0,"avg_daily_volume":100,"float_shares":null,
      "bars":[{"S":"AAA","o":1,"h":1,"l":1,"c":1,"v":5,"t":"2026-08-28T14:00:00Z"}],
      "trades":[{"S":"AAA","p":1.5,"s":9,"t":"2026-08-28T14:00:00.033913580Z","c":["@","T"]}],
      "quotes":[{"S":"AAA","bp":1.4,"bs":1,"ap":0.0,"as":0,"t":"2026-08-28T14:00:00.005Z"}]}"#;
    let f: DayFile = serde_json::from_str(json).unwrap();
    assert_eq!(f.trades.len(), 1);
    assert_eq!(f.trades[0].timestamp.timestamp_subsec_nanos(), 33_913_580);
    let m = Market::new(Clock::of(day()).unwrap(), day(), &f.bars, &f.quotes);
    assert!(m.quotes.is_empty(), "one-sided quote (ask 0) is not usable");
}

#[test]
fn price_eligible_mirrors_the_minute_bar_condition_table() {
    let c = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert!(price_eligible(&c(&[])));
    assert!(price_eligible(&c(&["@"])));
    assert!(price_eligible(&c(&["@", "F", "T"])));
    assert!(!price_eligible(&c(&["@", "I"])), "odd lot");
    assert!(!price_eligible(&c(&["@", "Z"])), "out of sequence");
    assert!(!price_eligible(&c(&["M"])));
    assert!(!price_eligible(&c(&["4"])));
    assert!(price_eligible(&c(&["@", "B"])), "UTP bunched");
    assert!(!price_eligible(&c(&["B"])), "CTA average price");
    let t = |p: f64, cond: &[&str]| TradeIn { price: p, size: 100, timestamp: DateTime::<Utc>::from_timestamp(et(10, 0, 0), 0).unwrap(), conditions: c(cond) };
    let trades = vec![t(5.0, &["@"]), t(9.0, &["@", "I"])];
    assert_eq!(readings(&trades, 0).len(), 2);
    assert_eq!(readings_with(&trades, 0, PrintFilter::PriceEligibleOnly).len(), 1);
}
