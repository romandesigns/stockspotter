//! Bullish momentum scorer — section 4.2 of the architecture doc. Combines
//! four weighted factors into a single confidence score. Pure and
//! synchronous like `fast_funnel`: takes a slice of candles, returns a
//! score, no I/O, no state — the caller (live loop or replay engine) owns
//! the rolling window (see `RollingWindow` in `candle.rs`).

use serde::{Deserialize, Serialize};

use crate::candle::Candle;

const MA_SHORT: usize = 9;
const MA_LONG: usize = 20;
/// Upper wick counts as a "rejection" once it's this fraction of the
/// candle's total high-low range — i.e. price pushed up but gave back at
/// least half that move before the close. Somewhat arbitrary; tune via
/// backtesting (doc section 8) once real hit-rate data exists.
const REJECTION_WICK_RATIO: f64 = 0.5;

/// Confidence gate. The doc's "90%+" example turned out to be
/// unreachable in practice: `backtest-metrics --bin tune` measured the
/// real distribution of `overall` across a full real trading session
/// (SWVL, 2026-08-28, a genuine +41% gap day) and it never exceeded 0.80
/// — median was 0.46. Requiring all four weighted factors to be
/// simultaneously near-perfect is a much higher bar than real market data
/// clears, even on a strong day; 0.90 as a gate meant this scorer could
/// never qualify anything, ever.
///
/// Lowered to 0.55 based on that same session's data — but flagged
/// explicitly as weaker evidence than the ignition detector's tuning
/// (see `ignition_detector::monitor::MonitorConfig`'s doc comment for
/// contrast): edge-triggered signal counting from one session produced
/// only a handful of qualification events to learn from (as few as 1 at
/// stricter thresholds), nowhere near ignition's 300+ signal sample. This
/// is a directional correction (0.90 is definitely wrong; something
/// reachable is definitely more right) backed by a real distribution
/// rather than a confidently hit-rate-optimized choice. Revisit once
/// multi-session/multi-symbol data (including quiet, non-qualifying days)
/// is available to actually validate a specific number.
pub const DEFAULT_QUALIFY_THRESHOLD: f64 = 0.55;

/// Relative weight of each factor in the overall score. The doc names the
/// four factors in strongest-to-weakest order (4.2) but doesn't give exact
/// numbers — these are a reasonable starting point pulled from that
/// ordering, not a backtested result. Kept as its own struct, same pattern
/// as `fast_funnel::FilterThresholds`, so weights are tunable without
/// touching the scoring logic itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MomentumWeights {
    pub volume_confirmation: f64,
    pub structure: f64,
    pub ma_slope: f64,
    pub wick_rejection: f64,
}

impl Default for MomentumWeights {
    fn default() -> Self {
        Self {
            volume_confirmation: 0.4,
            structure: 0.3,
            ma_slope: 0.2,
            wick_rejection: 0.1,
        }
    }
}

/// Per-factor breakdown plus the overall weighted score — same
/// transparency pattern as `fast_funnel::FunnelExplanation`: a caller (a
/// live scan loop, a future "why did this qualify" debug view) can see
/// *why*, not just a single number.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MomentumScore {
    /// 0..1 — share of up-candle volume vs. total up+down volume.
    pub volume_confirmation: f64,
    /// 0..1 — share of consecutive candle pairs forming a higher-high AND
    /// higher-low.
    pub structure: f64,
    /// 0..1 — how many of {9MA sloping up, 20MA sloping up, price above
    /// both} currently hold, graduated rather than all-or-nothing.
    pub ma_slope: f64,
    /// 0..1 — inverse of the fraction of candles showing a rejection wick;
    /// 1.0 means no rejection wicks at all.
    pub wick_rejection: f64,
    /// Weighted sum of the four factors above.
    pub overall: f64,
}

impl MomentumScore {
    pub fn qualifies(&self, threshold: f64) -> bool {
        self.overall >= threshold
    }
}

