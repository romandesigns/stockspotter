//! Panel scorecard, run inside this process after every US trading day
//! (2026-10-16). The scoring itself is `backtest_metrics::panel_scorecard`
//! (read its module docs for every definition); this module only decides
//! *when* to run it, feeds it the day's detections, writes the result, keeps
//! the directories bounded and serves the files over authenticated HTTP.
//!
//! # Schedule
//!
//! A day is due at 16:40 America/New_York on a day with a regular session
//! (`market_data::trading_session::regular_session_open`, so weekends and NYSE
//! holidays are never due; an early close still runs at 16:40). The rule
//! applied at startup and after every wake-up is the same: **the most recent
//! due day whose `YYYY-MM-DD.json` is missing gets one run**. So a process
//! started at 18:00 on a trading day scores that day, one started on a
//! Saturday scores Friday, and one started at 10:00 scores the previous
//! trading day if it was missed. Older gaps are not back-filled (the
//! `panel_scorecard` bin does that by hand). A failed run (for instance every
//! bar fetch failing, which writes nothing) logs WARN and is retried at most
//! three times in all, 30 minutes apart, per process; then the day is left
//! until a restart rather than hammering the data provider.
//!
//! One sequential task does everything, so at most one run is ever in
//! flight. Each run is spawned and awaited on its own, so a panic inside the
//! scorer is a WARN line, not a dead scheduler; CPU and file work (parsing the
//! detections, bootstrap statistics, writing, retention) is on the blocking
//! pool, and the bar fetches are paced async HTTP. Nothing here touches the
//! live feed. `SCORECARD=0` turns the whole job off.
//!
//! # Inputs
//!
//! `data/panel-detections/YYYY-MM-DD.jsonl` (the recorder, PR #94) when it
//! exists; otherwise `data/live_pending_signals.jsonl`, which is append-only
//! and hundreds of MB, so it is **streamed** line by line and only lines
//! whose `"timestamp"` starts with one of the two UTC dates a market day spans
//! (04:00 ET to 04:00 ET next day) are kept for parsing. Lines dropped by that
//! prefilter are added to the report's `lines` and `otherDay` counts, so the
//! accounting is the same as parsing the whole file.
//!
//! # Disk
//!
//! The VPS is short of disk. Day files keep 120 calendar days; the bars cache
//! keeps 30 days and at most 1 GiB (oldest days dropped first). Only
//! `YYYY-MM-DD` names are ever deleted. No raw bars are written to the
//! scorecard directory.

use std::io::BufRead;
use std::path::{Path as FsPath, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use backtest_metrics::panel_scorecard::{
    build_day_report, build_latest, load_day_bars, parse_detections, parse_pending, plan, AlpacaBars, BarSource, DayReport, Detection,
    SourceKind, SourceReport, BOOTSTRAP_B, MAX_SYMBOLS_PER_DAY, PRIMARY, ROLLING_SESSIONS, SCHEMA,
};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use market_data::trading_session::{market_day, regular_session_open};
use market_data::AlpacaConfig;
use serde::Deserialize;
use tracing::{info, warn};

pub const SCORECARD_DIR: &str = "data/scorecards";
pub const BARS_CACHE_DIR: &str = "data/scorecard-bars";
pub const DETECTIONS_DIR: &str = "data/panel-detections";
pub const PENDING_FALLBACK: &str = "data/live_pending_signals.jsonl";

/// Day files older than this many calendar days are deleted.
pub const KEEP_SCORECARD_DAYS: i64 = 120;
/// `/scorecard/days` lists at most this many.
pub const MAX_LISTED_DAYS: usize = 120;
/// Bars-cache days older than this many calendar days are deleted.
pub const KEEP_BARS_DAYS: i64 = 30;
pub const BARS_CACHE_MAX_BYTES: u64 = 1 << 30;
/// 09:30 + 7h10m = 16:40 ET. Added to the regular open rather than computed
/// from a time zone so this crate needs no tz dependency of its own; US DST
/// changes happen on Sundays, never inside a trading day.
const RUN_AFTER_OPEN_MINUTES: i64 = 7 * 60 + 10;
/// Longest single sleep: re-checking hourly keeps the schedule right across
/// a suspended host or a clock step without any busy waiting.
const MAX_SLEEP_SECS: i64 = 3600;
/// Pacing between Alpaca bar requests, as in the `panel_scorecard` bin.
const BAR_SPACING_MS: u64 = 350;
/// A failed day (provider outage, say) is retried this many times in all...
const MAX_ATTEMPTS: u32 = 3;
/// ...this far apart.
const RETRY_SECS: i64 = 1800;

// ---- schedule -------------------------------------------------------------------

/// When `day` is due, or `None` when it has no regular session.
pub fn run_at(day: NaiveDate) -> Option<DateTime<Utc>> {
    Some(regular_session_open(day)? + Duration::minutes(RUN_AFTER_OPEN_MINUTES))
}

/// The most recent trading day whose 16:40 ET is at or before `now`.
pub fn last_due_day(now: DateTime<Utc>) -> Option<NaiveDate> {
    let start = market_day(now) + Duration::days(1);
    (0..20).map(|k| start - Duration::days(k)).find(|d| run_at(*d).is_some_and(|t| t <= now))
}

/// The next 16:40 ET strictly after `now` on a trading day.
pub fn next_run(now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let start = market_day(now);
    (0..20).filter_map(|k| run_at(start + Duration::days(k))).find(|t| *t > now)
}

#[derive(Debug, Clone)]
pub struct Config {
    pub out_dir: PathBuf,
    pub bars_dir: PathBuf,
    pub detections_dir: PathBuf,
    pub pending: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            out_dir: SCORECARD_DIR.into(),
            bars_dir: BARS_CACHE_DIR.into(),
            detections_dir: DETECTIONS_DIR.into(),
            pending: PENDING_FALLBACK.into(),
        }
    }
}

