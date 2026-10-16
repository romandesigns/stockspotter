use super::*;
use chrono::{Duration as ChronoDuration, TimeZone};
use market_data::HaltAlertLevel;

/// 2026-10-16 10:00 ET (14:00Z), a market day.
fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 16, 14, 0, 0).unwrap()
}

fn mins(m: i64) -> DateTime<Utc> {
    t0() + ChronoDuration::minutes(m)
}

fn momentum(symbol: &str, at: DateTime<Utc>, qualifies: bool) -> ScanEvent {
    ScanEvent::MomentumUpdate {
        symbol: symbol.into(),
        timestamp: at,
        volume_confirmation: 0.9,
        structure: 0.8,
        ma_slope: 0.7,
        wick_rejection: 0.6,
        overall: 0.73,
        qualifies,
    }
}

fn funnel(symbol: &str, at: DateTime<Utc>, passed: bool) -> ScanEvent {
    ScanEvent::FunnelSignal {
        symbol: symbol.into(),
        timestamp: at,
        price: 3.12,
        gap_pct: 12.5,
        session_volume: 100_000,
        float_shares: Some(5_000_000),
        relative_volume: Some(6.0),
        price_ok: true,
        float_ok: true,
        rel_vol_ok: true,
        gap_ok: true,
        passed,
    }
}

fn ignition(symbol: &str, at: DateTime<Utc>, price: f64) -> ScanEvent {
    ScanEvent::IgnitionEvent { symbol: symbol.into(), timestamp: at, price, kind: IgnitionEventKind::FollowThroughConfirmed }
}

fn entry(symbol: &str, at: DateTime<Utc>, strategy: ConsolidationStrategy) -> ScanEvent {
    ScanEvent::ConsolidationEvent { symbol: symbol.into(), timestamp: at, price: 2.0, kind: ConsolidationEventKind::EntryTriggered, strategy }
}

fn halt_warning(symbol: &str, at: DateTime<Utc>, ratio: f64) -> ScanEvent {
    ScanEvent::HaltWarning {
        estimated_bands: true,
        symbol: symbol.into(),
        timestamp: at,
        reference_price: 1.0,
        current_price: 1.08,
        band_width_dollars: 0.1,
        band_doubled: false,
        proximity_ratio: ratio,
        relative_volume: Some(4.0),
        level: HaltAlertLevel::Calm,
        luld_in_effect: true,
    }
}

fn mover(symbol: &str, at: DateTime<Utc>) -> Mover {
    Mover { symbol: symbol.into(), price: 1.5, change_pct: 40.0, volume: 1_000_000, volume_source: None, session: None, observed_at: Some(at), provenance: None }
}

fn movers(at: DateTime<Utc>, gainers: &[&str], active: &[&str]) -> TodayMovers {
    TodayMovers {
        current_gainers: gainers.iter().map(|s| mover(s, at)).collect(),
        current_most_active: active.iter().map(|s| mover(s, at)).collect(),
        observed_at: Some(at),
        ..TodayMovers::default()
    }
}

fn halt(symbol: &str, at: DateTime<Utc>, kind: HaltTransitionKind) -> HaltStatus {
    HaltStatus {
        symbol: symbol.into(),
        kind,
        status_code: if kind == HaltTransitionKind::Halted { "M".into() } else { "Q".into() },
        reason_code: Some("LUDP".into()),
        reason_message: Some("Volatility Trading Pause".into()),
        market_at: at,
        received_at: at,
    }
}

#[test]
fn momentum_first_reading_is_a_baseline_then_false_to_true_edges() {
    let mut d = PanelDetector::new();
    // Baseline already true: no row.
    assert!(d.on_event(&momentum("AAA", mins(0), true), mins(0)).is_none());
    assert!(d.on_event(&momentum("AAA", mins(1), true), mins(1)).is_none());
    assert!(d.on_event(&momentum("AAA", mins(2), false), mins(2)).is_none());
    let r = d.on_event(&momentum("AAA", mins(3), true), mins(3)).expect("false->true edge");
    assert_eq!(r.panel, "momentum");
    assert_eq!(r.score, Some(0.73));
    assert_eq!(r.extra["components"]["maSlope"], json!(0.7));
    // Baseline false, then true: a row.
    assert!(d.on_event(&momentum("BBB", mins(0), false), mins(0)).is_none());
    assert!(d.on_event(&momentum("BBB", mins(1), true), mins(1)).is_some());
}

