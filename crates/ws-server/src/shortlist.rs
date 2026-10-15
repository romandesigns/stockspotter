//! Today's Shortlist (2026-10-15): the few symbols worth a trader's
//! attention right now, each with the evidence for its move, whether it can
//! be traded cheaply, and the price levels a plan would sit on.
//!
//! Why this ranking. The one research result that held up
//! (docs/consolidation-index-2026-10-08.md, R3) is that simple pre-open
//! rankers -- |gap| x relative volume, premarket range -- concentrate the
//! day's big movers; the shipped funnel ordering scored 7-12 points worse
//! than them. Nothing here claims an entry edge: R1/R2 found none. The
//! shortlist answers "where should I look", and says so.
//!
//! Inputs are events the live scan already broadcasts, so no wire contract
//! changes (`ScanEvent` is frozen; see observation_tests.rs):
//! * `FunnelSignal` -- price, gap % (last vs prior close), relative volume,
//!   float. Only funnel-promoted symbols get one, which is the pool.
//! * final 1-minute `BarUpdate`s -- premarket high/low, day high, VWAP and
//!   approximate dollar volume. Bars from before a symbol was promoted are
//!   backfilled once from the provider (`spawn`).
//! * `HaltWarning` -- the LULD reference and band width, so the band edges
//!   and the distance to the upper band can be stated in dollars and %.
//! * `CatalystUpdate` -- news tags and the newest headline.
//! * `market_data::quote_book` -- the current bid/ask, i.e. the spread.
//!
//! Served by `GET /shortlist` (http.rs) as a cached read, like
//! `/movers/today`, rather than a new broadcast variant.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use backtest_metrics::base_rates::BaseRates;
use chrono::{DateTime, NaiveDate, Utc};
use market_data::quote_book::LatestQuote;
use market_data::trading_session::{classify_session, market_day, market_day_open, TradingSession};
use market_data::{AlpacaConfig, HaltAlertLevel, ScanEvent};
use serde::Serialize;
use tokio::sync::{broadcast, Semaphore};
use tracing::{debug, warn};

/// How many candidates the shortlist shows. Small on purpose: the point is
/// to do the combining for the trader, not hand back another long list.
pub const TOP_N: usize = 10;
/// A funnel reading older than this no longer describes the symbol.
/// `FunnelSignal` arrives once per 1-minute bar, and a quiet small cap can
/// skip several minutes without trading.
pub const FUNNEL_FRESH_SECS: i64 = 600;
/// Before retrying a failed bar backfill for the same symbol.
const BACKFILL_RETRY_SECS: i64 = 300;
const BACKFILL_CONCURRENCY: usize = 2;
/// The regular session opens 5h30m after the 04:00 ET market-day open.
const REGULAR_OPEN_OFFSET_SECS: i64 = 5 * 3600 + 30 * 60;
/// A bar series starting within this long of 04:00 ET counts as covering
/// the whole premarket.
const COMPLETE_FROM_SECS: i64 = 5 * 60;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MinuteBar {
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: u64,
}

impl MinuteBar {
    fn typical(&self) -> f64 {
        (self.high + self.low + self.close) / 3.0
    }
}

#[derive(Debug, Clone)]
struct Funnel {
    at: DateTime<Utc>,
    price: f64,
    gap_pct: f64,
    session_volume: u64,
    float_shares: Option<u64>,
    relative_volume: Option<f64>,
    price_ok: bool,
    passed: bool,
}

#[derive(Debug, Clone)]
struct Halt {
    at: DateTime<Utc>,
    reference_price: f64,
    current_price: f64,
    band_width_dollars: f64,
    proximity_ratio: f64,
    level: HaltAlertLevel,
    luld_in_effect: bool,
    estimated: bool,
}

