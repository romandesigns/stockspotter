//! Large-trade detection (specification section 8).
//!
//! "Large" is relative to the symbol's own recent prints, never a
//! universal share count: 10,000 shares is routine on one name and an
//! event on another. A print is large when it is at or above BOTH the
//! 95th percentile of the last `capacity` trade sizes and five times
//! their median. The second condition is what keeps a tape of identical
//! 100-share prints from calling every one of them large (there the 95th
//! percentile is 100 too).
//!
//! Both numbers are untuned defaults; see `classify.rs` on why nothing
//! here should be read as calibrated.

use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub struct LargeTradeDetector {
    sizes: VecDeque<u64>,
    capacity: usize,
    min_samples: usize,
    percentile: f64,
    median_multiple: f64,
}

impl Default for LargeTradeDetector {
    fn default() -> Self {
        Self { sizes: VecDeque::new(), capacity: 500, min_samples: 50, percentile: 0.95, median_multiple: 5.0 }
    }
}

impl LargeTradeDetector {
    /// Smallest size currently counted as large. `None` until enough
    /// prints have been seen to describe the distribution at all.
    pub fn threshold(&self) -> Option<u64> {
        if self.sizes.len() < self.min_samples {
            return None;
        }
        let mut sorted: Vec<u64> = self.sizes.iter().copied().collect();
        sorted.sort_unstable();
        let at_percentile = sorted[((sorted.len() - 1) as f64 * self.percentile).round() as usize];
        let median = sorted[sorted.len() / 2];
        Some(at_percentile.max((median as f64 * self.median_multiple).ceil() as u64).max(1))
    }

    /// Judges `size` against the prints seen BEFORE it, then records it.
    /// Judging first matters: a single enormous print must not raise the
    /// bar it is itself measured against.
    pub fn observe(&mut self, size: u64) -> bool {
        let large = self.threshold().is_some_and(|t| size >= t);
        self.sizes.push_back(size);
        if self.sizes.len() > self.capacity {
            self.sizes.pop_front();
        }
        large
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_large_before_the_distribution_is_known() {
        let mut d = LargeTradeDetector::default();
        for _ in 0..49 {
            assert!(!d.observe(1_000_000));
        }
        assert_eq!(d.threshold(), None);
    }

    #[test]
    fn uniform_tape_has_no_large_trades() {
        let mut d = LargeTradeDetector::default();
        for _ in 0..200 {
            assert!(!d.observe(100));
        }
        assert_eq!(d.threshold(), Some(500));
    }

    #[test]
    fn a_print_far_above_the_recent_distribution_is_large() {
        let mut d = LargeTradeDetector::default();
        for i in 0..200u64 {
            d.observe(100 + (i % 10) * 20);
        }
        assert!(!d.observe(300));
        assert!(d.observe(5_000));
    }

    #[test]
    fn threshold_is_relative_to_the_symbol() {
        let mut busy = LargeTradeDetector::default();
        for i in 0..200u64 {
            busy.observe(5_000 + (i % 10) * 500);
        }
        // 10,000 shares is ordinary here and would be huge on a quiet name.
        assert!(!busy.observe(10_000));
    }
}
