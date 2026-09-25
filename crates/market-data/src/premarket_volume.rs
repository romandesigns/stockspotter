//! Today's extended-session volume for universe-scan symbols whose snapshot
//! cannot provide it -- D7b of docs/measurement-correctness-contract-2026-09-25.md.
//!
//! # The defect
//!
//! Alpaca's snapshot `dailyBar` does not roll to the new day at 04:00 ET, when
//! premarket trading starts. It rolls at about 09:31 ET (verified on the 09-22
//! discovery tape: BTTC, ZEO and TOPS all switched between 13:30:46Z and
//! 13:31:01Z). Until then `dailyBar` is **yesterday's** bar and `prevDailyBar`
//! the one before. The universe scan used them unconditionally, so for five and
//! a half hours every trading day:
//!
//! * `session_volume` was yesterday's full-day volume -- yesterday's runner
//!   cleared the 5x relative-volume gate all premarket (ZEO on 118M) while
//!   today's premarket runner could not (LHSW +184%, DCOY +108%);
//! * the gap was measured against the close two sessions back;
//! * the average proxy was the volume two sessions back.
//!
//! That decided FastFunnel tracking, quiet-watch coverage, the movers boards
//! and, through them, halt coverage.
//!
//! # The rule
//!
//! Staleness is read from the snapshot itself: the bar is *current* iff the
//! New York date of `dailyBar.t` equals `market_day(now)`. `dailyBar.t` is New
//! York midnight expressed in UTC (`04:00Z` under EDT, `05:00Z` under EST), so
//! only a New York date comparison is correct; no fixed hour or UTC date is.
//! Monday premarket (Friday's bar) and the day after a holiday are stale by the
//! same test, with no calendar needed.
//!
//! When stale, the snapshot knows the reference close (`dailyBar.c`) and the
//! last session's volume (`dailyBar.v`) but **not today's volume**. Today's
//! volume is then taken from 1-minute bars since the market-day open (04:00 ET)
//! for a bounded, prioritised set of symbols that already pass price and the
//! corrected gap -- this module. Every other stale symbol keeps an *unknown*
//! volume and fails the relative-volume gate closed, exactly as unknown float
//! fails Stage 1.
//!
//! # No double counting, no future information
//!
//! The two sources are never added. Before the roll the snapshot contributes
//! no volume at all; after it, `dailyBar.v` is used alone and it already
//! includes premarket (TOPS 13:31:01Z: snapshot 53,418,201 equals the stream
//! `SessionTracker`'s cumulative-since-04:00 exactly). Bars are summed only
//! when complete (`t + 1 min <= now`) and only from `t >= 04:00 ET`, and the
//! result is stamped `as_of` the end of the last completed minute, so nothing
//! later than the observation instant can leak in.
//!
//! # Restart and reconnect
//!
//! Nothing here is persisted. `run_live_scan` rebuilds the rescan task, and
//! with it this cache, on every reconnect and on the 04:00 ET new-session
//! bail, so a restart simply re-sums from 04:00 ET on its next scan. A symbol
//! that first appears mid-premarket is initialised the same way: its first
//! fetch covers every bar since the open.
//!
//! # API budget
//!
//! * One request covers up to `SYMBOLS_PER_REQUEST` symbols (Alpaca's
//!   multi-symbol `/v2/stocks/bars`), paginated at 10,000 bars per page. A
//!   full premarket is 330 bars per symbol, so a chunk is at most 2 pages.
//! * At most `MAX_SYMBOLS_PER_SCAN` symbols are refreshed per 15 s scan.
//! * A symbol's value is refreshed at most once per wall-clock minute: a new
//!   completed bar exists only once a minute, so a second fetch in the same
//!   minute cannot learn anything.
//! * At most `MAX_REQUESTS_PER_MINUTE` requests are *started* per minute.
//!   A chunk already started finishes its pages, so the hard ceiling is
//!   `MAX_REQUESTS_PER_MINUTE + MAX_PAGES_PER_CHUNK - 1` HTTP requests/minute.
//! * Fetches only happen while snapshots are stale, i.e. roughly 04:00-09:31
//!   ET on trading days. Symbols with no trade since the open are never
//!   fetched: their `latestTrade` already proves there is nothing to sum.
//!
//! For scale, the universe snapshot pass that already runs is ~67 requests per
//! 15 s scan (13,378 symbols / 200) plus the movers pass, so this adds at most
//! a few percent.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, NaiveDate, Timelike, Utc};
use fast_funnel::{SessionVolumeSource, TickerSnapshot};
use serde::{Deserialize, Serialize};

