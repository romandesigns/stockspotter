//! **Frozen.** The independent reference definition of a "meaningful market
//! opportunity" (assignment §14).
//!
//! # Why this is frozen, and frozen *here*
//!
//! Recall is only meaningful against a population defined without reference to
//! what the platform detected. If the label were chosen after seeing which
//! definition made Opportunity Intelligence V1 look strongest, the resulting
//! recall figure would measure the choice, not the platform.
//!
//! So this module is written and versioned **before** the first untouched
//! prospective session is evaluated, and every parameter in it is either
//! inherited from something already frozen in this repository or stated here
//! with its justification. Nothing in it was selected by looking at an outcome.
//!
//! # What is inherited rather than invented
//!
//! | Parameter | Value | Inherited from |
//! |---|---|---|
//! | Target thresholds | +2% / +5% / +10% | [`crate::horizon::TARGET_PCTS`] |
//! | Horizon grid | 30…1800s | [`crate::horizon::HORIZON_SECS`] |
//! | Maximum tolerable price gap | 120s | [`crate::horizon::MAX_GAP_SECS`] |
//! | Price eligibility | $0.25–$20.00 | `fast_funnel` `min_price`/`max_price` |
//!
//! The +2/+5/+10 family is the already-preregistered one. Section 14 requires
//! it be *preserved and versioned*, not replaced, and it is: this module adds a
//! reference-population label built on the same thresholds, it does not
//! redefine them.
//!
//! The price band is the platform's own declared addressable range. It matters
//! that it is inherited: a symbol outside $0.25–$20.00 is not a *miss*, it is
//! out of scope by construction, and counting it as a miss would manufacture a
//! recall failure out of a design decision. Out-of-band movers are still
//! counted and reported, separately, as context.
//!
//! # The semantics, stated exactly (§14)
//!
//! **Starting price.** The first price observed for the symbol at or after the
//! regular-session open, taken from the *independent* discovery capture — never
//! from a detector event, an opportunity, or any platform decision. A label
//! whose origin depended on detection would not be independent of it.
//!
//! **Crossing.** The symbol's observed high reaches `start × (1 + target/100)`
//! at any instant inside the session window. First crossing time is recorded.
//! High-water, not close: the question is whether the move was available, not
//! whether it persisted to a bar boundary.
//!
//! **Horizon.** The regular session window, 09:30–16:00 America/New_York.
//! This is an *intraday* label deliberately: the platform is a day-trading scanner, and a
//! move that only materialises overnight is not an opportunity it exists to
//! find. Per-opportunity outcomes keep the finer frozen grid separately.
//!
//! **Session window.** 09:30 ET inclusive to the regular close exclusive: 16:00
//! ET, or 13:00 ET on an NYSE early close (`halt_detector::calendar`). A date
//! with no regular session has an empty window, so every symbol on it is
//! `NoSessionPrice`. Pre-market and after-hours prices are excluded from both
//! the start price and the crossing search.
//!
//! v1 stated this window as 13:30:00Z–20:00:00Z. That is 09:30–16:00 ET under
//! EDT only: in winter it started an hour early (08:30 EST, premarket) and
//! ended an hour early (15:00 EST), and on an early close it ran three hours
//! into after-hours. v2 (D13, 2026-09-25) is the window v1 described in words,
//! on a DST- and calendar-aware clock. **On every full-day EDT session --
//! every session labelled so far -- v1 and v2 produce identical labels.**
//!
//! **Censoring.** A symbol whose observed series has a gap longer than
//! `MAX_GAP_SECS` (120s) before its first crossing is labelled **Unknown**, not
//! *not crossed*. Absence of evidence is never evidence of absence — the same
//! rule [`crate::horizon`] already applies, and the reason a three-valued
//! outcome exists at all.
//!
//! **Price eligibility.** Start price within $0.25–$20.00. Below the floor a
//! +10% move can be a single tick; above the ceiling the platform does not
//! look.
//!
//! **Flat base.** Deliberately **not** required. A flat-base precondition is a
//! *setup* filter, and imposing one here would quietly narrow the reference
//! population toward the kind of move the platform is built to catch — which
//! is precisely the bias an independent label exists to avoid.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::horizon::{PricePoint, MAX_GAP_SECS, TARGET_PCTS};

