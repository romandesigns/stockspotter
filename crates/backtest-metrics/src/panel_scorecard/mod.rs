//! Panel scorecard (2026-10-16): after each market close, score every
//! detection each dashboard panel produced that day -- what the price did
//! next after realistic costs, against matched random moments in the same
//! symbols -- and say plainly when the sample is too small to conclude
//! anything.
//!
//! **Input.** `panel-detection-v1` rows (one per thing a panel newly
//! showed, see `outputs/panel-evaluation-20261016/SCHEMA.md`), or for days
//! before that recorder existed, `live_pending_signals.jsonl` mapped to
//! panels (`input::parse_pending`). Edge rules this scorer can enforce are
//! re-applied (`input::apply_edge_rules`): ignition under $25 only, and a
//! 10-minute per-symbol cooldown for ignition / micropullback /
//! consolidation_breakout.
//!
//! **When a detection is known.** At `detectedAt`, plus one minute when
//! `priceSource` is `bar_close` (the bar's close is only known when the
//! bar ends; if `detectedAt` already is the bar end this costs one minute
//! of delay, never look-ahead).
//!
//! **Entry and costs.** The entry bar is the first 1-minute bar starting at
//! or after the known time (at most `MAX_ENTRY_DELAY_SECS` later, else the
//! detection is skipped as `noEntryBar`).
//! * `costModel: "quote"` -- the row carries a valid bid/ask no older than
//!   `QUOTE_MAX_AGE_SECS` at the known time: entry at the ask, exits valued
//!   at the bar close minus half the quoted spread. This is exactly
//!   `base_rates::observe_momentum`'s rule.
//! * `costModel: "proxy"` -- no usable quote: entry at the entry bar's open
//!   plus half of `strategy_config::round_trip_cost_pct()`, exits at the bar
//!   close minus the other half.
//!
//! **Outcome per detection.** `base_rates::follow` from the entry bar to the
//! regular close (+10% target / -5% stop on 1-minute highs/lows, stop first
//! within a minute, close valued at the bid proxy), plus net returns at
//! 5/15/30/60 minutes (close of the last bar completed by entry + h, valued
//! at the bid proxy; `null` when entry + h is after the close -- censored,
//! never truncated) and MFE/MAE over the first 30 minutes vs the entry.
//!
//! **Baseline.** For each detection, `K_RANDOM` random entry minutes drawn
//! (with replacement) from the same symbol's bars in the same session window
//! (premarket 04:00-09:30 ET or regular 09:30-close) on the same day, each
//! entered at its bar open with the detection's own cost (same spread %),
//! measured identically. The RNG seed is `RANDOM_SEED` mixed with a stable
//! hash of (panel, symbol, detectedAt), so a detection's baseline does not
//! change when other detections are added or removed. The per-horizon
//! baseline is the mean over the draws whose horizon is not censored.
//!
//! **Statistics.** Per panel and horizon over detections with both a value
//! and a baseline: n, mean, median, hit rate (net > 0), baseline mean,
//! difference, and percentile 95% CIs from `eval_stats::cluster_bootstrap`
//! (B = 10,000, clusters = symbol-days, the momentum experiment's "session").
//!
//! **Verdict** (primary horizon `30m`): "insufficient data" unless
//! n >= `MIN_DETECTIONS` over >= `MIN_SESSIONS` distinct market days; then
//! "beats random" if the CI of (panel - baseline) lies wholly above 0,
//! "worse than random" if wholly below, else "no difference shown". A single
//! day is therefore always "insufficient data"; `latest.json` carries the
//! rolling summary over the last `ROLLING_SESSIONS` scored days.

pub mod bars;
pub mod input;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use market_data::trading_session::{market_day, market_day_open, regular_session_close, regular_session_open};
use market_data::Bar;
use serde::{Deserialize, Serialize};

use crate::base_rates::{follow, Outcome, Path as FollowPath, MOMENTUM_QUOTE_MAX_AGE_SECS, STOP_PCT, TARGET_PCT};
use crate::eval_stats::{cluster_bootstrap, fnv1a, median, Ci, Paired, SplitMix64};

pub use bars::{load_day_bars, AlpacaBars, BarSource, BarsReport};
pub use input::{apply_edge_rules, parse_detections, parse_pending, Detection, Filtering, SourceKind, SourceReport};

