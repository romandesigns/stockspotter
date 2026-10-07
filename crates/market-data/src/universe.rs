//! Universe-wide Stage 1/2 scanning — architecture doc section 4.1's
//! actual "funnel" step. Everything else in this crate (`SessionTracker`,
//! `IgnitionMonitor`) tracks a handful of *already-chosen* symbols over a
//! live WS stream. That doesn't scale to the full tradable universe
//! (thousands of tickers) — the doc's own design is a periodic REST pass
//! over everything to shrink it down to a shortlist, and only *that*
//! shortlist gets promoted to live streaming. This module is the wide
//! net; `bin/scan.rs`'s tracked symbols are what a promoted shortlist
//! would look like.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use fast_funnel::{explain, run_fast_funnel, FilterThresholds, SessionVolumeSource, TickerSnapshot};
use serde::Deserialize;
use tracing::warn;

use crate::config::AlpacaConfig;
use crate::float_data::fetch_float_shares;
use crate::premarket_volume::{
    daily_bar_date, daily_bar_freshness, DailyBarFreshness, PremarketVolumeCache, ScanVolumeCounts, VolumeCandidate,
    VolumeSourceCounts,
};
use crate::trading_session::{market_day, market_day_open};

#[derive(Debug, Deserialize)]
struct AssetRaw {
    symbol: String,
    // Optional, not required -- if Alpaca ever omits this for some
    // asset, is_warrant() below fails OPEN (treats it as not a warrant,
    // keeps it in the universe) rather than the whole fetch_universe
    // call failing to deserialize at all over one missing field on a
    // classification-only concern.
    name: Option<String>,
    tradable: bool,
    status: String,
}

/// True if this asset's real Alpaca security name marks it as a warrant
/// rather than the company's actual common stock (e.g. "Rocket Lab USA,
/// Inc. Warrant") -- a real signal from Alpaca's own metadata, not a
/// ticker-suffix guess. A suffix convention (trailing W/.WS) is common
/// but not guaranteed across every exchange/listing, and a legitimate
/// common stock could coincidentally end the same way -- the name field
/// doesn't have that false-positive risk.
fn is_warrant(name: Option<&str>) -> bool {
    name.is_some_and(|n| n.to_lowercase().contains("warrant"))
}

/// Every active, tradable US-equity symbol Alpaca knows about --
/// warrants excluded (see is_warrant's own doc comment on why: they're a
/// leveraged, low-priced derivative of the underlying stock, not the
/// stock itself, and their outsized % swings on trivial price moves were
/// crowding out genuine common-stock movers across Top Gainers/Highly
/// Trading and, upstream of that, the funnel's own Stage 1/2 shortlist).
pub async fn fetch_universe(cfg: &AlpacaConfig) -> Result<Vec<String>> {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/v2/assets", cfg.trading_base))
        .header("APCA-API-KEY-ID", &cfg.api_key)
        .header("APCA-API-SECRET-KEY", &cfg.api_secret)
        .query(&[("status", "active"), ("asset_class", "us_equity")])
        .send()
        .await
        .context("requesting tradable asset universe from alpaca")?
        .error_for_status()
        .context("alpaca assets endpoint returned an error status")?;

    let assets: Vec<AssetRaw> = resp
        .json()
        .await
        .context("parsing alpaca assets response")?;

    Ok(assets
        .into_iter()
        .filter(|a| a.tradable && a.status == "active" && !is_warrant(a.name.as_deref()))
        .map(|a| a.symbol)
        .collect())
}

#[derive(Debug, serde::Serialize, Deserialize)]
struct SnapshotBar {
    #[serde(rename = "t")]
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(rename = "c")]
    close: f64,
    #[serde(rename = "v")]
    volume: u64,
}

#[derive(Debug, serde::Serialize, Deserialize)]
struct SnapshotTrade {
    #[serde(rename = "t")]
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(rename = "p")]
    price: f64,
}

#[derive(Debug, Default, serde::Serialize, Deserialize)]
pub(crate) struct SnapshotRaw {
    #[serde(rename = "latestTrade")]
    latest_trade: Option<SnapshotTrade>,
    #[serde(rename = "dailyBar")]
    daily_bar: Option<SnapshotBar>,
    #[serde(rename = "prevDailyBar")]
    prev_daily_bar: Option<SnapshotBar>,
}

/// Symbols per snapshot request — batched to keep request URLs and
/// response sizes reasonable across thousands of tickers, not because of
/// any documented Alpaca limit.
const SNAPSHOT_CHUNK_SIZE: usize = 200;

/// What a converted snapshot was built from -- everything the discovery
/// tape needs to say which day each number belongs to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SnapshotMeta {
    pub daily_bar_date: Option<NaiveDate>,
    pub freshness: DailyBarFreshness,
    pub latest_trade_at: Option<DateTime<Utc>>,
    /// Instant `session_volume` is complete through, when known.
    pub session_volume_as_of: Option<DateTime<Utc>>,
}

/// Turns one raw Alpaca snapshot into a `TickerSnapshot`, reading each field
/// from the day it actually belongs to (D7, 2026-09-25 -- see
/// `premarket_volume`'s module doc for the defect).
///
/// | `dailyBar` vs `market_day(now)` | reference close | average proxy | session volume |
/// |---|---|---|---|
/// | current (after the ~09:31 ET roll) | `prevDailyBar.c` | `prevDailyBar.v` | `dailyBar.v` |
/// | stale (premarket, weekend, holiday) | `dailyBar.c` | `dailyBar.v` | **unknown** |
/// | ahead / missing / undated | `prevDailyBar.c` | `prevDailyBar.v` | **unknown** |
///
/// Before this, the first row was applied at every hour: premarket the gap
/// was measured two sessions back and `session_volume` was yesterday's whole
/// day. The average proxy is still a single-day shortcut; survivors of price
/// and gap get the real 20-day seed in `scan_shortlist`.
pub(crate) fn snapshot_from_raw(
    symbol: String,
    snap: SnapshotRaw,
    today: NaiveDate,
    fetched_at: DateTime<Utc>,
) -> Option<(TickerSnapshot, SnapshotMeta)> {
    let daily_t = snap.daily_bar.as_ref().and_then(|b| b.timestamp);
    let freshness = match &snap.daily_bar {
        None => DailyBarFreshness::Missing,
        Some(_) => daily_bar_freshness(daily_t, today),
    };
    let latest_trade_at = snap.latest_trade.as_ref().and_then(|t| t.timestamp);
    let price = snap
        .latest_trade
        .as_ref()
        .map(|t| t.price)
        .or_else(|| snap.daily_bar.as_ref().map(|b| b.close))?;
    let (reference_close, avg_daily_volume, session_volume) = match freshness {
        DailyBarFreshness::Current => {
            let prev = snap.prev_daily_bar.as_ref()?;
            (prev.close, prev.volume, snap.daily_bar.as_ref().map(|b| b.volume))
        }
        DailyBarFreshness::Stale => {
            let daily = snap.daily_bar.as_ref()?;
            (daily.close, daily.volume, None)
        }
        DailyBarFreshness::Ahead | DailyBarFreshness::Missing => {
            let prev = snap.prev_daily_bar.as_ref()?;
            (prev.close, prev.volume, None)
        }
    };
    let gap_pct = if reference_close > 0.0 { (price - reference_close) / reference_close * 100.0 } else { 0.0 };
    let session_volume_source = if session_volume.is_some() {
        SessionVolumeSource::SnapshotDailyBarCurrent
    } else {
        SessionVolumeSource::Unknown
    };
    Some((
        TickerSnapshot {
            symbol,
            price,
            float_shares: None,
            avg_daily_volume,
            session_volume,
            session_volume_source,
            gap_pct,
        },
        SnapshotMeta {
            daily_bar_date: daily_t.map(daily_bar_date),
            freshness,
            latest_trade_at,
            session_volume_as_of: session_volume.map(|_| fetched_at),
        },
    ))
}

