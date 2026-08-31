//! LULD reference price — architecture doc part-3: "the average trade
//! price over the trailing 5 minutes, which only updates once the new
//! average is at least 1% away from the current one." Two distinct
//! mechanics in one sentence, both implemented here: a rolling-window
//! average (not the live tick price itself), and hysteresis on top of it
//! (the reference doesn't just track that average continuously — it
//! holds until the average has moved far enough to justify a step).

use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReferencePriceConfig {
    /// Trailing window the average is computed over — 300.0 (5 minutes)
    /// per the doc.
    pub window_secs: f64,
    /// The rolling average must move at least this % away from the
    /// current reference before the reference actually updates — 1.0 per
    /// the doc.
    pub update_threshold_pct: f64,
}

impl Default for ReferencePriceConfig {
    fn default() -> Self {
        Self {
            window_secs: 300.0,
            update_threshold_pct: 1.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReferencePriceTracker {
    config: ReferencePriceConfig,
    window: VecDeque<(f64, f64)>, // (timestamp_secs, price)
    current: Option<f64>,
}

impl ReferencePriceTracker {
    pub fn new(config: ReferencePriceConfig) -> Self {
        Self {
            config,
            window: VecDeque::new(),
            current: None,
        }
    }

    /// Feeds in one trade and returns the reference price *after*
    /// processing it — which may or may not have actually changed.
    pub fn on_trade(&mut self, timestamp_secs: f64, price: f64) -> f64 {
        self.window.push_back((timestamp_secs, price));
        let cutoff = timestamp_secs - self.config.window_secs;
        while self
            .window
            .front()
            .is_some_and(|&(t, _)| t < cutoff)
        {
            self.window.pop_front();
        }

        let windowed_avg = self.window.iter().map(|&(_, p)| p).sum::<f64>() / self.window.len() as f64;

        match self.current {
            // First-ever reading bootstraps directly — no prior reference
            // to hold hysteresis against.
            None => self.current = Some(windowed_avg),
            Some(current) if current > 0.0 => {
                let move_pct = (windowed_avg - current).abs() / current * 100.0;
                if move_pct >= self.config.update_threshold_pct {
                    self.current = Some(windowed_avg);
                }
            }
            Some(_) => self.current = Some(windowed_avg),
        }

        self.current.expect("just set above")
    }

    pub fn current(&self) -> Option<f64> {
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker() -> ReferencePriceTracker {
        ReferencePriceTracker::new(ReferencePriceConfig::default())
    }

    #[test]
    fn first_trade_bootstraps_the_reference_directly() {
        let mut t = tracker();
        let reference = t.on_trade(0.0, 5.00);
        assert_eq!(reference, 5.00);
    }

    #[test]
    fn small_moves_within_hysteresis_dont_update_the_reference() {
        let mut t = tracker();
        t.on_trade(0.0, 5.00);
        // 5.02 is a 0.4% move from 5.00 — well under the 1% threshold.
        let reference = t.on_trade(1.0, 5.02);
        assert_eq!(reference, 5.00, "reference should hold, not track every tick");
    }

    #[test]
    fn a_sustained_move_past_the_threshold_updates_the_reference() {
        let mut t = tracker();
        t.on_trade(0.0, 5.00);
        // Push enough trades at a clearly-higher price that the rolling
        // average itself crosses 1% away from 5.00.
        for i in 1..20 {
            t.on_trade(i as f64, 5.20);
        }
        let reference = t.current().unwrap();
        assert!(reference > 5.00, "reference should have stepped up, got {reference}");
    }

    #[test]
    fn trades_older_than_the_window_stop_influencing_the_average() {
        let mut t = tracker();
        // Old trades at a low price...
        t.on_trade(0.0, 1.00);
        t.on_trade(1.0, 1.00);
        // ...then the window rolls forward past them (300s later) with
        // new trades at a much higher price. If the old $1.00 trades were
        // still counted, the average would sit well below 5.00.
        for i in 0..10 {
            t.on_trade(400.0 + i as f64, 5.00);
        }
        let reference = t.current().unwrap();
        assert!(
            (reference - 5.00).abs() < 0.01,
            "stale trades should have aged out of the window, got {reference}"
        );
    }
}