pub const SCHEMA: &str = "panel-scorecard-v1";
/// Fixed horizons, minutes after entry.
pub const HORIZONS_MIN: [i64; 4] = [5, 15, 30, 60];
/// Labels of the per-row return arrays: the four horizons, then held to the
/// regular close (`follow`'s close return).
pub const HORIZON_LABELS: [&str; 5] = ["5m", "15m", "30m", "60m", "close"];
const N_H: usize = HORIZON_LABELS.len();
/// Index of the horizon the verdict is read from.
pub const PRIMARY: usize = 2;
/// Matched random entries per detection (as in the momentum experiment).
pub const K_RANDOM: usize = 20;
pub const RANDOM_SEED: u64 = 20261016;
pub const BOOTSTRAP_B: usize = 10_000;
pub const BOOTSTRAP_SEED: u64 = 0x5EED_2026_1016;
/// A verdict needs at least this many detections with a primary value...
pub const MIN_DETECTIONS: usize = 30;
/// ...spread over at least this many market days. One day is one market
/// regime; three is the least that can show the result is not one day's
/// tape. Both are deliberately low floors, not proof of anything.
pub const MIN_SESSIONS: usize = 3;
/// Scored days the rolling summary in latest.json covers.
pub const ROLLING_SESSIONS: usize = 20;
/// API budget: at most this many symbols get bars per day.
pub const MAX_SYMBOLS_PER_DAY: usize = 200;
pub const SAMPLING_SEED: u64 = 20261016;
pub const BAR_CLOSE_LAG_SECS: i64 = 60;
pub const QUOTE_MAX_AGE_SECS: i64 = MOMENTUM_QUOTE_MAX_AGE_SECS;
pub const MAX_ENTRY_DELAY_SECS: i64 = 300;
/// SCHEMA cooldown for ignition / micropullback / consolidation_breakout.
pub const COOLDOWN_SECS: i64 = 600;
/// The ignition panel shows price <= $25 (client `userAttentionCap`).
pub const IGNITION_MAX_PRICE: f64 = 25.0;
const MFE_WINDOW_SECS: i64 = 30 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Panel {
    Momentum,
    GapAndGo,
    Ignition,
    Micropullback,
    ConsolidationBreakout,
    TopGainers,
    HighlyTrading,
    HaltSpeed,
    HaltActual,
    Catalyst,
}

impl Panel {
    pub const ALL: [Panel; 10] = [
        Panel::Momentum,
        Panel::GapAndGo,
        Panel::Ignition,
        Panel::Micropullback,
        Panel::ConsolidationBreakout,
        Panel::TopGainers,
        Panel::HighlyTrading,
        Panel::HaltSpeed,
        Panel::HaltActual,
        Panel::Catalyst,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Panel::Momentum => "momentum",
            Panel::GapAndGo => "gap_and_go",
            Panel::Ignition => "ignition",
            Panel::Micropullback => "micropullback",
            Panel::ConsolidationBreakout => "consolidation_breakout",
            Panel::TopGainers => "top_gainers",
            Panel::HighlyTrading => "highly_trading",
            Panel::HaltSpeed => "halt_speed",
            Panel::HaltActual => "halt_actual",
            Panel::Catalyst => "catalyst",
        }
    }

    pub fn from_wire(s: &str) -> Option<Panel> {
        Panel::ALL.into_iter().find(|p| p.as_str() == s.trim())
    }

    pub fn has_cooldown(self) -> bool {
        matches!(self, Panel::Ignition | Panel::Micropullback | Panel::ConsolidationBreakout)
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Skip {
    NotSampled,
    NoBars,
    NoEntryBar,
    AfterClose,
    BadPrice,
}

impl Skip {
    pub fn as_str(self) -> &'static str {
        match self {
            Skip::NotSampled => "notSampled",
            Skip::NoBars => "noBars",
            Skip::NoEntryBar => "noEntryBar",
            Skip::AfterClose => "afterClose",
            Skip::BadPrice => "badPrice",
        }
    }
}

/// One scored detection. Return arrays are aligned with `HORIZON_LABELS`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub panel: Panel,
    pub symbol: String,
    pub day: NaiveDate,
    pub detected_at: DateTime<Utc>,
    pub known_at: DateTime<Utc>,
    /// Start of the first bar the position is exposed to.
    pub entry_bar_at: DateTime<Utc>,
    pub window: Window,
    pub price_source: String,
    pub cost_model: CostModel,
    /// Round-trip cost charged, % (quoted spread, or the proxy).
    pub spread_pct: f64,
    pub entry: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    pub outcome: Outcome,
    pub net_pct: [Option<f64>; N_H],
    pub baseline_net_pct: [Option<f64>; N_H],
    pub baseline_target_rate: Option<f64>,
    pub mfe30_pct: Option<f64>,
    pub mae30_pct: Option<f64>,
}