/// Batched snapshot fetch, turned directly into `fast_funnel`-ready
/// snapshots. `float_shares` is always `None` here — this module only
/// ever sees price/volume/gap; float is a separate lookup
/// (`float_data::fetch_float_shares`) applied only to whatever survives
/// Stage 1's other checks, not to the whole universe (FMP's free tier
/// couldn't cover that volume anyway).
///
/// `avg_daily_volume` is approximated from a single prior day rather than
/// a true multi-day trailing average — a deliberate shortcut for a fast,
/// wide, periodic pass. A symbol that survives and gets promoted to
/// individual tracking uses `rest::fetch_daily_seeds`'s real multi-day
/// average instead.
///
/// Premarket, `session_volume` is `None` (unknown) for every symbol: see
/// `snapshot_from_raw`. Callers that rank by volume must handle that.
pub async fn fetch_snapshots(
    cfg: &AlpacaConfig,
    symbols: &[String],
) -> Result<HashMap<String, TickerSnapshot>> {
    let (snapshots, _) = fetch_snapshots_recorded(cfg, symbols, None, Utc::now()).await?;
    Ok(snapshots)
}

async fn fetch_snapshots_recorded(
    cfg: &AlpacaConfig,
    symbols: &[String],
    audit_id: Option<&str>,
    now: DateTime<Utc>,
) -> Result<(HashMap<String, TickerSnapshot>, HashMap<String, SnapshotMeta>)> {
    let client = reqwest::Client::new();
    let mut out = HashMap::new();
    let mut meta = HashMap::new();
    let today = market_day(now);

    for chunk in symbols.chunks(SNAPSHOT_CHUNK_SIZE) {
        let resp = client
            .get(format!("{}/v2/stocks/snapshots", cfg.data_base))
            .header("APCA-API-KEY-ID", &cfg.api_key)
            .header("APCA-API-SECRET-KEY", &cfg.api_secret)
            .query(&[("symbols", chunk.join(",")), ("feed", cfg.feed.clone())])
            .send()
            .await
            .context("requesting snapshots from alpaca")?
            .error_for_status()
            .context("alpaca snapshots endpoint returned an error status")?;

        let parsed: HashMap<String, SnapshotRaw> = resp
            .json()
            .await
            .context("parsing alpaca snapshots response")?;

        if let Some(scan_id) = audit_id {
            crate::discovery_audit::emit("snapshot_batch", serde_json::json!({
                "scan_id":scan_id,"requested":chunk,"snapshots":parsed}));
        }

        for (symbol, snap) in parsed {
            if let Some((snapshot, m)) = snapshot_from_raw(symbol.clone(), snap, today, now) {
                out.insert(symbol.clone(), snapshot);
                meta.insert(symbol, m);
            }
        }
    }

    Ok((out, meta))
}

/// Read-only independent universe census, without FMP or scanner subscriptions.
pub async fn capture_discovery_snapshots(cfg: &AlpacaConfig) -> Result<usize> {
    anyhow::ensure!(crate::discovery_audit::enabled(), "set DISCOVERY_AUDIT_DIR first");
    let id = format!("{}-{}", std::process::id(), chrono::Utc::now().timestamp_micros());
    let universe = fetch_universe(cfg).await?;
    crate::discovery_audit::emit("scan_started", serde_json::json!({
        "scan_id":id,"feed":cfg.feed,"universe":universe,"capture_only":true}));
    let (snapshots, _) = fetch_snapshots_recorded(cfg, &universe, Some(&id), chrono::Utc::now()).await?;
    crate::discovery_audit::emit("snapshot_complete", serde_json::json!({"scan_id":id}));
    Ok(snapshots.len())
}

/// The full Stage 1/2 funnel scan across the whole tradable universe,
/// returning just the symbols that qualify — the "wide, cheap, periodic"
/// half of the live architecture (see `live::run_live_scan`'s doc
/// comment for the other half, and the isolated core logic
/// `bin/scan_universe.rs` used to duplicate before this was factored
/// out). Measured 2026-08-31: ~3s for the full ~13,378-symbol universe
/// via Alpaca's batched snapshot endpoint — cheap enough to run every
/// couple of minutes continuously, not just as a one-off manual pass.
///
/// Float lookups only run on symbols that already cleared price + Stage
/// 2 (not the whole universe — FMP's rate limits couldn't cover that),
/// same fail-closed-on-unknown-float handling as everywhere else float
/// appears in this codebase.
///
/// Per-scan cap on real FMP requests. Stage-2 survivor counts were only
/// 15-25/scan during the quiet premarket hours this was measured against
/// (2026-08-31), but regular hours — especially right at the open — push
/// that much higher. At the live 15s rescan interval, uncapped survivor
/// counts scale FMP calls 4x/min; this cap bounds the burst.
///
/// **This is a rate limit, and a rate limit alone cannot protect a
/// per-day quota** — corrected 2026-09-06. The previous version of this
/// comment reasoned about "the confirmed 300/min FMP Starter ceiling",
/// while `.env` documents this project's key as the FREE tier: 250 per
/// DAY. 60 checks x 4 scans/min is 240/min, which would exhaust a
/// free-tier day in roughly one minute of the open and then fail Stage 1
/// closed for every symbol until midnight. `FloatCache` is what actually
/// makes the spend affordable (caching resolved floats for the day, so
/// cost scales with distinct qualifying symbols rather than with time);
/// this constant just keeps any single scan from spiking.
///
/// Overflow candidates aren't lost, just deferred — they get re-checked
/// on the very next 15s cycle if still qualifying, prioritized by
/// `|gap_pct| * relative_volume` so the most extreme movers get
/// float-checked first when there's more demand than budget.
const MAX_FLOAT_CHECKS_PER_SCAN: usize = 60;

