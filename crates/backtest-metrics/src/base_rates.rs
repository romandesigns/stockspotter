//! Historical base rates for the shortlist's "Track record" (2026-10-15).
//!
//! The question a trader looking at a shortlisted candidate wants answered
//! is "how often has a setup like this actually followed through, after
//! paying the spread?" -- not a score, a frequency with its sample size.
//! This module answers it the same way for every session, so the number on
//! the card can be traced:
//!
//! * **Decision point**: 09:35 ET, five minutes into the regular session. A
//!   fixed time keeps observations comparable and free of look-ahead; the
//!   live shortlist ranks all day, so the card says the rate was measured at
//!   09:35 entries.
//! * **Eligible**: the stock traded between 09:30 and 09:35, priced within
//!   the scanner's range (fast_funnel SCAN_MIN_PRICE..=SCAN_MAX_PRICE),
//!   up on the day (last price above prior close), with a volume baseline
//!   and a two-sided quote before 09:35. Everything else is counted in
//!   `Skipped`, never silently dropped.
//! * **Profile**: day-change bucket x relative-volume bucket, with the same
//!   definitions the live funnel uses (last vs prior close; volume since
//!   04:00 ET over the 20-day average daily volume).
//! * **Entry**: the ask of the last two-sided quote before 09:35, so the
//!   spread is paid on the way in. **Exit** at the close is approximated at
//!   the close minus half the entry spread (a bid), so it is paid again.
//! * **Outcome**: +10% from entry reached before the close (on 1-minute
//!   highs), or -5% reached first (on lows). A minute that touches both
//!   counts as the stop: bar data cannot order them, and the conservative
//!   reading is the honest one.
//!
//! A profile with fewer than `MIN_SAMPLE` observations reports its count
//! and no rates. Sessions in a dataset chosen *because* they moved inflate
//! every rate; the generating run records that in `source.note`.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use market_data::trading_session::{market_day, market_day_open};
use market_data::{Bar, Quote};
use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = "shortlist-base-rates-v1";
pub const TARGET_PCT: f64 = 10.0;
pub const STOP_PCT: f64 = 5.0;
pub const MIN_SAMPLE: usize = 30;
pub const DECISION_TIME_ET: &str = "09:35";
const REGULAR_OPEN_OFFSET_SECS: i64 = 5 * 3600 + 30 * 60;
const DECISION_AFTER_OPEN_SECS: i64 = 5 * 60;
const REGULAR_SESSION_SECS: i64 = 6 * 3600 + 30 * 60;

/// Day-change buckets, % (lower bound inclusive).
pub const GAP_BUCKETS: [(f64, &str); 4] = [(0.0, "0-10%"), (10.0, "10-25%"), (25.0, "25-50%"), (50.0, "50%+")];
/// Relative-volume buckets, x (lower bound inclusive).
pub const RVOL_BUCKETS: [(f64, &str); 4] = [(0.0, "<1x"), (1.0, "1-3x"), (3.0, "3-10x"), (10.0, "10x+")];

