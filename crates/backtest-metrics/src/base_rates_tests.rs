use super::*;
use chrono::TimeZone;

/// 2026-10-15 (EDT): 09:30 ET = 13:30Z, 09:35 ET = 13:35Z, 16:00 ET = 20:00Z.
fn t(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 15, h, m, 0).unwrap()
}

fn bar(at: DateTime<Utc>, high: f64, low: f64, close: f64, volume: u64) -> Bar {
    Bar { symbol: "AAA".into(), open: close, high, low, close, volume, timestamp: at }
}

fn quote(at: DateTime<Utc>, bid: f64, ask: f64) -> Quote {
    Quote { symbol: "AAA".into(), bid_price: bid, bid_size: 100, ask_price: ask, ask_size: 100, timestamp: at }
}

/// Prior close $2.00; at 09:34 the stock trades at $3.00 (+50%) on
/// 1.2M shares against a 1M average (1.2x); ask $3.00, bid $2.94.
fn session(after: Vec<Bar>) -> SessionInput {
    let mut bars = vec![bar(t(8, 0), 2.6, 2.4, 2.5, 200_000), bar(t(13, 30), 2.9, 2.6, 2.8, 500_000), bar(t(13, 34), 3.05, 2.9, 3.0, 500_000)];
    bars.extend(after);
    SessionInput { symbol: "AAA".into(), prior_close: 2.0, avg_daily_volume: 1_000_000, bars, quotes: vec![quote(t(13, 34), 2.94, 3.00), quote(t(13, 36), 9.0, 9.1)] }
}

#[test]
fn measures_at_0935_with_the_ask_as_entry() {
    let o = observe(&session(vec![bar(t(13, 35), 3.1, 2.95, 3.05, 10), bar(t(19, 59), 3.2, 3.0, 3.1, 10)])).unwrap();
    assert!((o.gap_pct - 50.0).abs() < 1e-9);
    assert!((o.relative_volume - 1.2).abs() < 1e-9, "volume before 09:35 only");
    assert_eq!(o.entry, 3.00, "the quote after 09:35 is not visible at decision time");
    assert!((o.entry_spread_pct - 0.06 / 2.97 * 100.0).abs() < 1e-9);
    assert_eq!(o.outcome, Outcome::Neither);
    assert_eq!(o.day, NaiveDate::from_ymd_opt(2026, 10, 15).unwrap());
}

#[test]
fn target_before_stop() {
    let o = observe(&session(vec![bar(t(13, 40), 3.31, 2.96, 3.3, 10), bar(t(13, 41), 3.3, 2.5, 2.6, 10)])).unwrap();
    assert_eq!(o.outcome, Outcome::Target);
}

#[test]
fn stop_before_target() {
    let o = observe(&session(vec![bar(t(13, 40), 3.1, 2.84, 2.9, 10), bar(t(13, 41), 3.5, 2.9, 3.4, 10)])).unwrap();
    assert_eq!(o.outcome, Outcome::Stop);
    assert!((o.max_adverse_pct - (2.84 - 3.0) / 3.0 * 100.0).abs() < 1e-9);
}

#[test]
fn a_minute_touching_both_counts_as_the_stop() {
    let o = observe(&session(vec![bar(t(13, 40), 3.4, 2.8, 3.0, 10)])).unwrap();
    assert_eq!(o.outcome, Outcome::Stop);
}

#[test]
fn bars_after_the_close_do_not_count() {
    let o = observe(&session(vec![bar(t(19, 59), 3.05, 2.96, 3.0, 10), bar(t(20, 0), 4.0, 3.0, 4.0, 10)])).unwrap();
    assert_eq!(o.outcome, Outcome::Neither);
}

#[test]
fn close_return_pays_the_spread_both_ways() {
    let o = observe(&session(vec![bar(t(19, 59), 3.05, 2.96, 3.0, 10)])).unwrap();
    // Closing exactly at the entry ask still loses half a spread on exit.
    assert!(o.close_return_net_pct < 0.0);
    assert!((o.close_return_net_pct + o.entry_spread_pct / 2.0).abs() < 1e-9);
}