/// How long a failed float lookup is treated as "still failing" before
/// retrying — found live 2026-09-03 via the background accuracy watch:
/// `PLUN.RT` (a rights-offering ticker; FMP's shares-float endpoint has
/// no data for that security class, confirmed via the literal same error
/// on every attempt) sat in Stage-2 qualifying range continuously for 3+
/// hours, so with no cache it re-burned one of only
/// `MAX_FLOAT_CHECKS_PER_SCAN` float-check slots on the identical,
/// permanently-doomed lookup every single 15s rescan — 4 wasted FMP
/// calls/min indefinitely, worse, one of only 60 slots/scan a genuinely
/// checkable candidate could have used instead once Stage-2 survivor
/// counts run high (regular-hours open, per that constant's own doc
/// comment). Same fail-closed outcome either way (unknown float still
/// means Stage 1 can't pass), this only stops re-paying for an answer
/// we already have. 10 minutes: long enough to eliminate the overwhelming
/// majority of the waste for a genuinely permanent failure (a security
/// class FMP will never cover), short enough that a real transient
/// blip (rate limit, momentary FMP outage) still self-heals within one
/// scan of resuming, not treated as broken forever.
const FLOAT_LOOKUP_FAILURE_COOLDOWN: Duration = Duration::from_secs(600);

/// Pure, unit-testable half of the cooldown mechanism — drops any cache
/// entry whose failure is now older than `cooldown`, so a symbol gets a
/// fresh real retry instead of being excluded forever off one stale
/// failure. Split out from `scan_shortlist`'s async/network-bound body
/// the same way `diff_watchlist`/`not_covered_by_other_source` (live.rs)
/// pull their real decision logic out of the loops that call them.
fn prune_expired_float_failures(cache: &mut HashMap<String, Instant>, now: Instant, cooldown: Duration) {
    cache.retain(|_, failed_at| now.duration_since(*failed_at) < cooldown);
}

/// Default cap on real FMP requests per trading day. Deliberately sized
/// for the FREE tier (250/day, which is what `.env`'s own comment says
/// this project's key is on) minus a small safety margin, NOT for the
/// paid Starter tier — the cost of guessing wrong in that direction is
/// the entire Gap & Go panel silently going dark for the rest of the
/// session, so the default has to be the safe guess. Raise it via
/// `FMP_DAILY_REQUEST_BUDGET` after actually confirming a paid plan.
const DEFAULT_FMP_DAILY_REQUEST_BUDGET: u32 = 240;

/// Per-symbol float knowledge plus the daily FMP request budget.
///
/// Replaces the bare failure-cooldown `HashMap` this used to take
/// (2026-09-06). Two real problems it fixes, both of which ended in the
/// same place — Stage 1 fails closed on unknown float, so an exhausted
/// quota means the funnel passes *nothing* and the UI shows an empty
/// panel indistinguishable from a genuinely quiet market:
///
/// 1. **Successful lookups were never cached.** Only failures were. A
///    symbol that qualified on Stage 2 got a fresh FMP call every single
///    15s rescan, all day, for a number that cannot change intraday —
///    shares outstanding/float is a corporate-action-level fact. One
///    symbol qualifying for an hour burned ~240 requests by itself.
/// 2. **The budget was per-minute, against a per-day quota.**
///    `MAX_FLOAT_CHECKS_PER_SCAN`'s doc comment reasons about "the
///    confirmed 300/min FMP Starter ceiling", but `.env` documents this
///    project's key as free tier, 250/**day**. At 60 checks x 4 scans/min
///    that quota is gone roughly 60 seconds into the open.
///
/// Caching successful lookups is what actually makes this affordable:
/// spend drops from "requests per minute" to "distinct symbols that
/// qualified today", which is a few hundred at most.
#[derive(Debug, Default)]
pub struct FloatCache {
    daily_seeds: HashMap<String, crate::rest::DailySeed>,
    /// Symbols already resolved today. `Some(n)` is a real float;
    /// `None` is FMP confirming it has no float data for this symbol
    /// (still a real answer worth caching — re-asking gets the same
    /// `None` and costs a request).
    known: HashMap<String, Option<u64>>,
    /// Hard request/parse failures, held off for
    /// `FLOAT_LOOKUP_FAILURE_COOLDOWN` so a permanently-doomed symbol
    /// (see that constant's own `PLUN.RT` story) can't re-burn budget.
    failures: HashMap<String, Instant>,
    /// Real FMP requests actually spent on `day`.
    spent_today: u32,
    /// The ET trading date `known`/`spent_today` belong to. Float is
    /// cached for the day rather than forever so a genuine corporate
    /// action (offering, split, reverse split — routine on exactly the
    /// low-float names this scanner targets) is picked up next session.
    day: Option<chrono::NaiveDate>,
    budget: u32,
}

impl FloatCache {
    pub fn new(budget: u32) -> Self {
        Self { budget, ..Default::default() }
    }

    /// Budget from `FMP_DAILY_REQUEST_BUDGET`, else
    /// `DEFAULT_FMP_DAILY_REQUEST_BUDGET`. Same `env::var` + documented
    /// constant idiom as `auto_trader::config` and `QUALIFY_SERVICE_URL`.
    pub fn from_env() -> Self {
        let budget = std::env::var("FMP_DAILY_REQUEST_BUDGET")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_FMP_DAILY_REQUEST_BUDGET);
        Self::new(budget)
    }

    /// Rolls the day over if `today` differs from what's cached, clearing
    /// both the resolved-float map and the spend counter. Called at the
    /// top of every scan rather than on a timer — a scan is the only
    /// thing that spends budget, so it's the only place the rollover has
    /// to be correct.
    fn roll_day(&mut self, today: chrono::NaiveDate) {
        if self.day != Some(today) {
            self.known.clear();
            self.daily_seeds.clear();
            self.spent_today = 0;
            self.day = Some(today);
        }
    }

    pub fn budget_remaining(&self) -> u32 {
        self.budget.saturating_sub(self.spent_today)
    }

    pub fn is_exhausted(&self) -> bool {
        self.budget_remaining() == 0
    }

    fn record_success(&mut self, symbol: &str, float_shares: Option<u64>) {
        self.known.insert(symbol.to_string(), float_shares);
        self.failures.remove(symbol);
        self.spent_today += 1;
    }

    fn record_failure(&mut self, symbol: &str, now: Instant) {
        self.failures.insert(symbol.to_string(), now);
        self.spent_today += 1;
    }
}

/// What a scan learned about its float budget, broadcast so the UI can
/// tell "no setups right now" apart from "the funnel can't answer".
/// Without this the two look identical: an exhausted quota means every
/// float is unknown, unknown float fails Stage 1 closed, and the Gap &
/// Go panel just sits empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloatBudgetStatus {
    pub remaining: u32,
    pub budget: u32,
    /// Symbols that cleared Stage 2 this scan but couldn't be
    /// float-checked because the day's budget was already spent. Non-zero
    /// here is the precise "the funnel is blind right now" condition.
    pub starved_candidates: usize,
    /// True when no `FMP_API_KEY` is configured at all — a different
    /// failure with the same visible symptom, worth distinguishing.
    pub api_key_missing: bool,
}

