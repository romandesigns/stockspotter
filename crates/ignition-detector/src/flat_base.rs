//! Low-float flat-base ignition refinement —
//! docs/trading-scanner-architecture-part-3.md: "low-priced penny stocks
//! (as low as ~$0.15-$0.25) that trade flat and quiet for a period, then
//! suddenly ignite and run sharply." An *additive* qualifying condition,
//! not a new detection system — the doc is explicit: "this check only
//! applies to, and only tightens, alerts for stocks matching the
//! low-price profile. It must not change ignition detection behavior for
//! any other stock. It runs as an extra gate inside the ignition system,
//! not a separate system."
//!
//! `monitor.rs` is what actually enforces that isolation:
//! `MonitorConfig::flat_base` is `None` by default, so nothing here runs
//! at all unless a caller explicitly opts in — existing behavior for
//! every other stock and every default-configured monitor is provably
//! unchanged, not just intended to be.

use crate::tick::Trade;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlatBaseThresholds {
    /// Only trades at or below this price get the gate applied at all —
    /// the doc's ~$0.15-$0.25 low-float profile, not every stock.
    pub max_price_for_gate: f64,
    /// How many of the most recent trades (immediately before the
    /// candidate) must show a tight range to count as a "flat base".
    pub lookback_trades: usize,
    /// Max allowed (high-low)/average price ratio over that lookback
    /// window to count as flat — e.g. 0.03 = a 3% range.
    pub max_range_ratio: f64,
}

impl Default for FlatBaseThresholds {
    /// Starting values matching the doc's own price band — not yet
    /// backtested (no historical flat-base occurrences confirmed in real
    /// data yet, unlike ignition's confirmation_trade_count or
    /// momentum's qualify threshold). Revisit once real low-float
    /// flat-base data exists.
    fn default() -> Self {
        Self {
            max_price_for_gate: 0.25,
            lookback_trades: 20,
            max_range_ratio: 0.03,
        }
    }
}

/// True if `price` falls in the low-float band this gate applies to at
/// all — callers should skip the flat-base check entirely otherwise
/// (the doc's "only tightens... stocks matching the low-price profile").
pub fn in_gated_price_band(price: f64, thresholds: &FlatBaseThresholds) -> bool {
    price > 0.0 && price <= thresholds.max_price_for_gate
}

/// Did the `lookback_trades` most recent trades (chronological, oldest
/// first — the same order `IgnitionMonitor`'s rolling window keeps) trade
/// in a tight range? `false` if there isn't enough history yet — fail
/// closed, same reasoning as everywhere else unknown/insufficient data
/// appears in this codebase: a candidate that can't be confirmed flat
/// doesn't get to fire just because we haven't seen enough yet.
pub fn is_flat_base(recent_trades: &[Trade], thresholds: &FlatBaseThresholds) -> bool {
    if recent_trades.len() < thresholds.lookback_trades {
        return false;
    }
    let window = &recent_trades[recent_trades.len() - thresholds.lookback_trades..];

    let mut high = f64::MIN;
    let mut low = f64::MAX;
    let mut sum = 0.0;
    for t in window {
        high = high.max(t.price);
        low = low.min(t.price);
        sum += t.price;
    }
    let avg = sum / window.len() as f64;
    if avg <= 0.0 {
        return false;
    }

    (high - low) / avg <= thresholds.max_range_ratio
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(price: f64) -> Trade {
        Trade {
            timestamp_secs: 0.0,
            price,
            size: 100,
        }
    }

    #[test]
    fn in_gated_price_band_only_covers_the_low_price_range() {
        let t = FlatBaseThresholds::default();
        assert!(in_gated_price_band(0.20, &t));
        assert!(in_gated_price_band(0.25, &t)); // inclusive at the boundary
        assert!(!in_gated_price_band(0.26, &t));
        assert!(!in_gated_price_band(5.00, &t));
        assert!(!in_gated_price_band(0.0, &t));
    }

    #[test]
    fn is_flat_base_true_for_a_tight_range() {
        let t = FlatBaseThresholds {
            lookback_trades: 5,
            max_range_ratio: 0.03,
            ..FlatBaseThresholds::default()
        };
        let trades: Vec<Trade> = [0.20, 0.201, 0.199, 0.202, 0.198].into_iter().map(trade).collect();
        assert!(is_flat_base(&trades, &t));
    }

    #[test]
    fn is_flat_base_false_for_a_wide_range() {
        let t = FlatBaseThresholds {
            lookback_trades: 5,
            max_range_ratio: 0.03,
            ..FlatBaseThresholds::default()
        };
        let trades: Vec<Trade> = [0.20, 0.22, 0.19, 0.25, 0.18].into_iter().map(trade).collect();
        assert!(!is_flat_base(&trades, &t));
    }

    #[test]
    fn is_flat_base_fails_closed_on_insufficient_history() {
        let t = FlatBaseThresholds {
            lookback_trades: 10,
            ..FlatBaseThresholds::default()
        };
        let trades: Vec<Trade> = [0.20, 0.20, 0.20].into_iter().map(trade).collect();
        assert!(!is_flat_base(&trades, &t));
    }
}
