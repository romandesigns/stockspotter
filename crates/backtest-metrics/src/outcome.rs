//! Per-signal outcome evaluation — architecture doc section 8, step 2:
//! "For every signal fired..., log the actual subsequent price action."
//!
//! Methodology, made explicit rather than left implicit: a signal is a
//! "hit" if price moves up by `target_pct` before it drops by `stop_pct`,
//! within `lookforward_bars` bars — a simple target/stop model, not
//! anything more elaborate. This is a deliberately transparent starting
//! definition (the doc doesn't specify one), meant to be tuned once real
//! aggregate results exist, not treated as a final answer.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutcomeThresholds {
    /// A favorable move of at least this % (vs the signal price) counts
    /// as a hit.
    pub target_pct: f64,
    /// An adverse move of at least this % counts as a failure — checked
    /// bar-by-bar alongside the target, whichever comes first wins.
    pub stop_pct: f64,
    /// How many bars ahead to evaluate before giving up (neither target
    /// nor stop reached = not a hit, but not counted as a clean stop-out
    /// either — see `SignalOutcome::hit`).
    pub lookforward_bars: usize,
}

impl Default for OutcomeThresholds {
    /// Starting values, not backtested-and-chosen — the whole point of
    /// this crate is to generate the data that would let someone
    /// actually choose these deliberately later.
    fn default() -> Self {
        Self {
            target_pct: 5.0,
            stop_pct: 3.0,
            lookforward_bars: 20,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SignalOutcome {
    /// True if `target_pct` was reached before `stop_pct` within the
    /// lookforward window.
    pub hit: bool,
    /// Best favorable move seen within the window, as a percentage —
    /// recorded even on a miss, so "how close did it get" isn't lost.
    pub max_favorable_pct: f64,
    /// How many bars after the signal the target was reached, if it was.
    pub bars_to_target: Option<usize>,
}

/// `following_prices` is the price series *after* the signal fired,
/// chronological, as many bars as are available (may be shorter than
/// `lookforward_bars` near the end of a replay window — evaluated over
/// whatever exists, not padded or extrapolated).
pub fn evaluate_outcome(
    signal_price: f64,
    following_prices: &[f64],
    thresholds: &OutcomeThresholds,
) -> SignalOutcome {
    let mut max_favorable_pct = 0.0_f64;
    let mut hit = false;
    let mut bars_to_target = None;

    if signal_price <= 0.0 {
        return SignalOutcome {
            hit: false,
            max_favorable_pct: 0.0,
            bars_to_target: None,
        };
    }

    for (i, &price) in following_prices
        .iter()
        .take(thresholds.lookforward_bars)
        .enumerate()
    {
        let pct = (price - signal_price) / signal_price * 100.0;
        max_favorable_pct = max_favorable_pct.max(pct);

        if pct >= thresholds.target_pct {
            hit = true;
            bars_to_target = Some(i + 1);
            break;
        }
        if pct <= -thresholds.stop_pct {
            break;
        }
    }

    SignalOutcome {
        hit,
        max_favorable_pct,
        bars_to_target,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thresholds() -> OutcomeThresholds {
        OutcomeThresholds {
            target_pct: 5.0,
            stop_pct: 3.0,
            lookforward_bars: 10,
        }
    }

    #[test]
    fn hits_when_target_reached_before_stop() {
        let prices = [1.01, 1.02, 1.03, 1.06]; // +6% on the 4th bar
        let outcome = evaluate_outcome(1.00, &prices, &thresholds());
        assert!(outcome.hit);
        assert_eq!(outcome.bars_to_target, Some(4));
        assert!(outcome.max_favorable_pct >= 5.0);
    }

    #[test]
    fn misses_when_stopped_out_before_target() {
        let prices = [0.99, 0.98, 0.965, 1.10]; // -3.5% before the later +10%
        let outcome = evaluate_outcome(1.00, &prices, &thresholds());
        assert!(!outcome.hit);
        assert_eq!(outcome.bars_to_target, None);
    }

    #[test]
    fn misses_when_lookforward_window_runs_out() {
        let prices = [1.01, 1.02, 1.03]; // never reaches +5%, never stops out
        let outcome = evaluate_outcome(1.00, &prices, &thresholds());
        assert!(!outcome.hit);
        assert!((outcome.max_favorable_pct - 3.0).abs() < 1e-9);
    }

    #[test]
    fn respects_lookforward_bars_cap() {
        let mut prices = vec![1.001; 5];
        prices.push(1.10); // +10%, but past the 5-bar cap below
        let thresholds = OutcomeThresholds {
            lookforward_bars: 5,
            ..thresholds()
        };
        let outcome = evaluate_outcome(1.00, &prices, &thresholds);
        assert!(!outcome.hit);
    }

    #[test]
    fn zero_or_negative_signal_price_does_not_divide_by_zero() {
        let outcome = evaluate_outcome(0.0, &[1.0, 2.0], &thresholds());
        assert!(!outcome.hit);
        assert_eq!(outcome.max_favorable_pct, 0.0);
    }
}
