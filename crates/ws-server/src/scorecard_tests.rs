use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use chrono::TimeZone;
use market_data::Bar;

use super::*;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn z(y: i32, m: u32, day: u32, h: u32, min: u32) -> DateTime<Utc> {
    Utc.from_utc_datetime(&d(y, m, day).and_hms_opt(h, min, 0).unwrap())
}

fn temp_dir(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("ws-scorecard-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

// ---- schedule -------------------------------------------------------------------

#[test]
fn runs_at_1640_new_york_in_both_offsets() {
    // EDT: 16:40 ET = 20:40Z. EST: 21:40Z.
    assert_eq!(run_at(d(2026, 10, 15)), Some(z(2026, 10, 15, 20, 40)));
    assert_eq!(run_at(d(2026, 12, 1)), Some(z(2026, 12, 1, 21, 40)));
    // Early close (day after Thanksgiving) still runs at 16:40.
    assert_eq!(run_at(d(2026, 11, 27)), Some(z(2026, 11, 27, 21, 40)));
    // Weekend and holiday: never.
    assert_eq!(run_at(d(2026, 10, 17)), None);
    assert_eq!(run_at(d(2026, 11, 26)), None);
}

#[test]
fn the_due_day_is_the_latest_trading_day_past_1640() {
    // Thursday just before and at 16:40 ET.
    assert_eq!(last_due_day(z(2026, 10, 15, 20, 39)), Some(d(2026, 10, 14)));
    assert_eq!(last_due_day(z(2026, 10, 15, 20, 40)), Some(d(2026, 10, 15)));
    // Late evening / after UTC midnight still the same day.
    assert_eq!(last_due_day(z(2026, 10, 16, 3, 0)), Some(d(2026, 10, 15)));
    // Weekend -> Friday; Monday morning -> Friday.
    assert_eq!(last_due_day(z(2026, 10, 17, 15, 0)), Some(d(2026, 10, 16)));
    assert_eq!(last_due_day(z(2026, 10, 19, 14, 0)), Some(d(2026, 10, 16)));
    // Friday after Thanksgiving, before its run: Wednesday (Thursday is a holiday).
    assert_eq!(last_due_day(z(2026, 11, 27, 18, 0)), Some(d(2026, 11, 25)));
}

#[test]
fn next_run_skips_weekends_and_holidays() {
    assert_eq!(next_run(z(2026, 10, 15, 12, 0)), Some(z(2026, 10, 15, 20, 40)));
    assert_eq!(next_run(z(2026, 10, 15, 20, 40)), Some(z(2026, 10, 16, 20, 40)));
    assert_eq!(next_run(z(2026, 10, 16, 21, 0)), Some(z(2026, 10, 19, 20, 40)));
    assert_eq!(next_run(z(2026, 11, 25, 22, 0)), Some(z(2026, 11, 27, 21, 40)));
}

// ---- retention ------------------------------------------------------------------

#[test]
fn scorecard_retention_keeps_120_days() {
    let today = d(2026, 10, 16);
    let days = vec![d(2026, 10, 16), d(2026, 6, 18), d(2026, 6, 17), d(2026, 1, 2)];
    // cutoff = 2026-06-18: that day is kept, older ones go, oldest first.
    assert_eq!(scorecard_days_to_delete(&days, today, KEEP_SCORECARD_DAYS), vec![d(2026, 1, 2), d(2026, 6, 17)]);
    assert!(scorecard_days_to_delete(&[today], today, KEEP_SCORECARD_DAYS).is_empty());
}

#[test]
fn bars_retention_drops_old_days_then_oldest_until_under_the_cap() {
    let today = d(2026, 10, 16);
    let gib = 1u64 << 30;
    // Age only: 2026-09-15 is 31 days old, 09-16 exactly 30 (kept).
    let days = vec![(d(2026, 10, 15), 10), (d(2026, 9, 16), 10), (d(2026, 9, 15), 10)];
    assert_eq!(bars_days_to_delete(&days, today, KEEP_BARS_DAYS, gib), vec![d(2026, 9, 15)]);
    // Size: 4 x 400 MiB = 1.6 GiB -> drop the two oldest (0.8 GiB left).
    let m400 = 400 << 20;
    let days = vec![(d(2026, 10, 15), m400), (d(2026, 10, 13), m400), (d(2026, 10, 14), m400), (d(2026, 10, 12), m400)];
    assert_eq!(bars_days_to_delete(&days, today, KEEP_BARS_DAYS, gib), vec![d(2026, 10, 12), d(2026, 10, 13)]);
    // Old day removal also counts towards the cap.
    let days = vec![(d(2026, 8, 1), gib), (d(2026, 10, 15), 100)];
    assert_eq!(bars_days_to_delete(&days, today, KEEP_BARS_DAYS, gib), vec![d(2026, 8, 1)]);
    assert!(bars_days_to_delete(&[], today, KEEP_BARS_DAYS, gib).is_empty());
}

#[test]
fn retention_on_disk_only_touches_dated_names() {
    let root = temp_dir("retention");
    let conf = Config { out_dir: root.join("cards"), bars_dir: root.join("bars"), detections_dir: root.join("det"), pending: root.join("p.jsonl") };
    std::fs::create_dir_all(&conf.out_dir).unwrap();
    for name in ["2026-01-02.json", "2026-10-15.json", "latest.json", "notes.json"] {
        std::fs::write(conf.out_dir.join(name), "{}").unwrap();
    }
    for name in ["2026-08-01", "2026-10-15", "keep-me"] {
        std::fs::create_dir_all(conf.bars_dir.join(name)).unwrap();
        std::fs::write(conf.bars_dir.join(name).join("AAA.json"), "[]").unwrap();
    }
    apply_retention(&conf, d(2026, 10, 16));
    assert!(!conf.out_dir.join("2026-01-02.json").exists());
    for name in ["2026-10-15.json", "latest.json", "notes.json"] {
        assert!(conf.out_dir.join(name).exists(), "{name}");
    }
    assert!(!conf.bars_dir.join("2026-08-01").exists());
    assert!(conf.bars_dir.join("2026-10-15").exists());
    assert!(conf.bars_dir.join("keep-me").exists());
    let _ = std::fs::remove_dir_all(&root);
}

// ---- input ----------------------------------------------------------------------

fn pending_line(sym: &str, strategy: &str, ts: &str, price: f64) -> String {
    format!(r#"{{"symbol":"{sym}","strategy":"{strategy}","timestamp":"{ts}","signal_price":{price},"captured_at":"{ts}"}}"#)
}

#[test]
fn streaming_the_pending_file_counts_exactly_like_parsing_it_whole() {
    let root = temp_dir("stream");
    let path = root.join("live_pending_signals.jsonl");
    let lines = [
        pending_line("OLD", "MomentumScorer", "2026-10-14T15:00:00Z", 5.0),
        pending_line("AAA", "MomentumScorer", "2026-10-15T14:00:00Z", 5.0),
        // 21:30 ET on the 15th is 01:30Z on the 16th: still market day 15th.
        pending_line("BBB", "IgnitionDetector", "2026-10-16T01:30:00.5Z", 3.0),
        // 05:00 ET on the 16th: next market day, inside the prefilter's dates.
        pending_line("CCC", "MomentumScorer", "2026-10-16T09:00:00Z", 5.0),
        pending_line("DDD", "SomethingElse", "2026-10-15T14:00:00Z", 5.0),
        "not json".to_string(),
        pending_line("NEW", "MomentumScorer", "2026-10-20T14:00:00Z", 5.0),
    ];
    let text = lines.join("\n") + "\n\n";
    std::fs::write(&path, &text).unwrap();
    let day = d(2026, 10, 15);
    let (dets, rep) = stream_pending(&path, day).unwrap();
    let (whole, whole_rep) = parse_pending(&text, day);
    assert_eq!(dets, whole);
    assert_eq!(dets.iter().map(|x| x.symbol.as_str()).collect::<Vec<_>>(), vec!["AAA", "BBB"]);
    assert_eq!((rep.lines, rep.used), (whole_rep.lines, whole_rep.used));
    // The prefilter folds "not json" into otherDay (it cannot be this day);
    // everything else matches the whole-file parse.
    assert_eq!(rep.other_day + rep.unparsable + rep.unknown_panel, whole_rep.other_day + whole_rep.unparsable + whole_rep.unknown_panel);
    assert_eq!(rep.unknown_panel, 1);
    assert_eq!(rep.file.as_deref(), Some("live_pending_signals.jsonl"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_recorder_file_wins_over_the_fallback_and_nothing_is_not_an_error() {
    let root = temp_dir("source");
    let conf = Config { out_dir: root.join("cards"), bars_dir: root.join("bars"), detections_dir: root.join("det"), pending: root.join("p.jsonl") };
    let day = d(2026, 10, 15);
    let (dets, rep) = read_detections(&conf, day).unwrap();
    assert!(dets.is_empty());
    assert_eq!(rep.kind, SourceKind::None);

    std::fs::write(&conf.pending, pending_line("AAA", "MomentumScorer", "2026-10-15T14:00:00Z", 5.0) + "\n").unwrap();
    assert_eq!(read_detections(&conf, day).unwrap().1.kind, SourceKind::LivePendingFallback);

    std::fs::create_dir_all(&conf.detections_dir).unwrap();
    std::fs::write(conf.detections_dir.join("2026-10-15.jsonl"), "").unwrap();
    let (_, rep) = read_detections(&conf, day).unwrap();
    assert_eq!(rep.kind, SourceKind::PanelDetections);
    assert_eq!(rep.file.as_deref(), Some("2026-10-15.jsonl"));
    let _ = std::fs::remove_dir_all(&root);
}

// ---- a whole run, offline ---------------------------------------------------------

struct FakeBars {
    fail: bool,
    calls: AtomicUsize,
}

impl BarSource for FakeBars {
    fn minute_bars(&self, symbol: &str, start: DateTime<Utc>, end: DateTime<Utc>) -> impl std::future::Future<Output = Result<Vec<Bar>>> + Send {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let out = if self.fail {
            Err(anyhow::anyhow!("provider down"))
        } else {
            let mut v = Vec::new();
            let mut ts = start;
            let mut i = 0u32;
            while ts < end {
                let p = 10.0 + f64::from(i % 7) * 0.05;
                v.push(Bar { symbol: symbol.to_string(), open: p, high: p + 0.05, low: p - 0.05, close: p, volume: 100, timestamp: ts });
                ts += Duration::minutes(1);
                i += 1;
            }
            Ok(v)
        };
        async move { out }
    }
}

#[tokio::test]
async fn a_run_writes_the_day_and_latest_and_caches_bars() {
    let root = temp_dir("run");
    let conf = Config { out_dir: root.join("cards"), bars_dir: root.join("bars"), detections_dir: root.join("det"), pending: root.join("p.jsonl") };
    let lines = [
        pending_line("AAA", "MomentumScorer", "2026-10-15T14:00:00Z", 10.0),
        pending_line("BBB", "IgnitionDetector", "2026-10-15T15:00:00Z", 10.0),
        pending_line("AAA", "MomentumScorer", "2026-10-14T14:00:00Z", 10.0),
    ];
    std::fs::write(&conf.pending, lines.join("\n") + "\n").unwrap();
    let day = d(2026, 10, 15);

    // Every fetch failing writes nothing (so a retry can score the day).
    let down = FakeBars { fail: true, calls: AtomicUsize::new(0) };
    assert!(run_day(&conf, &down, day).await.is_err());
    assert!(!day_file(&conf.out_dir, day).exists());

    let src = FakeBars { fail: false, calls: AtomicUsize::new(0) };
    run_day(&conf, &src, day).await.unwrap();
    assert_eq!(src.calls.load(Ordering::SeqCst), 2);
    let report: DayReport = serde_json::from_slice(&std::fs::read(day_file(&conf.out_dir, day)).unwrap()).unwrap();
    assert_eq!(report.day, day);
    assert_eq!(report.source.kind, SourceKind::LivePendingFallback);
    assert_eq!(report.rows.len(), 2);
    let latest: serde_json::Value = serde_json::from_slice(&std::fs::read(conf.out_dir.join("latest.json")).unwrap()).unwrap();
    assert_eq!(latest["latest"]["day"], "2026-10-15");
    assert!(latest.get("rows").is_none() && latest["latest"].get("rows").is_none());
    assert!(conf.bars_dir.join("2026-10-15").join("AAA.json").exists());
    // No raw bars in the scorecard directory.
    let names: Vec<String> = std::fs::read_dir(&conf.out_dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    assert_eq!(names.len(), 2, "{names:?}");

    // Rerun is served from the cache.
    run_day(&conf, &src, day).await.unwrap();
    assert_eq!(src.calls.load(Ordering::SeqCst), 2);
    let _ = std::fs::remove_dir_all(&root);
}

// ---- HTTP -----------------------------------------------------------------------

#[test]
fn day_parameter_is_strict() {
    assert_eq!(parse_day("2026-10-16"), Some(d(2026, 10, 16)));
    for bad in ["../../etc/passwd", "..", "2026-1-16", "2026-10-16.json", "2026-02-30", "2026-10-16/", " 2026-10-16", "latest", ""] {
        assert_eq!(parse_day(bad), None, "{bad:?}");
    }
}

#[test]
fn rows_are_stripped_and_counted() {
    let out = without_rows(br#"{"day":"2026-10-15","panels":[],"rows":[{},{},{}]}"#).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert!(v.get("rows").is_none());
    assert_eq!(v["rowCount"], 3);
    assert_eq!(v["day"], "2026-10-15");
}

async fn serve(dir: PathBuf) -> (String, tokio::task::JoinHandle<()>) {
    let access = crate::access::Access::for_tests("scorecard-test-secret", Arc::new(crate::access::AuthLimiter::new()));
    let app = routes(dir).layer(axum::middleware::from_fn_with_state(access, crate::access::protect));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await;
    });
    (base, server)
}

#[tokio::test]
async fn routes_require_the_credential() {
    let dir = temp_dir("http-auth");
    std::fs::write(dir.join("latest.json"), "{}").unwrap();
    std::fs::write(dir.join("2026-10-15.json"), "{}").unwrap();
    let (base, server) = serve(dir.clone()).await;
    let client = reqwest::Client::new();
    for path in ["/scorecard/latest", "/scorecard/days", "/scorecard/day/2026-10-15", "/scorecard/day/2026-10-15?rows=1"] {
        let resp = client.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 401, "{path}");
        let resp = client.get(format!("{base}{path}")).bearer_auth("wrong-secret-of-same-len").send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 401, "{path}");
    }
    server.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn routes_serve_latest_days_and_one_day() {
    let dir = temp_dir("http");
    // Secret sibling a traversal would try to reach.
    std::fs::write(dir.parent().unwrap().join(format!("ws-scorecard-secret-{}.json", std::process::id())), "{\"secret\":1}").unwrap();
    let (base, server) = serve(dir.clone()).await;
    let client = reqwest::Client::new();
    let get = |path: &str| client.get(format!("{base}{path}")).bearer_auth("scorecard-test-secret").send();

    // Nothing yet.
    assert_eq!(get("/scorecard/latest").await.unwrap().status().as_u16(), 404);
    let resp = get("/scorecard/days").await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(resp.json::<serde_json::Value>().await.unwrap(), serde_json::json!({ "days": [] }));
    assert_eq!(get("/scorecard/day/2026-10-15").await.unwrap().status().as_u16(), 404);

    std::fs::write(dir.join("latest.json"), r#"{"schema":"panel-scorecard-v1","rolling":{}}"#).unwrap();
    std::fs::write(dir.join("2026-10-14.json"), r#"{"day":"2026-10-14","rows":[]}"#).unwrap();
    std::fs::write(dir.join("2026-10-15.json"), r#"{"day":"2026-10-15","panels":[],"rows":[{"symbol":"AAA"},{"symbol":"BBB"}]}"#).unwrap();
    std::fs::write(dir.join("2026-10-13.json.tmp"), "{}").unwrap();
    std::fs::write(dir.join("junk.json"), "{}").unwrap();

    let resp = get("/scorecard/latest").await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(resp.headers()["content-type"], "application/json");
    assert_eq!(resp.json::<serde_json::Value>().await.unwrap()["schema"], "panel-scorecard-v1");

    let resp = get("/scorecard/days").await.unwrap();
    assert_eq!(resp.headers()["content-type"], "application/json");
    assert_eq!(resp.json::<serde_json::Value>().await.unwrap(), serde_json::json!({ "days": ["2026-10-15", "2026-10-14"] }));

    let resp = get("/scorecard/day/2026-10-15").await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(resp.headers()["content-type"], "application/json");
    let v: serde_json::Value = resp.json().await.unwrap();
    assert!(v.get("rows").is_none());
    assert_eq!(v["rowCount"], 2);
    assert_eq!(v["day"], "2026-10-15");

    let v: serde_json::Value = get("/scorecard/day/2026-10-15?rows=1").await.unwrap().json().await.unwrap();
    assert_eq!(v["rows"].as_array().unwrap().len(), 2);

    assert_eq!(get("/scorecard/day/2026-10-16").await.unwrap().status().as_u16(), 404);
    assert_eq!(get("/scorecard/day/2026-1-5").await.unwrap().status().as_u16(), 400);
    assert_eq!(get("/scorecard/day/latest").await.unwrap().status().as_u16(), 400);
    // Encoded traversal reaches the handler as "../ws-scorecard-secret-N" and is refused.
    let traversal = format!("/scorecard/day/..%2Fws-scorecard-secret-{}", std::process::id());
    let resp = get(&traversal).await.unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    assert!(!resp.text().await.unwrap().contains("secret\":1"));
    let resp = get("/scorecard/day/..%2F..%2Fetc%2Fpasswd").await.unwrap();
    assert_eq!(resp.status().as_u16(), 400);

    server.abort();
    let _ = std::fs::remove_file(dir.parent().unwrap().join(format!("ws-scorecard-secret-{}.json", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir);
}