/// Everything one universe rescan produces. A struct rather than a
/// growing tuple: this went from one value to three in a single day
/// (2026-09-06 — float-budget health, then the quiet watch), and named
/// fields keep the call sites at the other end readable.
#[derive(Debug, Clone)]
pub struct ScanOutcome {
    pub daily_seeds: HashMap<String, crate::rest::DailySeed>,
    pub session_bars: HashMap<String, Vec<crate::bar::Bar>>,
    /// Symbols that cleared the full Stage 1/2 funnel.
    pub qualified: Vec<QualifiedSymbol>,
    pub float_status: FloatBudgetStatus,
    /// Quiet, low-priced symbols to watch for a flat-base ignition — see
    /// `QuietWatchConfig`'s doc comment for why this tier exists at all.
    /// Disjoint from `qualified` by construction (one requires a 10% gap
    /// and 5x relative volume, the other requires neither), though
    /// nothing depends on that.
    pub quiet_watch: Vec<String>,
}

/// `volume_cache` carries today's premarket minute-bar volumes between
/// scans; create it once alongside `float_cache` (see
/// `premarket_volume::PremarketVolumeCache`).
pub async fn scan_shortlist(
    cfg: &AlpacaConfig,
    thresholds: &FilterThresholds,
    float_cache: &mut FloatCache,
    volume_cache: &mut PremarketVolumeCache,
) -> Result<ScanOutcome> {
    scan_shortlist_at(cfg, thresholds, float_cache, volume_cache, Utc::now()).await
}

/// Price+gap survivors that still need a trailing seed. The gap here is the
/// snapshot's own, which since D7 is against the right close premarket.
fn seed_candidates(
    snapshots: &HashMap<String, TickerSnapshot>,
    seeds: &HashMap<String, crate::rest::DailySeed>,
    thresholds: &FilterThresholds,
) -> Vec<String> {
    snapshots
        .values()
        .filter(|s| {
            let v = explain(s, thresholds);
            v.price_ok && v.gap_ok && !seeds.contains_key(&s.symbol)
        })
        .map(|s| s.symbol.clone())
        .collect()
}

/// Replaces each price+gap survivor's one-day snapshot baseline with its
/// trailing seed (20-day average, prior close). A survivor without a seed
/// gets a zero average, which fails relative volume closed.
fn apply_daily_seeds(
    snapshots: &mut HashMap<String, TickerSnapshot>,
    seeds: &HashMap<String, crate::rest::DailySeed>,
    thresholds: &FilterThresholds,
) {
    for snapshot in snapshots.values_mut() {
        if let Some(seed) = seeds.get(&snapshot.symbol) {
            snapshot.avg_daily_volume = seed.avg_daily_volume;
            snapshot.gap_pct = if seed.prior_close > 0.0 { (snapshot.price / seed.prior_close - 1.0) * 100.0 } else { 0.0 };
        } else if explain(snapshot, thresholds).gap_ok {
            snapshot.avg_daily_volume = 0; // unavailable baseline fails closed
        }
    }
}

/// Price+gap survivors whose volume the snapshot could not provide, plus the
/// count of those skipped because they have not traded since the open.
///
/// Only symbols with a usable baseline are worth a request: a zero average
/// fails relative volume whatever the volume is. A symbol whose latest trade
/// predates today's 04:00 ET open has printed nothing today, so there is
/// nothing to sum -- it stays unknown and fails closed without spending
/// budget (the whole weekend falls here).
fn volume_candidates(
    snapshots: &HashMap<String, TickerSnapshot>,
    meta: &HashMap<String, SnapshotMeta>,
    thresholds: &FilterThresholds,
    now: DateTime<Utc>,
) -> (Vec<VolumeCandidate>, u64) {
    let open = market_day_open(market_day(now));
    let mut out = Vec::new();
    let mut no_trade = 0u64;
    for s in snapshots.values() {
        if s.session_volume.is_some() || s.avg_daily_volume == 0 {
            continue;
        }
        let v = explain(s, thresholds);
        if !(v.price_ok && v.gap_ok) {
            continue;
        }
        let traded_today = meta.get(&s.symbol).and_then(|m| m.latest_trade_at).is_some_and(|t| t >= open);
        if traded_today {
            out.push(VolumeCandidate { symbol: s.symbol.clone(), gap_pct: s.gap_pct });
        } else {
            no_trade += 1;
        }
    }
    (out, no_trade)
}

/// Fills every still-unknown volume the cache has a value for (not only this
/// scan's candidates: a known volume is better than an unknown one for the
/// quiet watch and the tape too). Never touches a known volume.
fn apply_bar_volumes(
    snapshots: &mut HashMap<String, TickerSnapshot>,
    meta: &mut HashMap<String, SnapshotMeta>,
    cache: &PremarketVolumeCache,
) {
    for snapshot in snapshots.values_mut() {
        if let Some(as_of) = cache.apply(snapshot) {
            if let Some(m) = meta.get_mut(&snapshot.symbol) {
                m.session_volume_as_of = Some(as_of);
            }
        }
    }
}

/// One `scan_completed.selection_inputs` row: the snapshot as the funnel saw
/// it, plus which day its daily bar belonged to, so the tape self-describes
/// (D7). Old rows lack these keys and must be read with the rule
/// "`session_volume` is yesterday's when recorded before the ~09:31 ET roll".
#[derive(serde::Serialize)]
struct SelectionInput<'a> {
    #[serde(flatten)]
    snapshot: &'a TickerSnapshot,
    #[serde(rename = "dailyBarDate")]
    daily_bar_date: Option<NaiveDate>,
    #[serde(rename = "dailyBarFreshness")]
    daily_bar_freshness: Option<DailyBarFreshness>,
    #[serde(rename = "sessionVolumeAsOf")]
    session_volume_as_of: Option<DateTime<Utc>>,
}

