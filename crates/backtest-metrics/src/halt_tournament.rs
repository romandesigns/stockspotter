//! Preregistered panel tournament (2026-10-16): Halt Early-Warning read as
//! a speed alert. Which alert rule on the gauge, if any, beats buying a
//! random minute of the same symbol-day, after costs, 30 minutes later?
//!
//! The protocol lives in
//! `outputs/halt-tournament-20261016/PREREGISTRATION.md`. This module is
//! its pure logic:
//!
//! * **The gauge** is `halt_detector::HaltWarningMonitor`'s real
//!   `proximity_ratio` (|price - 5-minute rolling reference| / LULD band
//!   width), fed every cached trade print in time order from 04:00 ET, one
//!   fresh monitor per symbol-day, exactly as `replay_engine::replay`
//!   does. The cache has no SIP LULD messages, so the bands are always the
//!   monitor's own estimate (what live uses when official bands are absent).
//! * **The alert rules** (`Rule`) mirror ws-server's `speed_alert.rs`
//!   (itself a mirror of `haltSpeed.ts`): the gauge percent is
//!   floor(ratio x 100 + 0.5) clamped to 0..=100, the first reading per
//!   symbol is a baseline, a reading at or before the last seen millisecond
//!   is ignored, "up" is price >= reference, the per-symbol cooldown is 10
//!   minutes of reading time, and "filling fast" is +15 points over the
//!   newest 5-second-sampled point at least 60 s older.
//! * **Entry** is one minute boundary later at most: the start of the first
//!   1-minute bar strictly after the alert print (`entry_instant`), at the
//!   ask of the last two-sided quote before that instant (<= 120 s old); with
//!   no such quote, the open of the first bar within 300 s plus half of a
//!   0.5% round-trip proxy. Exits are bar closes minus half the spread.
//! * **Baseline**: `K_RANDOM` random bar starts of the same symbol-day in
//!   the same session window, measured by the very same function.
//!
//! The statistics reuse `eval_stats` (SplitMix64, the cluster bootstrap).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use chrono::{DateTime, NaiveDate, Utc};
use halt_detector::{HaltWarningConfig, HaltWarningMonitor};
use market_data::trading_session::{market_day, market_day_open, regular_session_close, regular_session_open};
use market_data::Bar;
use serde::{Deserialize, Serialize};

use crate::base_rates::{follow, Outcome};
use crate::eval_stats::{cluster_bootstrap, fnv1a, median, Ci, Paired, SplitMix64};

// ---- preregistered constants ---------------------------------------------------

pub const K_RANDOM: usize = 20;
pub const RANDOM_SEED: u64 = 20_261_018;
pub const BOOTSTRAP_SEED: u64 = 0x5EED_2026_1018;
pub const BOOTSTRAP_B: usize = 10_000;
/// Selection floor: alerts with a primary value and a baseline...
pub const MIN_ALERTS: usize = 50;
/// ...over at least this many distinct trading dates.
pub const MIN_DATES: usize = 10;
/// Family-wise alpha, split over every candidate in the family.
pub const FAMILY_ALPHA: f64 = 0.05;
/// Horizons in minutes after the entry instant; "close" follows them.
pub const HORIZONS_MIN: [i64; 4] = [5, 15, 30, 60];
pub const HORIZON_LABELS: [&str; 5] = ["5m", "15m", "30m", "60m", "close"];
pub const N_H: usize = HORIZON_LABELS.len();
/// Index of the 30-minute horizon, the only one selection and verdict read.
pub const PRIMARY: usize = 2;
pub const PRIMARY_SECS: i64 = 30 * 60;
pub const QUOTE_MAX_AGE_MS: i64 = 120_000;
/// `strategy_config::DEFAULT_ROUND_TRIP_COST_PCT`, fixed here (the env var
/// is deliberately not read, so the run cannot depend on the shell).
pub const PROXY_ROUND_TRIP_PCT: f64 = 0.5;
pub const MAX_PROXY_ENTRY_DELAY_SECS: i64 = 300;

// Mirrors of ws-server speed_alert.rs (SPEED_ALERT_DEFAULTS in haltSpeed.ts).
pub const COOLDOWN_MS: i64 = 10 * 60_000;
pub const SPEED_HISTORY_WINDOW_MS: i64 = 10 * 60_000;
pub const SPEED_SAMPLE_MS: i64 = 5_000;
pub const FILLING_FAST_POINTS: i32 = 15;
pub const FILLING_FAST_LOOKBACK_MS: i64 = 60_000;