#[test]
fn ineligible_sessions_say_why() {
    let mut s = session(vec![bar(t(19, 59), 3.0, 3.0, 3.0, 10)]);
    s.quotes.retain(|q| q.timestamp >= t(13, 35));
    assert_eq!(observe(&s), Err(Skip::NoQuote));

    let mut s = session(vec![]);
    s.prior_close = 4.0;
    assert_eq!(observe(&s), Err(Skip::NotUp));

    let mut s = session(vec![]);
    s.bars.retain(|b| b.timestamp < t(13, 30));
    assert_eq!(observe(&s), Err(Skip::NoTradeByDecision));

    let mut s = session(vec![]);
    s.avg_daily_volume = 0;
    assert_eq!(observe(&s), Err(Skip::NoBaseline));

    assert_eq!(observe(&session(vec![])), Err(Skip::NoBarsAfterDecision));

    let mut s = session(vec![bar(t(19, 59), 30.0, 30.0, 30.0, 10)]);
    for b in &mut s.bars {
        b.close *= 10.0;
    }
    assert_eq!(observe(&s), Err(Skip::OutsidePriceRange));
}

fn obs(gap: f64, rv: f64, outcome: Outcome, ret: f64) -> Observation {
    Observation { symbol: format!("S{gap}"), day: NaiveDate::from_ymd_opt(2026, 10, 15).unwrap(), gap_pct: gap, relative_volume: rv, entry: 1.0, entry_spread_pct: 1.0, outcome, close_return_net_pct: ret, max_adverse_pct: -2.0 }
}

#[test]
fn small_profiles_report_counts_but_no_rates() {
    let observations: Vec<_> = (0..MIN_SAMPLE - 1).map(|_| obs(30.0, 5.0, Outcome::Target, 1.0)).collect();
    let r = aggregate(&observations, 40, &Skipped::default(), "test", t(21, 0));
    let p = r.lookup(30.0, 5.0).unwrap();
    assert_eq!(p.n, MIN_SAMPLE - 1);
    assert_eq!(p.target_rate, None);
    assert_eq!(r.profiles.len(), GAP_BUCKETS.len() * RVOL_BUCKETS.len(), "every profile listed, empty ones too");
}

#[test]
fn rates_and_medians_once_the_sample_is_large_enough() {
    let mut observations: Vec<_> = (0..20).map(|_| obs(30.0, 5.0, Outcome::Target, 4.0)).collect();
    observations.extend((0..10).map(|_| obs(30.0, 5.0, Outcome::Stop, -6.0)));
    let r = aggregate(&observations, 30, &Skipped::default(), "test", t(21, 0));
    let p = r.lookup(26.0, 9.0).unwrap();
    assert_eq!(p.n, 30);
    assert!((p.target_rate.unwrap() - 20.0 / 30.0).abs() < 1e-12);
    assert!((p.stop_first_rate.unwrap() - 10.0 / 30.0).abs() < 1e-12);
    assert_eq!(p.median_close_return_net_pct, Some(4.0));
    assert_eq!(r.source.observations, 30);
}

#[test]
fn profile_buckets_and_lookup() {
    assert_eq!(profile_of(5.0, 0.5), Some(("0-10%", "<1x")));
    assert_eq!(profile_of(10.0, 3.0), Some(("10-25%", "3-10x")));
    assert_eq!(profile_of(80.0, 25.0), Some(("50%+", "10x+")));
    assert_eq!(profile_of(0.0, 5.0), None, "not up on the day");
    assert_eq!(profile_of(-3.0, 5.0), None);
    assert_eq!(profile_of(5.0, f64::NAN), None);
}