pub fn enabled() -> bool {
    std::env::var("SCORECARD").map_or(true, |v| v.trim() != "0")
}

fn day_file(dir: &FsPath, day: NaiveDate) -> PathBuf {
    dir.join(format!("{}.json", day.format("%Y-%m-%d")))
}

/// Starts the scheduler unless `SCORECARD=0`. Never fails the caller.
pub fn spawn(cfg: AlpacaConfig) {
    if !enabled() {
        info!("panel scorecard disabled (SCORECARD=0)");
        return;
    }
    tokio::spawn(scheduler(Config::default(), cfg));
}

async fn scheduler(conf: Config, alpaca: AlpacaConfig) {
    info!(out = %conf.out_dir.display(), "panel scorecard scheduler started: runs 16:40 America/New_York on trading days");
    {
        let c = conf.clone();
        let today = market_day(Utc::now());
        if let Err(e) = tokio::task::spawn_blocking(move || apply_retention(&c, today)).await {
            warn!(error = %e, "scorecard retention at startup panicked");
        }
    }
    // (day, attempts): a failed day is retried at most MAX_ATTEMPTS times,
    // RETRY_SECS apart, then left alone until the next day.
    let mut attempts: (Option<NaiveDate>, u32) = (None, 0);
    loop {
        let now = Utc::now();
        let mut failed = false;
        if let Some(day) = last_due_day(now) {
            if attempts.0 != Some(day) {
                attempts = (Some(day), 0);
            }
            if attempts.1 < MAX_ATTEMPTS && !day_file(&conf.out_dir, day).exists() {
                attempts.1 += 1;
                let (c, a) = (conf.clone(), alpaca.clone());
                // Spawned so a panic in the scorer is reported, not fatal.
                let job = tokio::spawn(async move {
                    let src = AlpacaBars { cfg: a, min_spacing: std::time::Duration::from_millis(BAR_SPACING_MS) };
                    run_day(&c, &src, day).await
                });
                let attempt = attempts.1;
                match job.await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        failed = true;
                        warn!(%day, attempt, max = MAX_ATTEMPTS, error = %format!("{e:#}"), "panel scorecard run failed");
                    }
                    Err(e) => {
                        failed = true;
                        warn!(%day, attempt, max = MAX_ATTEMPTS, error = %e, "panel scorecard run panicked");
                    }
                }
            }
        }
        let now = Utc::now();
        let mut wait = next_run(now).map_or(MAX_SLEEP_SECS, |t| (t - now).num_seconds().clamp(1, MAX_SLEEP_SECS));
        if failed && attempts.1 < MAX_ATTEMPTS {
            wait = wait.min(RETRY_SECS);
        }
        tokio::time::sleep(std::time::Duration::from_secs(wait as u64)).await;
    }
}

