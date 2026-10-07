//! Clustered uncertainty for the qualification decision (§27).
//!
//! # The failure this exists to prevent
//!
//! A full regular session emits roughly 2.5 million ranking rows. Treating them
//! as independent observations would produce confidence intervals narrow enough
//! to make almost anything look decisive — and every one of those rows is
//! correlated with the others, three times over:
//!
//! * the same opportunity is re-ranked every 30 seconds, so one opportunity
//!   contributes dozens of rows;
//! * the same symbol produces many opportunities across a session;
//! * horizons overlap, so nearby rows share most of their forward path.
//!
//! Three defences, applied in order:
//!
//! 1. **Collapse to the opportunity.** Repeated ranking snapshots of one
//!    opportunity become a single observation, entered at the first window in
//!    which it met the cohort's condition and measured forward from there. This
//!    is causal — it never uses a later window to decide an earlier one — and it
//!    is what makes the analytical unit the opportunity rather than the row.
//! 2. **Cluster by symbol.** The bootstrap resamples *symbols*, not
//!    observations, so a symbol that produced forty opportunities contributes
//!    one draw rather than forty.
//! 3. **Report both counts.** Raw observations and distinct clusters are
//!    carried on every estimate, so a reader can see when an interval rests on
//!    very few independent units.
//!
//! # Determinism
//!
//! The resampler is seeded from the qualification specification's own hash, so
//! a run is reproducible and the seed cannot be shopped for a better interval.
//! The generator is a plain xorshift64*: dependency-free, and entirely adequate
//! for resampling, which needs uniformity rather than cryptographic quality.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// One opportunity's contribution to a comparison.
///
/// `outcome` is `None` when the measurement is censored. Censored observations
/// are excluded from both cohorts and reported as coverage — never folded into
/// the denominator as a failure, which would convert "we could not see" into
/// "it did not happen".
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    /// Cluster unit. The symbol.
    pub cluster: String,
    /// Analytical unit. The opportunity id.
    pub unit: String,
    pub in_candidate: bool,
    pub in_control: bool,
    pub outcome: Option<bool>,
    /// Continuous measurement, where the comparison is of means rather than
    /// rates (excursion ratios, lead times).
    pub value: Option<f64>,
}

/// A point estimate with a clustered interval, and the counts behind it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Estimate {
    pub point: Option<f64>,
    pub lower: Option<f64>,
    pub upper: Option<f64>,
    pub confidence: f64,
    /// Observations after collapsing to the analytical unit.
    pub n_units: usize,
    /// Distinct clusters — the number of genuinely independent draws.
    pub n_clusters: usize,
    /// Rows seen before collapsing, so inflation is visible rather than hidden.
    pub n_raw_rows: usize,
    /// Units excluded because their measurement was censored.
    pub n_censored: usize,
    pub method: String,
}

impl Estimate {
    /// Whether the interval excludes `value` from below. The shape every
    /// comparative criterion is decided on.
    pub fn lower_bound_above(&self, value: f64) -> Option<bool> {
        self.lower.map(|l| l > value)
    }

    pub fn is_estimable(&self) -> bool {
        self.point.is_some() && self.lower.is_some() && self.upper.is_some()
    }
}

/// Deterministic xorshift64*, seeded from the specification hash.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    /// Seeds from a hex digest. Any non-zero state works; zero is replaced
    /// because xorshift is absorbing at zero.
    pub fn from_hex_seed(hex: &str) -> Self {
        let mut state: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in hex.as_bytes() {
            state ^= u64::from(*byte);
            state = state.wrapping_mul(0x100_0000_01b3);
        }
        Self(if state == 0 { 0x9E37_79B9_7F4A_7C15 } else { state })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `0..n`.
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next_u64() % n as u64) as usize
    }
}