async fn scan_shortlist_at(
    cfg: &AlpacaConfig,
    thresholds: &FilterThresholds,
    float_cache: &mut FloatCache,
    volume_cache: &mut PremarketVolumeCache,
    now: DateTime<Utc>,
) -> Result<ScanOutcome> {
    let audit_id = crate::discovery_audit::enabled().then(||
        format!("{}-{}", std::process::id(), chrono::Utc::now().timestamp_micros()));
    let universe = fetch_universe(cfg).await?;
    if let Some(id) = &audit_id {
        crate::discovery_audit::emit("scan_started", serde_json::json!({
            "scan_id":id,"feed":cfg.feed,"universe":universe}));
    }
    let (mut snapshots, mut meta) = fetch_snapshots_recorded(cfg, &universe, audit_id.as_deref(), now).await?;
    if let Some(id) = &audit_id {
        crate::discovery_audit::emit("snapshot_complete", serde_json::json!({"scan_id":id}));
    }

    // Roll the day over first -- a new ET trading date clears both the
    // resolved-float map and the spend counter (see FloatCache::roll_day).
    float_cache.roll_day(now.with_timezone(&chrono_tz::America::New_York).date_naive());
    volume_cache.roll(now);

    // Price and gap are independent of relative volume. Resolve the same trailing
    // baseline as live tracking BEFORE applying the relative-volume gate.
    // Since D7 the gap preselecting here is already against the right close
    // premarket (`snapshot_from_raw`), so today's gapper after a down day is
    // seeded; before, it was measured two sessions back and never was.
    let missing = seed_candidates(&snapshots, &float_cache.daily_seeds, thresholds);
    if !missing.is_empty() {
        float_cache.daily_seeds.extend(crate::rest::fetch_daily_seeds_as_of(cfg, &missing, 20, now).await?);
    }
    apply_daily_seeds(&mut snapshots, &float_cache.daily_seeds, thresholds);

    // Today's volume for price+gap survivors the snapshot can't answer for
    // (premarket, until the ~09:31 ET daily-bar roll). Bounded and cached
    // per minute; whatever stays unknown fails relative volume closed.
    let (candidates, no_trade_since_open) = volume_candidates(&snapshots, &meta, thresholds, now);
    let volume_report = if candidates.is_empty() {
        crate::premarket_volume::ResolutionReport::default()
    } else {
        volume_cache.resolve(cfg, &candidates, now).await
    };
    apply_bar_volumes(&mut snapshots, &mut meta, volume_cache);
    let still_unknown =
        candidates.iter().filter(|c| snapshots.get(&c.symbol).is_some_and(|s| s.session_volume.is_none())).count() as u64;
    volume_cache.publish_scan(
        ScanVolumeCounts {
            by_source: VolumeSourceCounts::count(snapshots.values()),
            survivors_needing_volume: candidates.len() as u64 + no_trade_since_open,
            survivors_resolved: candidates.len() as u64 - still_unknown,
            survivors_deferred: still_unknown,
            survivors_no_trade_since_open: no_trade_since_open,
        },
        now,
    );

    // Drop expired cooldown entries next so this scan's budget isn't
    // spent re-excluding a symbol whose cooldown already lapsed (it'll
    // just get a fresh real attempt below, same as any other candidate).
    let now_instant = Instant::now();
    prune_expired_float_failures(&mut float_cache.failures, now_instant, FLOAT_LOOKUP_FAILURE_COOLDOWN);

    // Stage-2 survivors split three ways: already resolved today (free),
    // in failure cooldown (skipped), and genuinely needing a request.
    let mut resolved: Vec<TickerSnapshot> = Vec::new();
    let mut needs_fetch: Vec<&TickerSnapshot> = Vec::new();
    let mut skipped_in_cooldown = 0usize;
    for snapshot in snapshots.values() {
        let verdict = explain(snapshot, thresholds);
        if !(verdict.price_ok && verdict.rel_vol_ok && verdict.gap_ok) {
            continue;
        }
        // The big win over the previous version: a float already looked
        // up today is reused instead of re-fetched. Float can't change
        // intraday, so re-asking every 15s bought nothing and cost the
        // entire daily quota -- see FloatCache's own doc comment.
        if let Some(&cached) = float_cache.known.get(&snapshot.symbol) {
            let mut snapshot = snapshot.clone();
            snapshot.float_shares = cached;
            resolved.push(snapshot);
            continue;
        }
        // Still Stage-1-fails-closed (unknown float never passes) --
        // this only skips re-PAYING for an answer already known within
        // the cooldown window, see FLOAT_LOOKUP_FAILURE_COOLDOWN's own
        // doc comment. Filtered out here, before the MAX_FLOAT_CHECKS_
        // PER_SCAN truncation below, so a permanently-failing symbol
        // doesn't keep occupying a real candidate's budget slot either.
        if float_cache.failures.contains_key(&snapshot.symbol) {
            skipped_in_cooldown += 1;
            continue;
        }
        needs_fetch.push(snapshot);
    }
    if skipped_in_cooldown > 0 {
        tracing::debug!(skipped_in_cooldown, "skipped float lookups still in cooldown from a recent failure");
    }

    let api_key_missing = cfg.fmp_api_key.is_none();
    if api_key_missing && !needs_fetch.is_empty() {
        warn!(
            candidates = needs_fetch.len(),
            "FMP_API_KEY not set — these candidates can't clear Stage 1 without float data"
        );
    }

    // Per-scan cap AND remaining daily budget, whichever binds first.
    // The daily one is the new half: MAX_FLOAT_CHECKS_PER_SCAN alone is a
    // rate limit, and a rate limit can't protect a per-DAY quota.
    let wanted = needs_fetch.len();
    let allowed = if api_key_missing {
        0
    } else {
        MAX_FLOAT_CHECKS_PER_SCAN.min(float_cache.budget_remaining() as usize)
    };
    if wanted > allowed {
        // Most extreme movers first, so when demand outruns budget the
        // requests that DO get spent go to the best candidates.
        needs_fetch.sort_by(|a, b| {
            let score = |s: &TickerSnapshot| s.gap_pct.abs() * s.relative_volume().unwrap_or(0.0);
            score(b).partial_cmp(&score(a)).unwrap_or(std::cmp::Ordering::Equal)
        });
        needs_fetch.truncate(allowed);
    }
    let starved_candidates = wanted - needs_fetch.len();
    if starved_candidates > 0 {
        warn!(
            starved_candidates,
            checking = needs_fetch.len(),
            budget_remaining = float_cache.budget_remaining(),
            api_key_missing,
            "Stage-2 survivors could not be float-checked this scan; they fail Stage 1 closed until budget frees up"
        );
    }

    let symbols_to_fetch: Vec<String> = needs_fetch.into_iter().map(|s| s.symbol.clone()).collect();
    if let Some(fmp_key) = cfg.fmp_api_key.as_deref() {
        for symbol in &symbols_to_fetch {
            let float_shares = match fetch_float_shares(fmp_key, symbol).await {
                Ok(f) => {
                    // A real answer came back (even a legitimate "no float
                    // data" Ok(None) from FMP itself, as opposed to an error
                    // status) -- cache it for the day and clear any stale
                    // cooldown so a symbol that recovers isn't held past
                    // its own failure.
                    float_cache.record_success(symbol, f);
                    f
                }
                Err(e) => {
                    warn!(symbol, error = %e, "float lookup failed for universe scan; treating as unknown");
                    float_cache.record_failure(symbol, now_instant);
                    None
                }
            };
            let Some(mut snapshot) = snapshots.get(symbol).cloned() else {
                continue;
            };
            snapshot.float_shares = float_shares;
            resolved.push(snapshot);
        }
    }

    let status = FloatBudgetStatus {
        remaining: float_cache.budget_remaining(),
        budget: float_cache.budget,
        starved_candidates,
        api_key_missing,
    };

    // Reads the snapshots this scan already fetched -- no extra API
    // calls, see QuietWatchConfig's own doc comment.
    let quiet_watch = select_quiet_watch(&snapshots, &QuietWatchConfig::default());

    let qualified = run_fast_funnel(&resolved, thresholds);
    if let Some(id) = &audit_id {
        crate::discovery_audit::emit("scan_completed", serde_json::json!({
            "scan_id":id,"quiet_selected":quiet_watch,
            "qualified":qualified.iter().map(|s| &s.symbol).collect::<Vec<_>>(),
            // 2 = D7b (2026-09-25): session_volume may be null (unknown) and
            // rows carry sessionVolumeSource/dailyBarDate/dailyBarFreshness/
            // sessionVolumeAsOf. Absent = 1: session_volume is dailyBar.v,
            // i.e. yesterday's whole day when recorded before the ~09:31 ET
            // roll. The record envelope's own `schema` is unchanged so
            // existing readers keep parsing the file.
            "selection_inputs_schema":2,
            "selection_inputs":snapshots.values().filter(|s| s.price >= 0.25 && s.price <= 3.0).map(|s| {
                let m = meta.get(&s.symbol);
                SelectionInput { snapshot: s, daily_bar_date: m.and_then(|m| m.daily_bar_date),
                    daily_bar_freshness: m.map(|m| m.freshness), session_volume_as_of: m.and_then(|m| m.session_volume_as_of) }
            }).collect::<Vec<_>>(),
            "premarket_volume":{"marketDay":market_day(now),"candidates":candidates.len(),
                "noTradeSinceOpen":no_trade_since_open,"report":volume_report,
                "resolved":candidates.iter().filter_map(|c| snapshots.get(&c.symbol)
                    .and_then(|s| s.session_volume.map(|v| (c.symbol.clone(), v)))).collect::<HashMap<_, _>>()},
            "float_budget_remaining":status.remaining,
            "float_starved":status.starved_candidates,"float_key_missing":status.api_key_missing}));
    }
    Ok(ScanOutcome {
        daily_seeds: float_cache.daily_seeds.clone(),
        session_bars: HashMap::new(),
        qualified: qualified
            .into_iter()
            .map(|s| QualifiedSymbol { symbol: s.symbol.clone(), float_shares: s.float_shares })
            .collect(),
        float_status: status,
        quiet_watch,
    })
}

