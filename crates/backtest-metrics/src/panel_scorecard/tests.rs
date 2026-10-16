use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::{Duration, TimeZone};

use super::*;

/// 2026-10-15 (Thursday, EDT): 04:00 ET = 08:00Z, 09:30 ET = 13:30Z,
/// 16:00 ET = 20:00Z.
fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 15).unwrap()
}

fn at(d: NaiveDate, h: u32, m: u32, s: u32) -> DateTime<Utc> {
    Utc.from_utc_datetime(&d.and_hms_opt(h, m, s).unwrap())
}

fn t(h: u32, m: u32) -> DateTime<Utc> {
    at(day(), h, m, 0)
}

fn bar(sym: &str, ts: DateTime<Utc>, o: f64, h: f64, l: f64, c: f64) -> Bar {
    Bar { symbol: sym.into(), open: o, high: h, low: l, close: c, volume: 1000, timestamp: ts }
}

/// Flat bars at `p` every minute over [from, to).
fn flat(sym: &str, from: DateTime<Utc>, to: DateTime<Utc>, p: f64) -> Vec<Bar> {
    let mut v = Vec::new();
    let mut ts = from;
    while ts < to {
        v.push(bar(sym, ts, p, p, p, p));
        ts += Duration::minutes(1);
    }
    v
}

fn det(panel: Panel, sym: &str, when: DateTime<Utc>, source: &str) -> Detection {
    Detection { panel, symbol: sym.into(), detected_at: when, price: Some(10.0), price_source: source.into(), bid: None, ask: None, quote_at: None, score: None, under25: None }
}

fn clock() -> DayClock {
    DayClock::of(day()).unwrap()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-3
}

// ---- outcome per detection ----------------------------------------------------

#[test]
fn quote_rows_reuse_follow_entry_at_ask_exit_at_bid_proxy() {
    // Quote 9.95/10.05 at detection (13:40:00, trade source): entry at the
    // ask 10.05. Price climbs to 11.2 at 13:45 -> target (+10% of 10.05 is
    // 11.055) before any stop.
    let mut bars = flat("AAA", t(13, 30), t(13, 40), 10.0);
    bars.push(bar("AAA", t(13, 40), 10.0, 10.5, 10.0, 10.4));
    bars.extend(flat("AAA", t(13, 41), t(13, 45), 10.5));
    bars.push(bar("AAA", t(13, 45), 10.5, 11.2, 10.5, 11.0));
    bars.extend(flat("AAA", t(13, 46), t(20, 0), 11.0));
    let mut d = det(Panel::Momentum, "AAA", t(13, 40), "trade");
    (d.bid, d.ask, d.quote_at) = (Some(9.95), Some(10.05), Some(t(13, 39)));
    let r = score_detection(&d, &bars, clock(), 0.5).unwrap();
    assert_eq!(r.cost_model, CostModel::Quote);
    assert_eq!(r.entry, 10.05);
    let spread = 0.1 / 10.0 * 100.0;
    assert!(close(r.spread_pct, spread));
    assert_eq!(r.outcome, Outcome::Target);
    // Held to the close: 11.0 at the bid proxy vs the ask.
    let want_close = (11.0 * (1.0 - spread / 200.0) - 10.05) / 10.05 * 100.0;
    assert!(close(r.net_pct[4].unwrap(), want_close));
    // Same number base_rates::follow gives on the same bars.
    let after: Vec<&Bar> = bars.iter().filter(|b| b.timestamp >= t(13, 40)).collect();
    let p = follow(10.05, spread, &after).unwrap();
    assert!(close(r.net_pct[4].unwrap(), p.close_return_net_pct));
    assert_eq!(r.outcome, p.outcome);
    // 5m horizon: bars 13:40..13:44 completed by 13:45 -> close 10.5.
    assert!(close(r.net_pct[0].unwrap(), (10.5 * (1.0 - spread / 200.0) - 10.05) / 10.05 * 100.0));
    assert!(close(r.mfe30_pct.unwrap(), (11.2 - 10.05) / 10.05 * 100.0));
    assert!(close(r.mae30_pct.unwrap(), (10.0 - 10.05) / 10.05 * 100.0));
}