#[derive(Debug, Clone)]
struct Catalyst {
    tags: Vec<String>,
    headline_count: u32,
    headline: Option<String>,
    published_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Backfill {
    Pending,
    Done,
    Failed(DateTime<Utc>),
}

#[derive(Debug, Default)]
struct SymbolState {
    funnel: Option<Funnel>,
    /// Final 1-minute bars of the current market day, by bucket start (unix s).
    bars: BTreeMap<i64, MinuteBar>,
    halt: Option<Halt>,
    catalyst: Option<Catalyst>,
    backfill: Option<Backfill>,
}

#[derive(Debug, Default)]
pub struct Shortlist {
    day: Option<NaiveDate>,
    symbols: HashMap<String, SymbolState>,
}

// ---- wire shape -------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ShortlistBody {
    pub generated_at: DateTime<Utc>,
    pub market_day: Option<NaiveDate>,
    pub session: &'static str,
    /// The ranking, in words, so no client has to guess what order means.
    pub ranked_by: &'static str,
    /// Fresh funnel symbols considered before exclusions.
    pub pool_size: usize,
    pub excluded: Excluded,
    pub candidates: Vec<Candidate>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Excluded {
    pub outside_price_range: usize,
    pub not_gapping_up: usize,
    pub no_relative_volume: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub rank: usize,
    pub symbol: String,
    /// gapPct x relativeVolume.
    pub score: f64,
    pub price: f64,
    /// Last price vs prior close, as the funnel computes it -- the day's
    /// change, not an opening gap.
    pub gap_pct: f64,
    pub relative_volume: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub float_shares: Option<u64>,
    pub session_volume: u64,
    pub funnel_passed: bool,
    pub funnel_at: DateTime<Utc>,
    pub levels: Levels,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quote: Option<QuoteOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub halt: Option<HaltOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalyst: Option<CatalystOut>,
    /// How often this setup profile followed through historically
    /// (backtest_metrics::base_rates). Absent when no rates file is loaded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_record: Option<TrackRecordOut>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TrackRecordOut {
    /// e.g. "25-50% day change, 3-10x relative volume".
    pub profile: String,
    pub n: usize,
    /// False below the rates file's minimum sample: counts only, no rates.
    pub sufficient: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_first_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_close_return_net_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_max_adverse_pct: Option<f64>,
    pub target_pct: f64,
    pub stop_pct: f64,
    pub decision_time_et: String,
    pub source_note: String,
    pub generated_at: DateTime<Utc>,
}

fn track_record(rates: &BaseRates, gap_pct: f64, relative_volume: f64) -> Option<TrackRecordOut> {
    let p = rates.lookup(gap_pct, relative_volume)?;
    Some(TrackRecordOut {
        profile: format!("{} day change, {} relative volume", p.gap, p.relative_volume),
        n: p.n,
        sufficient: p.n >= rates.min_sample,
        target_rate: p.target_rate,
        stop_first_rate: p.stop_first_rate,
        median_close_return_net_pct: p.median_close_return_net_pct,
        median_max_adverse_pct: p.median_max_adverse_pct,
        target_pct: rates.target_pct,
        stop_pct: rates.stop_pct,
        decision_time_et: rates.decision_time_et.clone(),
        source_note: rates.source.note.clone(),
        generated_at: rates.generated_at,
    })
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Levels {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub premarket_high: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub premarket_low: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub day_high: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vwap: Option<f64>,
    /// "premarket" (anchored 04:00 ET) or "regular" (anchored 09:30 ET).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vwap_anchor: Option<&'static str>,
    /// Sum of typical price x volume over the day's 1-minute bars: an
    /// approximation of traded value, not an exact tape sum.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dollar_volume: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bars_from: Option<DateTime<Utc>>,
    /// False when the day's bars start late (no backfill yet), so premarket
    /// levels and VWAP describe only part of the session.
    pub bars_complete: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QuoteOut {
    pub bid: f64,
    pub ask: f64,
    pub spread_pct: f64,
    pub age_secs: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HaltOut {
    pub level: HaltAlertLevel,
    pub proximity_ratio: f64,
    pub upper_band: f64,
    pub lower_band: f64,
    /// (upper band - current price) / current price x 100.
    pub distance_to_upper_pct: f64,
    pub estimated: bool,
    pub luld_in_effect: bool,
    pub age_secs: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CatalystOut {
    pub tags: Vec<String>,
    pub headline_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headline: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_at: Option<DateTime<Utc>>,
}

pub const RANKED_BY: &str = "gapPct x relativeVolume (consolidation index R3); a watchlist ranking, not an entry signal";

fn session_name(s: TradingSession) -> &'static str {
    match s {
        TradingSession::Premarket => "premarket",
        TradingSession::Regular => "regular",
        TradingSession::AfterHours => "afterhours",
        TradingSession::Overnight => "overnight",
    }
}

impl Shortlist {
    /// Rolls the state over at the 04:00 ET market-day boundary: yesterday's
    /// bars and readings say nothing about today.
    fn roll(&mut self, at: DateTime<Utc>) {
        let day = market_day(at);
        if self.day != Some(day) {
            self.day = Some(day);
            self.symbols.clear();
        }
    }

    pub fn on_event(&mut self, event: &ScanEvent) {
        match event {
            ScanEvent::FunnelSignal { symbol, timestamp, price, gap_pct, session_volume, float_shares, relative_volume, price_ok, passed, .. } => {
                self.roll(*timestamp);
                let funnel = Funnel {
                    at: *timestamp,
                    price: *price,
                    gap_pct: *gap_pct,
                    session_volume: *session_volume,
                    float_shares: *float_shares,
                    relative_volume: *relative_volume,
                    price_ok: *price_ok,
                    passed: *passed,
                };
                let state = self.symbols.entry(symbol.clone()).or_default();
                if state.funnel.as_ref().is_none_or(|f| f.at <= funnel.at) {
                    state.funnel = Some(funnel);
                }
            }
            ScanEvent::BarUpdate { is_final: true, interval_secs: 60, symbol, timestamp, open, high, low, close, volume } => {
                self.roll(*timestamp);
                let bar = MinuteBar { open: *open, high: *high, low: *low, close: *close, volume: *volume };
                self.symbols.entry(symbol.clone()).or_default().bars.insert(timestamp.timestamp(), bar);
            }
            ScanEvent::HaltWarning { symbol, timestamp, reference_price, current_price, band_width_dollars, proximity_ratio, level, luld_in_effect, estimated_bands, .. } => {
                // Halt warnings arrive per trade for every tracked symbol;
                // only symbols the shortlist already knows are worth keeping.
                if let Some(state) = self.symbols.get_mut(symbol) {
                    state.halt = Some(Halt {
                        at: *timestamp,
                        reference_price: *reference_price,
                        current_price: *current_price,
                        band_width_dollars: *band_width_dollars,
                        proximity_ratio: *proximity_ratio,
                        level: *level,
                        luld_in_effect: *luld_in_effect,
                        estimated: *estimated_bands,
                    });
                }
            }
            ScanEvent::CatalystUpdate { symbol, timestamp, catalyst_tags, headline_count, most_recent_headline, most_recent_published_at } => {
                self.roll(*timestamp);
                self.symbols.entry(symbol.clone()).or_default().catalyst = Some(Catalyst {
                    tags: catalyst_tags.clone(),
                    headline_count: *headline_count,
                    headline: most_recent_headline.clone(),
                    published_at: *most_recent_published_at,
                });
            }
            _ => {}
        }
    }

    /// Merges provider bars fetched for `symbol`; live bars already held for
    /// the same minute win, since they are the official final bar.
    pub fn merge_backfill(&mut self, symbol: &str, bars: impl IntoIterator<Item = (i64, MinuteBar)>, ok: bool, now: DateTime<Utc>) {
        let Some(day) = self.day else { return };
        let Some(state) = self.symbols.get_mut(symbol) else { return };
        for (t, bar) in bars {
            if DateTime::from_timestamp(t, 0).is_some_and(|at| market_day(at) == day) {
                state.bars.entry(t).or_insert(bar);
            }
        }
        state.backfill = Some(if ok { Backfill::Done } else { Backfill::Failed(now) });
    }

    /// Symbols currently ranked whose bars do not cover the premarket and
    /// have no backfill under way; each returned symbol is marked pending.
    pub fn take_backfill_requests(&mut self, now: DateTime<Utc>) -> Vec<String> {
        let ranked: Vec<String> = self.ranked(now).into_iter().map(|(s, _)| s).collect();
        let Some(day) = self.day else { return Vec::new() };
        let open = market_day_open(day).timestamp();
        let mut out = Vec::new();
        for symbol in ranked {
            let Some(state) = self.symbols.get_mut(&symbol) else { continue };
            let covered = state.bars.keys().next().is_some_and(|first| *first <= open + COMPLETE_FROM_SECS);
            let due = match state.backfill {
                None => !covered,
                Some(Backfill::Failed(at)) => (now - at).num_seconds() >= BACKFILL_RETRY_SECS,
                Some(Backfill::Pending) | Some(Backfill::Done) => false,
            };
            if due {
                state.backfill = Some(Backfill::Pending);
                out.push(symbol);
            }
        }
        out
    }

    /// Fresh, eligible symbols with their score, best first.
    fn ranked(&self, now: DateTime<Utc>) -> Vec<(String, f64)> {
        let mut out: Vec<(String, f64)> = self
            .symbols
            .iter()
            .filter_map(|(symbol, s)| {
                let f = s.funnel.as_ref()?;
                if (now - f.at).num_seconds() > FUNNEL_FRESH_SECS || !f.price_ok || f.gap_pct <= 0.0 {
                    return None;
                }
                let rv = f.relative_volume.filter(|v| v.is_finite() && *v > 0.0)?;
                Some((symbol.clone(), f.gap_pct * rv))
            })
            .collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out.truncate(TOP_N);
        out
    }

    pub fn body(&self, now: DateTime<Utc>, quote_of: impl Fn(&str) -> Option<LatestQuote>, rates: Option<&BaseRates>) -> ShortlistBody {
        let mut excluded = Excluded::default();
        let mut pool_size = 0;
        for s in self.symbols.values() {
            let Some(f) = s.funnel.as_ref() else { continue };
            if (now - f.at).num_seconds() > FUNNEL_FRESH_SECS {
                continue;
            }
            pool_size += 1;
            if !f.price_ok {
                excluded.outside_price_range += 1;
            } else if f.gap_pct <= 0.0 {
                excluded.not_gapping_up += 1;
            } else if f.relative_volume.filter(|v| v.is_finite() && *v > 0.0).is_none() {
                excluded.no_relative_volume += 1;
            }
        }
        let candidates = self
            .ranked(now)
            .into_iter()
            .enumerate()
            .filter_map(|(i, (symbol, score))| {
                let s = self.symbols.get(&symbol)?;
                let f = s.funnel.as_ref()?;
                Some(Candidate {
                    rank: i + 1,
                    score,
                    price: f.price,
                    gap_pct: f.gap_pct,
                    relative_volume: f.relative_volume.unwrap_or_default(),
                    float_shares: f.float_shares,
                    session_volume: f.session_volume,
                    funnel_passed: f.passed,
                    funnel_at: f.at,
                    levels: self.levels(s, now),
                    quote: quote_of(&symbol).map(|q| QuoteOut { bid: q.bid, ask: q.ask, spread_pct: q.spread_pct(), age_secs: (now - q.at).num_seconds().max(0) }),
                    halt: s.halt.as_ref().map(|h| {
                        let upper = h.reference_price + h.band_width_dollars;
                        HaltOut {
                            level: h.level,
                            proximity_ratio: h.proximity_ratio,
                            upper_band: upper,
                            lower_band: h.reference_price - h.band_width_dollars,
                            distance_to_upper_pct: if h.current_price > 0.0 { (upper - h.current_price) / h.current_price * 100.0 } else { 0.0 },
                            estimated: h.estimated,
                            luld_in_effect: h.luld_in_effect,
                            age_secs: (now - h.at).num_seconds().max(0),
                        }
                    }),
                    catalyst: s.catalyst.as_ref().map(|c| CatalystOut { tags: c.tags.clone(), headline_count: c.headline_count, headline: c.headline.clone(), published_at: c.published_at }),
                    track_record: rates.and_then(|r| track_record(r, f.gap_pct, f.relative_volume.unwrap_or_default())),
                    symbol,
                })
            })
            .collect();
        ShortlistBody {
            generated_at: now,
            market_day: self.day,
            session: session_name(classify_session(now)),
            ranked_by: RANKED_BY,
            pool_size,
            excluded,
            candidates,
        }
    }

    fn levels(&self, s: &SymbolState, now: DateTime<Utc>) -> Levels {
        let Some(day) = self.day else { return Levels::default() };
        let day_open = market_day_open(day).timestamp();
        let regular_open = day_open + REGULAR_OPEN_OFFSET_SECS;
        let bars: Vec<(i64, MinuteBar)> = s.bars.range(day_open..).map(|(t, b)| (*t, *b)).collect();
        if bars.is_empty() {
            return Levels::default();
        }
        let premarket: Vec<&MinuteBar> = bars.iter().filter(|(t, _)| *t < regular_open).map(|(_, b)| b).collect();
        let anchor_regular = now.timestamp() >= regular_open && bars.iter().any(|(t, _)| *t >= regular_open);
        let (from, anchor) = if anchor_regular { (regular_open, "regular") } else { (day_open, "premarket") };
        let (pv, v) = bars.iter().filter(|(t, _)| *t >= from).fold((0.0, 0u64), |(pv, v), (_, b)| (pv + b.typical() * b.volume as f64, v + b.volume));
        let first = bars[0].0;
        Levels {
            premarket_high: premarket.iter().map(|b| b.high).reduce(f64::max),
            premarket_low: premarket.iter().map(|b| b.low).reduce(f64::min),
            day_high: bars.iter().map(|(_, b)| b.high).reduce(f64::max),
            vwap: (v > 0).then(|| pv / v as f64),
            vwap_anchor: (v > 0).then_some(anchor),
            dollar_volume: Some(bars.iter().map(|(_, b)| b.typical() * b.volume as f64).sum()),
            bars_from: DateTime::from_timestamp(first, 0),
            bars_complete: first <= day_open + COMPLETE_FROM_SECS || s.backfill == Some(Backfill::Done),
        }
    }
}

// ---- process-wide instance ---------------------------------------------

static SHORTLIST: OnceLock<Mutex<Shortlist>> = OnceLock::new();

fn locked() -> std::sync::MutexGuard<'static, Shortlist> {
    SHORTLIST.get_or_init(|| Mutex::new(Shortlist::default())).lock().unwrap_or_else(|e| e.into_inner())
}

/// Where the base rates are read from, once, at first use. Built offline by
/// `backtest-metrics`'s `base_rates` binary; a missing or unreadable file
/// leaves every card's track record absent ("not measured yet").
const BASE_RATES_ENV: &str = "SHORTLIST_BASE_RATES_PATH";
const BASE_RATES_DEFAULT: &str = "data/base_rates.json";

static BASE_RATES: OnceLock<Option<BaseRates>> = OnceLock::new();

fn base_rates() -> Option<&'static BaseRates> {
    BASE_RATES
        .get_or_init(|| {
            let path = std::env::var(BASE_RATES_ENV).unwrap_or_else(|_| BASE_RATES_DEFAULT.into());
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    debug!(%path, error = %e, "shortlist: no base rates file; track records stay empty");
                    return None;
                }
            };
            match serde_json::from_str::<BaseRates>(&text) {
                Ok(r) if r.schema == backtest_metrics::base_rates::SCHEMA => Some(r),
                Ok(r) => {
                    warn!(%path, schema = %r.schema, "shortlist: base rates file has an unknown schema; ignored");
                    None
                }
                Err(e) => {
                    warn!(%path, error = %e, "shortlist: base rates file is unreadable; ignored");
                    None
                }
            }
        })
        .as_ref()
}