/// Selection rules for the "quiet watch" — the coverage tier that exists
/// so the ignition detector can actually see the doc's own headline
/// low-float flat-base pattern.
///
/// **The problem this fixes (2026-09-06).** The architecture doc says
/// three separate times that ignition detection must watch the *entire
/// eligible universe*, "since explosive moves can happen on stocks with
/// no prior setup". In practice it watched only two things: symbols that
/// cleared the Stage 1/2 funnel, and symbols already on the movers
/// leaderboard. Both of those are, by construction, **stocks that have
/// already moved** — the funnel requires a 10% gap and 5x relative
/// volume, and the leaderboard requires being a top mover.
///
/// The flat-base pattern is the exact inverse of that profile: a
/// low-priced stock trading *flat and quiet* before it ignites. Such a
/// stock has no gap and below-average volume, so it could never appear
/// in either source until after the ignition it was supposed to warn
/// about. The detector was structurally blind to the one pattern
/// `ignition_detector::flat_base` was written for.
///
/// **Why this isn't just "subscribe to everything".** Full tick coverage
/// of ~13,000 symbols isn't a threshold to loosen, it's a bandwidth and
/// WS-subscription problem. This tier instead spends a bounded
/// subscription budget on the symbols that actually match the flat-base
/// profile, which is a far better use of it than uniform coverage would
/// be. Honest scope: this is *wider* coverage aimed at a specific
/// documented pattern, not literal universe-wide coverage.
///
/// **It costs no extra API calls.** The 15s universe rescan already
/// fetches snapshots (price/volume/gap) for the whole universe to run
/// Stage 2; this reads the same snapshots.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuietWatchConfig {
    /// Lower price bound — the funnel's own $0.25 practical floor from
    /// part-3's "Price Floor Decision", not a separate judgment.
    pub min_price: f64,
    /// Upper price bound. Wider than `FlatBaseThresholds`' own $0.25
    /// gate band on purpose: the gate decides whether a *candidate* needs
    /// a confirmed flat base, while this decides who gets *watched at
    /// all*. Watching a slightly wider low-price range costs only
    /// subscription slots and lets the pattern be observed above the
    /// gate band too.
    pub max_price: f64,
    /// Maximum relative volume to still count as "quiet". This is the
    /// inversion that makes the tier work — the funnel wants
    /// `>= min_relative_volume` (5x), this wants stocks well *below*
    /// average, which is what a flat base looks like before it breaks.
    pub max_relative_volume: f64,
    /// Maximum absolute gap. A stock that already gapped isn't flat.
    pub max_abs_gap_pct: f64,
    /// Minimum trailing average daily volume. Without a floor this tier
    /// fills up with untradeable dead tickers that will never ignite —
    /// the point is quiet-but-alive, not abandoned.
    pub min_avg_daily_volume: u64,
    /// Hard cap on symbols watched this way, so the WS subscription list
    /// stays bounded no matter how many names qualify.
    pub max_symbols: usize,
}

impl Default for QuietWatchConfig {
    /// Starting values, explicitly not backtested — there is no
    /// historical flat-base ignition sample to tune against yet, which
    /// is itself a consequence of never having watched for one. Same
    /// honesty as `FlatBaseThresholds::default`: these are reasoned
    /// defaults meant to start producing the data that will replace
    /// them, not measured optima.
    fn default() -> Self {
        Self {
            min_price: 0.25,
            max_price: 3.00,
            max_relative_volume: 1.0,
            max_abs_gap_pct: 5.0,
            min_avg_daily_volume: 100_000,
            max_symbols: 150,
        }
    }
}

/// Picks the quiet, low-priced symbols worth watching for a flat-base
/// ignition. Pure and snapshot-driven so it's unit-testable without any
/// network — same split as `prune_expired_float_failures`.
///
/// Ranked by trailing average daily volume, descending: among stocks
/// that all look equally flat right now, the most liquid ones are the
/// ones whose eventual ignition is both most likely to be real and
/// actually tradeable. Deliberately NOT ranked by how quiet they are —
/// "quietest" optimizes for dead, which is the opposite of useful.
pub fn select_quiet_watch(
    snapshots: &HashMap<String, TickerSnapshot>,
    config: &QuietWatchConfig,
) -> Vec<String> {
    let mut candidates: Vec<&TickerSnapshot> = snapshots
        .values()
        .filter(|s| {
            // Unknown baseline -- can't call it quiet, so don't. Fails
            // closed, same as unknown float in Stage 1.
            if s.avg_daily_volume == 0 {
                return false;
            }
            // Unknown *session* volume is different (D7, 2026-09-25):
            // premarket, the snapshot cannot say what today's volume is,
            // and this tier used to select on yesterday's volume instead.
            // The volume test exists to exclude a stock that is already
            // running; with no volume known that job falls to the gap test
            // below, which premarket is now against the right close. So an
            // unknown volume neither passes nor fails the volume test -- it
            // is not applied -- and never is yesterday's volume read as
            // today's. (Premarket, cumulative volume since 04:00 over a
            // full-day average is below 1.0 for nearly every symbol anyway,
            // so applying it to the few known values changes little.)
            let quiet_volume = s
                .session_volume
                .is_none_or(|v| v as f64 / s.avg_daily_volume as f64 <= config.max_relative_volume);
            s.price >= config.min_price
                && s.price <= config.max_price
                && s.avg_daily_volume >= config.min_avg_daily_volume
                && quiet_volume
                && s.gap_pct.abs() <= config.max_abs_gap_pct
        })
        .collect();

    candidates.sort_by(|a, b| {
        b.avg_daily_volume
            .cmp(&a.avg_daily_volume)
            // Symbol as a tiebreak so the selection is deterministic
            // across scans -- otherwise HashMap iteration order would
            // churn the subscription list for no reason.
            .then_with(|| a.symbol.cmp(&b.symbol))
    });
    candidates.truncate(config.max_symbols);
    candidates.into_iter().map(|s| s.symbol.clone()).collect()
}