// Rules new in this tournament.
pub const SECOND_PUSH_DIP_PCT: u8 = 40;
pub const SECOND_PUSH_WINDOW_MS: i64 = 15 * 60_000;
pub const CONFLUENCE_MIN_RVOL: f64 = 1.0;

// ---- input -----------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct TradeIn {
    #[serde(rename = "p")]
    pub price: f64,
    #[serde(rename = "s")]
    pub size: u64,
    #[serde(rename = "t")]
    pub timestamp: DateTime<Utc>,
    /// Condition codes. Read only by the post-hoc sensitivity
    /// (`PrintFilter::PriceEligibleOnly`), never by the preregistered run.
    #[serde(rename = "c", default)]
    pub conditions: Vec<String>,
}

/// Which prints feed the gauge. `All` is the preregistered reconstruction
/// (live feeds every print). `PriceEligibleOnly` is a POST-HOC sensitivity
/// added after the EXPLORE outcomes were seen (see REPORT.md): it drops
/// prints that may not move a minute bar's price.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintFilter {
    All,
    PriceEligibleOnly,
}

/// Mirror of `market_data::forming_bar::trade_updates(..).0` (private to
/// that crate): false when any condition is one Alpaca's minute bars treat
/// as not moving the price (official open/close, odd lot, out of sequence,
/// average price, prior reference, derivatively priced, ...). `B` counts
/// only without the UTP `@`.
pub fn price_eligible(conditions: &[String]) -> bool {
    let utp = conditions.iter().any(|c| c.trim() == "@");
    !conditions.iter().any(|c| match c.trim() {
        "M" | "Q" | "9" => true,
        "B" => !utp,
        "C" | "G" | "H" | "I" | "N" | "P" | "R" | "U" | "V" | "W" | "Z" | "4" | "7" => true,
        _ => false,
    })
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct QuoteIn {
    #[serde(rename = "bp")]
    pub bid: f64,
    #[serde(rename = "ap")]
    pub ask: f64,
    #[serde(rename = "t")]
    pub timestamp: DateTime<Utc>,
}

/// One `<SYM>-<DATE>-sip-v1.json` file, only the fields used here.
#[derive(Debug, Clone, Deserialize)]
pub struct DayFile {
    pub symbol: String,
    pub prior_close: f64,
    pub avg_daily_volume: u64,
    pub bars: Vec<Bar>,
    pub quotes: Vec<QuoteIn>,
    pub trades: Vec<TradeIn>,
}

pub fn date_from_file_name(name: &str) -> Option<NaiveDate> {
    let stem = name.strip_suffix("-sip-v1.json")?;
    let date = stem.get(stem.len().checked_sub(10)?..)?;
    NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()
}

/// The preregistered cut: the date at index (n - 1) / 2 of the sorted
/// distinct dates (the extra date of an odd count goes to EXPLORE).
pub fn split_cut(dates: &[NaiveDate]) -> Option<NaiveDate> {
    let d: Vec<NaiveDate> = dates.iter().copied().collect::<BTreeSet<_>>().into_iter().collect();
    d.get(d.len().checked_sub(1)? / 2).copied()
}

// ---- the gauge -----------------------------------------------------------------------

/// What the panel knows after one trade print.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reading {
    pub at_ms: i64,
    pub price: f64,
    pub reference: f64,
    pub ratio: f64,
    pub relative_volume: Option<f64>,
    /// Volume-weighted price of every print since 04:00 ET through this one.
    pub vwap: f64,
    pub luld_in_effect: bool,
}

/// Feeds every print of one symbol-day (sorted by time, stable) through a
/// fresh `HaltWarningMonitor`. Prints with a non-finite or non-positive
/// price are dropped (they would poison the rolling reference).
pub fn readings(trades: &[TradeIn], avg_daily_volume: u64) -> Vec<Reading> {
    readings_with(trades, avg_daily_volume, PrintFilter::All)
}