#[test]
fn proxy_rows_enter_at_bar_open_plus_half_cost_and_bar_close_waits_a_minute() {
    let bars = flat("AAA", t(13, 30), t(20, 0), 10.0);
    let d = det(Panel::GapAndGo, "AAA", t(14, 0), "bar_close");
    let r = score_detection(&d, &bars, clock(), 0.5).unwrap();
    assert_eq!(r.cost_model, CostModel::Proxy);
    assert_eq!(r.known_at, t(14, 1));
    assert_eq!(r.entry_bar_at, t(14, 1), "a bar_close detection is known one minute later");
    assert!(close(r.entry, 10.025));
    // Flat market: every horizon loses exactly the round trip (approx).
    let want = (10.0 * 0.9975 - 10.025) / 10.025 * 100.0;
    for v in r.net_pct {
        assert!(close(v.unwrap(), want));
    }
    // ...and so does every random entry: panel == baseline.
    for v in r.baseline_net_pct {
        assert!(close(v.unwrap(), want));
    }
    assert_eq!(r.outcome, Outcome::Neither);
}

#[test]
fn stale_or_crossed_quotes_fall_back_to_the_proxy() {
    let bars = flat("AAA", t(13, 30), t(20, 0), 10.0);
    let mut d = det(Panel::Ignition, "AAA", t(14, 0), "event");
    (d.bid, d.ask, d.quote_at) = (Some(9.9), Some(10.1), Some(t(13, 57)));
    assert_eq!(score_detection(&d, &bars, clock(), 0.5).unwrap().cost_model, CostModel::Proxy, "quote 3 minutes old");
    (d.bid, d.ask, d.quote_at) = (Some(10.2), Some(10.1), Some(t(14, 0)));
    assert_eq!(score_detection(&d, &bars, clock(), 0.5).unwrap().cost_model, CostModel::Proxy, "crossed quote");
}

#[test]
fn horizons_past_the_close_are_censored_not_truncated() {
    let bars = flat("AAA", t(13, 30), t(20, 0), 10.0);
    let d = det(Panel::TopGainers, "AAA", t(19, 45), "movers_snapshot");
    let r = score_detection(&d, &bars, clock(), 0.5).unwrap();
    assert!(r.net_pct[0].is_some() && r.net_pct[1].is_some());
    assert!(r.net_pct[2].is_none() && r.net_pct[3].is_none());
    assert!(r.net_pct[4].is_some());
}

#[test]
fn skip_reasons() {
    let bars = flat("AAA", t(13, 30), t(20, 0), 10.0);
    assert_eq!(score_detection(&det(Panel::HaltActual, "AAA", t(20, 5), "event"), &bars, clock(), 0.5), Err(Skip::AfterClose));
    assert_eq!(score_detection(&det(Panel::HaltActual, "AAA", t(14, 0), "event"), &[], clock(), 0.5), Err(Skip::NoBars));
    // Next bar 20 minutes after detection (halted): not a tradeable entry.
    let gappy: Vec<Bar> = bars.iter().filter(|b| b.timestamp < t(14, 0) || b.timestamp >= t(14, 20)).cloned().collect();
    assert_eq!(score_detection(&det(Panel::HaltActual, "AAA", t(14, 0), "event"), &gappy, clock(), 0.5), Err(Skip::NoEntryBar));
}