/// A symbol that cleared the full Stage 1/2 funnel, carrying the float
/// value the scan already paid an FMP call to confirm — plumbed through
/// so a live-promoted symbol's `SessionTracker` doesn't have to re-fetch
/// it (or worse, silently default to `None` and show `float_ok: false`
/// forever despite having a real qualifying float; see
/// `live::track_symbol`'s doc comment for the bug this fixes).
#[derive(Debug, Clone)]
pub struct QualifiedSymbol {
    pub symbol: String,
    pub float_shares: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_snapshot_retains_market_timestamps_and_missing_fields() {
        let raw: SnapshotRaw = serde_json::from_value(serde_json::json!({
            "latestTrade":{"p":0.5,"t":"2026-09-08T14:00:00Z"},
            "prevDailyBar":{"c":0.49,"v":100000,"t":"2026-09-04T04:00:00Z"}
        })).unwrap();
        let encoded = serde_json::to_value(raw).unwrap();
        assert_eq!(encoded["latestTrade"]["t"], "2026-09-08T14:00:00Z");
        assert_eq!(encoded["prevDailyBar"]["v"], 100000);
        assert!(encoded["dailyBar"].is_null());
        let old_shape: SnapshotRaw = serde_json::from_value(serde_json::json!({
            "latestTrade":{"p":0.5}
        })).unwrap();
        assert!(old_shape.latest_trade.unwrap().timestamp.is_none());
    }

    // --- FloatCache (2026-09-06) ---
    //
    // The network half of scan_shortlist isn't unit-testable without a
    // live FMP key, so these pin the budget/caching decisions themselves
    // -- which is where the actual bug was, not in the HTTP call.

    fn day(d: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(2026, 9, d).unwrap()
    }

    #[test]
    fn a_resolved_float_is_reused_instead_of_re_fetched() {
        // The core fix: float can't change intraday, so the second scan
        // of the same qualifying symbol must cost zero requests. Before
        // this, every 15s rescan re-paid for the identical answer and
        // burned the whole daily quota inside a minute.
        let mut cache = FloatCache::new(250);
        cache.roll_day(day(6));
        cache.record_success("SWVL", Some(5_000_000));

        assert_eq!(cache.known.get("SWVL"), Some(&Some(5_000_000)));
        assert_eq!(cache.budget_remaining(), 249, "exactly one request should have been spent");
    }

    #[test]
    fn a_confirmed_no_float_answer_is_cached_too() {
        // Ok(None) is FMP telling us it has no float for this symbol --
        // a real answer that costs a request. Re-asking returns the same
        // None and costs another, so it gets cached like any other.
        let mut cache = FloatCache::new(250);
        cache.roll_day(day(6));
        cache.record_success("PLUN.RT", None);

        assert_eq!(cache.known.get("PLUN.RT"), Some(&None));
        assert_eq!(cache.budget_remaining(), 249);
    }

    #[test]
    fn the_budget_actually_runs_out() {
        let mut cache = FloatCache::new(2);
        cache.roll_day(day(6));
        assert!(!cache.is_exhausted());
        cache.record_success("AAA", Some(1));
        cache.record_failure("BBB", Instant::now());
        assert!(cache.is_exhausted(), "both a success and a failure spend a real request");
        assert_eq!(cache.budget_remaining(), 0);
    }

    #[test]
    fn a_new_trading_day_clears_resolved_floats_and_the_spend() {
        // Cached for the day, not forever: offerings/splits/reverse
        // splits are routine on exactly the low-float names this scanner
        // targets, so yesterday's float must not be trusted today.
        let mut cache = FloatCache::new(250);
        cache.roll_day(day(6));
        cache.record_success("SWVL", Some(5_000_000));
        assert_eq!(cache.budget_remaining(), 249);

        cache.roll_day(day(7));
        assert!(cache.known.is_empty(), "yesterday's floats must not carry over");
        assert_eq!(cache.budget_remaining(), 250, "a new day restores the full budget");
    }

    #[test]
    fn rolling_to_the_same_day_twice_is_a_no_op() {
        // roll_day runs at the top of EVERY scan (4x/minute), so it has
        // to be idempotent -- if it reset on same-day calls it would
        // wipe the cache continuously and reintroduce the exact bug it
        // exists to fix.
        let mut cache = FloatCache::new(250);
        cache.roll_day(day(6));
        cache.record_success("SWVL", Some(5_000_000));
        cache.roll_day(day(6));

        assert_eq!(cache.known.get("SWVL"), Some(&Some(5_000_000)));
        assert_eq!(cache.budget_remaining(), 249);
    }

    #[test]
    fn a_recovered_symbol_leaves_the_failure_cooldown() {
        let mut cache = FloatCache::new(250);
        cache.roll_day(day(6));
        cache.record_failure("FLAKY", Instant::now());
        assert!(cache.failures.contains_key("FLAKY"));

        cache.record_success("FLAKY", Some(3_000_000));
        assert!(!cache.failures.contains_key("FLAKY"), "a real answer clears the cooldown");
    }

    #[test]
    fn the_default_budget_is_sized_for_the_free_tier() {
        // Pins the safe-direction default explicitly: guessing "paid
        // tier" wrong takes the whole Gap & Go panel down for a session,
        // guessing "free tier" wrong just defers some lookups.
        assert!(
            DEFAULT_FMP_DAILY_REQUEST_BUDGET <= 250,
            "default must fit FMP's free-tier 250/day quota, see .env"
        );
    }

    // --- quiet watch / flat-base coverage (2026-09-06) ---

    fn snap(symbol: &str, price: f64, avg_daily_volume: u64, session_volume: u64, gap_pct: f64) -> TickerSnapshot {
        TickerSnapshot {
            symbol: symbol.to_string(),
            price,
            float_shares: None,
            avg_daily_volume,
            session_volume: Some(session_volume),
            session_volume_source: SessionVolumeSource::SnapshotDailyBarCurrent,
            gap_pct,
        }
    }

    fn snaps(list: Vec<TickerSnapshot>) -> HashMap<String, TickerSnapshot> {
        list.into_iter().map(|s| (s.symbol.clone(), s)).collect()
    }

