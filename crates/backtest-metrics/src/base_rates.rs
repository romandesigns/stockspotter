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
//! * **Eligible**: the stock traded between 09:30 and 09:35, priced $1-$20,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
    if !(1.0..=20.0).contains(&price) {
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
    let last = after.last().ok_or(Skip::NoBarsAfterDecision)?;
    let target = entry * (1.0 + TARGET_PCT / 100.0);
    let stop = entry * (1.0 - STOP_PCT / 100.0);
    let mut outcome = Outcome::Neither;
    let mut worst = entry;
    for b in &after {
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
    Ok(Observation {
        symbol: s.symbol.clone(),
        day,
        gap_pct,
        relative_volume,
        entry,
        entry_spread_pct,
        outcome,
        close_return_net_pct: (exit - entry) / entry * 100.0,
        max_adverse_pct: ((worst - entry) / entry * 100.0).min(0.0),
    })
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