#[test]
fn premarket_detections_draw_baselines_from_premarket_only() {
    // Premarket flat at 5, regular flat at 10. A random premarket entry
    // held to the close earns ~ +100%; a regular one earns ~ -cost.
    let mut bars = flat("AAA", t(8, 0), t(13, 30), 5.0);
    bars.extend(flat("AAA", t(13, 30), t(20, 0), 10.0));
    let r = score_detection(&det(Panel::Catalyst, "AAA", t(9, 0), "event"), &bars, clock(), 0.5).unwrap();
    assert_eq!(r.window, Window::Premarket);
    assert!(r.baseline_net_pct[4].unwrap() > 90.0);
    let r = score_detection(&det(Panel::Catalyst, "AAA", t(15, 0), "event"), &bars, clock(), 0.5).unwrap();
    assert_eq!(r.window, Window::Regular);
    assert!(r.baseline_net_pct[4].unwrap() < 0.0);
}

// ---- baseline and bootstrap determinism ------------------------------------------

fn wiggly(sym: &str, seed: u64) -> Vec<Bar> {
    let mut rng = SplitMix64::new(seed);
    let mut p = 10.0;
    let mut v = Vec::new();
    let mut ts = t(8, 0);
    while ts < t(20, 0) {
        let o = p;
        p *= 1.0 + (rng.below(2001) as f64 - 1000.0) / 100_000.0;
        v.push(bar(sym, ts, o, o.max(p) * 1.002, o.min(p) * 0.998, p));
        ts += Duration::minutes(1);
    }
    v
}

#[test]
fn baseline_is_deterministic_and_independent_of_other_detections() {
    let bars = wiggly("AAA", 3);
    let d = det(Panel::Micropullback, "AAA", t(15, 7), "event");
    let a = score_detection(&d, &bars, clock(), 0.5).unwrap();
    let b = score_detection(&d, &bars, clock(), 0.5).unwrap();
    assert_eq!(a, b);
    // Scored inside a plan alongside other detections: same row.
    let others = vec![det(Panel::Momentum, "AAA", t(14, 0), "bar_close"), d.clone(), det(Panel::Ignition, "AAA", t(16, 0), "event")];
    let p = plan(day(), others, MAX_SYMBOLS_PER_DAY);
    let map = BTreeMap::from([("AAA".to_string(), bars.clone())]);
    let rep = build_day_report(&p, SourceReport::empty(SourceKind::PanelDetections, ""), &map, BarsReport::default(), 0.5, t(21, 0), 200);
    assert!(rep.rows.contains(&a));
    // A different detection time draws different minutes.
    let c = score_detection(&det(Panel::Micropullback, "AAA", t(15, 8), "event"), &bars, clock(), 0.5).unwrap();
    assert_ne!(a.baseline_net_pct, c.baseline_net_pct);
}

fn report_for(d: NaiveDate, dets: Vec<Detection>, bars: BTreeMap<String, Vec<Bar>>, b: usize) -> DayReport {
    let p = plan(d, dets, MAX_SYMBOLS_PER_DAY);
    build_day_report(&p, SourceReport::empty(SourceKind::PanelDetections, ""), &bars, BarsReport::default(), 0.5, at(d, 21, 0, 0), b)
}

#[test]
fn bootstrap_ci_is_deterministic() {
    let mut dets = Vec::new();
    let mut bars = BTreeMap::new();
    for i in 0..12u64 {
        let sym = format!("S{i}");
        bars.insert(sym.clone(), wiggly(&sym, i));
        dets.push(det(Panel::HighlyTrading, &sym, t(14, 10 + i as u32), "movers_snapshot"));
    }
    let a = report_for(day(), dets.clone(), bars.clone(), 1000);
    let b = report_for(day(), dets, bars, 1000);
    assert_eq!(a.panels, b.panels);
    let h = &a.panels[0].horizons[PRIMARY];
    let (m, ci) = (h.mean_net_pct.unwrap(), h.mean_ci.unwrap());
    assert!(ci.lo <= m + 1e-3 && m - 1e-3 <= ci.hi, "{ci:?} vs {m}");
    assert!(h.diff_ci.is_some());
}

