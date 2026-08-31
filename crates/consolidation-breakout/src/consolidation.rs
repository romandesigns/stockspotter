//! Per-candle validity check for the consolidation phase, and the
//! breakout trigger — the doc's three conditions ("volume contraction",
//! "range tightening", "holding above support") plus "the first candle
//! that breaks back above the high of the consolidation range".

use crate::candle::Candle;
use crate::surge::SurgeInfo;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConsolidationThresholds {
    /// How many consecutive valid consolidation candles must hold before
    /// a breakout above them counts as a real entry trigger — the doc's
    /// "once all three conditions above have been holding", not a single
    /// candle's coincidence.
    pub min_consolidation_candles: usize,
    /// Give up and go back to watching for a new surge if consolidation
    /// drags on this long without ever breaking out — an indefinitely
    /// "still consolidating" state isn't useful to a live scanner.
    pub max_consolidation_candles: usize,
    /// A consolidation candle's own high-low range must be no more than
    /// this fraction of the surge's largest single-candle range — the
    /// doc's "range tightening... compared to the ignition move".
    pub max_range_ratio_of_surge: f64,
    /// How many *consecutive* invalid candles are tolerated before giving
    /// up on the current consolidation attempt entirely (see the doc
    /// comment on `monitor::step_consolidation`'s consecutive-invalid
    /// handling — real premarket data showed a single noisy candle
    /// resetting all the way back to "watching for a new surge" throws
    /// away a genuinely-forming consolidation, not just rejects one bad
    /// tick). A valid candle resets the count back to zero.
    pub max_consecutive_invalid: usize,
}

/// Starting values, not yet backtested — same caveat as
/// `SurgeThresholds::default()`. `max_consecutive_invalid` specifically
/// was picked 2026-08-31 after replaying a real premarket session (COOT)
/// where the very first post-surge candle failed on a lagging support
/// value (see `support_level`'s doc comment) despite genuinely clean
/// volume/range — one bad candle is common enough real noise that
/// requiring zero tolerance was throwing away real consolidations.
impl Default for ConsolidationThresholds {
    fn default() -> Self {
        Self {
            min_consolidation_candles: 2,
            max_consolidation_candles: 20,
            max_range_ratio_of_surge: 0.6,
            max_consecutive_invalid: 2,
        }
    }
}

/// The support level a consolidation candle's low must hold above — the
/// higher (tighter) of the surge's own low and a *post-surge* moving
/// average, matching the doc's "e.g., the 9-period moving average, or
/// the low of the ignition candle itself" by using whichever of the two
/// is currently the stricter floor.
///
/// `post_surge_ma` must be computed from candles *after* the surge ended
/// (see `monitor.rs`'s caller) — deliberately not a blanket trailing MA
/// spanning the surge itself. A real premarket replay (COOT, 2026-08-31)
/// found the original blanket-MA version fails immediately after every
/// real surge: the MA is still inflated by the spike's own high closes
/// for several bars afterward, so a perfectly reasonable pullback reads
/// as "below support" purely because the average hasn't caught up yet,
/// not because anything about the pullback itself was invalid.
/// `None` (no post-surge candles yet) falls back to `surge_low` alone
/// rather than blending in a stale/contaminated average.
pub fn support_level(surge_low: f64, post_surge_ma: Option<f64>) -> f64 {
    match post_surge_ma {
        Some(ma) => surge_low.max(ma),
        None => surge_low,
    }
}

/// Checks one candle against all three of the doc's consolidation
/// conditions. `prior` is the previous *consolidation* candle (not the
/// surge candle) — `None` for the first candle after the surge, in which
/// case only the surge-relative volume check applies (there's nothing
/// yet to have "contracted" candle-over-candle from).
pub fn is_valid_consolidation_candle(
    candle: &Candle,
    surge: &SurgeInfo,
    prior: Option<&Candle>,
    support: f64,
    thresholds: &ConsolidationThresholds,
) -> bool {
    let volume_below_surge = (candle.volume as f64) < surge.avg_volume;
    let volume_contracting_from_prior = prior.is_none_or(|p| candle.volume <= p.volume);
    let range_tight = candle.range() <= surge.max_range * thresholds.max_range_ratio_of_surge;
    let holding_support = candle.low >= support;

    volume_below_surge && volume_contracting_from_prior && range_tight && holding_support
}

