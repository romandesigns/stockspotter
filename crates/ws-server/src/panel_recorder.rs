//! Panel detection recorder (2026-10-16): one JSONL row per thing a scanner
//! panel newly showed, so each panel's hit rate can be evaluated later
//! against real subsequent price action. Format `panel-detection-v1`, spec in
//! `outputs/panel-evaluation-20261016/SCHEMA.md`.
//!
//! Shape, same as the other broadcast subscribers in main.rs:
//!
//! * `PanelDetector` is pure and synchronous: it turns `ScanEvent`s, movers
//!   snapshots and halt transitions into records on EDGES only (never a raw
//!   `halt_warning`), with per-market-day state.
//! * The subscriber task in main.rs feeds it and hands each record to
//!   `PanelWriter::offer`, which is a `try_send` into a bounded channel --
//!   a full channel drops and counts, it never waits. The broadcast
//!   receiver therefore never blocks on disk.
//! * A dedicated writer THREAD owns the file: buffered, flushed about once
//!   a second, rotated by market day, retention applied at startup and on
//!   every day change. A write error (ENOSPC, ...) is logged and counted;
//!   the thread keeps going and ws-server never crashes on it.
//!
//! Market day is `market_data::market_day` (04:00 America/New_York boundary),
//! the definition every other research file in this process already uses.
//!
//! Kill switch: `PANEL_DETECTIONS=0`. Directory: `PANEL_DETECTIONS_DIR`,
//! default `data/panel-detections` relative to the process cwd, like every
//! other `data/` path in main.rs.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use market_data::halt_tap::{HaltStatus, HaltTransitionKind};
use market_data::{market_day, ConsolidationEventKind, ConsolidationStrategy, IgnitionEventKind, Mover, ScanEvent, TodayMovers};
use serde::Serialize;
use serde_json::json;
use tracing::{info, warn};

use crate::speed_alert::SpeedAlertTracker;

pub const SCHEMA: &str = "panel-detection-v1";
pub const DEFAULT_DIR: &str = "data/panel-detections";
pub const ENV_SWITCH: &str = "PANEL_DETECTIONS";
pub const ENV_DIR: &str = "PANEL_DETECTIONS_DIR";

/// Ignition: first follow-through per symbol per this window (event time).
pub const IGNITION_COOLDOWN_MS: i64 = 10 * 60_000;
/// Micropullback / consolidation breakout entry triggers: per-symbol cooldown.
pub const ENTRY_COOLDOWN_MS: i64 = 10 * 60_000;
/// Movers panels show this many rows.
pub const MOVERS_TOP_N: usize = 25;

/// Bounded queue between the subscriber and the writer thread. Rows are
/// rare (a few thousand a day); this only fills if the disk stalls.
pub const CHANNEL_CAPACITY: usize = 8_192;
pub const FLUSH_INTERVAL: Duration = Duration::from_secs(1);
pub const RETENTION_MARKET_DAYS: usize = 90;
pub const RETENTION_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Enabled unless `PANEL_DETECTIONS` is `0`/`false`/`off`.
pub fn enabled_from(value: Option<&str>) -> bool {
    !matches!(value.map(|v| v.trim().to_ascii_lowercase()).as_deref(), Some("0" | "false" | "off" | "no"))
}