/// Scores a rolling window of candles (oldest first, most recent last).
/// Factors that need more history than is available (MA slope needs at
/// least `MA_LONG + 1` candles) score 0 rather than panicking or
/// extrapolating from insufficient data — same fail-closed philosophy as
/// `fast_funnel`'s handling of unknown float/zero volume.
pub fn score(candles: &[Candle], weights: &MomentumWeights) -> MomentumScore {
    let volume_confirmation = volume_confirmation_score(candles);
    let structure = structure_score(candles);
    let ma_slope = ma_slope_score(candles);
    let wick_rejection = wick_rejection_score(candles);

    let overall = volume_confirmation * weights.volume_confirmation
        + structure * weights.structure
        + ma_slope * weights.ma_slope
        + wick_rejection * weights.wick_rejection;

    MomentumScore {
        volume_confirmation,
        structure,
        ma_slope,
        wick_rejection,
        overall,
    }
}

fn volume_confirmation_score(candles: &[Candle]) -> f64 {
    let (up_vol, down_vol) = candles.iter().fold((0u64, 0u64), |(up, down), c| {
        if c.close > c.open {
            (up + c.volume, down)
        } else if c.close < c.open {
            (up, down + c.volume)
        } else {
            (up, down)
        }
    });
    let total = up_vol + down_vol;
    if total == 0 {
        return 0.0;
    }
    up_vol as f64 / total as f64
}

fn structure_score(candles: &[Candle]) -> f64 {
    if candles.len() < 2 {
        return 0.0;
    }
    let mut hh_hl = 0;
    let mut total = 0;
    for pair in candles.windows(2) {
        let (prev, cur) = (&pair[0], &pair[1]);
        total += 1;
        if cur.high > prev.high && cur.low > prev.low {
            hh_hl += 1;
        }
    }
    hh_hl as f64 / total as f64
}

fn sma(candles: &[Candle], period: usize) -> Option<f64> {
    if candles.len() < period {
        return None;
    }
    let window = &candles[candles.len() - period..];
    Some(window.iter().map(|c| c.close).sum::<f64>() / period as f64)
}

fn ma_slope_score(candles: &[Candle]) -> f64 {
    ma_slope_score_checked(candles).unwrap_or(0.0)
}

fn ma_slope_score_checked(candles: &[Candle]) -> Option<f64> {
    // Need one extra bar of history beyond MA_LONG to compare "MA now" vs
    // "MA one bar ago" and get an actual slope, not just a level.
    if candles.len() < MA_LONG + 1 {
        return None;
    }
    let last = candles.last()?;
    let prior = &candles[..candles.len() - 1];

    let ma9_now = sma(candles, MA_SHORT)?;
    let ma9_prev = sma(prior, MA_SHORT)?;
    let ma20_now = sma(candles, MA_LONG)?;
    let ma20_prev = sma(prior, MA_LONG)?;

    let ma9_up = ma9_now > ma9_prev;
    let ma20_up = ma20_now > ma20_prev;
    let price_above_both = last.close > ma9_now && last.close > ma20_now;

    let hits = [ma9_up, ma20_up, price_above_both]
        .iter()
        .filter(|b| **b)
        .count();
    Some(hits as f64 / 3.0)
}

fn wick_rejection_score(candles: &[Candle]) -> f64 {
    if candles.is_empty() {
        return 0.0;
    }
    let rejections = candles.iter().filter(|c| has_rejection_wick(c)).count();
    1.0 - (rejections as f64 / candles.len() as f64)
}