#[test]
fn momentum_uses_last_seen_trade_price() {
    let mut d = PanelDetector::new();
    d.on_event(&momentum("AAA", mins(0), false), mins(0));
    d.on_event(&halt_warning("AAA", mins(0), 0.1), mins(0));
    let r = d.on_event(&momentum("AAA", mins(1), true), mins(1)).unwrap();
    assert_eq!(r.price, Some(1.08));
    assert_eq!(r.price_source, "trade");
}

#[test]
fn gap_and_go_edges_on_passed_with_extra_fields() {
    let mut d = PanelDetector::new();
    assert!(d.on_event(&funnel("AAA", mins(0), false), mins(0)).is_none());
    let r = d.on_event(&funnel("AAA", mins(1), true), mins(1)).unwrap();
    assert_eq!(r.panel, "gap_and_go");
    assert_eq!(r.price, Some(3.12));
    assert_eq!(r.price_source, "event");
    assert_eq!(r.extra, json!({ "gapPct": 12.5, "relativeVolume": 6.0, "floatShares": 5_000_000 }));
    assert!(d.on_event(&funnel("AAA", mins(2), true), mins(2)).is_none());
}

#[test]
fn day_rollover_resets_edges_and_rebaselines() {
    let mut d = PanelDetector::new();
    d.on_event(&momentum("AAA", mins(0), false), mins(0));
    assert!(d.on_event(&momentum("AAA", mins(1), true), mins(1)).is_some());
    // Next market day (24 h later): first reading is a baseline again,
    // even though it is true.
    let next = mins(24 * 60);
    assert!(d.on_event(&momentum("AAA", next, true), next).is_none());
    // Ignition cooldown is per day too.
    let mut d = PanelDetector::new();
    // 03:50 ET, previous market day (boundary is 04:00 ET).
    let late = Utc.with_ymd_and_hms(2026, 10, 16, 7, 50, 0).unwrap();
    assert!(d.on_event(&ignition("AAA", late, 2.0), late).is_some());
    let early = Utc.with_ymd_and_hms(2026, 10, 16, 8, 1, 0).unwrap();
    assert!(d.on_event(&ignition("AAA", early, 2.0), early).is_some(), "04:01 ET is a new market day");
}

#[test]
fn day_never_moves_backwards() {
    let mut d = PanelDetector::new();
    d.on_event(&momentum("AAA", mins(0), false), mins(0));
    let yesterday = mins(-24 * 60);
    // A wall clock that steps back must not wipe today's state.
    d.on_event(&momentum("BBB", yesterday, false), yesterday);
    assert!(d.on_event(&momentum("AAA", mins(1), true), mins(1)).is_some());
}

#[test]
fn ignition_cooldown_counts_suppressed_and_flags_under_25() {
    let mut d = PanelDetector::new();
    let first = d.on_event(&ignition("AAA", mins(0), 3.0), mins(0)).unwrap();
    assert_eq!(first.extra, json!({ "suppressedSinceLast": 0, "under25": true }));
    for m in 1..=3 {
        assert!(d.on_event(&ignition("AAA", mins(m), 3.0), mins(m)).is_none());
    }
    // Other kinds never count.
    let opened = ScanEvent::IgnitionEvent { symbol: "AAA".into(), timestamp: mins(4), price: 3.0, kind: IgnitionEventKind::CandidateOpened };
    assert!(d.on_event(&opened, mins(4)).is_none());
    let second = d.on_event(&ignition("AAA", mins(10), 30.0), mins(10)).unwrap();
    assert_eq!(second.extra, json!({ "suppressedSinceLast": 3, "under25": false }));
    // Exactly 25 is under25.
    let r = d.on_event(&ignition("BBB", mins(0), 25.0), mins(0)).unwrap();
    assert_eq!(r.extra["under25"], json!(true));
}