fn r4(x: f64) -> f64 {
    (x * 1e4).round() / 1e4
}

struct Measured {
    path: FollowPath,
    net: [Option<f64>; N_H],
    mfe30: Option<f64>,
    mae30: Option<f64>,
}

/// Buying at `entry` at `entry_at` (unix secs), exposed to `after` (sorted,
/// all before `close_ts`).
fn measure(entry: f64, spread_pct: f64, entry_at: i64, after: &[&Bar], close_ts: i64) -> Option<Measured> {
    let path = follow(entry, spread_pct, after)?;
    let exit_factor = 1.0 - spread_pct / 200.0;
    let ended_by = |deadline: i64| after.iter().take_while(move |b| b.timestamp.timestamp() + 60 <= deadline);
    let mut net = [None; N_H];
    for (i, h) in HORIZONS_MIN.iter().enumerate() {
        let deadline = entry_at + h * 60;
        if deadline > close_ts {
            continue;
        }
        net[i] = ended_by(deadline).last().map(|b| (b.close * exit_factor - entry) / entry * 100.0);
    }
    net[N_H - 1] = Some(path.close_return_net_pct);
    let window: Vec<&&Bar> = ended_by(entry_at + MFE_WINDOW_SECS).collect();
    let mfe30 = window.iter().map(|b| b.high).reduce(f64::max).map(|h| ((h - entry) / entry * 100.0).max(0.0));
    let mae30 = window.iter().map(|b| b.low).reduce(f64::min).map(|l| ((l - entry) / entry * 100.0).min(0.0));
    Some(Measured { path, net, mfe30, mae30 })
}

/// Session boundaries of one market day, unix seconds.
#[derive(Debug, Clone, Copy)]
pub struct DayClock {
    pub day_open: i64,
    pub regular_open: i64,
    pub close: i64,
}

impl DayClock {
    pub fn of(day: NaiveDate) -> Option<DayClock> {
        Some(DayClock {
            day_open: market_day_open(day).timestamp(),
            regular_open: regular_session_open(day)?.timestamp(),
            close: regular_session_close(day)?.timestamp(),
        })
    }
}

fn quote_spread(d: &Detection, known: i64) -> Option<(f64, f64)> {
    let (bid, ask) = (d.bid?, d.ask?);
    if !(bid.is_finite() && ask.is_finite() && bid > 0.0 && ask >= bid) {
        return None;
    }
    let qa = d.quote_at.unwrap_or(d.detected_at).timestamp();
    if qa > known || known - qa > QUOTE_MAX_AGE_SECS {
        return None;
    }
    Some((ask, (ask - bid) / ((ask + bid) / 2.0) * 100.0))
}