fn has_rejection_wick(c: &Candle) -> bool {
    let range = c.high - c.low;
    if range <= 0.0 {
        return false;
    }
    let upper_wick = c.high - c.open.max(c.close);
    (upper_wick / range) >= REJECTION_WICK_RATIO
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candle(open: f64, high: f64, low: f64, close: f64, volume: u64) -> Candle {
        Candle {
            open,
            high,
            low,
            close,
            volume,
        }
    }

    #[test]
    fn volume_confirmation_favors_up_volume() {
        let candles = vec![
            candle(1.0, 1.1, 0.95, 1.08, 1000), // up
            candle(1.08, 1.05, 1.0, 1.02, 100), // down, thin
        ];
        let s = volume_confirmation_score(&candles);
        assert!(s > 0.9, "expected strong up-volume dominance, got {s}");
    }

    #[test]
    fn volume_confirmation_zero_volume_is_neutral_not_nan() {
        let candles = vec![candle(1.0, 1.0, 1.0, 1.0, 0)];
        assert_eq!(volume_confirmation_score(&candles), 0.0);
    }

    #[test]
    fn structure_score_perfect_uptrend_is_one() {
        let candles = vec![
            candle(1.0, 1.1, 0.9, 1.05, 100),
            candle(1.05, 1.2, 1.0, 1.15, 100),
            candle(1.15, 1.3, 1.1, 1.25, 100),
        ];
        assert_eq!(structure_score(&candles), 1.0);
    }

    #[test]
    fn structure_score_choppy_is_low() {
        let candles = vec![
            candle(1.0, 1.1, 0.9, 1.05, 100),
            candle(1.05, 1.02, 0.8, 0.85, 100), // lower high, lower low
        ];
        assert_eq!(structure_score(&candles), 0.0);
    }

    #[test]
    fn ma_slope_insufficient_history_scores_zero_not_panics() {
        let candles = vec![candle(1.0, 1.0, 1.0, 1.0, 100); 5];
        assert_eq!(ma_slope_score(&candles), 0.0);
    }

    #[test]
    fn ma_slope_rewards_rising_price_above_both_averages() {
        // Steadily rising closes for MA_LONG + 1 bars: both MAs slope up
        // and the latest close sits above both.
        let mut candles = Vec::new();
        for i in 0..(MA_LONG + 1) {
            let price = 1.0 + i as f64 * 0.05;
            candles.push(candle(price, price + 0.01, price - 0.01, price, 100));
        }
        assert_eq!(ma_slope_score(&candles), 1.0);
    }

    #[test]
    fn wick_rejection_flags_large_upper_wicks() {
        // Big upper wick: pushed to 2.0 but closed back down near open.
        let candles = vec![candle(1.0, 2.0, 0.95, 1.05, 100)];
        assert_eq!(wick_rejection_score(&candles), 0.0);
    }

    #[test]
    fn wick_rejection_full_score_with_no_wicks() {
        let candles = vec![candle(1.0, 1.05, 0.95, 1.05, 100)];
        assert_eq!(wick_rejection_score(&candles), 1.0);
    }

    #[test]
    fn overall_score_weights_factors_correctly() {
        let weights = MomentumWeights {
            volume_confirmation: 0.4,
            structure: 0.3,
            ma_slope: 0.2,
            wick_rejection: 0.1,
        };
        // A clean, strong uptrend: rising volume-confirmed closes, no
        // rejection wicks, enough history for MA slope too.
        let mut candles = Vec::new();
        for i in 0..(MA_LONG + 1) {
            let price = 1.0 + i as f64 * 0.05;
            candles.push(candle(price, price + 0.01, price - 0.001, price + 0.008, 1000));
        }
        let result = score(&candles, &weights);
        assert!(
            result.overall > 0.9,
            "expected a clean uptrend to score highly, got {:?}",
            result
        );
        assert!(result.qualifies(DEFAULT_QUALIFY_THRESHOLD));
    }

    #[test]
    fn overall_score_low_for_flat_thin_data() {
        let weights = MomentumWeights::default();
        let candles = vec![candle(1.0, 1.0, 1.0, 1.0, 0); 3];
        let result = score(&candles, &weights);
        assert!(!result.qualifies(DEFAULT_QUALIFY_THRESHOLD));
    }
}