/// `readings` with a print filter (see `PrintFilter`). A dropped print
/// contributes neither price nor volume.
pub fn readings_with(trades: &[TradeIn], avg_daily_volume: u64, filter: PrintFilter) -> Vec<Reading> {
    let mut t: Vec<&TradeIn> = trades
        .iter()
        .filter(|t| t.price.is_finite() && t.price > 0.0 && (filter == PrintFilter::All || price_eligible(&t.conditions)))
        .collect();
    t.sort_by_key(|t| t.timestamp);
    let mut monitor = HaltWarningMonitor::new(HaltWarningConfig::default(), avg_daily_volume);
    let (mut pv, mut v) = (0.0f64, 0.0f64);
    t.into_iter()
        .map(|tr| {
            let secs = tr.timestamp.timestamp() as f64 + f64::from(tr.timestamp.timestamp_subsec_nanos()) / 1e9;
            let r = monitor.on_trade(halt_detector::Trade { timestamp_secs: secs, price: tr.price, size: tr.size }, tr.timestamp);
            pv += tr.price * tr.size as f64;
            v += tr.size as f64;
            Reading {
                at_ms: tr.timestamp.timestamp_millis(),
                price: tr.price,
                reference: r.reference_price,
                ratio: r.proximity_ratio,
                relative_volume: r.relative_volume,
                vwap: if v > 0.0 { pv / v } else { tr.price },
                luld_in_effect: r.luld_in_effect,
            }
        })
        .collect()
}

/// speed_alert.rs `gauge_pct`.
pub fn gauge_pct(ratio: f64) -> u8 {
    let ratio = if ratio.is_finite() { ratio } else { 0.0 };
    (ratio * 100.0 + 0.5).floor().clamp(0.0, 100.0) as u8
}

// ---- candidates ------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Rule {
    /// Up reading rising through `threshold` (the live rule's shape).
    Cross { threshold: u8, require_filling_fast: bool, regular_only: bool },
    /// Up reading rising through `threshold` at most `window_ms` after the
    /// previous up crossing of `threshold`, with a reading below `dip`
    /// in between.
    SecondPush { threshold: u8, dip: u8, window_ms: i64 },
    /// Up reading rising through `threshold` while relative volume >=
    /// `min_rvol` and price >= the session VWAP.
    Confluence { threshold: u8, min_rvol: f64 },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub index: usize,
    pub id: &'static str,
    pub rule: Rule,
}

/// The 8 preregistered candidates, in preregistration order.
pub fn candidates() -> Vec<Candidate> {
    let cross = |threshold, ff, reg| Rule::Cross { threshold, require_filling_fast: ff, regular_only: reg };
    [
        ("G70", cross(70, false, false)),
        ("G60", cross(60, false, false)),
        ("G80", cross(80, false, false)),
        ("G90", cross(90, false, false)),
        ("G70-FF", cross(70, true, false)),
        ("G70-REG", cross(70, false, true)),
        ("G70-2ND", Rule::SecondPush { threshold: 70, dip: SECOND_PUSH_DIP_PCT, window_ms: SECOND_PUSH_WINDOW_MS }),
        ("G50-RV-VWAP", Rule::Confluence { threshold: 50, min_rvol: CONFLUENCE_MIN_RVOL }),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (id, rule))| Candidate { index, id, rule })
    .collect()
}

/// Per-symbol-day alert state for one candidate: speed_alert.rs's
/// `SymbolState` plus the second-push memory.
#[derive(Debug, Default, Clone)]
pub struct Tracker {
    seen: bool,
    last_seen_ms: i64,
    last_pct: u8,
    last_alert_ms: Option<i64>,
    points: VecDeque<(i64, u8)>,
    last_push_ms: Option<i64>,
    dipped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Fired {
    pub pct: u8,
    pub filling_fast: bool,
}

impl Tracker {
    fn add_point(&mut self, at_ms: i64, pct: u8) {
        match self.points.back_mut() {
            Some(last) if last.0.div_euclid(SPEED_SAMPLE_MS) == at_ms.div_euclid(SPEED_SAMPLE_MS) => *last = (at_ms, pct),
            _ => self.points.push_back((at_ms, pct)),
        }
        let cutoff = at_ms - SPEED_HISTORY_WINDOW_MS;
        while self.points.len() > 1 && self.points.front().is_some_and(|p| p.0 < cutoff) {
            self.points.pop_front();
        }
    }