use crate::config::AlpacaConfig;
use crate::trading_session::{market_day, market_day_open};

/// Symbols per multi-symbol bars request.
pub const SYMBOLS_PER_REQUEST: usize = 50;
/// Symbols whose bar volume may be refreshed in one 15 s scan.
pub const MAX_SYMBOLS_PER_SCAN: usize = 100;
/// Bars requests that may be started per wall-clock minute.
pub const MAX_REQUESTS_PER_MINUTE: u32 = 12;
/// A chunk needing more pages than this is treated as a failed fetch rather
/// than paginated without bound. 50 symbols x 330 premarket minutes is 16,500
/// bars, i.e. 2 pages at 10,000; 4 leaves room for the 09:30 minute and slack.
pub const MAX_PAGES_PER_CHUNK: u32 = 4;

/// How a snapshot's `dailyBar` relates to the current market day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DailyBarFreshness {
    /// `dailyBar` is today's bar (after the ~09:31 ET roll).
    Current,
    /// `dailyBar` is an earlier session (premarket, weekends, holidays).
    Stale,
    /// `dailyBar` is dated after the current market day. Only possible between
    /// New York midnight and 04:00 ET if the provider ever rolled that early;
    /// never observed. Handled fail-closed: volume unknown, the gap referenced
    /// to `prevDailyBar` as before.
    Ahead,
    /// No `dailyBar`, or one without a timestamp.
    Missing,
}

/// The New York calendar date a daily bar's `t` stands for.
///
/// `t` is New York midnight in UTC, so its New York date is the bar's date in
/// both EDT (`04:00Z`) and EST (`05:00Z`). A UTC-date read happens to agree
/// for those encodings but would break for any other hour; this does not.
pub fn daily_bar_date(t: DateTime<Utc>) -> NaiveDate {
    t.with_timezone(&chrono_tz::America::New_York).date_naive()
}

/// Staleness of `dailyBar` against market day `today`.
pub fn daily_bar_freshness(
    daily_bar_t: Option<DateTime<Utc>>,
    today: NaiveDate,
) -> DailyBarFreshness {
    match daily_bar_t.map(daily_bar_date) {
        None => DailyBarFreshness::Missing,
        Some(d) if d == today => DailyBarFreshness::Current,
        Some(d) if d < today => DailyBarFreshness::Stale,
        Some(_) => DailyBarFreshness::Ahead,
    }
}

/// Start of the minute containing `t`: the end of the last completed 1-minute
/// bar at instant `t`.
pub fn floor_minute(t: DateTime<Utc>) -> DateTime<Utc> {
    t.with_second(0)
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or(t)
}

/// Sums the completed 1-minute bars of market day `day` observable at `now`.
///
/// Pure half of the fetch, so the causal boundaries are testable without a
/// network: bars before the 04:00 ET open (the previous market day's
/// after-hours) and bars not yet complete at `now` are excluded, and a
/// timestamp repeated across pages is counted once.
pub fn sum_completed_bars(
    bars: &[(DateTime<Utc>, u64)],
    day: NaiveDate,
    now: DateTime<Utc>,
) -> u64 {
    let open = market_day_open(day);
    let mut seen = HashSet::new();
    bars.iter()
        .filter(|(t, _)| *t >= open && *t + Duration::minutes(1) <= now)
        .filter(|(t, _)| seen.insert(*t))
        .map(|(_, v)| *v)
        .sum()
}