#[test]
fn entry_triggers_cooldown_per_panel_and_symbol() {
    let mut d = PanelDetector::new();
    let r = d.on_event(&entry("AAA", mins(0), ConsolidationStrategy::Micropullback), mins(0)).unwrap();
    assert_eq!(r.panel, "micropullback");
    assert!(d.on_event(&entry("AAA", mins(5), ConsolidationStrategy::Micropullback), mins(5)).is_none());
    // Different panel, same symbol: its own cooldown.
    let r = d.on_event(&entry("AAA", mins(5), ConsolidationStrategy::ConsolidationBreakout), mins(5)).unwrap();
    assert_eq!(r.panel, "consolidation_breakout");
    assert!(d.on_event(&entry("AAA", mins(10), ConsolidationStrategy::Micropullback), mins(10)).is_some());
    // Non-entry kinds are not rows.
    let surge = ScanEvent::ConsolidationEvent { symbol: "CCC".into(), timestamp: mins(0), price: 2.0, kind: ConsolidationEventKind::SurgeDetected, strategy: ConsolidationStrategy::Micropullback };
    assert!(d.on_event(&surge, mins(0)).is_none());
}

#[test]
fn halt_speed_rows_only_on_the_speed_alert_rule() {
    let mut d = PanelDetector::new();
    // Baseline, then rise through 70%.
    assert!(d.on_event(&halt_warning("AAA", mins(0), 0.2), mins(0)).is_none());
    let r = d.on_event(&halt_warning("AAA", mins(1), 0.75), mins(1)).unwrap();
    assert_eq!(r.panel, "halt_speed");
    assert_eq!(r.score, Some(75.0));
    assert_eq!(r.extra["luldInEffect"], json!(true));
    assert_eq!(r.extra["relativeVolume"], json!(4.0));
    assert!(r.extra.get("movePct").is_some() && r.extra.get("fillingFast").is_some());
    // Staying high, or re-crossing inside 10 min: nothing.
    assert!(d.on_event(&halt_warning("AAA", mins(2), 0.8), mins(2)).is_none());
    assert!(d.on_event(&halt_warning("AAA", mins(3), 0.1), mins(3)).is_none());
    assert!(d.on_event(&halt_warning("AAA", mins(4), 0.9), mins(4)).is_none());
}

#[test]
fn catalyst_rows_on_every_update() {
    let mut d = PanelDetector::new();
    let ev = ScanEvent::CatalystUpdate {
        symbol: "AAA".into(),
        timestamp: mins(0),
        catalyst_tags: vec!["fda".into()],
        headline_count: 3,
        most_recent_headline: Some("x".into()),
        most_recent_published_at: Some(mins(-5)),
    };
    let r = d.on_event(&ev, mins(0)).unwrap();
    assert_eq!(r.panel, "catalyst");
    assert_eq!(r.extra, json!({ "tags": ["fda"], "headlineCount": 3, "mostRecentPublishedAt": "2026-10-16T13:55:00.000Z" }));
    assert!(d.on_event(&ev, mins(1)).is_some());
}

#[test]
fn movers_rows_on_entering_top_25_only() {
    let mut d = PanelDetector::new();
    let first = d.on_movers(&movers(mins(0), &["AAA", "BBB"], &["AAA"]), mins(0));
    assert_eq!(first.len(), 3);
    assert!(first.iter().all(|r| r.extra["firstSnapshot"] == json!(true)));
    // Same snapshot again: nothing.
    assert!(d.on_movers(&movers(mins(0), &["AAA", "BBB"], &["AAA"]), mins(0)).is_empty());
    let next = d.on_movers(&movers(mins(1), &["CCC", "AAA"], &["AAA", "BBB"]), mins(1));
    let got: Vec<(&str, &str, u64)> = next.iter().map(|r| (r.panel, r.symbol.as_str(), r.extra["rank"].as_u64().unwrap())).collect();
    assert_eq!(got, vec![("top_gainers", "CCC", 1), ("highly_trading", "BBB", 2)]);
    assert!(next.iter().all(|r| r.extra.get("firstSnapshot").is_none() && r.price_source == "movers_snapshot"));
    assert_eq!(next[0].extra["changePct"], json!(40.0));
    assert_eq!(next[0].extra["volume"], json!(1_000_000));
    // Leaving and re-entering is a new row.
    d.on_movers(&movers(mins(2), &["AAA"], &["AAA", "BBB"]), mins(2));
    let back = d.on_movers(&movers(mins(3), &["CCC", "AAA"], &["AAA", "BBB"]), mins(3));
    assert_eq!(back.len(), 1);
    // A 26th row is not on the panel.
    let many: Vec<String> = (0..30).map(|i| format!("S{i:02}")).collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    let big = d.on_movers(&movers(mins(4), &refs, &[]), mins(4));
    assert_eq!(big.len(), MOVERS_TOP_N);
}

