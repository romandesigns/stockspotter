//! Turns a running stream of realtime bars into `TickerSnapshot`s the fast
//! funnel can evaluate. The bar stream alone only ever carries OHLCV for
//! the current bar — session-cumulative volume, prior close (for gap %),
//! and float all have to come from somewhere else, which is what this
//! module (plus `rest::fetch_daily_seeds`) exists to bridge.

use fast_funnel::TickerSnapshot;

use crate::bar::Bar;

#[derive(Debug, Clone)]
pub struct SessionTracker {
    pub symbol: String,
    pub prior_close: f64,
    pub avg_daily_volume: u64,
    /// Free-float share count. Alpaca's market data API has no float
    /// endpoint, so this is `None` until a separate float data source is
    /// wired in — Stage 1 fails closed on unknown float (see
    /// `fast_funnel::types`), which is the correct, conservative behavior,
    /// not a bug: it means the funnel currently can't clear any ticker on
    /// float alone rather than silently ignoring the check.
    pub float_shares: Option<u64>,
    session_volume: u64,
    last_price: f64,
}

impl SessionTracker {
    pub fn new(
        symbol: String,
        prior_close: f64,
        avg_daily_volume: u64,
        float_shares: Option<u64>,
    ) -> Self {
        Self {
            symbol,
            prior_close,
            avg_daily_volume,
            float_shares,
            session_volume: 0,
            last_price: prior_close,
        }
    }

    /// Folds one incoming bar into running session state and returns the
    /// resulting snapshot. Volume accumulates across the whole session
    /// (never resets mid-run) — callers that need a fresh session (e.g. a
    /// new trading day) should construct a new tracker.
    pub fn on_bar(&mut self, bar: &Bar) -> TickerSnapshot {
        self.session_volume += bar.volume;
        self.last_price = bar.close;

        let gap_pct = if self.prior_close > 0.0 {
            (self.last_price - self.prior_close) / self.prior_close * 100.0
        } else {
            0.0
        };

        TickerSnapshot {
            symbol: self.symbol.clone(),
            price: self.last_price,
            float_shares: self.float_shares,
            avg_daily_volume: self.avg_daily_volume,
            session_volume: self.session_volume,
            gap_pct,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn bar(close: f64, volume: u64) -> Bar {
        Bar {
            symbol: "TEST".to_string(),
            open: close,
            high: close,
            low: close,
            close,
            volume,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn session_volume_accumulates_across_bars() {
        let mut t = SessionTracker::new("TEST".to_string(), 5.0, 1_000_000, Some(1_000_000));
        t.on_bar(&bar(5.1, 1000));
        let snap = t.on_bar(&bar(5.2, 500));
        assert_eq!(snap.session_volume, 1500);
        assert_eq!(snap.price, 5.2);
    }

    #[test]
    fn gap_pct_computed_against_prior_close_not_first_bar() {
        let mut t = SessionTracker::new("TEST".to_string(), 10.0, 1_000_000, None);
        let snap = t.on_bar(&bar(12.0, 100));
        assert!((snap.gap_pct - 20.0).abs() < 1e-9);
    }

    #[test]
    fn zero_prior_close_does_not_divide_by_zero() {
        let mut t = SessionTracker::new("TEST".to_string(), 0.0, 1_000_000, None);
        let snap = t.on_bar(&bar(5.0, 100));
        assert_eq!(snap.gap_pct, 0.0);
    }
}