// ---- one run --------------------------------------------------------------------

/// Reads the day's detections: the recorder file when present, otherwise the
/// streamed pending-signals fallback, otherwise nothing (an empty report is
/// still written, so the day reads as "no data" rather than as missing).
pub fn read_detections(conf: &Config, day: NaiveDate) -> Result<(Vec<Detection>, SourceReport)> {
    let det_file = conf.detections_dir.join(format!("{}.jsonl", day.format("%Y-%m-%d")));
    if det_file.exists() {
        let text = std::fs::read_to_string(&det_file).with_context(|| format!("reading {}", det_file.display()))?;
        let (d, mut s) = parse_detections(&text, day);
        s.file = det_file.file_name().map(|n| n.to_string_lossy().into_owned());
        return Ok((d, s));
    }
    if conf.pending.exists() {
        return stream_pending(&conf.pending, day);
    }
    Ok((Vec::new(), SourceReport::empty(SourceKind::None, "no panel-detections file and no live_pending_signals.jsonl")))
}

/// `parse_pending` over only the lines that can belong to `day`, read one
/// line at a time so the whole file is never in memory.
pub fn stream_pending(path: &FsPath, day: NaiveDate) -> Result<(Vec<Detection>, SourceReport)> {
    let needles: Vec<String> = [day, day + Duration::days(1)].iter().map(|d| format!("\"timestamp\":\"{}", d.format("%Y-%m-%d"))).collect();
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut reader = std::io::BufReader::with_capacity(1 << 20, file);
    let mut kept = String::new();
    let mut skipped = 0usize;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf).with_context(|| format!("reading {}", path.display()))? == 0 {
            break;
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if needles.iter().any(|n| line.contains(n.as_str())) {
            kept.push_str(line);
            kept.push('\n');
        } else {
            skipped += 1;
        }
    }
    let (d, mut s) = parse_pending(&kept, day);
    s.lines += skipped;
    s.other_day += skipped;
    s.file = path.file_name().map(|n| n.to_string_lossy().into_owned());
    Ok((d, s))
}

fn write_atomic(path: &FsPath, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))
}

/// The newest `n` day reports at or before `day` (not all 120: only the
/// rolling window is needed, and each file carries every row).
fn load_recent_reports(dir: &FsPath, day: NaiveDate, n: usize) -> Vec<DayReport> {
    let mut days = list_days(dir);
    days.retain(|d| *d <= day);
    days.truncate(n);
    days.into_iter()
        .filter_map(|d| {
            let p = day_file(dir, d);
            let text = std::fs::read_to_string(&p).ok()?;
            match serde_json::from_str::<DayReport>(&text) {
                Ok(r) if r.schema == SCHEMA => Some(r),
                _ => {
                    warn!(file = %p.display(), "ignoring scorecard file (not a {SCHEMA} day report)");
                    None
                }
            }
        })
        .collect()
}