// ---- verdicts -------------------------------------------------------------------------

#[test]
fn verdict_thresholds() {
    let up = Some(Ci { lo: 0.1, hi: 0.5 });
    let down = Some(Ci { lo: -0.5, hi: -0.1 });
    let straddle = Some(Ci { lo: -0.1, hi: 0.1 });
    assert_eq!(verdict(MIN_DETECTIONS - 1, 10, up).0, Verdict::InsufficientData);
    assert_eq!(verdict(500, MIN_SESSIONS - 1, up).0, Verdict::InsufficientData);
    assert_eq!(verdict(MIN_DETECTIONS, MIN_SESSIONS, up).0, Verdict::BeatsRandom);
    assert_eq!(verdict(MIN_DETECTIONS, MIN_SESSIONS, down).0, Verdict::WorseThanRandom);
    assert_eq!(verdict(MIN_DETECTIONS, MIN_SESSIONS, straddle).0, Verdict::NoDifferenceShown);
    assert_eq!(verdict(MIN_DETECTIONS, MIN_SESSIONS, None).0, Verdict::InsufficientData);
    assert_eq!(verdict(MIN_DETECTIONS, MIN_SESSIONS, Some(Ci { lo: 0.0, hi: 1.0 })).0, Verdict::NoDifferenceShown, "touching 0 does not exclude it");
    assert_eq!(serde_json::to_string(&Verdict::InsufficientData).unwrap(), "\"insufficient data\"");
}

/// Flat at 10 except a +1%/minute run for 30 minutes starting at `from`.
fn runner(sym: &str, d: NaiveDate, from: DateTime<Utc>) -> Vec<Bar> {
    let mut v = Vec::new();
    let mut ts = at(d, 13, 30, 0);
    let mut p = 10.0;
    while ts < at(d, 20, 0, 0) {
        let o = p;
        if ts >= from && ts < from + Duration::minutes(30) {
            p *= 1.01;
        }
        v.push(bar(sym, ts, o, p.max(o), o.min(p), p));
        ts += Duration::minutes(1);
    }
    v
}

#[test]
fn one_day_is_never_enough_but_three_strong_days_beat_random() {
    let days = [NaiveDate::from_ymd_opt(2026, 10, 13).unwrap(), NaiveDate::from_ymd_opt(2026, 10, 14).unwrap(), day()];
    let mut reports = Vec::new();
    for d in days {
        let mut dets = Vec::new();
        let mut bars = BTreeMap::new();
        for i in 0..12u32 {
            let sym = format!("R{i}");
            let when = at(d, 14, 3 * i, 0);
            bars.insert(sym.clone(), runner(&sym, d, when));
            dets.push(det(Panel::Momentum, &sym, when, "trade"));
        }
        reports.push(report_for(d, dets, bars, 500));
    }
    let last = reports.last().unwrap();
    let p = &last.panels[0];
    assert_eq!(p.scored, 12);
    assert_eq!(p.verdict, Verdict::InsufficientData, "{}", p.verdict_reason);
    let latest = build_latest(last, &reports, t(21, 0), 500);
    assert_eq!(latest.rolling.days, days.to_vec());
    let rp = &latest.rolling.panels[0];
    assert_eq!((rp.horizons[PRIMARY].n, rp.sessions), (36, 3));
    assert_eq!(rp.verdict, Verdict::BeatsRandom, "{}", rp.verdict_reason);
    assert_eq!(latest.latest.panels, last.panels);
}

#[test]
fn rolling_window_keeps_the_last_n_days_up_to_the_current_day() {
    let mut reports = Vec::new();
    let start = NaiveDate::from_ymd_opt(2026, 8, 3).unwrap();
    for k in 0..(ROLLING_SESSIONS as i64 + 5) {
        let d = start + Duration::days(k);
        reports.push(report_for(d, Vec::new(), BTreeMap::new(), 10));
    }
    let current = &reports[ROLLING_SESSIONS + 2];
    let l = build_latest(current, &reports, Utc::now(), 10);
    assert_eq!(l.rolling.days.len(), ROLLING_SESSIONS);
    assert_eq!(*l.rolling.days.last().unwrap(), current.day, "later days are not included");
}