#[test]
fn reads_a_cached_replay_file_shape() {
    let json = r#"{"session_seeds":{},"symbol":"AAA","prior_close":2.0,"avg_daily_volume":100,"float_shares":null,
        "bars":[{"S":"AAA","o":1,"h":1,"l":1,"c":1,"v":1,"t":"2026-10-15T13:30:00Z"}],
        "trades":[{"S":"AAA","p":1.0,"s":1,"t":"2026-10-15T13:30:00Z","c":["@"]}],
        "quotes":[{"S":"AAA","bp":1.0,"bs":1,"ap":1.01,"as":1,"t":"2026-10-15T13:30:00Z"}]}"#;
    let s: SessionInput = serde_json::from_str(json).unwrap();
    assert_eq!((s.bars.len(), s.quotes.len()), (1, 1));
}

/// Flat at $2.00 from 09:30 (no qualifying momentum), then a clean climb
/// from 10:00 that crosses the qualify threshold, then a run to +12%.
fn momentum_session() -> SessionInput {
    let mut bars = Vec::new();
    for m in 0..30u32 {
        bars.push(Bar { symbol: "AAA".into(), open: 2.0, high: 2.0, low: 2.0, close: 2.0, volume: 100, timestamp: t(13, 30) + chrono::Duration::minutes(m as i64) });
    }
    for m in 0..40u32 {
        let p = 2.0 + m as f64 * 0.01;
        bars.push(Bar { symbol: "AAA".into(), open: p, high: p + 0.0101, low: p + 0.0001, close: p + 0.01, volume: 1000, timestamp: t(14, 0) + chrono::Duration::minutes(m as i64) });
    }
    let quotes = (0..70u32).map(|m| quote(t(13, 30) + chrono::Duration::minutes(m as i64) + chrono::Duration::seconds(59), 1.99 + m as f64 * 0.0, 2.01)).collect();
    SessionInput { symbol: "AAA".into(), prior_close: 1.8, avg_daily_volume: 100_000, bars, quotes }
}

#[test]
fn a_momentum_crossing_is_measured_once_when_it_crosses() {
    let obs = observe_momentum(&momentum_session());
    assert!(!obs.is_empty(), "the climb crosses the threshold");
    let first = &obs[0];
    assert!(first.score >= momentum_scorer::DEFAULT_QUALIFY_THRESHOLD);
    assert_eq!(first.band, momentum_band(first.score).unwrap());
    assert_eq!(first.entry, 2.01, "bought at the ask of the last quote once the crossing bar closed");
    // Still-qualifying minutes after the crossing are not new crossings.
    assert!(obs.len() <= 2, "edge-triggered, got {}", obs.len());
}

#[test]
fn momentum_crossings_need_a_fresh_quote() {
    let mut s = momentum_session();
    s.quotes.clear();
    assert!(observe_momentum(&s).is_empty());
}

#[test]
fn momentum_bands_and_rollup() {
    assert_eq!(momentum_band(0.59), None);
    assert_eq!(momentum_band(0.62), Some("Mild"));
    assert_eq!(momentum_band(0.65), Some("Solid"));
    assert_eq!(momentum_band(0.72), Some("Strong"));
    let o = |band: &'static str, outcome| MomentumObservation {
        symbol: "X".into(), day: NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(), score: 0.66, band, entry: 1.0,
        entry_spread_pct: 1.0, outcome, close_return_net_pct: -2.0, max_adverse_pct: -4.0,
    };
    let mut obs: Vec<_> = (0..12).map(|_| o("Solid", Outcome::Target)).collect();
    obs.extend((0..18).map(|_| o("Solid", Outcome::Stop)));
    obs.push(o("Strong", Outcome::Target));
    let rows = aggregate_momentum(&obs);
    assert_eq!(rows.iter().map(|r| r.band.as_str()).collect::<Vec<_>>(), vec!["Mild", "Solid", "Strong"]);
    let solid = &rows[1];
    assert_eq!((solid.n, solid.sessions), (30, 1));
    assert!((solid.target_rate.unwrap() - 0.4).abs() < 1e-12);
    assert_eq!(rows[2].target_rate, None, "one case is too few for a rate");
}