#[test]
fn movers_new_day_records_the_first_scan_as_entries() {
    let mut d = PanelDetector::new();
    d.on_movers(&movers(mins(0), &["AAA"], &[]), mins(0));
    let next = mins(24 * 60);
    let rows = d.on_movers(&movers(next, &["AAA"], &[]), next);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].extra.get("firstSnapshot").is_none());
}

#[test]
fn halt_actual_records_transitions_only() {
    let mut d = PanelDetector::new();
    // Unmatched resume (the ordinary open): no row.
    assert!(d.on_halt(&halt("AAA", mins(0), HaltTransitionKind::Resumed), mins(0)).is_none());
    let h = d.on_halt(&halt("AAA", mins(1), HaltTransitionKind::Halted), mins(1)).unwrap();
    assert_eq!(h.panel, "halt_actual");
    assert_eq!(h.extra["status"], json!("halted"));
    assert_eq!(h.extra["reason"], json!("Volatility Trading Pause"));
    // Repeated halted (quotation-only period): no row.
    assert!(d.on_halt(&halt("AAA", mins(5), HaltTransitionKind::Halted), mins(5)).is_none());
    let r = d.on_halt(&halt("AAA", mins(6), HaltTransitionKind::Resumed), mins(6)).unwrap();
    assert_eq!(r.extra["status"], json!("resumed"));
}

#[test]
fn record_serializes_exactly_to_schema_field_names() {
    let mut r = PanelRecord::new("ignition", "ESLA", Utc.with_ymd_and_hms(2026, 10, 16, 13, 41, 7).unwrap() + ChronoDuration::milliseconds(123), Some(0.5599), "event", Some(0.73), json!({}));
    r.captured_at = Utc.with_ymd_and_hms(2026, 10, 16, 13, 41, 7).unwrap() + ChronoDuration::milliseconds(300);
    r.attach_quote(Some(market_data::quote_book::LatestQuote {
        bid: 0.558,
        ask: 0.561,
        bid_size: 1,
        ask_size: 1,
        at: Utc.with_ymd_and_hms(2026, 10, 16, 13, 41, 6).unwrap() + ChronoDuration::milliseconds(950),
    }));
    let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
    let keys: std::collections::BTreeSet<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    let expected: std::collections::BTreeSet<&str> =
        ["schema", "panel", "symbol", "detectedAt", "capturedAt", "price", "priceSource", "bid", "ask", "quoteAt", "score", "extra"].into_iter().collect();
    assert_eq!(keys, expected);
    assert_eq!(v["schema"], json!("panel-detection-v1"));
    assert_eq!(v["detectedAt"], json!("2026-10-16T13:41:07.123Z"));
    assert_eq!(v["capturedAt"], json!("2026-10-16T13:41:07.300Z"));
    assert_eq!(v["quoteAt"], json!("2026-10-16T13:41:06.950Z"));
    assert_eq!(v["bid"], json!(0.558));
    // No quote: nulls, still present.
    let bare = PanelRecord::new("catalyst", "X", t0(), None, "trade", None, json!({}));
    let v: serde_json::Value = serde_json::to_value(&bare).unwrap();
    assert!(v["bid"].is_null() && v["ask"].is_null() && v["quoteAt"].is_null() && v["price"].is_null() && v["score"].is_null());
}

#[test]
fn kill_switch_values() {
    assert!(enabled_from(None));
    assert!(enabled_from(Some("1")));
    assert!(!enabled_from(Some("0")));
    assert!(!enabled_from(Some("false")));
    assert!(!enabled_from(Some(" OFF ")));
}

fn day(n: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 1, 1).unwrap() + chrono::Days::new(u64::from(n))
}