// ---- inputs ---------------------------------------------------------------------------

#[test]
fn every_panel_parses_and_scores_from_recorder_fixtures() {
    let lines: Vec<String> = Panel::ALL
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let extra = if *p == Panel::Ignition { r#"{"suppressedSinceLast":0,"under25":true}"# } else { "{}" };
            format!(
                r#"{{"schema":"panel-detection-v1","panel":"{}","symbol":"p{i}","detectedAt":"2026-10-15T14:{:02}:07.123Z","capturedAt":"2026-10-15T14:{:02}:07.300Z","price":10.0,"priceSource":"trade","bid":9.99,"ask":10.01,"quoteAt":"2026-10-15T14:{:02}:06.950Z","score":0.7,"extra":{extra}}}"#,
                p.as_str(),
                10 + i,
                10 + i,
                10 + i
            )
        })
        .chain([
            "not json".to_string(),
            r#"{"schema":"panel-detection-v2","panel":"momentum","symbol":"X","detectedAt":"2026-10-15T14:00:00Z"}"#.to_string(),
            r#"{"schema":"panel-detection-v1","panel":"mystery","symbol":"X","detectedAt":"2026-10-15T14:00:00Z"}"#.to_string(),
            r#"{"schema":"panel-detection-v1","panel":"momentum","symbol":"X","detectedAt":"2026-10-16T14:00:00Z"}"#.to_string(),
        ])
        .collect();
    let (dets, src) = parse_detections(&lines.join("\n"), day());
    assert_eq!((src.lines, src.used, src.unparsable, src.wrong_schema, src.unknown_panel, src.other_day), (14, 10, 1, 1, 1, 1));
    assert!(dets.iter().all(|d| d.symbol.starts_with('P')), "symbols upper-cased");
    let bars: BTreeMap<String, Vec<Bar>> = (0..10).map(|i| (format!("P{i}"), wiggly(&format!("P{i}"), i))).collect();
    let rep = report_for(day(), dets, bars, 100);
    assert_eq!(rep.panels.len(), 10);
    for p in &rep.panels {
        assert_eq!((p.scored, p.quote_cost_rows), (1, 1), "{:?}", p.panel);
        assert_eq!(p.verdict, Verdict::InsufficientData);
    }
    let json = serde_json::to_value(&rep).unwrap();
    assert_eq!(json["schema"], SCHEMA);
    assert!(json["rows"][0]["netPct"].is_array() && json["panels"][0]["horizons"][2]["horizon"] == "30m");
}