/// Collapses repeated rows into one observation per analytical unit.
///
/// **First-entry wins.** An opportunity joins a cohort at the first row in
/// which it met that cohort's condition, and its outcome is the one measured
/// from that instant. Taking the best row instead would let the analysis choose,
/// after the fact, the window in which each opportunity looked strongest —
/// which is exactly the selection this module exists to prevent.
///
/// Input order therefore matters and is the caller's responsibility: rows must
/// arrive in window order. `collapse` does not sort, because it cannot know
/// which field carries time without being told, and silently sorting by the
/// wrong key would be worse than requiring the caller to be explicit.
pub fn collapse(rows: &[Observation]) -> Vec<Observation> {
    let mut first: BTreeMap<&str, Observation> = BTreeMap::new();
    let mut order: Vec<&str> = Vec::new();
    for row in rows {
        match first.get_mut(row.unit.as_str()) {
            None => {
                order.push(row.unit.as_str());
                first.insert(row.unit.as_str(), row.clone());
            }
            Some(existing) => {
                // Membership is monotone: an opportunity that ever entered a
                // cohort stays in it, entered at its first qualifying row. The
                // outcome recorded is the one from that first entry.
                if row.in_candidate && !existing.in_candidate {
                    existing.in_candidate = true;
                    existing.outcome = row.outcome;
                    existing.value = row.value;
                }
                if row.in_control && !existing.in_control {
                    existing.in_control = true;
                    if !existing.in_candidate {
                        existing.outcome = row.outcome;
                        existing.value = row.value;
                    }
                }
            }
        }
    }
    order.into_iter().filter_map(|unit| first.remove(unit)).collect()
}

fn clusters_of(units: &[Observation]) -> Vec<(String, Vec<usize>)> {
    let mut map: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, observation) in units.iter().enumerate() {
        map.entry(observation.cluster.clone()).or_default().push(index);
    }
    map.into_iter().collect()
}

fn rate(units: &[Observation], indices: &[usize], candidate: bool) -> Option<f64> {
    let mut hits = 0usize;
    let mut total = 0usize;
    for &index in indices {
        let observation = &units[index];
        let member = if candidate { observation.in_candidate } else { observation.in_control };
        if !member {
            continue;
        }
        let Some(outcome) = observation.outcome else { continue };
        total += 1;
        if outcome {
            hits += 1;
        }
    }
    if total == 0 {
        None
    } else {
        Some(hits as f64 / total as f64)
    }
}

fn mean(units: &[Observation], indices: &[usize], candidate: bool) -> Option<f64> {
    let mut sum = 0.0;
    let mut total = 0usize;
    for &index in indices {
        let observation = &units[index];
        let member = if candidate { observation.in_candidate } else { observation.in_control };
        if !member {
            continue;
        }
        let Some(value) = observation.value else { continue };
        if !value.is_finite() {
            continue;
        }
        sum += value;
        total += 1;
    }
    if total == 0 {
        None
    } else {
        Some(sum / total as f64)
    }
}

fn median(units: &[Observation], indices: &[usize], candidate: bool) -> Option<f64> {
    let mut values: Vec<f64> = indices
        .iter()
        .filter_map(|&index| {
            let observation = &units[index];
            let member =
                if candidate { observation.in_candidate } else { observation.in_control };
            if member { observation.value.filter(|v| v.is_finite()) } else { None }
        })
        .collect();
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    percentile(&values, 0.5)
}

fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let position = (sorted.len() - 1) as f64 * p;
    let low = position.floor() as usize;
    let high = position.ceil() as usize;
    if low == high {
        Some(sorted[low])
    } else {
        Some(sorted[low] + (sorted[high] - sorted[low]) * (position - low as f64))
    }
}

/// What quantity a comparison estimates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quantity {
    /// candidate rate / control rate.
    RateRatio,
    /// candidate rate − control rate.
    RateDifference,
    /// candidate mean / control mean.
    MeanRatio,
    /// The candidate cohort's rate alone.
    Rate,
    /// The candidate cohort's mean alone.
    Mean,
    /// The candidate cohort's median. Used where the distribution is skewed
    /// enough that a mean would be dominated by a tail -- lead times, where one
    /// opportunity ranked six hours early would drag the average positive on
    /// its own.
    Median,
}