/// One symbol's minute-bar volume since the open, and the instant it is
/// complete through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BarVolume {
    pub volume: u64,
    pub as_of: DateTime<Utc>,
}

/// A symbol that passes price and the corrected gap but whose volume the
/// snapshot cannot provide.
#[derive(Debug, Clone, PartialEq)]
pub struct VolumeCandidate {
    pub symbol: String,
    pub gap_pct: f64,
}

/// Per-symbol cache of today's minute-bar volume plus the request budget.
/// Lives as long as one rescan task; see the module doc on restarts.
#[derive(Debug, Default)]
pub struct PremarketVolumeCache {
    market_day: Option<NaiveDate>,
    entries: HashMap<String, BarVolume>,
    budget_minute: Option<DateTime<Utc>>,
    requests_this_minute: u32,
    health: PremarketVolumeHealth,
}

/// What one scan's resolution did, for the discovery tape.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolutionReport {
    pub requested: Vec<String>,
    pub deferred: Vec<String>,
    pub failed: Vec<String>,
    pub requests: u32,
}

impl PremarketVolumeCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Clears everything day-scoped when the market day changes. Runs on
    /// market-day inequality, so weekends and holidays need no special case.
    pub fn roll(&mut self, now: DateTime<Utc>) {
        let today = market_day(now);
        if self.market_day != Some(today) {
            self.market_day = Some(today);
            self.entries.clear();
            self.health = PremarketVolumeHealth {
                market_day: Some(today),
                ..PremarketVolumeHealth::default()
            };
        }
        let minute = floor_minute(now);
        if self.budget_minute != Some(minute) {
            self.budget_minute = Some(minute);
            self.requests_this_minute = 0;
        }
    }

    /// Today's cached minute-bar volume for `symbol`, if any.
    pub fn get(&self, symbol: &str) -> Option<BarVolume> {
        self.entries.get(symbol).copied()
    }

    /// Which candidates to fetch this scan, in priority order.
    ///
    /// A symbol already refreshed this minute is skipped (nothing new can
    /// exist). Never-fetched symbols come first -- an unknown volume fails
    /// closed, a cached one is merely a minute or two old and can only
    /// understate -- then the largest absolute gap, then the symbol name so
    /// the choice is deterministic. Relative volume cannot rank them: it is
    /// exactly what is unknown.
    pub fn plan(
        &self,
        candidates: &[VolumeCandidate],
        now: DateTime<Utc>,
    ) -> (Vec<String>, Vec<String>) {
        let minute = floor_minute(now);
        let mut due: Vec<&VolumeCandidate> = candidates
            .iter()
            .filter(|c| self.entries.get(&c.symbol).is_none_or(|e| e.as_of < minute))
            .collect();
        due.sort_by(|a, b| {
            let fetched_before = |c: &VolumeCandidate| self.entries.contains_key(&c.symbol);
            fetched_before(a)
                .cmp(&fetched_before(b))
                .then_with(|| b.gap_pct.abs().total_cmp(&a.gap_pct.abs()))
                .then_with(|| a.symbol.cmp(&b.symbol))
        });
        let requests_left =
            MAX_REQUESTS_PER_MINUTE.saturating_sub(self.requests_this_minute) as usize;
        let cap = MAX_SYMBOLS_PER_SCAN.min(requests_left * SYMBOLS_PER_REQUEST);
        let deferred = due.iter().skip(cap).map(|c| c.symbol.clone()).collect();
        (
            due.into_iter()
                .take(cap)
                .map(|c| c.symbol.clone())
                .collect(),
            deferred,
        )
    }

    fn record_success(
        &mut self,
        chunk: &[String],
        volumes: &HashMap<String, u64>,
        pages: u32,
        now: DateTime<Utc>,
    ) {
        let as_of = floor_minute(now);
        for symbol in chunk {
            // A symbol absent from the response had no bar since the open: a
            // known zero, not an unknown.
            let volume = volumes.get(symbol).copied().unwrap_or(0);
            self.entries
                .insert(symbol.clone(), BarVolume { volume, as_of });
        }
        self.requests_this_minute += pages;
        self.health.requests_this_market_day += u64::from(pages);
        self.health.last_successful_fetch_at = Some(now);
        self.health.initialized_at.get_or_insert(now);
    }

    fn record_failure(
        &mut self,
        chunk: &[String],
        pages: u32,
        error: &anyhow::Error,
        now: DateTime<Utc>,
    ) {
        self.requests_this_minute += pages.max(1);
        self.health.requests_this_market_day += u64::from(pages.max(1));
        self.health.fetch_failures += 1;
        // Symbols that had nothing cached stay unknown: an initialisation
        // failure. Those with an earlier value keep it (causal, lower bound).
        self.health.init_failures += chunk
            .iter()
            .filter(|s| !self.entries.contains_key(*s))
            .count() as u64;
        self.health.last_fetch_error = Some(format!("{now}: {error:#}"));
    }

    /// Fetches the planned candidates. Never errors: a failed chunk is
    /// counted and its symbols stay unknown (or keep an earlier value).
    pub async fn resolve(
        &mut self,
        cfg: &AlpacaConfig,
        candidates: &[VolumeCandidate],
        now: DateTime<Utc>,
    ) -> ResolutionReport {
        self.roll(now);
        let (planned, deferred) = self.plan(candidates, now);
        let mut report = ResolutionReport {
            deferred,
            ..ResolutionReport::default()
        };
        let day = market_day(now);
        for chunk in planned.chunks(SYMBOLS_PER_REQUEST) {
            if self.requests_this_minute >= MAX_REQUESTS_PER_MINUTE {
                report.deferred.extend(chunk.iter().cloned());
                continue;
            }
            report.requested.extend(chunk.iter().cloned());
            match fetch_minute_bar_volumes(cfg, chunk, day, now).await {
                Ok((volumes, pages)) => {
                    report.requests += pages;
                    self.record_success(chunk, &volumes, pages, now);
                }
                Err(e) => {
                    tracing::warn!(error = %e, symbols = chunk.len(), "premarket minute-bar volume fetch failed; these symbols fail relative volume closed");
                    report.requests += 1;
                    report.failed.extend(chunk.iter().cloned());
                    self.record_failure(chunk, 1, &e, now);
                }
            }
        }
        report
    }

    /// Fills `snapshot`'s volume from the cache when the snapshot could not.
    /// Returns the `as_of` used, if any. Only ever fills an *unknown* volume:
    /// a current daily bar is never added to or replaced (no double count).
    pub fn apply(&self, snapshot: &mut TickerSnapshot) -> Option<DateTime<Utc>> {
        if snapshot.session_volume.is_some() {
            return None;
        }
        let entry = self.entries.get(&snapshot.symbol)?;
        snapshot.session_volume = Some(entry.volume);
        snapshot.session_volume_source = SessionVolumeSource::MinuteBarsSinceOpen;
        Some(entry.as_of)
    }

    /// Records the scan's outcome in the health block and publishes it.
    pub fn publish_scan(&mut self, scan: ScanVolumeCounts, now: DateTime<Utc>) {
        self.health.market_day = self.market_day;
        self.health.last_scan_at = Some(now);
        self.health.by_source = scan.by_source;
        self.health.survivors_needing_volume = scan.survivors_needing_volume;
        self.health.survivors_resolved = scan.survivors_resolved;
        self.health.survivors_deferred = scan.survivors_deferred;
        self.health.survivors_no_trade_since_open = scan.survivors_no_trade_since_open;
        self.health.requests_this_minute = self.requests_this_minute;
        self.health.cached_symbols = self.entries.len() as u64;
        publish(self.stamped());
    }

    /// The health block as it is published: the counters plus the budget
    /// constants they are measured against.
    pub(crate) fn stamped(&self) -> PremarketVolumeHealth {
        PremarketVolumeHealth {
            max_symbols_per_scan: MAX_SYMBOLS_PER_SCAN,
            symbols_per_request: SYMBOLS_PER_REQUEST,
            max_requests_per_minute: MAX_REQUESTS_PER_MINUTE,
            ..self.health.clone()
        }
    }

    #[cfg(test)]
    pub(crate) fn health_for_test(&self) -> &PremarketVolumeHealth {
        &self.health
    }

    /// Test seam: ingest raw bars for one symbol exactly as a successful fetch
    /// at `now` would (same summation, same `as_of`, same budget accounting).
    #[cfg(test)]
    pub(crate) fn ingest_bars_for_test(
        &mut self,
        symbol: &str,
        bars: &[(DateTime<Utc>, u64)],
        now: DateTime<Utc>,
    ) {
        self.roll(now);
        let volume = sum_completed_bars(bars, market_day(now), now);
        self.record_success(
            &[symbol.to_string()],
            &HashMap::from([(symbol.to_string(), volume)]),
            1,
            now,
        );
    }
}