/// Bump on any change to the semantics above. A result is only comparable to
/// another result carrying the same version.
///
/// v1 -> v2 (D13): the session window moved from fixed UTC hours to New York
/// wall-clock with the NYSE calendar; see "Session window" above.
pub const REFERENCE_LABEL_VERSION: &str = "reference-opportunity-v2";

/// Inherited from `fast_funnel::Thresholds`.
pub const MIN_PRICE: f64 = 0.25;
pub const MAX_PRICE: f64 = 20.00;

/// The frozen specification, carried into every report so a result is always
/// attributable to the definition that produced it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceLabelSpec {
    pub version: String,
    pub targets_pct: Vec<f64>,
    /// New York wall-clock, not UTC (v2; v1 carried `sessionOpenUtc`/
    /// `sessionCloseUtc` = 13:30:00Z/20:00:00Z).
    pub session_open: String,
    pub session_close: String,
    pub min_price: f64,
    pub max_price: f64,
    pub max_gap_secs: i64,
    pub start_price_rule: String,
    pub crossing_rule: String,
    pub censoring_rule: String,
    pub flat_base_required: bool,
}

impl Default for ReferenceLabelSpec {
    fn default() -> Self {
        Self {
            version: REFERENCE_LABEL_VERSION.to_string(),
            targets_pct: TARGET_PCTS.to_vec(),
            session_open: "09:30 America/New_York".to_string(),
            session_close: "16:00 America/New_York; 13:00 on NYSE early closes; no window on                             NYSE holidays"
                .to_string(),
            min_price: MIN_PRICE,
            max_price: MAX_PRICE,
            max_gap_secs: MAX_GAP_SECS,
            start_price_rule:
                "first independently observed price at or after the session open".to_string(),
            crossing_rule:
                "observed high reaches start x (1 + target/100) at any instant inside the session"
                    .to_string(),
            censoring_rule:
                "a gap longer than max_gap_secs before first crossing yields Unknown, never NotCrossed"
                    .to_string(),
            flat_base_required: false,
        }
    }
}

/// Why a symbol is not part of the reference population.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ineligible {
    /// No price observed inside the session window.
    NoSessionPrice,
    BelowPriceFloor,
    AbovePriceCeiling,
    /// A start price that is not a usable number.
    UnusableStartPrice,
}

/// Whether the symbol reached one target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum Crossing {
    /// Reached, with the first instant at which it was reached.
    Crossed {
        at: DateTime<Utc>,
        /// Seconds from the session-open start price to first crossing.
        after_secs: i64,
    },
    /// The series is dense enough to say it did not reach the target.
    NotCrossed,
    /// The series is not dense enough to say. **Never counted as NotCrossed.**
    Unknown { largest_gap_secs: i64 },
}

impl Crossing {
    pub fn crossed(&self) -> bool {
        matches!(self, Crossing::Crossed { .. })
    }
    pub fn known(&self) -> bool {
        !matches!(self, Crossing::Unknown { .. })
    }
}

/// One symbol's independent label for one session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceOpportunity {
    pub symbol: String,
    pub session_date: NaiveDate,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ineligible: Option<Ineligible>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_high: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_high_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_low: Option<f64>,
    /// Maximum favourable excursion from the start price, percent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mfe_pct: Option<f64>,
    /// Maximum adverse excursion from the start price, percent (negative).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mae_pct: Option<f64>,
    /// One entry per `spec.targets_pct`, in the same order.
    pub crossings: Vec<Crossing>,
    pub observations: usize,
    pub largest_gap_secs: i64,
}

impl ReferenceOpportunity {
    /// Whether this symbol is a meaningful opportunity at `target_index`.
    ///
    /// `None` means unknown, and must never be folded into `false`.
    pub fn is_opportunity(&self, target_index: usize) -> Option<bool> {
        match self.crossings.get(target_index)? {
            Crossing::Crossed { .. } => Some(true),
            Crossing::NotCrossed => Some(false),
            Crossing::Unknown { .. } => None,
        }
    }
}

/// `[open, close)` of `date`'s regular session in UTC, or `None` when the
/// NYSE has no session that day.
pub fn session_bounds(date: NaiveDate) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    use market_data::trading_session::{regular_session_close, regular_session_open};
    Some((regular_session_open(date)?, regular_session_close(date)?))
}