    fn filling_fast(&self) -> bool {
        let Some(last) = self.points.back() else { return false };
        self.points
            .iter()
            .rev()
            .skip(1)
            .find(|p| last.0 - p.0 >= FILLING_FAST_LOOKBACK_MS)
            .is_some_and(|p| i32::from(last.1) - i32::from(p.1) >= FILLING_FAST_POINTS)
    }

    /// One reading; `Some` when this candidate alerts on it. Uses only this
    /// reading and the tracker's memory of earlier ones.
    pub fn observe(&mut self, rule: &Rule, r: &Reading) -> Option<Fired> {
        let pct = gauge_pct(r.ratio);
        if !self.seen {
            self.seen = true;
            self.last_seen_ms = r.at_ms;
            self.last_pct = pct;
            self.add_point(r.at_ms, pct);
            return None;
        }
        if r.at_ms <= self.last_seen_ms {
            return None;
        }
        self.last_seen_ms = r.at_ms;
        self.add_point(r.at_ms, pct);
        let prev = std::mem::replace(&mut self.last_pct, pct);
        let bullish = r.price >= r.reference;
        let cooled = self.last_alert_ms.is_none_or(|t| r.at_ms - t >= COOLDOWN_MS);
        let up_through = |th: u8| prev < th && pct >= th && bullish;
        let fire = match *rule {
            Rule::Cross { threshold, require_filling_fast, regular_only } => {
                up_through(threshold) && cooled && (!regular_only || r.luld_in_effect) && (!require_filling_fast || self.filling_fast())
            }
            Rule::Confluence { threshold, min_rvol } => {
                up_through(threshold) && cooled && r.relative_volume.is_some_and(|v| v >= min_rvol) && r.price >= r.vwap
            }
            Rule::SecondPush { threshold, dip, window_ms } => {
                if up_through(threshold) {
                    let fire = self.dipped && self.last_push_ms.is_some_and(|t| r.at_ms - t <= window_ms) && cooled;
                    self.last_push_ms = Some(r.at_ms);
                    self.dipped = false;
                    fire
                } else {
                    if self.last_push_ms.is_some() && pct < dip {
                        self.dipped = true;
                    }
                    false
                }
            }
        };
        if !fire {
            return None;
        }
        self.last_alert_ms = Some(r.at_ms);
        Some(Fired { pct, filling_fast: self.filling_fast() })
    }
}

// ---- measurement ----------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Window {
    Premarket,
    Regular,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CostModel {
    Quote,
    Proxy,
}

/// Session boundaries, unix seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Clock {
    pub day_open: i64,
    pub regular_open: i64,
    pub close: i64,
}

impl Clock {
    pub fn of(day: NaiveDate) -> Option<Clock> {
        Some(Clock { day_open: market_day_open(day).timestamp(), regular_open: regular_session_open(day)?.timestamp(), close: regular_session_close(day)?.timestamp() })
    }

    pub fn window(&self, secs: i64) -> Option<Window> {
        if secs < self.day_open {
            None
        } else if secs < self.regular_open {
            Some(Window::Premarket)
        } else if secs < self.close {
            Some(Window::Regular)
        } else {
            None
        }
    }
}

/// Bars (this market day, start < close, sorted) and valid two-sided
/// quotes (ms, bid, ask), sorted.
#[derive(Debug, Clone)]
pub struct Market {
    pub clock: Clock,
    pub bars: Vec<Bar>,
    pub quotes: Vec<(i64, f64, f64)>,
}

impl Market {
    pub fn new(clock: Clock, day: NaiveDate, bars: &[Bar], quotes: &[QuoteIn]) -> Market {
        let mut b: Vec<Bar> = bars.iter().filter(|b| market_day(b.timestamp) == day && b.timestamp.timestamp() < clock.close).cloned().collect();
        b.sort_by_key(|b| b.timestamp);
        let mut q: Vec<(i64, f64, f64)> = quotes
            .iter()
            .filter(|q| q.bid.is_finite() && q.ask.is_finite() && q.bid > 0.0 && q.ask >= q.bid)
            .map(|q| (q.timestamp.timestamp_millis(), q.bid, q.ask))
            .collect();
        q.sort_by_key(|q| q.0);
        Market { clock, bars: b, quotes: q }
    }