/// Scores one detection against its symbol's bars (sorted, this day only).
pub fn score_detection(d: &Detection, bars: &[Bar], clock: DayClock, cost_pct: f64) -> Result<Row, Skip> {
    let t = d.detected_at.timestamp();
    let window = if t < clock.regular_open {
        Window::Premarket
    } else if t < clock.close {
        Window::Regular
    } else {
        return Err(Skip::AfterClose);
    };
    if bars.is_empty() {
        return Err(Skip::NoBars);
    }
    let known = t + if d.price_source == "bar_close" { BAR_CLOSE_LAG_SECS } else { 0 };
    let ts = |b: &Bar| b.timestamp.timestamp();
    let idx = bars.partition_point(|b| ts(b) < known);
    let entry_bar = bars.get(idx).filter(|b| ts(b) < clock.close && ts(b) - known <= MAX_ENTRY_DELAY_SECS).ok_or(Skip::NoEntryBar)?;

    let (cost_model, entry, spread_pct, entry_at) = match quote_spread(d, known) {
        Some((ask, spread)) => (CostModel::Quote, ask, spread, known),
        None => (CostModel::Proxy, entry_bar.open * (1.0 + cost_pct / 200.0), cost_pct, ts(entry_bar)),
    };
    if !(entry.is_finite() && entry > 0.0 && spread_pct.is_finite()) {
        return Err(Skip::BadPrice);
    }
    let after: Vec<&Bar> = bars[idx..].iter().take_while(|b| ts(b) < clock.close).collect();
    let m = measure(entry, spread_pct, entry_at, &after, clock.close).ok_or(Skip::NoEntryBar)?;

    // Matched random entries: same symbol, same day, same session window.
    let (w0, w1) = match window {
        Window::Premarket => (clock.day_open, clock.regular_open),
        Window::Regular => (clock.regular_open, clock.close),
    };
    let pool: Vec<usize> = (0..bars.len()).filter(|&j| (w0..w1).contains(&ts(&bars[j])) && bars[j].open.is_finite() && bars[j].open > 0.0).collect();
    let key = format!("{}|{}|{}", d.panel.as_str(), d.symbol, d.detected_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    let mut rng = SplitMix64::new(RANDOM_SEED ^ fnv1a(key.as_bytes()));
    let mut sums = [(0.0f64, 0usize); N_H];
    let (mut targets, mut draws) = (0usize, 0usize);
    if !pool.is_empty() {
        for _ in 0..K_RANDOM {
            let j = pool[rng.below(pool.len())];
            let rb = &bars[j];
            let r_after: Vec<&Bar> = bars[j..].iter().take_while(|b| ts(b) < clock.close).collect();
            let Some(rm) = measure(rb.open * (1.0 + spread_pct / 200.0), spread_pct, ts(rb), &r_after, clock.close) else { continue };
            draws += 1;
            targets += usize::from(rm.path.outcome == Outcome::Target);
            for (s, v) in sums.iter_mut().zip(rm.net) {
                if let Some(v) = v {
                    s.0 += v;
                    s.1 += 1;
                }
            }
        }
    }
    let baseline_net_pct = sums.map(|(s, n)| (n > 0).then(|| r4(s / n as f64)));
    Ok(Row {
        panel: d.panel,
        symbol: d.symbol.clone(),
        day: market_day(d.detected_at),
        detected_at: d.detected_at,
        known_at: DateTime::<Utc>::from_timestamp(known, 0).unwrap_or(d.detected_at),
        entry_bar_at: entry_bar.timestamp,
        window,
        price_source: d.price_source.clone(),
        cost_model,
        spread_pct: r4(spread_pct),
        entry: (entry * 1e6).round() / 1e6,
        score: d.score.map(r4),
        outcome: m.path.outcome,
        net_pct: m.net.map(|v| v.map(r4)),
        baseline_net_pct,
        baseline_target_rate: (draws > 0).then(|| r4(targets as f64 / draws as f64)),
        mfe30_pct: m.mfe30.map(r4),
        mae30_pct: m.mae30.map(r4),
    })
}

// ---- sampling ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelSampling {
    pub panel: Panel,
    pub detections_available: usize,
    pub detections_kept: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sampling {
    /// True when the symbol cap bit and some detections were not scored.
    pub sampled: bool,
    pub max_symbols: usize,
    pub symbols_available: usize,
    pub symbols_scored: usize,
    pub detections_available: usize,
    pub detections_kept: usize,
    pub method: String,
    pub seed: u64,
    pub per_panel: Vec<PanelSampling>,
}

/// What to score on a day: the detections kept and the symbols to fetch.
#[derive(Debug, Clone)]
pub struct Plan {
    pub day: NaiveDate,
    pub detections: Vec<Detection>,
    pub symbols: Vec<String>,
    pub filtering: Filtering,
    pub sampling: Sampling,
}

/// Applies the edge rules, then the symbol budget. Over budget, symbols are
/// chosen round-robin across panels (so a small panel is not crowded out by
/// a big one), each panel's symbols in a fixed pseudo-random order
/// (FNV-1a of seed, day, symbol). Every detection of a chosen symbol is
/// kept, whichever panel it came from.
pub fn plan(day: NaiveDate, detections: Vec<Detection>, max_symbols: usize) -> Plan {
    let (dets, filtering) = apply_edge_rules(detections);
    let order = |s: &str| fnv1a(format!("{SAMPLING_SEED}|{day}|{s}").as_bytes());
    let mut by_panel: BTreeMap<Panel, Vec<String>> = BTreeMap::new();
    for d in &dets {
        by_panel.entry(d.panel).or_default().push(d.symbol.clone());
    }
    for v in by_panel.values_mut() {
        v.sort();
        v.dedup();
        v.sort_by_key(|s| (order(s), s.clone()));
    }
    let all: BTreeSet<&String> = by_panel.values().flatten().collect();
    let symbols_available = all.len();
    let chosen: BTreeSet<String> = if symbols_available <= max_symbols {
        all.into_iter().cloned().collect()
    } else {
        let mut chosen = BTreeSet::new();
        let mut cursors: Vec<(&Vec<String>, usize)> = by_panel.values().map(|v| (v, 0)).collect();
        'fill: loop {
            let mut progressed = false;
            for (list, i) in cursors.iter_mut() {
                while *i < list.len() && chosen.contains(&list[*i]) {
                    *i += 1;
                }
                if *i < list.len() {
                    chosen.insert(list[*i].clone());
                    *i += 1;
                    progressed = true;
                    if chosen.len() >= max_symbols {
                        break 'fill;
                    }
                }
            }
            if !progressed {
                break;
            }
        }
        chosen
    };
    let detections_available = dets.len();
    let per_panel = Panel::ALL
        .iter()
        .filter_map(|&p| {
            let avail = dets.iter().filter(|d| d.panel == p).count();
            (avail > 0).then(|| PanelSampling { panel: p, detections_available: avail, detections_kept: dets.iter().filter(|d| d.panel == p && chosen.contains(&d.symbol)).count() })
        })
        .collect();
    let kept: Vec<Detection> = dets.into_iter().filter(|d| chosen.contains(&d.symbol)).collect();
    Plan {
        day,
        sampling: Sampling {
            sampled: chosen.len() < symbols_available,
            max_symbols,
            symbols_available,
            symbols_scored: chosen.len(),
            detections_available,
            detections_kept: kept.len(),
            method: "symbol cap; round-robin across panels, each panel's symbols in FNV-1a(seed|day|symbol) order; all detections of a chosen symbol kept".into(),
            seed: SAMPLING_SEED,
            per_panel,
        },
        detections: kept,
        symbols: chosen.into_iter().collect(),
        filtering,
    }
}