/// What `GET /shortlist` returns.
pub fn current_body(now: DateTime<Utc>) -> ShortlistBody {
    locked().body(now, market_data::quote_book::latest, base_rates())
}

/// Feeds the shortlist from the live broadcast and backfills the day's bars
/// for newly ranked symbols, at most `BACKFILL_CONCURRENCY` at a time.
pub fn spawn(events: broadcast::Receiver<ScanEvent>, cfg: Arc<AlpacaConfig>) {
    let mut events = events;
    let slots = Arc::new(Semaphore::new(BACKFILL_CONCURRENCY));
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(15));
        loop {
            tokio::select! {
                received = events.recv() => match received {
                    Ok(event) => { locked().on_event(&event); continue; }
                    // A lost bar leaves a minute out of VWAP; the next
                    // backfill (or the next day) restores it. Funnel and
                    // halt readings are refreshed by the next event anyway.
                    Err(broadcast::error::RecvError::Lagged(missed)) => { warn!(missed, "shortlist: fell behind the live feed"); continue; }
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                _ = tick.tick() => {}
            }
            let now = Utc::now();
            let requests = locked().take_backfill_requests(now);
            for symbol in requests {
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    // Busy: put it back to be retried on a later tick.
                    locked().merge_backfill(&symbol, std::iter::empty(), false, now - chrono::Duration::seconds(BACKFILL_RETRY_SECS));
                    continue;
                };
                let cfg = cfg.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let day_open = market_day_open(market_day(now));
                    let result = market_data::rest::fetch_recent_minute_bars(&cfg, &symbol, &day_open.to_rfc3339(), &now.to_rfc3339()).await;
                    let finished = Utc::now();
                    match result {
                        Ok(bars) => {
                            let n = bars.len();
                            let bars = bars.into_iter().map(|b| (b.timestamp.timestamp(), MinuteBar { open: b.open, high: b.high, low: b.low, close: b.close, volume: b.volume }));
                            locked().merge_backfill(&symbol, bars, true, finished);
                            debug!(%symbol, bars = n, "shortlist: backfilled the day's bars");
                        }
                        Err(e) => {
                            warn!(%symbol, error = %e, "shortlist: bar backfill failed; will retry");
                            locked().merge_backfill(&symbol, std::iter::empty(), false, finished);
                        }
                    }
                });
            }
        }
    });
}

#[cfg(test)]
#[path = "shortlist_tests.rs"]
mod shortlist_tests;