    fn bar_start(&self, i: usize) -> i64 {
        self.bars[i].timestamp.timestamp()
    }

    /// First bar index starting at or after `secs`.
    fn bar_at_or_after(&self, secs: i64) -> usize {
        self.bars.partition_point(|b| b.timestamp.timestamp() < secs)
    }

    /// Last valid quote strictly before `secs`, at most 120 s old.
    fn quote_before(&self, secs: i64) -> Option<(f64, f64)> {
        let ms = secs * 1000;
        let i = self.quotes.partition_point(|q| q.0 < ms).checked_sub(1)?;
        let q = self.quotes[i];
        (ms - q.0 <= QUOTE_MAX_AGE_MS).then_some((q.1, q.2))
    }
}

/// The minute boundary after an alert print: the start of the first
/// 1-minute bar strictly after `at_ms`.
pub fn entry_instant(at_ms: i64) -> i64 {
    at_ms.div_euclid(60_000) * 60 + 60
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Measured {
    pub cost_model: CostModel,
    pub entry: f64,
    pub spread_pct: f64,
    /// Unix seconds the position is held from.
    pub entry_at: i64,
    /// Net % at 5/15/30/60 min, then held to the regular close (`follow`).
    pub net: [Option<f64>; N_H],
    pub outcome: Outcome,
}

/// Buying at instant `at` (unix secs): the ask of the last quote before it
/// (<= 120 s old), else the proxy at the first bar within 300 s. Horizon h
/// is the close of the last bar ended by entry_at + h, minus half the
/// spread; `None` past the close or with no such bar.
pub fn measure(m: &Market, at: i64) -> Option<Measured> {
    let close = m.clock.close;
    if at >= close {
        return None;
    }
    let (cost_model, entry, spread_pct, entry_at, first) = match m.quote_before(at) {
        Some((bid, ask)) => (CostModel::Quote, ask, (ask - bid) / ((ask + bid) / 2.0) * 100.0, at, m.bar_at_or_after(at)),
        None => {
            let i = m.bar_at_or_after(at);
            let start = (i < m.bars.len()).then(|| m.bar_start(i)).filter(|s| s - at <= MAX_PROXY_ENTRY_DELAY_SECS && *s < close)?;
            (CostModel::Proxy, m.bars[i].open * (1.0 + PROXY_ROUND_TRIP_PCT / 200.0), PROXY_ROUND_TRIP_PCT, start, i)
        }
    };
    if !(entry.is_finite() && entry > 0.0 && spread_pct.is_finite()) {
        return None;
    }
    let after: Vec<&Bar> = m.bars[first..].iter().collect();
    let path = follow(entry, spread_pct, &after)?;
    let factor = 1.0 - spread_pct / 200.0;
    let mut net = [None; N_H];
    for (k, h) in HORIZONS_MIN.iter().enumerate() {
        let deadline = entry_at + h * 60;
        if deadline > close {
            continue;
        }
        net[k] = after.iter().take_while(|b| b.timestamp.timestamp() + 60 <= deadline).last().map(|b| (b.close * factor - entry) / entry * 100.0);
    }
    net[N_H - 1] = Some(path.close_return_net_pct);
    Some(Measured { cost_model, entry, spread_pct, entry_at, net, outcome: path.outcome })
}

/// Every bar start of the day whose primary horizon fits before the close,
/// measured, with its window. The random pool of both windows.
pub fn random_pool(m: &Market) -> Vec<(Window, Measured)> {
    (0..m.bars.len())
        .filter_map(|i| {
            let s = m.bar_start(i);
            let w = m.clock.window(s)?;
            if s + PRIMARY_SECS > m.clock.close {
                return None;
            }
            let x = measure(m, s)?;
            x.net[PRIMARY].map(|_| (w, x))
        })
        .collect()
}

// ---- per-session evaluation ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertRow {
    pub symbol: String,
    pub day: NaiveDate,
    pub alert_at: DateTime<Utc>,
    pub window: Window,
    pub price: f64,
    pub pct: u8,
    pub filling_fast: bool,
    pub measured: Measured,
    /// Mean of the K draws per horizon (draws with that horizon defined).
    pub baseline: [Option<f64>; N_H],
    pub baseline_target_rate: Option<f64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    /// Every alert the rule raised, any time of day.
    pub raised: usize,
    /// Raised at or after the close, or with the entry instant + 30 min
    /// past the close (the primary horizon would be censored).
    pub late: usize,
    /// No quote and no proxy bar, or no primary value.
    pub unmeasurable: usize,
    /// No random minute in the same window.
    pub no_baseline: usize,
}

