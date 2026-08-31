//! Surge detection — the "initial surge" the doc's Post-Ignition
//! Consolidation Breakout strategy is meant to watch *after*.
//!
//! Deliberately does **not** consume `ignition_detector`'s alerts —
//! per the doc's own Strategy Isolation principle ("no strategy's
//! detection logic depends on another strategy's state or output"),
//! this crate identifies its own surge independently from the same raw
//! bar data every other strategy sees, rather than literally waiting on
//! `IgnitionMonitor`'s confirmation. In practice a real surge usually
//! trips both this and the ignition detector's tick-level trigger around
//! the same time (that's expected overlap, not a coincidence to route
//! around) — but there is no code-level dependency between them, so
//! either can run, be tested, or be disabled with zero effect on the
//! other, matching how `flat_base`/`halt_detector` were built earlier.

use crate::candle::Candle;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurgeThresholds {
    /// How many of the most recent candles count as "the surge window" —
    /// the move is measured start-to-end across this many candles, not a
    /// single one, since a real ignition move is rarely exactly one bar.
    pub lookback_candles: usize,
    /// How many candles immediately before the surge window establish
    /// "normal" volume to compare against.
    pub baseline_candles: usize,
    /// Minimum % move (low of the window to its high) to count as a
    /// surge, not just ordinary noise.
    pub min_move_pct: f64,
    /// The surge window's average volume must be at least this many
    /// times the baseline's average volume.
    pub min_volume_ratio: f64,
}

/// Starting values, not yet backtested — consistent with the doc's
/// qualitative description (a real, unmistakable surge, not routine
/// movement) rather than a tuned number. Same honesty as
/// `momentum_scorer::DEFAULT_QUALIFY_THRESHOLD`'s original 0.90: revisit
/// once this is wired into `backtest-metrics --bin tune_broad` against
/// the broad session set gathered 2026-08-31.
impl Default for SurgeThresholds {
    fn default() -> Self {
        Self {
            lookback_candles: 5,
            baseline_candles: 20,
            min_move_pct: 8.0,
            min_volume_ratio: 3.0,
        }
    }
}

/// What the surge window looked like — becomes the reference frame the
/// consolidation phase is measured against (its low is a support
/// candidate, its largest candle's range is the "tightness" benchmark
/// consolidation candles must shrink below).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurgeInfo {
    pub low: f64,
    pub high: f64,
    pub avg_volume: f64,
    pub max_range: f64,
}

/// `recent` is the trailing candle history, oldest first — must hold at
/// least `baseline_candles + lookback_candles` entries or this fails
/// closed (`None`), same pattern as `ignition_detector::flat_base`'s
/// insufficient-history handling: an unproven surge is not a surge.
pub fn detect_surge(recent: &[Candle], thresholds: &SurgeThresholds) -> Option<SurgeInfo> {
    let needed = thresholds.baseline_candles + thresholds.lookback_candles;
    if recent.len() < needed {
        return None;
    }

    let split = recent.len() - thresholds.lookback_candles;
    let baseline = &recent[split - thresholds.baseline_candles..split];
    let window = &recent[split..];

    let baseline_avg_volume = baseline.iter().map(|c| c.volume).sum::<u64>() as f64 / baseline.len() as f64;
    if baseline_avg_volume <= 0.0 {
        return None;
    }

    let window_low = window.iter().map(|c| c.low).fold(f64::INFINITY, f64::min);
    let window_high = window.iter().map(|c| c.high).fold(f64::NEG_INFINITY, f64::max);
    if window_low <= 0.0 {
        return None;
    }

    let move_pct = (window_high - window_low) / window_low * 100.0;
    let window_avg_volume = window.iter().map(|c| c.volume).sum::<u64>() as f64 / window.len() as f64;
    let volume_ratio = window_avg_volume / baseline_avg_volume;

    if move_pct >= thresholds.min_move_pct && volume_ratio >= thresholds.min_volume_ratio {
        Some(SurgeInfo {
            low: window_low,
            high: window_high,
            avg_volume: window_avg_volume,
            max_range: window.iter().map(Candle::range).fold(0.0, f64::max),
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candle(open: f64, high: f64, low: f64, close: f64, volume: u64) -> Candle {
        Candle { open, high, low, close, volume }
    }

    fn quiet_baseline(n: usize) -> Vec<Candle> {
        (0..n).map(|_| candle(1.0, 1.01, 0.99, 1.0, 1000)).collect()
    }

    #[test]
    fn no_surge_detected_with_insufficient_history() {
        let recent = quiet_baseline(10); // fewer than the default's needed 25
        assert!(detect_surge(&recent, &SurgeThresholds::default()).is_none());
    }

    #[test]
    fn no_surge_on_ordinary_flat_data() {
        let recent = quiet_baseline(25);
        assert!(detect_surge(&recent, &SurgeThresholds::default()).is_none());
    }

    #[test]
    fn surge_detected_on_a_real_move_with_volume_confirmation() {
        let mut recent = quiet_baseline(20);
        // A 5-candle window climbing from 1.00 to 1.20 (+20%) on ~5x volume.
        recent.push(candle(1.00, 1.05, 1.00, 1.05, 5000));
        recent.push(candle(1.05, 1.10, 1.04, 1.09, 6000));
        recent.push(candle(1.09, 1.15, 1.08, 1.14, 7000));
        recent.push(candle(1.14, 1.18, 1.13, 1.17, 5000));
        recent.push(candle(1.17, 1.20, 1.16, 1.19, 4000));

        let surge = detect_surge(&recent, &SurgeThresholds::default());
        assert!(surge.is_some());
        let surge = surge.unwrap();
        assert!((surge.low - 1.00).abs() < 1e-9);
        assert!((surge.high - 1.20).abs() < 1e-9);
    }

    #[test]
    fn no_surge_when_move_is_real_but_volume_never_confirms() {
        let mut recent = quiet_baseline(20);
        // Same +20% move as above, but volume stays flat — a real move
        // this codebase's other detectors also treat as less trustworthy
        // (see ignition_detector's own volume-gated signals).
        recent.push(candle(1.00, 1.05, 1.00, 1.05, 1000));
        recent.push(candle(1.05, 1.10, 1.04, 1.09, 1000));
        recent.push(candle(1.09, 1.15, 1.08, 1.14, 1000));
        recent.push(candle(1.14, 1.18, 1.13, 1.17, 1000));
        recent.push(candle(1.17, 1.20, 1.16, 1.19, 1000));

        assert!(detect_surge(&recent, &SurgeThresholds::default()).is_none());
    }
}