// ---- aggregation --------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    #[serde(rename = "insufficient data")]
    InsufficientData,
    #[serde(rename = "beats random")]
    BeatsRandom,
    #[serde(rename = "worse than random")]
    WorseThanRandom,
    #[serde(rename = "no difference shown")]
    NoDifferenceShown,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::InsufficientData => "insufficient data",
            Verdict::BeatsRandom => "beats random",
            Verdict::WorseThanRandom => "worse than random",
            Verdict::NoDifferenceShown => "no difference shown",
        }
    }
}

/// The verdict rule (see module docs).
pub fn verdict(n: usize, sessions: usize, diff_ci: Option<Ci>) -> (Verdict, String) {
    if n < MIN_DETECTIONS || sessions < MIN_SESSIONS {
        return (Verdict::InsufficientData, format!("n={n} over {sessions} market day(s); a verdict needs n>={MIN_DETECTIONS} over >={MIN_SESSIONS} days"));
    }
    match diff_ci {
        None => (Verdict::InsufficientData, "no confidence interval (fewer than two symbol-days)".into()),
        Some(ci) if ci.lo > 0.0 => (Verdict::BeatsRandom, format!("95% CI of panel minus random [{:.3}, {:.3}]% lies above 0", ci.lo, ci.hi)),
        Some(ci) if ci.hi < 0.0 => (Verdict::WorseThanRandom, format!("95% CI of panel minus random [{:.3}, {:.3}]% lies below 0", ci.lo, ci.hi)),
        Some(ci) => (Verdict::NoDifferenceShown, format!("95% CI of panel minus random [{:.3}, {:.3}]% includes 0", ci.lo, ci.hi)),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HorizonStats {
    pub horizon: String,
    /// Detections with both a value and a baseline at this horizon.
    pub n: usize,
    pub mean_net_pct: Option<f64>,
    pub median_net_pct: Option<f64>,
    /// Share with net return > 0.
    pub hit_rate: Option<f64>,
    pub baseline_mean_net_pct: Option<f64>,
    pub diff_pct: Option<f64>,
    pub mean_ci: Option<Ci>,
    pub diff_ci: Option<Ci>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelStats {
    pub panel: Panel,
    /// Every detection considered (scored + skipped).
    pub detections: usize,
    pub scored: usize,
    /// Over detections counted at the primary horizon.
    pub symbols: usize,
    pub sessions: usize,
    pub symbol_days: usize,
    pub quote_cost_rows: usize,
    pub proxy_cost_rows: usize,
    pub primary_horizon: String,
    pub horizons: Vec<HorizonStats>,
    pub target_first_rate: Option<f64>,
    pub stop_first_rate: Option<f64>,
    pub baseline_target_first_rate: Option<f64>,
    pub median_mfe30_pct: Option<f64>,
    pub median_mae30_pct: Option<f64>,
    pub verdict: Verdict,
    pub verdict_reason: String,
    pub skipped: BTreeMap<String, usize>,
}

fn mean(v: &[f64]) -> Option<f64> {
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

fn round_ci(c: Ci) -> Ci {
    Ci { lo: r4(c.lo), hi: r4(c.hi) }
}

pub fn panel_stats(panel: Panel, rows: &[&Row], skipped: BTreeMap<String, usize>, b: usize) -> PanelStats {
    let horizons: Vec<HorizonStats> = (0..N_H)
        .map(|i| {
            let pairs: Vec<Paired<(NaiveDate, String)>> = rows
                .iter()
                .filter_map(|r| Some(Paired { cluster: (r.day, r.symbol.clone()), ret: r.net_pct[i]?, random_mean: r.baseline_net_pct[i]? }))
                .collect();
            let rets: Vec<f64> = pairs.iter().map(|p| p.ret).collect();
            let base: Vec<f64> = pairs.iter().map(|p| p.random_mean).collect();
            let seed = BOOTSTRAP_SEED ^ fnv1a(format!("{}|{}", panel.as_str(), HORIZON_LABELS[i]).as_bytes());
            let boot = cluster_bootstrap(&pairs, b, seed);
            let (m, bm) = (mean(&rets), mean(&base));
            HorizonStats {
                horizon: HORIZON_LABELS[i].into(),
                n: pairs.len(),
                mean_net_pct: m.map(r4),
                median_net_pct: median(rets.clone()).map(r4),
                hit_rate: (!rets.is_empty()).then(|| r4(rets.iter().filter(|x| **x > 0.0).count() as f64 / rets.len() as f64)),
                baseline_mean_net_pct: bm.map(r4),
                diff_pct: m.zip(bm).map(|(a, b)| r4(a - b)),
                mean_ci: boot.map(|x| round_ci(x.0)),
                diff_ci: boot.map(|x| round_ci(x.1)),
            }
        })
        .collect();
    let primary_rows: Vec<&&Row> = rows.iter().filter(|r| r.net_pct[PRIMARY].is_some() && r.baseline_net_pct[PRIMARY].is_some()).collect();
    let symbols: BTreeSet<&str> = primary_rows.iter().map(|r| r.symbol.as_str()).collect();
    let sessions: BTreeSet<NaiveDate> = primary_rows.iter().map(|r| r.day).collect();
    let symbol_days: BTreeSet<(NaiveDate, &str)> = primary_rows.iter().map(|r| (r.day, r.symbol.as_str())).collect();
    let n = rows.len();
    let rate = |o: Outcome| (n > 0).then(|| r4(rows.iter().filter(|r| r.outcome == o).count() as f64 / n as f64));
    let btr: Vec<f64> = rows.iter().filter_map(|r| r.baseline_target_rate).collect();
    let (v, reason) = verdict(horizons[PRIMARY].n, sessions.len(), horizons[PRIMARY].diff_ci);
    PanelStats {
        panel,
        detections: n + skipped.values().sum::<usize>(),
        scored: n,
        symbols: symbols.len(),
        sessions: sessions.len(),
        symbol_days: symbol_days.len(),
        quote_cost_rows: rows.iter().filter(|r| r.cost_model == CostModel::Quote).count(),
        proxy_cost_rows: rows.iter().filter(|r| r.cost_model == CostModel::Proxy).count(),
        primary_horizon: HORIZON_LABELS[PRIMARY].into(),
        horizons,
        target_first_rate: rate(Outcome::Target),
        stop_first_rate: rate(Outcome::Stop),
        baseline_target_first_rate: mean(&btr).map(r4),
        median_mfe30_pct: median(rows.iter().filter_map(|r| r.mfe30_pct).collect()).map(r4),
        median_mae30_pct: median(rows.iter().filter_map(|r| r.mae30_pct).collect()).map(r4),
        verdict: v,
        verdict_reason: reason,
        skipped,
    }
}

/// Stats for every panel that has rows or skips, in `Panel::ALL` order.
pub fn all_panel_stats(rows: &[Row], skipped: &BTreeMap<Panel, BTreeMap<String, usize>>, b: usize) -> Vec<PanelStats> {
    Panel::ALL
        .iter()
        .filter_map(|&p| {
            let rs: Vec<&Row> = rows.iter().filter(|r| r.panel == p).collect();
            let sk = skipped.get(&p).cloned().unwrap_or_default();
            (!rs.is_empty() || !sk.is_empty()).then(|| panel_stats(p, &rs, sk, b))
        })
        .collect()
}

// ---- reports ----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Method {
    pub horizons: Vec<String>,
    pub primary_horizon: String,
    pub target_pct: f64,
    pub stop_pct: f64,
    pub k_random: usize,
    pub random_seed: u64,
    pub bootstrap_b: usize,
    pub bootstrap_seed: u64,
    pub ci_level: f64,
    pub cluster_unit: String,
    pub min_detections: usize,
    pub min_sessions: usize,
    pub rolling_sessions: usize,
    pub round_trip_cost_proxy_pct: f64,
    pub quote_max_age_secs: i64,
    pub bar_close_lag_secs: i64,
    pub max_entry_delay_secs: i64,
    pub cooldown_secs: i64,
    pub notes: Vec<String>,
}

pub fn method(cost_pct: f64) -> Method {
    Method {
        horizons: HORIZON_LABELS.iter().map(|s| s.to_string()).collect(),
        primary_horizon: HORIZON_LABELS[PRIMARY].into(),
        target_pct: TARGET_PCT,
        stop_pct: STOP_PCT,
        k_random: K_RANDOM,
        random_seed: RANDOM_SEED,
        bootstrap_b: BOOTSTRAP_B,
        bootstrap_seed: BOOTSTRAP_SEED,
        ci_level: 0.95,
        cluster_unit: "symbol-day".into(),
        min_detections: MIN_DETECTIONS,
        min_sessions: MIN_SESSIONS,
        rolling_sessions: ROLLING_SESSIONS,
        round_trip_cost_proxy_pct: cost_pct,
        quote_max_age_secs: QUOTE_MAX_AGE_SECS,
        bar_close_lag_secs: BAR_CLOSE_LAG_SECS,
        max_entry_delay_secs: MAX_ENTRY_DELAY_SECS,
        cooldown_secs: COOLDOWN_SECS,
        notes: vec![
            "Returns are % after costs: quote rows pay the quoted spread (entry at ask, exits at close minus half the spread); proxy rows pay the round-trip proxy.".into(),
            "Baseline: K random entry minutes per detection, same symbol, same day, same session window, same cost; the panel is compared with these, not with zero.".into(),
            "Horizon values are null when entry + horizon is after the regular close (censored, not truncated); 'close' is held to the regular close.".into(),
            "Bars are 1-minute: a minute that touches both target and stop counts as the stop.".into(),
            "One day is one market regime: a single day's verdict is always 'insufficient data'; read the rolling summary in latest.json.".into(),
        ],
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DayReport {
    pub schema: String,
    pub generated_at: DateTime<Utc>,
    pub day: NaiveDate,
    pub source: SourceReport,
    pub method: Method,
    pub filtering: Filtering,
    pub sampling: Sampling,
    pub bars: BarsReport,
    pub panels: Vec<PanelStats>,
    pub rows: Vec<Row>,
}

/// Scores a plan against fetched bars (keyed by symbol; missing symbols are
/// `noBars`).
pub fn build_day_report(plan: &Plan, source: SourceReport, bars: &BTreeMap<String, Vec<Bar>>, bars_report: BarsReport, cost_pct: f64, generated_at: DateTime<Utc>, b: usize) -> DayReport {
    let mut skipped: BTreeMap<Panel, BTreeMap<String, usize>> = BTreeMap::new();
    for ps in &plan.sampling.per_panel {
        let dropped = ps.detections_available - ps.detections_kept;
        if dropped > 0 {
            skipped.entry(ps.panel).or_default().insert(Skip::NotSampled.as_str().into(), dropped);
        }
    }
    let mut rows = Vec::new();
    match DayClock::of(plan.day) {
        None => {
            for d in &plan.detections {
                *skipped.entry(d.panel).or_default().entry(Skip::AfterClose.as_str().into()).or_default() += 1;
            }
        }
        Some(clock) => {
            let mut sorted: BTreeMap<&str, Vec<Bar>> = BTreeMap::new();
            for (s, v) in bars {
                let mut v: Vec<Bar> = v.iter().filter(|b| market_day(b.timestamp) == plan.day).cloned().collect();
                v.sort_by_key(|b| b.timestamp);
                sorted.insert(s.as_str(), v);
            }
            for d in &plan.detections {
                let sym_bars = sorted.get(d.symbol.as_str()).map(|v| v.as_slice()).unwrap_or(&[]);
                match score_detection(d, sym_bars, clock, cost_pct) {
                    Ok(r) => rows.push(r),
                    Err(e) => *skipped.entry(d.panel).or_default().entry(e.as_str().into()).or_default() += 1,
                }
            }
        }
    }
    DayReport {
        schema: SCHEMA.into(),
        generated_at,
        day: plan.day,
        source,
        method: method(cost_pct),
        filtering: plan.filtering.clone(),
        sampling: plan.sampling.clone(),
        bars: bars_report,
        panels: all_panel_stats(&rows, &skipped, b),
        rows,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rolling {
    pub max_sessions: usize,
    pub days: Vec<NaiveDate>,
    pub detections_scored: usize,
    pub panels: Vec<PanelStats>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatestDay {
    pub day: NaiveDate,
    pub source: SourceReport,
    pub filtering: Filtering,
    pub sampling: Sampling,
    pub bars: BarsReport,
    pub panels: Vec<PanelStats>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Latest {
    pub schema: String,
    pub generated_at: DateTime<Utc>,
    pub method: Method,
    pub rolling: Rolling,
    pub latest: LatestDay,
}

/// Rolling summary over the last `ROLLING_SESSIONS` scored days up to and
/// including `current.day` (`history` may include `current`'s own day; the
/// in-memory `current` wins). Stats are recomputed from the day files' rows,
/// skip counts summed from their panel stats.
pub fn build_latest(current: &DayReport, history: &[DayReport], generated_at: DateTime<Utc>, b: usize) -> Latest {
    let mut by_day: BTreeMap<NaiveDate, &DayReport> = history.iter().filter(|r| r.day <= current.day).map(|r| (r.day, r)).collect();
    by_day.insert(current.day, current);
    let days: Vec<NaiveDate> = by_day.keys().rev().take(ROLLING_SESSIONS).rev().copied().collect();
    let mut rows = Vec::new();
    let mut skipped: BTreeMap<Panel, BTreeMap<String, usize>> = BTreeMap::new();
    for d in &days {
        let r = by_day[d];
        rows.extend(r.rows.iter().cloned());
        for p in &r.panels {
            for (k, v) in &p.skipped {
                *skipped.entry(p.panel).or_default().entry(k.clone()).or_default() += v;
            }
        }
    }
    Latest {
        schema: SCHEMA.into(),
        generated_at,
        method: current.method.clone(),
        rolling: Rolling { max_sessions: ROLLING_SESSIONS, days, detections_scored: rows.len(), panels: all_panel_stats(&rows, &skipped, b) },
        latest: LatestDay {
            day: current.day,
            source: current.source.clone(),
            filtering: current.filtering.clone(),
            sampling: current.sampling.clone(),
            bars: current.bars.clone(),
            panels: current.panels.clone(),
        },
    }
}

/// Every `YYYY-MM-DD.json` day report in `dir` that parses with this schema.
/// Anything else (latest.json, other schemas, unreadable files) is ignored.
pub fn load_day_reports(dir: &Path) -> Result<Vec<DayReport>> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return Ok(out) };
    for e in entries {
        let p = e.with_context(|| format!("listing {}", dir.display()))?.path();
        let Some(stem) = p.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".json")) else { continue };
        if NaiveDate::parse_from_str(stem, "%Y-%m-%d").is_err() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        match serde_json::from_str::<DayReport>(&text) {
            Ok(r) if r.schema == SCHEMA => out.push(r),
            _ => tracing::warn!("ignoring {} (not a {SCHEMA} day report)", p.display()),
        }
    }
    out.sort_by_key(|r| r.day);
    Ok(out)
}

#[cfg(test)]
mod tests;