/// All candidates on one symbol-day. Alerts are decided from readings only;
/// outcomes are looked up afterwards.
pub fn evaluate_session(cands: &[Candidate], symbol: &str, day: NaiveDate, clock: Clock, market: &Market, readings: &[Reading], k: usize, seed: u64) -> Vec<(Vec<AlertRow>, Counts)> {
    let pool = random_pool(market);
    cands
        .iter()
        .map(|c| {
            let mut tr = Tracker::default();
            let mut rows = Vec::new();
            let mut n = Counts::default();
            for r in readings {
                let Some(f) = tr.observe(&c.rule, r) else { continue };
                n.raised += 1;
                let secs = r.at_ms.div_euclid(1000);
                let Some(window) = clock.window(secs) else {
                    n.late += 1;
                    continue;
                };
                let at = entry_instant(r.at_ms);
                if at + PRIMARY_SECS > clock.close {
                    n.late += 1;
                    continue;
                }
                let Some(x) = measure(market, at).filter(|x| x.net[PRIMARY].is_some()) else {
                    n.unmeasurable += 1;
                    continue;
                };
                let draws: Vec<&Measured> = pool.iter().filter(|p| p.0 == window).map(|p| &p.1).collect();
                if draws.is_empty() {
                    n.no_baseline += 1;
                    continue;
                }
                let key = format!("{}|{}|{}|{}", c.id, symbol, day, r.at_ms);
                let mut rng = SplitMix64::new(seed ^ fnv1a(key.as_bytes()));
                let mut sums = [(0.0f64, 0usize); N_H];
                let mut targets = 0usize;
                for _ in 0..k {
                    let d = draws[rng.below(draws.len())];
                    targets += usize::from(d.outcome == Outcome::Target);
                    for (s, v) in sums.iter_mut().zip(d.net) {
                        if let Some(v) = v {
                            s.0 += v;
                            s.1 += 1;
                        }
                    }
                }
                rows.push(AlertRow {
                    symbol: symbol.to_string(),
                    day,
                    alert_at: DateTime::<Utc>::from_timestamp_millis(r.at_ms).unwrap_or_default(),
                    window,
                    price: r.price,
                    pct: f.pct,
                    filling_fast: f.filling_fast,
                    measured: x,
                    baseline: sums.map(|(s, n)| (n > 0).then(|| s / n as f64)),
                    baseline_target_rate: (k > 0).then(|| targets as f64 / k as f64),
                });
            }
            (rows, n)
        })
        .collect()
}

// ---- statistics ---------------------------------------------------------------------------

/// `eval_stats::cluster_bootstrap`'s difference replicates (same grouping,
/// same RNG stream), returned whole so any percentile can be read off.
/// The 2.5/97.5 percentiles of these equal `cluster_bootstrap`'s diff CI.
pub fn diff_replicates<K: Ord + Clone>(trades: &[Paired<K>], b: usize, seed: u64) -> Option<Vec<f64>> {
    let mut groups: BTreeMap<K, (f64, f64, usize)> = BTreeMap::new();
    for t in trades {
        let g = groups.entry(t.cluster.clone()).or_default();
        g.0 += t.ret;
        g.1 += t.random_mean;
        g.2 += 1;
    }
    let g: Vec<(f64, f64, usize)> = groups.into_values().collect();
    if g.len() < 2 || b == 0 {
        return None;
    }
    let mut rng = SplitMix64::new(seed);
    let mut diffs = Vec::with_capacity(b);
    for _ in 0..b {
        let (mut r, mut q, mut n) = (0.0, 0.0, 0usize);
        for _ in 0..g.len() {
            let x = g[rng.below(g.len())];
            r += x.0;
            q += x.1;
            n += x.2;
        }
        diffs.push((r - q) / n as f64);
    }
    Some(diffs)
}

