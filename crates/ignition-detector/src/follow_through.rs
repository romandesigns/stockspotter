//! Follow-through confirmation window — architecture doc section 4.3,
//! "to filter fake spikes / liquidity grabs before alerting". Runs
//! *after* `detect::detect` flags a candidate: takes the price series that
//! followed the candidate signal and decides whether it actually held up,
//! rather than alerting the instant a raw signal crosses its threshold.
//! This is deliberately a separate pure function, not folded into
//! `detect()` — the doc frames it as a distinct second stage with its own
//! purpose (filtering, not detecting), and callers need the raw
//! candidate signal and the confirmation result independently (e.g. to
//! show "candidate, awaiting confirmation" in a live UI).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FollowThroughThresholds {
    /// How far below the breakout level counts as "fully round-tripped"
    /// rather than held — a fraction of the breakout level (e.g. 0.02 =
    /// 2% give-back tolerance).
    pub round_trip_tolerance: f64,
    /// A dip must recover by at least this fraction above its own low to
    /// count as "bought" rather than air-pocketing further down.
    pub dip_recovery_margin: f64,
}

impl Default for FollowThroughThresholds {
    fn default() -> Self {
        Self {
            round_trip_tolerance: 0.02,
            dip_recovery_margin: 0.005,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FollowThroughResult {
    pub held_above_breakout: bool,
    pub dips_bought: bool,
    pub confirmed: bool,
}

/// `prices_after` is the price series (chronological) observed *after*
/// the candidate ignition signal fired — typically hundreds of ms to ~1s
/// of subsequent ticks, per the doc's own framing of the cost of waiting
/// for confirmation.
pub fn confirm(
    breakout_level: f64,
    prices_after: &[f64],
    thresholds: &FollowThroughThresholds,
) -> FollowThroughResult {
    if prices_after.is_empty() {
        return FollowThroughResult {
            held_above_breakout: false,
            dips_bought: false,
            confirmed: false,
        };
    }

    let floor = breakout_level * (1.0 - thresholds.round_trip_tolerance);
    let held_above_breakout = prices_after.iter().all(|&p| p >= floor);

    let dips_bought = local_dips(prices_after).into_iter().all(|(idx, dip_price)| {
        prices_after[idx + 1..]
            .iter()
            .any(|&later| later >= dip_price * (1.0 + thresholds.dip_recovery_margin))
    });

    FollowThroughResult {
        held_above_breakout,
        dips_bought,
        confirmed: held_above_breakout && dips_bought,
    }
}

/// Every decline away from the running peak, tracked until price recovers
/// back to that peak. Each such decline's lowest point is a "dip" that
/// needs a later recovery for `dips_bought` to hold.
///
/// Deliberately *not* just "points below both neighbors" (a plain local-
/// minimum test): a straight, unbroken decline to the end of the window
/// has no interior V-shape at all, so that definition would let a pure
/// air-pocket — the exact failure mode the doc calls out — pass through
/// as "no dips, trivially bought". Tracking the still-open decline at the
/// end of the series as an unresolved dip (with nothing after it to
/// recover from) is what actually catches that case.
fn local_dips(prices: &[f64]) -> Vec<(usize, f64)> {
    let mut dips = Vec::new();
    let Some(&first) = prices.first() else {
        return dips;
    };
    let mut peak = first;
    let mut in_decline = false;
    let mut trough_idx = 0;
    let mut trough_val = first;

    for (i, &price) in prices.iter().enumerate().skip(1) {
        if price < peak {
            if !in_decline || price < trough_val {
                trough_idx = i;
                trough_val = price;
            }
            in_decline = true;
        } else {
            if in_decline {
                dips.push((trough_idx, trough_val));
                in_decline = false;
            }
            peak = price;
        }
    }
    if in_decline {
        dips.push((trough_idx, trough_val));
    }
    dips
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirms_a_clean_hold_with_no_dips() {
        let result = confirm(5.00, &[5.01, 5.02, 5.03, 5.05], &FollowThroughThresholds::default());
        assert!(result.confirmed);
    }

    #[test]
    fn rejects_a_full_round_trip_below_breakout() {
        let result = confirm(5.00, &[5.02, 4.90, 4.80], &FollowThroughThresholds::default());
        assert!(!result.held_above_breakout);
        assert!(!result.confirmed);
    }

    #[test]
    fn confirms_a_dip_that_gets_bought() {
        let prices = vec![5.05, 5.02, 4.99, 5.03, 5.06]; // dip at 4.99, recovers
        let result = confirm(5.00, &prices, &FollowThroughThresholds::default());
        assert!(result.dips_bought);
        assert!(result.confirmed);
    }

    #[test]
    fn rejects_a_dip_that_air_pockets_without_recovering() {
        let prices = vec![5.05, 5.02, 4.99, 4.97, 4.95]; // dip at 4.99, never recovers
        let result = confirm(5.00, &prices, &FollowThroughThresholds::default());
        assert!(!result.dips_bought);
        assert!(!result.confirmed);
    }

    #[test]
    fn empty_series_never_confirms() {
        let result = confirm(5.00, &[], &FollowThroughThresholds::default());
        assert!(!result.confirmed);
    }
}