    #[test]
    fn a_quiet_low_priced_stock_is_selected() {
        // The whole point: this symbol has no gap and below-average
        // volume, so the funnel (10% gap, 5x rel vol) and the movers
        // leaderboard both structurally exclude it -- yet it is exactly
        // the flat-base profile the ignition detector needs to watch.
        let s = snaps(vec![snap("QUIET", 0.80, 1_000_000, 200_000, 0.5)]);
        assert_eq!(select_quiet_watch(&s, &QuietWatchConfig::default()), vec!["QUIET"]);
    }

    #[test]
    fn a_stock_that_already_moved_is_not_quiet() {
        // Already gapping and running hot -- the funnel/movers tiers
        // already cover this one, and it isn't a flat base by definition.
        let s = snaps(vec![snap("RUNNER", 0.80, 1_000_000, 8_000_000, 45.0)]);
        assert!(select_quiet_watch(&s, &QuietWatchConfig::default()).is_empty());
    }

    #[test]
    fn each_filter_excludes_on_its_own() {
        let cfg = QuietWatchConfig::default();
        // Too expensive for the low-float flat-base profile.
        assert!(select_quiet_watch(&snaps(vec![snap("PRICEY", 42.00, 1_000_000, 100_000, 0.0)]), &cfg).is_empty());
        // Below the $0.25 practical floor from part-3.
        assert!(select_quiet_watch(&snaps(vec![snap("SUBPENNY", 0.02, 1_000_000, 100_000, 0.0)]), &cfg).is_empty());
        // Quiet but effectively untradeable -- "quiet-but-alive" is the
        // target, not abandoned.
        assert!(select_quiet_watch(&snaps(vec![snap("DEAD", 0.80, 5_000, 100, 0.0)]), &cfg).is_empty());
        // Volume already 3x average: not flat.
        assert!(select_quiet_watch(&snaps(vec![snap("BUSY", 0.80, 1_000_000, 3_000_000, 0.0)]), &cfg).is_empty());
        // Big gap, even on light volume: not flat.
        assert!(select_quiet_watch(&snaps(vec![snap("GAPPER", 0.80, 1_000_000, 100_000, 30.0)]), &cfg).is_empty());
    }

    #[test]
    fn an_unknown_volume_baseline_fails_closed() {
        // avg_daily_volume of 0 makes relative volume incomputable. Same
        // rule as unknown float in Stage 1: unknown never qualifies.
        let s = snaps(vec![snap("NOBASE", 0.80, 0, 0, 0.0)]);
        assert!(select_quiet_watch(&s, &QuietWatchConfig::default()).is_empty());
    }

    #[test]
    fn selection_is_capped_and_ranked_by_liquidity() {
        let cfg = QuietWatchConfig { max_symbols: 2, ..QuietWatchConfig::default() };
        let s = snaps(vec![
            snap("LOW", 0.80, 200_000, 10_000, 0.0),
            snap("HIGH", 0.80, 9_000_000, 100_000, 0.0),
            snap("MID", 0.80, 3_000_000, 100_000, 0.0),
        ]);
        assert_eq!(select_quiet_watch(&s, &cfg), vec!["HIGH", "MID"]);
    }

    #[test]
    fn selection_is_deterministic_across_scans() {
        // HashMap iteration order varies run to run; without the symbol
        // tiebreak an equal-liquidity set would churn the WS
        // subscription list every 15s for no reason.
        let cfg = QuietWatchConfig { max_symbols: 2, ..QuietWatchConfig::default() };
        let s = snaps(vec![
            snap("CCC", 0.80, 1_000_000, 10_000, 0.0),
            snap("AAA", 0.80, 1_000_000, 10_000, 0.0),
            snap("BBB", 0.80, 1_000_000, 10_000, 0.0),
        ]);
        let first = select_quiet_watch(&s, &cfg);
        assert_eq!(first, vec!["AAA", "BBB"]);
        for _ in 0..20 {
            assert_eq!(select_quiet_watch(&s, &cfg), first);
        }
    }

    #[test]
    fn real_warrant_names_are_detected() {
        // Real examples seen live -- RCKTW/BIAFW/DSX.WS/ARQQW/GFAIW/LHSW
        // all dominated Top Gainers/Highly Trading before this filter.
        assert!(is_warrant(Some("Rocket Lab USA, Inc. Warrant")));
        assert!(is_warrant(Some("BiOptio Inc. Warrants")));
        assert!(is_warrant(Some("Diana Shipping Inc. Warrant")));
        // Case-insensitive -- Alpaca's own casing isn't guaranteed consistent.
        assert!(is_warrant(Some("Example Corp WARRANT")));
    }

    #[test]
    fn real_common_stock_names_are_not_flagged() {
        assert!(!is_warrant(Some("Apple Inc.")));
        assert!(!is_warrant(Some("Rocket Lab USA, Inc.")));
        // A name that happens to contain "War" (not "Warrant") must not
        // false-positive on a naive substring match of just "war".
        assert!(!is_warrant(Some("Warner Bros. Discovery, Inc.")));
    }

    #[test]
    fn missing_name_fails_open_not_closed() {
        // A classification-only concern -- an asset with no name field
        // shouldn't be silently dropped from the universe over it.
        assert!(!is_warrant(None));
    }

    // --- Float-lookup failure cooldown (2026-09-03, the real PLUN.RT finding) ---

    #[test]
    fn a_recent_failure_is_not_pruned() {
        let now = Instant::now();
        let mut cache = HashMap::new();
        cache.insert("PLUN.RT".to_string(), now - Duration::from_secs(60));
        prune_expired_float_failures(&mut cache, now, Duration::from_secs(600));
        assert!(cache.contains_key("PLUN.RT"), "a failure only 60s old is well within a 600s cooldown, should still be excluded");
    }

    #[test]
    fn a_failure_older_than_the_cooldown_is_pruned_so_it_can_be_retried() {
        let now = Instant::now();
        let mut cache = HashMap::new();
        cache.insert("PLUN.RT".to_string(), now - Duration::from_secs(700));
        prune_expired_float_failures(&mut cache, now, Duration::from_secs(600));
        assert!(!cache.contains_key("PLUN.RT"), "a failure past the cooldown must be pruned so the next scan gives it a fresh real attempt");
    }

    #[test]
    fn unrelated_symbols_keep_independent_cooldowns() {
        // Real scenario this guards: PLUN.RT permanently fails (FMP has
        // no data for rights-offering tickers) while a genuinely
        // transient failure on a different symbol should still clear on
        // its own schedule, not get held hostage by an unrelated entry.
        let now = Instant::now();
        let mut cache = HashMap::new();
        cache.insert("PLUN.RT".to_string(), now - Duration::from_secs(60));
        cache.insert("XYZ".to_string(), now - Duration::from_secs(700));
        prune_expired_float_failures(&mut cache, now, Duration::from_secs(600));
        assert!(cache.contains_key("PLUN.RT"));
        assert!(!cache.contains_key("XYZ"));
    }
}

#[cfg(test)]
#[path = "universe_d7_tests.rs"]
mod d7_tests;