/// Counts of the latest scan's session-volume provenance, universe-wide.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeSourceCounts {
    pub snapshot_daily_bar_current: u64,
    pub minute_bars_since_open: u64,
    pub unknown: u64,
}

impl VolumeSourceCounts {
    pub fn count<'a>(snapshots: impl IntoIterator<Item = &'a TickerSnapshot>) -> Self {
        let mut out = Self::default();
        for s in snapshots {
            match s.session_volume_source {
                SessionVolumeSource::SnapshotDailyBarCurrent => out.snapshot_daily_bar_current += 1,
                SessionVolumeSource::MinuteBarsSinceOpen => out.minute_bars_since_open += 1,
                SessionVolumeSource::Unknown => out.unknown += 1,
            }
        }
        out
    }
}

/// One scan's inputs to the health block.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScanVolumeCounts {
    pub by_source: VolumeSourceCounts,
    pub survivors_needing_volume: u64,
    pub survivors_resolved: u64,
    pub survivors_deferred: u64,
    pub survivors_no_trade_since_open: u64,
}

/// The bounded health block `/research/completeness` reports as
/// `premarketVolume`. Every field is a scalar or a fixed-shape object, so it
/// cannot grow with the universe.
///
/// Gate-relevant fields: `fetchFailures` and `initFailures` are market-day
/// cumulative failure counters and are zero on a clean day;
/// `survivorsDeferred` is the latest scan's count of price+gap survivors left
/// unknown by the request budget (non-zero means coverage was rationed, not
/// lost to an error).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PremarketVolumeHealth {
    /// Market day (04:00 America/New_York boundary) these counters cover.
    pub market_day: Option<NaiveDate>,
    /// First successful minute-bar fetch of this market day in this process.
    /// `null` until one happens (for example all day once snapshots are
    /// current, or on a day with no premarket survivors).
    pub initialized_at: Option<DateTime<Utc>>,
    pub last_scan_at: Option<DateTime<Utc>>,
    pub last_successful_fetch_at: Option<DateTime<Utc>>,
    /// Latest scan, every universe symbol with a snapshot.
    pub by_source: VolumeSourceCounts,
    /// Latest scan: price+gap survivors whose snapshot volume was unknown.
    pub survivors_needing_volume: u64,
    /// ... of which now carry a minute-bar volume.
    pub survivors_resolved: u64,
    /// ... of which were left unknown by the request budget.
    pub survivors_deferred: u64,
    /// ... of which have no trade since the open (not fetched: nothing to sum).
    pub survivors_no_trade_since_open: u64,
    /// Failed bars requests this market day.
    pub fetch_failures: u64,
    /// Symbols left unknown because their first fetch of the day failed.
    pub init_failures: u64,
    pub last_fetch_error: Option<String>,
    pub requests_this_minute: u32,
    pub requests_this_market_day: u64,
    pub cached_symbols: u64,
    pub max_symbols_per_scan: usize,
    pub symbols_per_request: usize,
    pub max_requests_per_minute: u32,
}