#[test]
fn pending_fallback_maps_strategies_and_dedups_ignition() {
    let text = [
        r#"{"symbol":"AAA","strategy":"MomentumScorer","timestamp":"2026-10-15T14:00:00Z","signal_price":3.0,"captured_at":"2026-10-15T14:00:01Z"}"#,
        r#"{"symbol":"AAA","strategy":"FastFunnel","timestamp":"2026-10-15T14:01:00Z","signal_price":3.0,"captured_at":"2026-10-15T14:01:01Z"}"#,
        r#"{"symbol":"BBB","strategy":"IgnitionDetector","timestamp":"2026-10-15T14:00:00Z","signal_price":4.0,"captured_at":"2026-10-15T14:00:01Z"}"#,
        r#"{"symbol":"BBB","strategy":"IgnitionDetector","timestamp":"2026-10-15T14:05:00Z","signal_price":4.1,"captured_at":"2026-10-15T14:05:01Z"}"#,
        r#"{"symbol":"BBB","strategy":"IgnitionDetector","timestamp":"2026-10-15T14:10:00Z","signal_price":4.2,"captured_at":"2026-10-15T14:10:01Z"}"#,
        r#"{"symbol":"CCC","strategy":"IgnitionDetector","timestamp":"2026-10-15T14:00:00Z","signal_price":40.0,"captured_at":"2026-10-15T14:00:01Z"}"#,
        r#"{"symbol":"DDD","strategy":"Micropullback","timestamp":"2026-10-15T15:00:00Z","signal_price":2.0,"captured_at":"2026-10-15T15:00:01Z"}"#,
        r#"{"symbol":"DDD","strategy":"Micropullback","timestamp":"2026-10-15T15:09:59Z","signal_price":2.0,"captured_at":"2026-10-15T15:10:00Z"}"#,
        r#"{"symbol":"EEE","strategy":"ConsolidationBreakout","timestamp":"2026-10-15T15:00:00Z","signal_price":2.0,"captured_at":"2026-10-15T15:00:01Z"}"#,
        r#"{"symbol":"EEE","strategy":"Unknown","timestamp":"2026-10-15T15:00:00Z","signal_price":2.0,"captured_at":"2026-10-15T15:00:01Z"}"#,
        r#"{"symbol":"EEE","strategy":"MomentumScorer","timestamp":"2026-10-14T15:00:00Z","signal_price":2.0,"captured_at":"2026-10-14T15:00:01Z"}"#,
    ]
    .join("\n");
    let (dets, src) = parse_pending(&text, day());
    assert_eq!(src.kind, SourceKind::LivePendingFallback);
    assert_eq!((src.used, src.unknown_panel, src.other_day), (9, 1, 1));
    let src_of = |p: Panel| dets.iter().find(|d| d.panel == p).unwrap().price_source.clone();
    assert_eq!(src_of(Panel::Momentum), "bar_close");
    assert_eq!(src_of(Panel::GapAndGo), "bar_close");
    assert_eq!(src_of(Panel::Ignition), "event");
    assert!(dets.iter().all(|d| d.bid.is_none() && d.ask.is_none()));

    let p = plan(day(), dets, MAX_SYMBOLS_PER_DAY);
    assert_eq!(p.filtering.ignition_over25_dropped, 1, "CCC at $40");
    // BBB 14:05 is inside the 10-minute cooldown of 14:00; 14:10 is not.
    // DDD 15:09:59 is inside the cooldown of 15:00.
    assert_eq!(p.filtering.cooldown_dropped, 2);
    let ign: Vec<DateTime<Utc>> = p.detections.iter().filter(|d| d.panel == Panel::Ignition).map(|d| d.detected_at).collect();
    assert_eq!(ign, vec![t(14, 0), t(14, 10)]);
    assert_eq!(p.detections.len(), 6);
}

// ---- sampling ---------------------------------------------------------------------------

#[test]
fn sampling_is_deterministic_fair_across_panels_and_disclosed() {
    let mut dets: Vec<Detection> = (0..20).map(|i| det(Panel::TopGainers, &format!("T{i:02}"), t(14, i), "movers_snapshot")).collect();
    dets.push(det(Panel::HaltActual, "H1", t(15, 0), "event"));
    dets.push(det(Panel::HaltActual, "H2", t(15, 1), "event"));
    let a = plan(day(), dets.clone(), 4);
    let b = plan(day(), dets.clone(), 4);
    assert_eq!(a.symbols, b.symbols);
    assert_eq!(a.symbols.len(), 4);
    assert!(a.symbols.iter().filter(|s| s.starts_with('H')).count() == 2, "round-robin gives the small panel its share: {:?}", a.symbols);
    let s = &a.sampling;
    assert!(s.sampled);
    assert_eq!((s.symbols_available, s.symbols_scored, s.detections_available, s.detections_kept), (22, 4, 22, 4));
    let rep = build_day_report(&a, SourceReport::empty(SourceKind::PanelDetections, ""), &BTreeMap::new(), BarsReport::default(), 0.5, t(21, 0), 10);
    let tg = rep.panels.iter().find(|p| p.panel == Panel::TopGainers).unwrap();
    assert_eq!(tg.skipped.get("notSampled"), Some(&18));
    assert_eq!(tg.skipped.get("noBars"), Some(&2));
    assert_eq!(tg.detections, 20);
    // Under budget: nothing sampled.
    assert!(!plan(day(), dets, 100).sampling.sampled);
}