/// Labels one symbol from an independently-observed price series.
///
/// `series` need not be sorted; it is sorted here so a caller cannot change the
/// answer by changing the order it happened to read records in.
pub fn label(
    symbol: &str,
    session_date: NaiveDate,
    series: &[PricePoint],
    spec: &ReferenceLabelSpec,
) -> ReferenceOpportunity {
    // No session that day: an empty window, so `NoSessionPrice` below.
    let (open, close) =
        session_bounds(session_date).unwrap_or((DateTime::<Utc>::MIN_UTC, DateTime::<Utc>::MIN_UTC));
    let mut points: Vec<PricePoint> = series
        .iter()
        .copied()
        .filter(|(at, price)| *at >= open && *at < close && price.is_finite() && *price > 0.0)
        .collect();
    points.sort_by_key(|(at, _)| *at);

    let mut out = ReferenceOpportunity {
        symbol: symbol.to_string(),
        session_date,
        ineligible: None,
        start_price: None,
        start_at: None,
        session_high: None,
        session_high_at: None,
        session_low: None,
        mfe_pct: None,
        mae_pct: None,
        crossings: vec![Crossing::Unknown { largest_gap_secs: 0 }; spec.targets_pct.len()],
        observations: points.len(),
        largest_gap_secs: 0,
    };

    let Some(&(start_at, start_price)) = points.first() else {
        out.ineligible = Some(Ineligible::NoSessionPrice);
        return out;
    };
    if !start_price.is_finite() || start_price <= 0.0 {
        out.ineligible = Some(Ineligible::UnusableStartPrice);
        return out;
    }
    out.start_price = Some(start_price);
    out.start_at = Some(start_at);

    if start_price < spec.min_price {
        out.ineligible = Some(Ineligible::BelowPriceFloor);
        return out;
    }
    if start_price > spec.max_price {
        out.ineligible = Some(Ineligible::AbovePriceCeiling);
        return out;
    }

    // Excursions and the largest observation gap, in one pass.
    let mut high = start_price;
    let mut high_at = start_at;
    let mut low = start_price;
    let mut largest_gap = 0i64;
    let mut previous = start_at;
    // First crossing per target, and the largest gap seen *before* it.
    let mut first: Vec<Option<(DateTime<Utc>, i64)>> = vec![None; spec.targets_pct.len()];
    let mut gap_before: Vec<i64> = vec![0; spec.targets_pct.len()];

    for &(at, price) in &points {
        let gap = (at - previous).num_seconds().max(0);
        largest_gap = largest_gap.max(gap);
        previous = at;
        if price > high {
            high = price;
            high_at = at;
        }
        if price < low {
            low = price;
        }
        for (index, target) in spec.targets_pct.iter().enumerate() {
            if first[index].is_some() {
                continue;
            }
            // Gaps only matter up to the moment the question is answered: a
            // sparse tail after a confirmed crossing cannot un-cross it.
            gap_before[index] = gap_before[index].max(gap);
            if price >= start_price * (1.0 + target / 100.0) {
                first[index] = Some((at, (at - start_at).num_seconds().max(0)));
            }
        }
    }
    // The tail: from the last observation to the close is itself a gap, and a
    // symbol that stopped printing at 14:00 cannot be said not to have run.
    let tail_gap = (close - previous).num_seconds().max(0);
    largest_gap = largest_gap.max(tail_gap);

    out.session_high = Some(high);
    out.session_high_at = Some(high_at);
    out.session_low = Some(low);
    out.mfe_pct = Some((high - start_price) / start_price * 100.0);
    out.mae_pct = Some((low - start_price) / start_price * 100.0);
    out.largest_gap_secs = largest_gap;

    out.crossings = first
        .into_iter()
        .enumerate()
        .map(|(index, hit)| match hit {
            Some((at, after_secs)) => Crossing::Crossed { at, after_secs },
            None => {
                let gap = gap_before[index].max(tail_gap);
                if gap > spec.max_gap_secs {
                    Crossing::Unknown { largest_gap_secs: gap }
                } else {
                    Crossing::NotCrossed
                }
            }
        })
        .collect();
    out
}

#[cfg(test)]
#[path = "labels_tests.rs"]
mod tests;