#[test]
fn retention_keeps_newest_n_days() {
    let files: Vec<(NaiveDate, u64)> = (0..95).map(|i| (day(i), 10)).collect();
    let policy = RetentionPolicy { keep_days: 90, max_bytes: u64::MAX };
    let doomed = select_for_deletion(&files, day(94), &policy);
    assert_eq!(doomed, (0..5).map(day).collect::<Vec<_>>());
}

#[test]
fn retention_size_cap_trims_oldest_but_never_today() {
    let files = vec![(day(0), 100), (day(1), 100), (day(2), 100), (day(3), 500)];
    let policy = RetentionPolicy { keep_days: 90, max_bytes: 250 };
    // Today (day 3) alone is over the cap: everything else goes, today stays.
    assert_eq!(select_for_deletion(&files, day(3), &policy), vec![day(0), day(1), day(2)]);
    let policy = RetentionPolicy { keep_days: 90, max_bytes: 650 };
    assert_eq!(select_for_deletion(&files, day(3), &policy), vec![day(0), day(1)]);
    assert!(select_for_deletion(&files, day(3), &RetentionPolicy { keep_days: 90, max_bytes: 10_000 }).is_empty());
}

#[test]
fn retention_on_disk_ignores_foreign_files() {
    let dir = std::env::temp_dir().join(format!("panel-det-ret-{}-{}", std::process::id(), Utc::now().timestamp_nanos_opt().unwrap()));
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..3 {
        std::fs::write(dir.join(file_name(day(i))), "x\n").unwrap();
    }
    std::fs::write(dir.join("notes.txt"), "keep").unwrap();
    let removed = apply_retention(&dir, day(2), &RetentionPolicy { keep_days: 1, max_bytes: u64::MAX });
    assert_eq!(removed, vec![day(0), day(1)]);
    assert!(dir.join("notes.txt").exists());
    assert!(dir.join(file_name(day(2))).exists());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn writer_drops_and_counts_when_full_never_blocks() {
    let (writer, _rx) = PanelWriter::detached(2);
    let rec = || PanelRecord::new("catalyst", "X", t0(), None, "trade", None, json!({}));
    assert!(writer.offer(rec()));
    assert!(writer.offer(rec()));
    let started = Instant::now();
    assert!(!writer.offer(rec()));
    assert!(!writer.offer(rec()));
    assert!(started.elapsed() < Duration::from_millis(100));
    assert_eq!(writer.health().dropped.load(Ordering::Relaxed), 2);
    assert_eq!(writer.health().offered.load(Ordering::Relaxed), 4);
}

#[test]
fn writer_thread_writes_jsonl_by_market_day() {
    let dir = std::env::temp_dir().join(format!("panel-det-w-{}-{}", std::process::id(), Utc::now().timestamp_nanos_opt().unwrap()));
    let writer = PanelWriter::start_with(dir.clone(), 16, RetentionPolicy::default()).unwrap();
    // 03:59 ET belongs to the previous market day (10-15).
    let before_open = Utc.with_ymd_and_hms(2026, 10, 16, 7, 59, 0).unwrap();
    writer.offer(PanelRecord::new("catalyst", "OLD", before_open, None, "trade", None, json!({})));
    writer.offer(PanelRecord::new("catalyst", "NEW", t0(), Some(1.0), "trade", None, json!({})));
    writer.offer(PanelRecord::new("catalyst", "NEW2", t0(), Some(1.0), "trade", None, json!({})));
    let health = writer.health().clone();
    drop(writer);
    let deadline = Instant::now() + Duration::from_secs(5);
    while health.written.load(Ordering::Relaxed) < 3 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(100));
    let old = std::fs::read_to_string(dir.join("2026-10-15.jsonl")).unwrap();
    let new = std::fs::read_to_string(dir.join("2026-10-16.jsonl")).unwrap();
    assert_eq!(old.lines().count(), 1);
    assert_eq!(new.lines().count(), 2);
    let row: serde_json::Value = serde_json::from_str(new.lines().next().unwrap()).unwrap();
    assert_eq!(row["symbol"], json!("NEW"));
    std::fs::remove_dir_all(&dir).ok();
}