fn bucket(value: f64, buckets: &[(f64, &'static str)]) -> Option<&'static str> {
    if !value.is_finite() || value < buckets[0].0 {
        return None;
    }
    buckets.iter().rev().find(|(lo, _)| value >= *lo).map(|(_, name)| *name)
}

/// The profile a (day change %, relative volume) pair falls in, or `None`
/// for a stock that is not up on the day.
pub fn profile_of(gap_pct: f64, relative_volume: f64) -> Option<(&'static str, &'static str)> {
    if gap_pct <= 0.0 {
        return None;
    }
    Some((bucket(gap_pct, &GAP_BUCKETS)?, bucket(relative_volume, &RVOL_BUCKETS)?))
}

/// One historical session's inputs: the subset of a cached
/// `replay_engine::ReplayData` file this needs (trades are not read).
#[derive(Debug, Clone, Deserialize)]
pub struct SessionInput {
    pub symbol: String,
    pub prior_close: f64,
    pub avg_daily_volume: u64,
    pub bars: Vec<Bar>,
    pub quotes: Vec<Quote>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    /// +TARGET_PCT reached before the close and before the stop.
    Target,
    /// -STOP_PCT reached first (including a minute that touched both).
    Stop,
    /// Neither by the close.
    Neither,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Observation {
    pub symbol: String,
    pub day: NaiveDate,
    pub gap_pct: f64,
    pub relative_volume: f64,
    pub entry: f64,
    pub entry_spread_pct: f64,
    pub outcome: Outcome,
    /// Close (approximated at the bid) vs entry, %, after both half-spreads.
    pub close_return_net_pct: f64,
    /// Worst low before the outcome (or the close), vs entry, % (<= 0).
    pub max_adverse_pct: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    NoTradeByDecision,
    OutsidePriceRange,
    NotUp,
    NoBaseline,
    NoQuote,
    NoBarsAfterDecision,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Skipped {
    pub no_trade_by_decision: usize,
    pub outside_price_range: usize,
    pub not_up: usize,
    pub no_baseline: usize,
    pub no_quote: usize,
    pub no_bars_after_decision: usize,
}

impl Skipped {
    pub fn count(&mut self, why: Skip) {
        match why {
            Skip::NoTradeByDecision => self.no_trade_by_decision += 1,
            Skip::OutsidePriceRange => self.outside_price_range += 1,
            Skip::NotUp => self.not_up += 1,
            Skip::NoBaseline => self.no_baseline += 1,
            Skip::NoQuote => self.no_quote += 1,
            Skip::NoBarsAfterDecision => self.no_bars_after_decision += 1,
        }
    }
}

/// Measures one session at 09:35 ET, or says why it cannot be.
pub fn observe(s: &SessionInput) -> Result<Observation, Skip> {
    let mut bars: Vec<&Bar> = s.bars.iter().collect();
    bars.sort_by_key(|b| b.timestamp);
    let first = bars.first().ok_or(Skip::NoTradeByDecision)?;
    let day = market_day(first.timestamp);
    let open = market_day_open(day).timestamp() + REGULAR_OPEN_OFFSET_SECS;
    let decision = open + DECISION_AFTER_OPEN_SECS;
    let close = open + REGULAR_SESSION_SECS;
    let at = |b: &&Bar| b.timestamp.timestamp();

    // A 1-minute bar stamped t covers [t, t+60): bars before `decision`
    // are fully known at 09:35.
    let known: Vec<&Bar> = bars.iter().copied().filter(|b| at(b) < decision && market_day(b.timestamp) == day).collect();
    let last_regular = known.iter().rev().find(|b| at(b) >= open).ok_or(Skip::NoTradeByDecision)?;
    let price = last_regular.close;
    if !(fast_funnel::SCAN_MIN_PRICE..=fast_funnel::SCAN_MAX_PRICE).contains(&price) {
        return Err(Skip::OutsidePriceRange);
    }
    if s.prior_close.is_nan() || s.prior_close <= 0.0 {
        return Err(Skip::NoBaseline);
    }
    let gap_pct = (price - s.prior_close) / s.prior_close * 100.0;
    if gap_pct <= 0.0 {
        return Err(Skip::NotUp);
    }
    if s.avg_daily_volume == 0 {
        return Err(Skip::NoBaseline);
    }
    let relative_volume = known.iter().map(|b| b.volume).sum::<u64>() as f64 / s.avg_daily_volume as f64;

    let decision_at = DateTime::<Utc>::from_timestamp(decision, 0).ok_or(Skip::NoQuote)?;
    let quote = s
        .quotes
        .iter()
        .filter(|q| q.timestamp < decision_at && q.bid_price > 0.0 && q.ask_price >= q.bid_price && q.ask_price.is_finite())
        .max_by_key(|q| q.timestamp)
        .ok_or(Skip::NoQuote)?;
    let entry = quote.ask_price;
    let entry_spread_pct = (quote.ask_price - quote.bid_price) / ((quote.ask_price + quote.bid_price) / 2.0) * 100.0;

    let after: Vec<&Bar> = bars.iter().copied().filter(|b| at(b) >= decision && at(b) < close).collect();
    let path = follow(entry, entry_spread_pct, &after).ok_or(Skip::NoBarsAfterDecision)?;
    Ok(Observation {
        symbol: s.symbol.clone(),
        day,
        gap_pct,
        relative_volume,
        entry,
        entry_spread_pct,
        outcome: path.outcome,
        close_return_net_pct: path.close_return_net_pct,
        max_adverse_pct: path.max_adverse_pct,
    })
}

pub(crate) struct Path {
    pub(crate) outcome: Outcome,
    pub(crate) close_return_net_pct: f64,
    pub(crate) max_adverse_pct: f64,
}

/// What happened after buying at `entry`: the bars from entry to the
/// close, target and stop checked minute by minute (stop first within a
/// minute), the close valued at the bid. `None` when there are no bars.
pub(crate) fn follow(entry: f64, entry_spread_pct: f64, after: &[&Bar]) -> Option<Path> {
    let last = after.last()?;
    let target = entry * (1.0 + TARGET_PCT / 100.0);
    let stop = entry * (1.0 - STOP_PCT / 100.0);
    let mut outcome = Outcome::Neither;
    let mut worst = entry;
    for b in after {
        worst = worst.min(b.low);
        if b.low <= stop {
            outcome = Outcome::Stop;
            break;
        }
        if b.high >= target {
            outcome = Outcome::Target;
            break;
        }
    }
    let exit = last.close * (1.0 - entry_spread_pct / 200.0);
    Some(Path {
        outcome,
        close_return_net_pct: (exit - entry) / entry * 100.0,
        max_adverse_pct: ((worst - entry) / entry * 100.0).min(0.0),
    })
}

// ---- Bullish Momentum crossings (2026-10-15) ---------------------------
//
// The Bullish Momentum panel lists the moments a symbol's momentum score
// crosses the scorer's qualify threshold. This replays exactly that on the
// cached sessions -- the live 30-bar window and `momentum_scorer::score`
// with its default weights -- and asks the same question as the 09:35
// rates: buying at the ask once the crossing bar has closed, how often did
// +TARGET_PCT come before -STOP_PCT, and what was left at the close after
// the spread? Crossings are counted in the regular session up to 30
// minutes before the close, within the scanner's price range, with a two-sided quote no older
// than `MOMENTUM_QUOTE_MAX_AGE_SECS`; every crossing counts, so one
// symbol-day can contribute several.

/// The live scanner's momentum window (market-data live.rs MOMENTUM_WINDOW).
pub const MOMENTUM_WINDOW: usize = 30;
pub const MOMENTUM_QUOTE_MAX_AGE_SECS: i64 = 120;
const MOMENTUM_LAST_ENTRY_BEFORE_CLOSE_SECS: i64 = 30 * 60;
/// The panel's strength bands, by lower bound (same as the web client's
/// lib/momentumChart.ts MOMENTUM_BANDS).
pub const MOMENTUM_BANDS: [(f64, &str, &str); 3] = [(0.60, "Mild", "0.60–0.65"), (0.65, "Solid", "0.65–0.70"), (0.70, "Strong", "0.70+")];

pub fn momentum_band(score: f64) -> Option<&'static str> {
    MOMENTUM_BANDS.iter().rev().find(|(lo, _, _)| score >= *lo).map(|(_, name, _)| *name)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentumObservation {
    pub symbol: String,
    pub day: NaiveDate,
    pub score: f64,
    pub band: &'static str,
    pub entry: f64,
    pub entry_spread_pct: f64,
    pub outcome: Outcome,
    pub close_return_net_pct: f64,
    pub max_adverse_pct: f64,
}

/// Every qualifying momentum crossing in one session, measured.
pub fn observe_momentum(s: &SessionInput) -> Vec<MomentumObservation> {
    let mut bars: Vec<&Bar> = s.bars.iter().collect();
    bars.sort_by_key(|b| b.timestamp);
    let Some(first) = bars.first() else { return Vec::new() };
    let day = market_day(first.timestamp);
    let open = market_day_open(day).timestamp() + REGULAR_OPEN_OFFSET_SECS;
    let close = open + REGULAR_SESSION_SECS;
    let bars: Vec<&Bar> = bars.into_iter().filter(|b| market_day(b.timestamp) == day).collect();

    let mut quotes: Vec<&Quote> = s.quotes.iter().filter(|q| q.bid_price > 0.0 && q.ask_price >= q.bid_price && q.ask_price.is_finite()).collect();
    quotes.sort_by_key(|q| q.timestamp);

    let weights = momentum_scorer::MomentumWeights::default();
    let mut window = momentum_scorer::RollingWindow::new(MOMENTUM_WINDOW);
    let mut was_qualified = false;
    let mut out = Vec::new();
    for (i, b) in bars.iter().enumerate() {
        window.push(momentum_scorer::Candle { open: b.open, high: b.high, low: b.low, close: b.close, volume: b.volume });
        let score = momentum_scorer::score(window.as_slice(), &weights).overall;
        let qualified = score >= momentum_scorer::DEFAULT_QUALIFY_THRESHOLD;
        let crossed = qualified && !was_qualified;
        was_qualified = qualified;
        let known_at = b.timestamp.timestamp() + 60;
        if !crossed || b.timestamp.timestamp() < open || known_at > close - MOMENTUM_LAST_ENTRY_BEFORE_CLOSE_SECS || !(fast_funnel::SCAN_MIN_PRICE..=fast_funnel::SCAN_MAX_PRICE).contains(&b.close) {
            continue;
        }
        let Some(known) = DateTime::<Utc>::from_timestamp(known_at, 0) else { continue };
        let idx = quotes.partition_point(|q| q.timestamp < known);
        let Some(q) = idx.checked_sub(1).map(|j| quotes[j]) else { continue };
        if (known - q.timestamp).num_seconds() > MOMENTUM_QUOTE_MAX_AGE_SECS {
            continue;
        }
        let entry = q.ask_price;
        let spread = (q.ask_price - q.bid_price) / ((q.ask_price + q.bid_price) / 2.0) * 100.0;
        let after: Vec<&Bar> = bars[i + 1..].iter().copied().filter(|x| x.timestamp.timestamp() < close).collect();
        let (Some(path), Some(band)) = (follow(entry, spread, &after), momentum_band(score)) else { continue };
        out.push(MomentumObservation {
            symbol: s.symbol.clone(),
            day,
            score,
            band,
            entry,
            entry_spread_pct: spread,
            outcome: path.outcome,
            close_return_net_pct: path.close_return_net_pct,
            max_adverse_pct: path.max_adverse_pct,
        });
    }
    out
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentumBandRates {
    pub band: String,
    pub range: String,
    pub n: usize,
    /// Distinct symbol-days the crossings came from.
    pub sessions: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_first_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub median_close_return_net_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub median_max_adverse_pct: Option<f64>,
}

pub fn aggregate_momentum(observations: &[MomentumObservation]) -> Vec<MomentumBandRates> {
    MOMENTUM_BANDS
        .iter()
        .map(|(_, band, range)| {
            let obs: Vec<&MomentumObservation> = observations.iter().filter(|o| o.band == *band).collect();
            let n = obs.len();
            let enough = n >= MIN_SAMPLE;
            let rate = |want: Outcome| enough.then(|| obs.iter().filter(|o| o.outcome == want).count() as f64 / n as f64);
            let sessions: std::collections::BTreeSet<(&str, NaiveDate)> = obs.iter().map(|o| (o.symbol.as_str(), o.day)).collect();
            MomentumBandRates {
                band: band.to_string(),
                range: range.to_string(),
                n,
                sessions: sessions.len(),
                target_rate: rate(Outcome::Target),
                stop_first_rate: rate(Outcome::Stop),
                median_close_return_net_pct: if enough { median(obs.iter().map(|o| o.close_return_net_pct).collect()) } else { None },
                median_max_adverse_pct: if enough { median(obs.iter().map(|o| o.max_adverse_pct).collect()) } else { None },
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileRates {
    pub gap: String,
    pub relative_volume: String,
    pub n: usize,
    /// Present only when `n >= MIN_SAMPLE`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_first_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub median_close_return_net_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub median_max_adverse_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub median_entry_spread_pct: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    pub sessions_read: usize,
    pub observations: usize,
    pub symbols: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_day: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_day: Option<NaiveDate>,
    pub skipped: serde_json::Value,
    /// What a reader must know about how the sessions were chosen.
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaseRates {
    pub schema: String,
    pub generated_at: DateTime<Utc>,
    pub decision_time_et: String,
    pub target_pct: f64,
    pub stop_pct: f64,
    pub min_sample: usize,
    pub source: Source,
    pub profiles: Vec<ProfileRates>,
    /// Bullish Momentum crossings by strength band (see observe_momentum).
    /// Optional, so files written before it existed still load.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub momentum: Vec<MomentumBandRates>,
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let m = v.len() / 2;
    Some(if v.len() % 2 == 0 { (v[m - 1] + v[m]) / 2.0 } else { v[m] })
}

/// Rolls observations up into one row per profile, every profile listed
/// (empty ones with n = 0) so a reader sees where there is no evidence.
pub fn aggregate(observations: &[Observation], sessions_read: usize, skipped: &Skipped, note: &str, generated_at: DateTime<Utc>) -> BaseRates {
    let mut groups: BTreeMap<(usize, usize), Vec<&Observation>> = BTreeMap::new();
    for o in observations {
        let gi = GAP_BUCKETS.iter().rposition(|(lo, _)| o.gap_pct >= *lo);
        let ri = RVOL_BUCKETS.iter().rposition(|(lo, _)| o.relative_volume >= *lo);
        if let (Some(gi), Some(ri)) = (gi, ri) {
            groups.entry((gi, ri)).or_default().push(o);
        }
    }
    let mut profiles = Vec::new();
    for (gi, (_, gap)) in GAP_BUCKETS.iter().enumerate() {
        for (ri, (_, rv)) in RVOL_BUCKETS.iter().enumerate() {
            let obs = groups.remove(&(gi, ri)).unwrap_or_default();
            let n = obs.len();
            let enough = n >= MIN_SAMPLE;
            let rate = |want: Outcome| enough.then(|| obs.iter().filter(|o| o.outcome == want).count() as f64 / n as f64);
            let med = |f: fn(&Observation) -> f64| if enough { median(obs.iter().map(|o| f(o)).collect()) } else { None };
            profiles.push(ProfileRates {
                gap: gap.to_string(),
                relative_volume: rv.to_string(),
                n,
                target_rate: rate(Outcome::Target),
                stop_first_rate: rate(Outcome::Stop),
                median_close_return_net_pct: med(|o| o.close_return_net_pct),
                median_max_adverse_pct: med(|o| o.max_adverse_pct),
                median_entry_spread_pct: med(|o| o.entry_spread_pct),
            });
        }
    }
    let symbols: std::collections::BTreeSet<&str> = observations.iter().map(|o| o.symbol.as_str()).collect();
    BaseRates {
        schema: SCHEMA.into(),
        generated_at,
        decision_time_et: DECISION_TIME_ET.into(),
        target_pct: TARGET_PCT,
        stop_pct: STOP_PCT,
        min_sample: MIN_SAMPLE,
        source: Source {
            sessions_read,
            observations: observations.len(),
            symbols: symbols.len(),
            first_day: observations.iter().map(|o| o.day).min(),
            last_day: observations.iter().map(|o| o.day).max(),
            skipped: serde_json::to_value(skipped).unwrap_or_default(),
            note: note.into(),
        },
        profiles,
        momentum: Vec::new(),
    }
}

impl BaseRates {
    /// The row a live (day change %, relative volume) reading falls in.
    pub fn lookup(&self, gap_pct: f64, relative_volume: f64) -> Option<&ProfileRates> {
        let (gap, rv) = profile_of(gap_pct, relative_volume)?;
        self.profiles.iter().find(|p| p.gap == gap && p.relative_volume == rv)
    }
}

#[cfg(test)]
#[path = "base_rates_tests.rs"]
mod base_rates_tests;