static HEALTH: Mutex<Option<PremarketVolumeHealth>> = Mutex::new(None);

fn publish(health: PremarketVolumeHealth) {
    if let Ok(mut slot) = HEALTH.lock() {
        *slot = Some(health);
    }
}

/// The latest published health block, or `None` before the first universe
/// scan in this process (or in a process that never runs one).
pub fn health() -> Option<PremarketVolumeHealth> {
    HEALTH.lock().ok().and_then(|slot| slot.clone())
}

#[derive(Debug, Deserialize)]
struct MinuteBarRaw {
    #[serde(rename = "t")]
    timestamp: DateTime<Utc>,
    #[serde(rename = "v")]
    volume: u64,
}

/// Both levels may be `null` on a real Alpaca bars response: the whole map
/// when no requested symbol has a bar in the window (a quiet premarket), and
/// one symbol's list (see `alpaca_json`). Either means "no bars", never an
/// error.
#[derive(Debug, Deserialize)]
struct MinuteBarsPage {
    #[serde(default)]
    bars: Option<HashMap<String, Option<Vec<MinuteBarRaw>>>>,
    next_page_token: Option<String>,
}

/// Completed-minute volume since the open of market day `day`, per symbol,
/// through one multi-symbol bars request (paginated). Returns the volumes and
/// the number of HTTP requests spent. Symbols with no bars are absent.
pub async fn fetch_minute_bar_volumes(
    cfg: &AlpacaConfig,
    symbols: &[String],
    day: NaiveDate,
    now: DateTime<Utc>,
) -> Result<(HashMap<String, u64>, u32)> {
    let start = market_day_open(day);
    let end = floor_minute(now);
    if symbols.is_empty() || end <= start {
        return Ok((HashMap::new(), 0));
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let mut raw: HashMap<String, Vec<(DateTime<Utc>, u64)>> = HashMap::new();
    let mut token: Option<String> = None;
    let mut pages = 0u32;
    loop {
        anyhow::ensure!(
            pages < MAX_PAGES_PER_CHUNK,
            "minute-bar volume needed more than {MAX_PAGES_PER_CHUNK} pages"
        );
        let mut query = vec![
            ("symbols", symbols.join(",")),
            ("timeframe", "1Min".to_string()),
            ("start", start.to_rfc3339()),
            ("end", end.to_rfc3339()),
            ("limit", "10000".to_string()),
            ("feed", cfg.feed.clone()),
            ("adjustment", "raw".to_string()),
            ("sort", "asc".to_string()),
        ];
        if let Some(t) = &token {
            query.push(("page_token", t.clone()));
        }
        pages += 1;
        let page: MinuteBarsPage = client
            .get(format!("{}/v2/stocks/bars", cfg.data_base))
            .header("APCA-API-KEY-ID", &cfg.api_key)
            .header("APCA-API-SECRET-KEY", &cfg.api_secret)
            .query(&query)
            .send()
            .await
            .context("requesting premarket minute bars")?
            .error_for_status()
            .context("alpaca bars endpoint returned an error status for premarket minute bars")?
            .json()
            .await
            .context("parsing premarket minute bars")?;
        for (symbol, bars) in page.bars.unwrap_or_default() {
            raw.entry(symbol).or_default().extend(
                bars.unwrap_or_default()
                    .into_iter()
                    .map(|b| (b.timestamp, b.volume)),
            );
        }
        match page.next_page_token.filter(|t| !t.is_empty()) {
            Some(t) => {
                anyhow::ensure!(
                    token.as_deref() != Some(t.as_str()),
                    "Alpaca repeated a minute-bar pagination token"
                );
                token = Some(t);
            }
            None => break,
        }
    }
    let volumes = raw
        .into_iter()
        .map(|(symbol, bars)| (symbol, sum_completed_bars(&bars, day, now)))
        .collect();
    Ok((volumes, pages))
}

#[cfg(test)]
#[path = "premarket_volume_tests.rs"]
mod tests;