/// Scores `day`, writes `YYYY-MM-DD.json` and `latest.json`, applies
/// retention. Generic over the bar source so tests never touch the network.
pub async fn run_day<S: BarSource + Sync>(conf: &Config, src: &S, day: NaiveDate) -> Result<()> {
    let started = Instant::now();
    info!(%day, "panel scorecard run starting");
    let c = conf.clone();
    let (dets, source) = tokio::task::spawn_blocking(move || read_detections(&c, day)).await??;
    let source_kind = source.kind;
    let p = tokio::task::spawn_blocking(move || plan(day, dets, MAX_SYMBOLS_PER_DAY)).await?;
    info!(
        %day,
        source = ?source_kind,
        detections_kept = p.sampling.detections_kept,
        detections_available = p.sampling.detections_available,
        symbols = p.sampling.symbols_scored,
        sampled = p.sampling.sampled,
        "panel scorecard: fetching bars"
    );
    std::fs::create_dir_all(&conf.bars_dir).with_context(|| format!("creating {}", conf.bars_dir.display()))?;
    let now = Utc::now();
    let (bars, bars_report) = load_day_bars(src, Some(&conf.bars_dir), &p.symbols, day, now).await?;
    if !p.symbols.is_empty() && bars_report.fetched + bars_report.from_cache == 0 {
        // Every fetch failed: writing this would freeze an all-noBars day
        // that a retry could score. Nothing is written; the scheduler retries.
        anyhow::bail!("no bars for any of {} symbols (provider down or credentials rejected?)", p.symbols.len());
    }
    if !bars_report.failed.is_empty() {
        warn!(%day, failed = bars_report.failed.len(), "panel scorecard: no bars for some symbols; their detections are skipped as noBars");
    }
    let (api_calls, from_cache) = (bars_report.api_calls, bars_report.from_cache);

    let c = conf.clone();
    let latest = tokio::task::spawn_blocking(move || -> Result<_> {
        let cost = backtest_metrics::round_trip_cost_pct();
        let report = build_day_report(&p, source, &bars, bars_report, cost, now, BOOTSTRAP_B);
        drop(bars);
        std::fs::create_dir_all(&c.out_dir).with_context(|| format!("creating {}", c.out_dir.display()))?;
        write_atomic(&day_file(&c.out_dir, day), &serde_json::to_vec(&report)?)?;
        let history = load_recent_reports(&c.out_dir, day, ROLLING_SESSIONS);
        let latest = build_latest(&report, &history, now, BOOTSTRAP_B);
        write_atomic(&c.out_dir.join("latest.json"), &serde_json::to_vec(&latest)?)?;
        apply_retention(&c, day);
        Ok(latest)
    })
    .await??;

    info!(
        %day,
        secs = started.elapsed().as_secs(),
        scored = latest.rolling.detections_scored,
        rolling_days = latest.rolling.days.len(),
        api_calls,
        from_cache,
        "panel scorecard run finished"
    );
    for ps in &latest.rolling.panels {
        let h = &ps.horizons[PRIMARY];
        info!(
            panel = ps.panel.as_str(),
            n = h.n,
            days = ps.sessions,
            mean_pct = ?h.mean_net_pct,
            baseline_pct = ?h.baseline_mean_net_pct,
            "panel scorecard verdict: {} ({})",
            ps.verdict.as_str(),
            ps.verdict_reason
        );
    }
    Ok(())
}

// ---- retention ------------------------------------------------------------------

/// Day files to delete: dated before `today - keep_days`.
pub fn scorecard_days_to_delete(days: &[NaiveDate], today: NaiveDate, keep_days: i64) -> Vec<NaiveDate> {
    let cutoff = today - Duration::days(keep_days);
    let mut out: Vec<NaiveDate> = days.iter().copied().filter(|d| *d < cutoff).collect();
    out.sort();
    out
}

/// Bars-cache days to delete, oldest first: every day before
/// `today - keep_days`, then the oldest remaining until the total is at or
/// under `cap_bytes`.
pub fn bars_days_to_delete(days: &[(NaiveDate, u64)], today: NaiveDate, keep_days: i64, cap_bytes: u64) -> Vec<NaiveDate> {
    let mut sorted = days.to_vec();
    sorted.sort();
    let cutoff = today - Duration::days(keep_days);
    let mut total: u64 = sorted.iter().map(|(_, b)| b).sum();
    let mut out = Vec::new();
    for (d, b) in sorted {
        if d < cutoff || total > cap_bytes {
            out.push(d);
            total -= b;
        }
    }
    out
}

fn dated_entries(dir: &FsPath, suffix: &str) -> Vec<(NaiveDate, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    rd.filter_map(|e| {
        let e = e.ok()?;
        let name = e.file_name().into_string().ok()?;
        let stem = name.strip_suffix(suffix)?;
        Some((parse_day(stem)?, e.path()))
    })
    .collect()
}

fn dir_bytes(p: &FsPath) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else { return 0 };
    rd.filter_map(|e| e.ok()).map(|e| e.metadata().map_or(0, |m| if m.is_dir() { dir_bytes(&e.path()) } else { m.len() })).sum()
}