/// The doc's entry trigger: the first candle whose close breaks back
/// above the consolidation range's high. Uses `close`, not `high` — a
/// wick poking above the range isn't itself a held breakout, matching
/// the same "don't trust a bare touch" caution `ignition_detector`
/// applies via its own follow-through confirmation.
pub fn breakout_triggered(candle: &Candle, consolidation_high: f64) -> bool {
    candle.close > consolidation_high
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candle(open: f64, high: f64, low: f64, close: f64, volume: u64) -> Candle {
        Candle { open, high, low, close, volume }
    }

    fn surge() -> SurgeInfo {
        SurgeInfo {
            low: 1.00,
            high: 1.20,
            avg_volume: 5000.0,
            max_range: 0.06,
        }
    }

    #[test]
    fn valid_consolidation_candle_passes_all_three_conditions() {
        let c = candle(1.18, 1.19, 1.17, 1.18, 1500); // low vol, tight range, above support
        assert!(is_valid_consolidation_candle(&c, &surge(), None, 1.00, &ConsolidationThresholds::default()));
    }

    #[test]
    fn rejects_when_volume_does_not_contract_below_the_surge() {
        let c = candle(1.18, 1.19, 1.17, 1.18, 6000); // above surge's avg_volume
        assert!(!is_valid_consolidation_candle(&c, &surge(), None, 1.00, &ConsolidationThresholds::default()));
    }

    #[test]
    fn rejects_when_volume_increases_over_the_prior_consolidation_candle() {
        let prior = candle(1.18, 1.19, 1.17, 1.18, 1000);
        let c = candle(1.18, 1.19, 1.17, 1.18, 1500); // more than prior, even though still below surge avg
        assert!(!is_valid_consolidation_candle(&c, &surge(), Some(&prior), 1.00, &ConsolidationThresholds::default()));
    }

    #[test]
    fn rejects_when_range_is_not_tight_enough() {
        let c = candle(1.10, 1.19, 1.00, 1.15, 1500); // range 0.19, way over 60% of surge's 0.06
        assert!(!is_valid_consolidation_candle(&c, &surge(), None, 1.00, &ConsolidationThresholds::default()));
    }

    #[test]
    fn rejects_when_support_breaks() {
        let c = candle(1.18, 1.19, 0.98, 1.18, 1500); // low dips under support (1.00)
        assert!(!is_valid_consolidation_candle(&c, &surge(), None, 1.00, &ConsolidationThresholds::default()));
    }

    #[test]
    fn support_level_uses_the_stricter_of_surge_low_and_post_surge_ma() {
        assert_eq!(support_level(1.00, Some(1.05)), 1.05); // the MA is the tighter floor
        assert_eq!(support_level(1.10, Some(1.05)), 1.10); // surge low is the tighter floor
    }

    #[test]
    fn support_level_falls_back_to_surge_low_with_no_post_surge_ma_yet() {
        // No post-surge candles yet to average — don't blend in a stale
        // MA that's still contaminated by the surge's own closes.
        assert_eq!(support_level(1.00, None), 1.00);
    }

    #[test]
    fn breakout_requires_a_held_close_not_just_a_wick() {
        let wick_only = candle(1.15, 1.22, 1.14, 1.18, 1000); // high pokes above 1.20 but close doesn't
        assert!(!breakout_triggered(&wick_only, 1.20));

        let held_close = candle(1.15, 1.22, 1.14, 1.21, 1000);
        assert!(breakout_triggered(&held_close, 1.20));
    }
}