impl Quantity {
    fn label(self) -> &'static str {
        match self {
            Quantity::RateRatio => "rate ratio",
            Quantity::RateDifference => "rate difference",
            Quantity::MeanRatio => "mean ratio",
            Quantity::Rate => "rate",
            Quantity::Mean => "mean",
            Quantity::Median => "median",
        }
    }
}

fn compute(units: &[Observation], indices: &[usize], quantity: Quantity) -> Option<f64> {
    match quantity {
        Quantity::Rate => rate(units, indices, true),
        Quantity::Mean => mean(units, indices, true),
        Quantity::Median => median(units, indices, true),
        Quantity::RateRatio => {
            let candidate = rate(units, indices, true)?;
            let control = rate(units, indices, false)?;
            if control == 0.0 {
                None
            } else {
                Some(candidate / control)
            }
        }
        Quantity::RateDifference => {
            Some(rate(units, indices, true)? - rate(units, indices, false)?)
        }
        Quantity::MeanRatio => {
            let candidate = mean(units, indices, true)?;
            let control = mean(units, indices, false)?;
            if control == 0.0 {
                None
            } else {
                Some(candidate / control)
            }
        }
    }
}

/// Cluster bootstrap with a percentile interval.
///
/// `rows` may contain repeated ranking snapshots; they are collapsed to one
/// observation per opportunity first. Symbols are then resampled with
/// replacement, so the interval reflects the number of independent symbols
/// rather than the number of correlated rows.
pub fn cluster_bootstrap(
    rows: &[Observation],
    quantity: Quantity,
    resamples: usize,
    confidence: f64,
    rng: &mut Rng,
) -> Estimate {
    let units = collapse(rows);
    let n_raw_rows = rows.len();
    let n_censored = units.iter().filter(|u| u.outcome.is_none() && u.value.is_none()).count();
    let clusters = clusters_of(&units);
    let all: Vec<usize> = (0..units.len()).collect();
    let point = compute(&units, &all, quantity);

    let mut estimate = Estimate {
        point,
        lower: None,
        upper: None,
        confidence,
        n_units: units.len(),
        n_clusters: clusters.len(),
        n_raw_rows,
        n_censored,
        method: format!(
            "{}, cluster bootstrap by symbol, {resamples} resamples, {:.0}% percentile interval",
            quantity.label(),
            confidence * 100.0
        ),
    };

    // An interval over fewer than two clusters is not an interval. Reporting
    // the point estimate without one is the honest answer.
    if clusters.len() < 2 || point.is_none() || resamples == 0 {
        return estimate;
    }

    let mut draws: Vec<f64> = Vec::with_capacity(resamples);
    for _ in 0..resamples {
        let mut indices: Vec<usize> = Vec::with_capacity(units.len());
        for _ in 0..clusters.len() {
            let chosen = rng.below(clusters.len());
            indices.extend_from_slice(&clusters[chosen].1);
        }
        if let Some(value) = compute(&units, &indices, quantity) {
            if value.is_finite() {
                draws.push(value);
            }
        }
    }
    // Too few usable resamples means the quantity is undefined in most draws —
    // a control rate of zero, say — and a percentile over the survivors would
    // be a biased subset rather than an interval.
    if (draws.len() as f64) < resamples as f64 * 0.5 {
        return estimate;
    }
    draws.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let tail = (1.0 - confidence) / 2.0;
    estimate.lower = percentile(&draws, tail);
    estimate.upper = percentile(&draws, 1.0 - tail);
    estimate
}

/// Distinct clusters and units in a collapsed sample, for evidence checks.
pub fn counts(rows: &[Observation]) -> (usize, usize) {
    let units = collapse(rows);
    let clusters: BTreeSet<&str> = units.iter().map(|u| u.cluster.as_str()).collect();
    (units.len(), clusters.len())
}

#[cfg(test)]
#[path = "stats_tests.rs"]
mod tests;