fn ts_millis<S: serde::Serializer>(t: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&t.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn opt_ts_millis<S: serde::Serializer>(t: &Option<DateTime<Utc>>, s: S) -> Result<S::Ok, S::Error> {
    match t {
        Some(t) => ts_millis(t, s),
        None => s.serialize_none(),
    }
}

/// One `panel-detection-v1` row. Every field is always present (nulls
/// included), so a reader never has to distinguish "absent" from "null".
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelRecord {
    pub schema: &'static str,
    pub panel: &'static str,
    pub symbol: String,
    #[serde(serialize_with = "ts_millis")]
    pub detected_at: DateTime<Utc>,
    #[serde(serialize_with = "ts_millis")]
    pub captured_at: DateTime<Utc>,
    pub price: Option<f64>,
    pub price_source: &'static str,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    #[serde(serialize_with = "opt_ts_millis")]
    pub quote_at: Option<DateTime<Utc>>,
    pub score: Option<f64>,
    pub extra: serde_json::Value,
}

impl PanelRecord {
    fn new(panel: &'static str, symbol: &str, detected_at: DateTime<Utc>, price: Option<f64>, price_source: &'static str, score: Option<f64>, extra: serde_json::Value) -> Self {
        Self {
            schema: SCHEMA,
            panel,
            symbol: symbol.to_string(),
            detected_at,
            // Overwritten by the subscriber when it actually hands the row
            // off; set here so a pure caller still gets a sane value.
            captured_at: detected_at,
            price: price.filter(|p| p.is_finite()),
            price_source,
            bid: None,
            ask: None,
            quote_at: None,
            score: score.filter(|s| s.is_finite()),
            extra,
        }
    }

    /// Fills the quote fields from `market_data::quote_book` (null when the
    /// symbol has no quote, as universe-mode ignition symbols usually do).
    pub fn attach_quote(&mut self, quote: Option<market_data::quote_book::LatestQuote>) {
        if let Some(q) = quote {
            self.bid = Some(q.bid).filter(|v| v.is_finite());
            self.ask = Some(q.ask).filter(|v| v.is_finite());
            self.quote_at = Some(q.at);
        }
    }
}

#[derive(Debug, Default)]
struct CooldownState {
    last_ms: i64,
    suppressed: u64,
}

/// Pure edge/de-dup logic for every panel. See SCHEMA.md for the rules.
#[derive(Debug, Default)]
pub struct PanelDetector {
    day: Option<NaiveDate>,
    momentum: HashMap<String, bool>,
    gap_and_go: HashMap<String, bool>,
    ignition: HashMap<String, CooldownState>,
    /// Keyed by (panel, symbol).
    entries: HashMap<(&'static str, String), i64>,
    top_gainers: Option<HashSet<String>>,
    highly_trading: Option<HashSet<String>>,
    movers_observed_at: Option<DateTime<Utc>>,
    halts: HashMap<String, HaltTransitionKind>,
    /// Latest price per symbol, for panels whose event carries none
    /// (momentum, catalyst). Cleared at the day change.
    last_price: HashMap<String, (f64, &'static str)>,
    /// Process lifetime, deliberately not reset per day: it is the same
    /// tracker the speed push uses, and has its own first-reading baseline.
    speed: SpeedAlertTracker,
    /// True until the first movers snapshot this process records.
    movers_first_snapshot: bool,
}

impl PanelDetector {
    pub fn new() -> Self {
        Self { movers_first_snapshot: true, ..Self::default() }
    }

    /// Resets per-market-day edge state when `now`'s market day is later
    /// than the one being tracked. The day only moves forward.
    pub fn roll_day(&mut self, now: DateTime<Utc>) {
        let day = market_day(now);
        if self.day.is_some_and(|d| d >= day) {
            return;
        }
        self.day = Some(day);
        self.momentum.clear();
        self.gap_and_go.clear();
        self.ignition.clear();
        self.entries.clear();
        // Empty, not None: on a new day every symbol in the first scan
        // newly entered that day's list.
        self.top_gainers = self.top_gainers.as_ref().map(|_| HashSet::new());
        self.highly_trading = self.highly_trading.as_ref().map(|_| HashSet::new());
        self.halts.clear();
        self.last_price.clear();
    }

    fn note_price(&mut self, symbol: &str, price: f64, source: &'static str) {
        if !price.is_finite() {
            return;
        }
        match self.last_price.get_mut(symbol) {
            Some(slot) => *slot = (price, source),
            None => {
                self.last_price.insert(symbol.to_string(), (price, source));
            }
        }
    }

    fn price_of(&self, symbol: &str) -> (Option<f64>, &'static str) {
        match self.last_price.get(symbol) {
            Some((p, src)) => (Some(*p), src),
            None => (None, "trade"),
        }
    }

    /// Bool edge per symbol per day: the first reading is a baseline and
    /// never a row; a row only on false -> true.
    fn bool_edge(map: &mut HashMap<String, bool>, symbol: &str, value: bool) -> bool {
        match map.get_mut(symbol) {
            None => {
                map.insert(symbol.to_string(), value);
                false
            }
            Some(prev) => {
                let rose = !*prev && value;
                *prev = value;
                rose
            }
        }
    }

    /// `now` is wall clock, used only for the day roll.
    pub fn on_event(&mut self, event: &ScanEvent, now: DateTime<Utc>) -> Option<PanelRecord> {
        self.roll_day(now);
        match event {
            ScanEvent::MomentumUpdate { symbol, timestamp, volume_confirmation, structure, ma_slope, wick_rejection, overall, qualifies } => {
                if !Self::bool_edge(&mut self.momentum, symbol, *qualifies) {
                    return None;
                }
                let (price, source) = self.price_of(symbol);
                Some(PanelRecord::new(
                    "momentum",
                    symbol,
                    *timestamp,
                    price,
                    source,
                    Some(*overall),
                    json!({ "components": {
                        "volumeConfirmation": volume_confirmation,
                        "structure": structure,
                        "maSlope": ma_slope,
                        "wickRejection": wick_rejection,
                    }}),
                ))
            }
            ScanEvent::FunnelSignal { symbol, timestamp, price, gap_pct, float_shares, relative_volume, passed, .. } => {
                self.note_price(symbol, *price, "trade");
                if !Self::bool_edge(&mut self.gap_and_go, symbol, *passed) {
                    return None;
                }
                Some(PanelRecord::new(
                    "gap_and_go",
                    symbol,
                    *timestamp,
                    Some(*price),
                    "event",
                    None,
                    json!({ "gapPct": gap_pct, "relativeVolume": relative_volume, "floatShares": float_shares }),
                ))
            }
            ScanEvent::IgnitionEvent { symbol, timestamp, price, kind } => {
                self.note_price(symbol, *price, "trade");
                if *kind != IgnitionEventKind::FollowThroughConfirmed {
                    return None;
                }
                let at_ms = timestamp.timestamp_millis();
                let suppressed = match self.ignition.get_mut(symbol) {
                    Some(state) if at_ms - state.last_ms < IGNITION_COOLDOWN_MS => {
                        state.suppressed += 1;
                        return None;
                    }
                    Some(state) => {
                        let s = state.suppressed;
                        *state = CooldownState { last_ms: at_ms, suppressed: 0 };
                        s
                    }
                    None => {
                        self.ignition.insert(symbol.clone(), CooldownState { last_ms: at_ms, suppressed: 0 });
                        0
                    }
                };
                Some(PanelRecord::new(
                    "ignition",
                    symbol,
                    *timestamp,
                    Some(*price),
                    "event",
                    None,
                    json!({ "suppressedSinceLast": suppressed, "under25": *price <= 25.0 }),
                ))
            }
            ScanEvent::ConsolidationEvent { symbol, timestamp, price, kind, strategy } => {
                self.note_price(symbol, *price, "trade");
                if *kind != ConsolidationEventKind::EntryTriggered {
                    return None;
                }
                let panel = match strategy {
                    ConsolidationStrategy::Micropullback => "micropullback",
                    ConsolidationStrategy::ConsolidationBreakout => "consolidation_breakout",
                };
                let at_ms = timestamp.timestamp_millis();
                let key = (panel, symbol.clone());
                if self.entries.get(&key).is_some_and(|last| at_ms - *last < ENTRY_COOLDOWN_MS) {
                    return None;
                }
                self.entries.insert(key, at_ms);
                Some(PanelRecord::new(panel, symbol, *timestamp, Some(*price), "event", None, json!({})))
            }
            ScanEvent::HaltWarning { symbol, timestamp, reference_price, current_price, proximity_ratio, relative_volume, luld_in_effect, .. } => {
                self.note_price(symbol, *current_price, "trade");
                let alert = self.speed.observe(symbol, *timestamp, *reference_price, *current_price, *proximity_ratio, *relative_volume)?;
                Some(PanelRecord::new(
                    "halt_speed",
                    symbol,
                    alert.at,
                    Some(alert.price),
                    "trade",
                    Some(f64::from(alert.pct)),
                    json!({
                        "movePct": alert.move_pct,
                        "relativeVolume": alert.relative_volume,
                        "fillingFast": alert.filling_fast,
                        "luldInEffect": luld_in_effect,
                    }),
                ))
            }
            ScanEvent::BarUpdate { symbol, close, .. } => {
                self.note_price(symbol, *close, "bar_close");
                None
            }
            ScanEvent::CatalystUpdate { symbol, timestamp, catalyst_tags, headline_count, most_recent_published_at, .. } => {
                let (price, source) = self.price_of(symbol);
                Some(PanelRecord::new(
                    "catalyst",
                    symbol,
                    *timestamp,
                    price,
                    source,
                    None,
                    json!({
                        "tags": catalyst_tags,
                        "headlineCount": headline_count,
                        "mostRecentPublishedAt": most_recent_published_at.map(|t| t.to_rfc3339_opts(SecondsFormat::Millis, true)),
                    }),
                ))
            }
            _ => None,
        }
    }

    /// One movers snapshot (Top Gainers / Highly Trading current lists).
    /// Rows for symbols that entered either top 25 since the previous
    /// snapshot. A snapshot already seen (same `observed_at`) yields
    /// nothing. The first snapshot after process start records every row
    /// with `extra.firstSnapshot = true`: it is a genuine panel showing,
    /// but not proof the symbol had just entered.
    pub fn on_movers(&mut self, movers: &TodayMovers, now: DateTime<Utc>) -> Vec<PanelRecord> {
        let Some(observed_at) = movers.observed_at else { return Vec::new() };
        if self.movers_observed_at.is_some_and(|prev| prev >= observed_at) {
            return Vec::new();
        }
        self.movers_observed_at = Some(observed_at);
        self.roll_day(now);
        let first = std::mem::replace(&mut self.movers_first_snapshot, false);
        let mut out = Vec::new();
        for (panel, list, previous) in [
            ("top_gainers", &movers.current_gainers, &mut self.top_gainers),
            ("highly_trading", &movers.current_most_active, &mut self.highly_trading),
        ] {
            let current: HashSet<String> = list.iter().take(MOVERS_TOP_N).map(|m| m.symbol.clone()).collect();
            let prev = previous.get_or_insert_with(HashSet::new);
            for (rank, mover) in list.iter().take(MOVERS_TOP_N).enumerate() {
                if prev.contains(&mover.symbol) {
                    continue;
                }
                out.push(mover_record(panel, mover, rank + 1, observed_at, first));
            }
            *prev = current;
        }
        out
    }

    /// One exchange halt/resume status. Transitions only: a repeated
    /// "halted" (the quotation-only period inside one halt) and a resume
    /// with no halt on record (the ordinary open, or a process that started
    /// mid-halt) are not rows -- the same rules `halt_log` applies.
    pub fn on_halt(&mut self, status: &HaltStatus, now: DateTime<Utc>) -> Option<PanelRecord> {
        self.roll_day(now);
        let prev = self.halts.get(&status.symbol).copied();
        match status.kind {
            HaltTransitionKind::Halted if prev == Some(HaltTransitionKind::Halted) => return None,
            HaltTransitionKind::Resumed if prev != Some(HaltTransitionKind::Halted) => return None,
            _ => {}
        }
        self.halts.insert(status.symbol.clone(), status.kind);
        let (price, source) = self.price_of(&status.symbol);
        let reason = status.reason_message.clone().or_else(|| status.reason_code.clone());
        Some(PanelRecord::new(
            "halt_actual",
            &status.symbol,
            status.market_at,
            price,
            source,
            None,
            json!({
                "status": match status.kind { HaltTransitionKind::Halted => "halted", HaltTransitionKind::Resumed => "resumed" },
                "reason": reason,
                "statusCode": status.status_code,
                "reasonCode": status.reason_code,
            }),
        ))
    }
}

fn mover_record(panel: &'static str, mover: &Mover, rank: usize, observed_at: DateTime<Utc>, first: bool) -> PanelRecord {
    let mut extra = json!({ "rank": rank, "changePct": mover.change_pct, "volume": mover.volume });
    if first {
        extra["firstSnapshot"] = json!(true);
    }
    PanelRecord::new(panel, &mover.symbol, mover.observed_at.unwrap_or(observed_at), Some(mover.price), "movers_snapshot", None, extra)
}

// ---------------------------------------------------------------- writer

/// Counters shared between the offering side and the writer thread.
#[derive(Debug, Default)]
pub struct WriterHealth {
    pub offered: AtomicU64,
    pub dropped: AtomicU64,
    pub written: AtomicU64,
    pub write_errors: AtomicU64,
}

/// Offering half of the recorder. `offer` never blocks.
#[derive(Clone)]
pub struct PanelWriter {
    tx: SyncSender<PanelRecord>,
    health: Arc<WriterHealth>,
}

impl PanelWriter {
    /// Spawns the writer thread for `dir`. Retention runs once at startup
    /// and then on every market-day change.
    pub fn start(dir: PathBuf) -> std::io::Result<Self> {
        Self::start_with(dir, CHANNEL_CAPACITY, RetentionPolicy::default())
    }

    pub fn start_with(dir: PathBuf, capacity: usize, retention: RetentionPolicy) -> std::io::Result<Self> {
        let (tx, rx) = sync_channel(capacity.max(1));
        let health = Arc::new(WriterHealth::default());
        let thread_health = health.clone();
        std::thread::Builder::new()
            .name("panel-detections".into())
            .spawn(move || writer_loop(dir, rx, thread_health, retention))?;
        Ok(Self { tx, health })
    }

    /// Test-only constructor: the caller holds the receiver and nothing
    /// drains it.
    #[cfg(test)]
    fn detached(capacity: usize) -> (Self, Receiver<PanelRecord>) {
        let (tx, rx) = sync_channel(capacity);
        (Self { tx, health: Arc::new(WriterHealth::default()) }, rx)
    }

    /// Hands one record to the writer. Returns false (and counts a drop)
    /// when the queue is full or the writer is gone.
    pub fn offer(&self, record: PanelRecord) -> bool {
        self.health.offered.fetch_add(1, Ordering::Relaxed);
        match self.tx.try_send(record) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.health.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    pub fn health(&self) -> &Arc<WriterHealth> {
        &self.health
    }
}

pub fn file_name(day: NaiveDate) -> String {
    format!("{}.jsonl", day.format("%Y-%m-%d"))
}

struct OpenFile {
    day: NaiveDate,
    out: std::io::BufWriter<std::fs::File>,
}

fn open_day(dir: &Path, day: NaiveDate) -> std::io::Result<OpenFile> {
    std::fs::create_dir_all(dir)?;
    let file = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(file_name(day)))?;
    Ok(OpenFile { day, out: std::io::BufWriter::with_capacity(64 * 1024, file) })
}

/// Rate-limits a repeated warning to one line a minute.
struct Throttle(Option<Instant>);

impl Throttle {
    fn ready(&mut self) -> bool {
        let now = Instant::now();
        if self.0.is_some_and(|t| now.duration_since(t) < Duration::from_secs(60)) {
            return false;
        }
        self.0 = Some(now);
        true
    }
}

fn writer_loop(dir: PathBuf, rx: Receiver<PanelRecord>, health: Arc<WriterHealth>, retention: RetentionPolicy) {
    let mut open: Option<OpenFile> = None;
    let mut error_log = Throttle(None);
    let mut last_flush = Instant::now();
    let mut retained_for = market_day(Utc::now());
    apply_retention(&dir, retained_for, &retention);
    let mut fail = |health: &WriterHealth, error: &std::io::Error, what: &str| {
        health.write_errors.fetch_add(1, Ordering::Relaxed);
        if error_log.ready() {
            warn!(error = %error, what, write_errors = health.write_errors.load(Ordering::Relaxed), "panel detections: write failed; row lost, recorder keeps running");
        }
    };
    loop {
        let message = rx.recv_timeout(FLUSH_INTERVAL);
        let disconnected = matches!(message, Err(RecvTimeoutError::Disconnected));
        if let Ok(record) = message {
            let day = market_day(record.detected_at);
            if open.as_ref().is_none_or(|f| f.day != day) {
                if let Some(mut f) = open.take() {
                    if let Err(e) = f.out.flush() {
                        fail(&health, &e, "flush");
                    }
                }
                match open_day(&dir, day) {
                    Ok(f) => open = Some(f),
                    Err(e) => fail(&health, &e, "open"),
                }
            }
            // Retention once per market day (wall clock), never on the
            // file just opened.
            let today = market_day(Utc::now());
            if today > retained_for {
                retained_for = today;
                apply_retention(&dir, today, &retention);
            }
            if let Some(f) = open.as_mut() {
                let line = serde_json::to_string(&record).map(|mut s| {
                    s.push('\n');
                    s
                });
                match line {
                    Ok(line) => match f.out.write_all(line.as_bytes()) {
                        Ok(()) => {
                            health.written.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            fail(&health, &e, "write");
                            // Reopen on the next row rather than keep
                            // writing into a buffer in an unknown state.
                            open = None;
                        }
                    },
                    Err(e) => fail(&health, &std::io::Error::other(e), "serialize"),
                }
            }
        }
        if disconnected || last_flush.elapsed() >= FLUSH_INTERVAL {
            last_flush = Instant::now();
            if let Some(f) = open.as_mut() {
                if let Err(e) = f.out.flush() {
                    fail(&health, &e, "flush");
                    open = None;
                }
            }
        }
        if disconnected {
            return;
        }
    }
}

// ------------------------------------------------------------- retention

#[derive(Debug, Clone, Copy)]
pub struct RetentionPolicy {
    pub keep_days: usize,
    pub max_bytes: u64,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self { keep_days: RETENTION_MARKET_DAYS, max_bytes: RETENTION_MAX_BYTES }
    }
}

/// Which day files to delete. Files are market days (one exists only for a
/// day something was recorded), so "older than 90 market days" is "beyond
/// the 90 newest files". Then, oldest first, delete until the total fits
/// the size cap -- but never `today`'s file, which is being written.
pub fn select_for_deletion(files: &[(NaiveDate, u64)], today: NaiveDate, policy: &RetentionPolicy) -> Vec<NaiveDate> {
    let mut sorted: Vec<(NaiveDate, u64)> = files.to_vec();
    sorted.sort_by_key(|(d, _)| std::cmp::Reverse(*d));
    let mut delete = Vec::new();
    let mut kept = Vec::new();
    for (i, (day, size)) in sorted.into_iter().enumerate() {
        if i >= policy.keep_days && day != today {
            delete.push(day);
        } else {
            kept.push((day, size));
        }
    }
    let mut total: u64 = kept.iter().map(|(_, s)| *s).sum();
    // kept is newest first; trim from the oldest end.
    for (day, size) in kept.iter().rev() {
        if total <= policy.max_bytes {
            break;
        }
        if *day == today {
            continue;
        }
        delete.push(*day);
        total = total.saturating_sub(*size);
    }
    delete.sort();
    delete
}

fn list_day_files(dir: &Path) -> Vec<(NaiveDate, u64)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let day = NaiveDate::parse_from_str(name.strip_suffix(".jsonl")?, "%Y-%m-%d").ok()?;
            let size = e.metadata().ok().filter(|m| m.is_file())?.len();
            Some((day, size))
        })
        .collect()
}

pub fn apply_retention(dir: &Path, today: NaiveDate, policy: &RetentionPolicy) -> Vec<NaiveDate> {
    let files = list_day_files(dir);
    let doomed = select_for_deletion(&files, today, policy);
    for day in &doomed {
        let path = dir.join(file_name(*day));
        match std::fs::remove_file(&path) {
            Ok(()) => info!(path = %path.display(), "panel detections: retention removed day file"),
            Err(e) => warn!(error = %e, path = %path.display(), "panel detections: retention could not remove day file"),
        }
    }
    doomed
}

#[cfg(test)]
#[path = "panel_recorder_tests.rs"]
mod tests;