/// Two-sided percentile interval at level 1 - alpha, nearest rank (the
/// same rule as `eval_stats::percentile_ci`, which is alpha = 0.05).
pub fn percentile_interval(mut v: Vec<f64>, alpha: f64) -> Ci {
    v.sort_by(f64::total_cmp);
    let at = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    Ci { lo: at(alpha / 2.0), hi: at(1.0 - alpha / 2.0) }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HorizonStat {
    pub label: String,
    pub n: usize,
    pub mean_pct: Option<f64>,
    pub random_mean_pct: Option<f64>,
    pub diff_pct: Option<f64>,
    pub mean_ci: Option<Ci>,
    pub diff_ci: Option<Ci>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowStat {
    pub window: Window,
    pub n: usize,
    pub mean_pct: Option<f64>,
    pub random_mean_pct: Option<f64>,
    pub diff_pct: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub id: String,
    pub counts: Counts,
    /// Alerts with a 30-minute value and a 30-minute baseline.
    pub n: usize,
    pub symbol_days: usize,
    pub dates: usize,
    pub mean_pct: Option<f64>,
    pub median_pct: Option<f64>,
    pub positive_rate: Option<f64>,
    pub random_mean_pct: Option<f64>,
    pub diff_pct: Option<f64>,
    /// 95% CIs, symbol-day clusters (the preregistered "session").
    pub mean_ci: Option<Ci>,
    pub diff_ci: Option<Ci>,
    /// (1 - FAMILY_ALPHA / k) interval of the difference, symbol-day clusters.
    pub diff_ci_adjusted: Option<Ci>,
    pub adjusted_level: f64,
    /// Sensitivity only: 95% CI of the difference with whole dates as clusters.
    pub diff_ci_by_date: Option<Ci>,
    pub horizons: Vec<HorizonStat>,
    pub windows: Vec<WindowStat>,
    pub target_first_rate: Option<f64>,
    pub stop_first_rate: Option<f64>,
    pub baseline_target_rate: Option<f64>,
    pub quote_cost_share: Option<f64>,
    pub median_spread_pct: Option<f64>,
    pub max_date_share: Option<f64>,
    pub max_symbol_day_share: Option<f64>,
    pub filling_fast_share: Option<f64>,
}

fn mean(v: impl Iterator<Item = f64>) -> Option<f64> {
    let (s, n) = v.fold((0.0, 0usize), |(s, n), x| (s + x, n + 1));
    (n > 0).then(|| s / n as f64)
}

/// The per-candidate statistics. `family` is k, the number of candidates
/// the adjustment splits FAMILY_ALPHA over; `seed` is the bootstrap seed.
pub fn summarize(id: &str, rows: &[AlertRow], counts: Counts, family: usize, b: usize, seed: u64) -> Summary {
    let primary: Vec<&AlertRow> = rows.iter().filter(|r| r.measured.net[PRIMARY].is_some() && r.baseline[PRIMARY].is_some()).collect();
    let n = primary.len();
    let nf = n as f64;
    let paired = |h: usize| -> Vec<Paired<(NaiveDate, String)>> {
        rows.iter()
            .filter_map(|r| Some(Paired { cluster: (r.day, r.symbol.clone()), ret: r.measured.net[h]?, random_mean: r.baseline[h]? }))
            .collect()
    };
    let horizons: Vec<HorizonStat> = (0..N_H)
        .map(|h| {
            let p = paired(h);
            let boot = cluster_bootstrap(&p, b, seed);
            let m = mean(p.iter().map(|x| x.ret));
            let r = mean(p.iter().map(|x| x.random_mean));
            HorizonStat { label: HORIZON_LABELS[h].to_string(), n: p.len(), mean_pct: m, random_mean_pct: r, diff_pct: m.zip(r).map(|(a, b)| a - b), mean_ci: boot.map(|x| x.0), diff_ci: boot.map(|x| x.1) }
        })
        .collect();
    let p = paired(PRIMARY);
    let adjusted_level = 1.0 - FAMILY_ALPHA / family.max(1) as f64;
    let diff_ci_adjusted = diff_replicates(&p, b, seed).map(|v| percentile_interval(v, FAMILY_ALPHA / family.max(1) as f64));
    let by_date: Vec<Paired<NaiveDate>> = p.iter().map(|x| Paired { cluster: x.cluster.0, ret: x.ret, random_mean: x.random_mean }).collect();
    let diff_ci_by_date = cluster_bootstrap(&by_date, b, seed).map(|x| x.1);
    let mut per_date: BTreeMap<NaiveDate, usize> = BTreeMap::new();
    let mut per_sd: BTreeMap<(NaiveDate, &str), usize> = BTreeMap::new();
    for r in &primary {
        *per_date.entry(r.day).or_default() += 1;
        *per_sd.entry((r.day, r.symbol.as_str())).or_default() += 1;
    }
    let rate = |f: &dyn Fn(&AlertRow) -> bool| (n > 0).then(|| primary.iter().filter(|r| f(r)).count() as f64 / nf);
    let windows = [Window::Premarket, Window::Regular]
        .into_iter()
        .map(|w| {
            let sel: Vec<&&AlertRow> = primary.iter().filter(|r| r.window == w).collect();
            let m = mean(sel.iter().filter_map(|r| r.measured.net[PRIMARY]));
            let rm = mean(sel.iter().filter_map(|r| r.baseline[PRIMARY]));
            WindowStat { window: w, n: sel.len(), mean_pct: m, random_mean_pct: rm, diff_pct: m.zip(rm).map(|(a, b)| a - b) }
        })
        .collect();
    let h = &horizons[PRIMARY];
    Summary {
        id: id.to_string(),
        counts,
        n,
        symbol_days: per_sd.len(),
        dates: per_date.len(),
        mean_pct: h.mean_pct,
        median_pct: median(primary.iter().filter_map(|r| r.measured.net[PRIMARY]).collect()),
        positive_rate: rate(&|r| r.measured.net[PRIMARY].is_some_and(|v| v > 0.0)),
        random_mean_pct: h.random_mean_pct,
        diff_pct: h.diff_pct,
        mean_ci: h.mean_ci,
        diff_ci: h.diff_ci,
        diff_ci_adjusted,
        adjusted_level,
        diff_ci_by_date,
        windows,
        target_first_rate: rate(&|r| r.measured.outcome == Outcome::Target),
        stop_first_rate: rate(&|r| r.measured.outcome == Outcome::Stop),
        baseline_target_rate: mean(primary.iter().filter_map(|r| r.baseline_target_rate)),
        quote_cost_share: rate(&|r| r.measured.cost_model == CostModel::Quote),
        median_spread_pct: median(primary.iter().filter(|r| r.measured.cost_model == CostModel::Quote).map(|r| r.measured.spread_pct).collect()),
        max_date_share: per_date.values().max().map(|k| *k as f64 / nf),
        max_symbol_day_share: per_sd.values().max().map(|k| *k as f64 / nf),
        filling_fast_share: rate(&|r| r.filling_fast),
        horizons,
    }
}

/// Meets the preregistered floors (n >= 50 over >= 10 dates, a CI exists).
pub fn eligible(s: &Summary) -> bool {
    s.n >= MIN_ALERTS && s.dates >= MIN_DATES && s.diff_ci.is_some() && s.diff_ci_adjusted.is_some()
}

/// The preregistered winner: among eligible candidates, the highest 95%
/// lower bound of (candidate - random) at 30 min; ties by larger n, then
/// earlier preregistration order.
pub fn winner(summaries: &[Summary]) -> Option<&Summary> {
    let mut best: Option<&Summary> = None;
    for s in summaries.iter().filter(|s| eligible(s)) {
        let better = match best {
            None => true,
            Some(b) => {
                let (x, y) = (s.diff_ci.unwrap().lo, b.diff_ci.unwrap().lo);
                x > y || (x == y && s.n > b.n)
            }
        };
        if better {
            best = Some(s);
        }
    }
    best
}

/// The winner qualifies for the holdout iff its Bonferroni-adjusted lower
/// bound is above zero.
pub fn qualifies(s: &Summary) -> bool {
    eligible(s) && s.diff_ci_adjusted.is_some_and(|c| c.lo > 0.0)
}

/// Holdout: CONFIRMED iff the plain 95% CI of the difference is above zero.
pub fn confirms(s: &Summary) -> bool {
    s.diff_ci.is_some_and(|c| c.lo > 0.0)
}

#[cfg(test)]
#[path = "halt_tournament_tests.rs"]
mod halt_tournament_tests;