/// Applies both retention rules relative to `today`. Failures are WARN.
pub fn apply_retention(conf: &Config, today: NaiveDate) {
    let files = dated_entries(&conf.out_dir, ".json");
    let days: Vec<NaiveDate> = files.iter().map(|(d, _)| *d).collect();
    let del = scorecard_days_to_delete(&days, today, KEEP_SCORECARD_DAYS);
    for (d, p) in &files {
        if del.contains(d) {
            if let Err(e) = std::fs::remove_file(p) {
                warn!(file = %p.display(), error = %e, "scorecard retention: could not delete");
            }
        }
    }
    let dirs: Vec<(NaiveDate, PathBuf)> = dated_entries(&conf.bars_dir, "").into_iter().filter(|(_, p)| p.is_dir()).collect();
    let sized: Vec<(NaiveDate, u64)> = dirs.iter().map(|(d, p)| (*d, dir_bytes(p))).collect();
    let del = bars_days_to_delete(&sized, today, KEEP_BARS_DAYS, BARS_CACHE_MAX_BYTES);
    for (d, p) in &dirs {
        if del.contains(d) {
            if let Err(e) = std::fs::remove_dir_all(p) {
                warn!(dir = %p.display(), error = %e, "scorecard bars retention: could not delete");
            }
        }
    }
    if !del.is_empty() {
        info!(days = del.len(), "scorecard bars retention: dropped cached days");
    }
}

// ---- HTTP -----------------------------------------------------------------------

/// Strict `YYYY-MM-DD` (round-trips exactly), so a path parameter can never
/// name anything but a day file.
pub fn parse_day(s: &str) -> Option<NaiveDate> {
    if s.len() != 10 {
        return None;
    }
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?;
    (d.format("%Y-%m-%d").to_string() == s).then_some(d)
}

/// Available day files, newest first, at most `MAX_LISTED_DAYS`.
pub fn list_days(dir: &FsPath) -> Vec<NaiveDate> {
    let mut days: Vec<NaiveDate> = dated_entries(dir, ".json").into_iter().map(|(d, _)| d).collect();
    days.sort_by(|a, b| b.cmp(a));
    days.dedup();
    days.truncate(MAX_LISTED_DAYS);
    days
}

/// The routes, with their own state (the scorecard directory). Merged into
/// the main router before its `protect` layer, so they are authenticated
/// exactly like every other data route.
pub fn routes(dir: PathBuf) -> Router {
    Router::new()
        .route("/scorecard/latest", get(get_latest))
        .route("/scorecard/days", get(get_days))
        .route("/scorecard/day/:day", get(get_day))
        .with_state(std::sync::Arc::new(dir))
}

type Dir = std::sync::Arc<PathBuf>;

fn json_bytes(bytes: Vec<u8>) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], bytes).into_response()
}

fn not_found(what: &str) -> Response {
    (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "application/json")], serde_json::json!({ "error": format!("{what} not found") }).to_string())
        .into_response()
}

async fn get_latest(State(dir): State<Dir>) -> Response {
    match tokio::fs::read(dir.join("latest.json")).await {
        Ok(b) => json_bytes(b),
        Err(_) => not_found("latest scorecard"),
    }
}

async fn get_days(State(dir): State<Dir>) -> Response {
    let days = tokio::task::spawn_blocking(move || list_days(&dir)).await.unwrap_or_default();
    let days: Vec<String> = days.iter().map(|d| d.format("%Y-%m-%d").to_string()).collect();
    json_bytes(serde_json::to_vec(&serde_json::json!({ "days": days })).unwrap_or_default())
}

#[derive(Debug, Deserialize)]
pub struct DayQuery {
    rows: Option<String>,
}

async fn get_day(State(dir): State<Dir>, Path(day): Path<String>, Query(q): Query<DayQuery>) -> Response {
    let Some(day) = parse_day(&day) else {
        return (StatusCode::BAD_REQUEST, [(header::CONTENT_TYPE, "application/json")], r#"{"error":"day must be YYYY-MM-DD"}"#).into_response();
    };
    let Ok(bytes) = tokio::fs::read(day_file(&dir, day)).await else { return not_found("scorecard day") };
    if matches!(q.rows.as_deref(), Some("1") | Some("true")) {
        return json_bytes(bytes);
    }
    // Rows are the bulk of a day file; strip them by default.
    match tokio::task::spawn_blocking(move || without_rows(&bytes)).await {
        Ok(Some(b)) => json_bytes(b),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "unreadable scorecard file").into_response(),
    }
}

/// The day file minus `rows`, with `rowCount` in its place.
pub fn without_rows(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut v: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(bytes).ok()?;
    let n = v.remove("rows").and_then(|r| r.as_array().map(Vec::len)).unwrap_or(0);
    v.insert("rowCount".into(), n.into());
    serde_json::to_vec(&v).ok()
}

#[cfg(test)]
#[path = "scorecard_tests.rs"]
mod tests;
