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
    session_date: Option<chrono::NaiveDate>,
    bars: std::collections::BTreeMap<chrono::DateTime<chrono::Utc>, (u64, f64)>,
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
            session_date: None,
            bars: std::collections::BTreeMap::new(),
        }
    }

    pub fn refresh_seed(&mut self, seed: crate::rest::DailySeed) {
        self.prior_close = seed.prior_close;
        self.avg_daily_volume = seed.avg_daily_volume;
    }

    /// Folds one incoming bar into running session state and returns the
    /// resulting snapshot. Volume resets on a new New York date; repeated/corrected bars replace their prior contribution.
    pub fn on_bar(&mut self, bar: &Bar) -> TickerSnapshot {
        let date = bar.timestamp.with_timezone(&chrono_tz::America::New_York).date_naive();
        if self.session_date.is_none_or(|d| date > d) {
            if self.session_date.is_some() {
                // A fresh daily seed is required before the new session can qualify.
                self.avg_daily_volume = 0;
                self.prior_close = 0.0;
            }
            self.session_date = Some(date);
            self.session_volume = 0;
            self.bars.clear();
        }
        if self.session_date == Some(date) {
            let previous = self.bars.insert(bar.timestamp, (bar.volume, bar.close));
            self.session_volume = self.session_volume.saturating_sub(previous.map_or(0, |b| b.0)).saturating_add(bar.volume);
            self.last_price = self.bars.last_key_value().map_or(bar.close, |(_, b)| b.1);
        }

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

    #[test]
    fn corrections_replace_volume_and_old_sessions_cannot_contaminate_new_sessions() {
        let mut tracker = SessionTracker::new("TEST".into(), 10.0, 1000, None);
        let mut first = bar(11.0, 100);
        first.timestamp = "2026-09-03T14:00:00Z".parse().unwrap();
        tracker.on_bar(&first);
        first.volume = 150;
        assert_eq!(tracker.on_bar(&first).session_volume,150);
        let mut second = first.clone();
        second.timestamp = "2026-09-04T14:00:00Z".parse().unwrap();
        second.volume = 20;
        assert_eq!(tracker.on_bar(&second).session_volume,20);
        assert_eq!(tracker.on_bar(&first).session_volume,20);
        assert_eq!(tracker.avg_daily_volume,0);
    }

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