// ---- bar cache ---------------------------------------------------------------------------

struct FakeSource {
    calls: AtomicUsize,
}

impl BarSource for FakeSource {
    fn minute_bars(&self, symbol: &str, start: DateTime<Utc>, end: DateTime<Utc>) -> impl std::future::Future<Output = Result<Vec<Bar>>> + Send {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let out = if symbol == "FAIL" { Err(anyhow::anyhow!("boom")) } else { Ok(flat(symbol, start, start + Duration::minutes(3), 1.0)) };
        assert_eq!(end, t(20, 0));
        async move { out }
    }
}

#[tokio::test]
async fn bars_are_cached_after_a_complete_day_and_reruns_are_free() {
    let dir = std::env::temp_dir().join(format!("panel-scorecard-cache-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let src = FakeSource { calls: AtomicUsize::new(0) };
    let syms = vec!["AAA".to_string(), "FAIL".to_string(), "../x".to_string()];

    // Before the close: fetched, not cached.
    let (_, rep) = load_day_bars(&src, Some(&dir), &syms, day(), t(15, 0)).await.unwrap();
    assert_eq!((rep.fetched, rep.not_cached_incomplete_day, rep.from_cache), (1, 1, 0));
    assert!(!bars::cache_path(&dir, day(), "AAA").exists());

    let (m, rep) = load_day_bars(&src, Some(&dir), &syms, day(), t(21, 0)).await.unwrap();
    assert_eq!((rep.fetched, rep.from_cache, rep.api_calls), (1, 0, 2));
    assert_eq!(rep.failed, vec!["FAIL".to_string(), "../x".to_string()], "unsafe symbols never reach the network or the disk");
    assert_eq!(m["AAA"].len(), 3);
    assert!(bars::cache_path(&dir, day(), "AAA").exists());

    let before = src.calls.load(Ordering::SeqCst);
    let (m2, rep) = load_day_bars(&src, Some(&dir), &["AAA".to_string()], day(), t(21, 0)).await.unwrap();
    assert_eq!(src.calls.load(Ordering::SeqCst), before, "cached symbol costs no call");
    assert_eq!((rep.from_cache, rep.api_calls), (1, 0));
    assert_eq!(m2["AAA"].len(), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn day_reports_round_trip_through_disk() {
    let dir = std::env::temp_dir().join(format!("panel-scorecard-days-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let bars = BTreeMap::from([("AAA".to_string(), wiggly("AAA", 9))]);
    let rep = report_for(day(), vec![det(Panel::Momentum, "AAA", t(14, 0), "trade")], bars, 50);
    std::fs::write(dir.join("2026-10-15.json"), serde_json::to_vec(&rep).unwrap()).unwrap();
    std::fs::write(dir.join("latest.json"), "{}").unwrap();
    std::fs::write(dir.join("2026-10-14.json"), "{\"schema\":\"other\"}").unwrap();
    let loaded = load_day_reports(&dir).unwrap();
    assert_eq!(loaded.len(), 1, "latest.json and other schemas are ignored");
    assert_eq!((loaded[0].day, loaded[0].rows.len(), loaded[0].rows[0].symbol.as_str()), (rep.day, 1, "AAA"));
    assert_eq!(loaded[0].panels[0].verdict, rep.panels[0].verdict);
    let _ = std::fs::remove_dir_all(&dir);
}
