//! Chart-only trade aggregation. Detector inputs and provider bars are unchanged.
//! Event time determines OHLC; a monotonic clock controls publication.
use crate::{ScanEvent, Trade};
use chrono::{DateTime, Utc};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

pub const PUBLISH_INTERVAL: Duration = Duration::from_millis(50);
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(25);
// Bounded correction window, measured in populated buckets, not wall time.
const RETAINED_BUCKETS: usize = 20;

#[derive(Default)]
pub struct ChartBars {
    buckets: BTreeMap<DateTime<Utc>, Bucket>,
    /// Trades older than the retained correction window are not silently accepted.
    pub rejected_old: u64,
    pub rejected_invalid: u64,
}

struct Bucket {
    first: DateTime<Utc>,
    last: DateTime<Utc>,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: u64,
    published: Option<Instant>,
    dirty: bool,
}

pub fn floor_to_interval(t: DateTime<Utc>, seconds: i64) -> DateTime<Utc> {
    assert!(seconds > 0);
    DateTime::from_timestamp(t.timestamp() - t.timestamp().rem_euclid(seconds), 0).unwrap()
}

impl ChartBars {
    pub fn on_trade(&mut self, trade: &Trade, seconds: i64, now: Instant) -> Vec<ScanEvent> {
        if !trade.price.is_finite() || trade.price <= 0.0 || trade.size == 0 {
            self.rejected_invalid += 1;
            if self.rejected_invalid.is_power_of_two() {
                tracing::warn!(symbol = %trade.symbol, count = self.rejected_invalid, "invalid chart trade rejected");
            }
            return Vec::new();
        }
        let start = floor_to_interval(trade.timestamp, seconds);
        if self.buckets.len() >= RETAINED_BUCKETS
            && start < *self.buckets.first_key_value().unwrap().0
        {
            self.rejected_old += 1;
            if self.rejected_old.is_power_of_two() {
                tracing::warn!(symbol = %trade.symbol, count = self.rejected_old, "chart trade older than retained correction window");
            }
            return Vec::new();
        }
        let rollover = self
            .buckets
            .last_key_value()
            .is_some_and(|(last, _)| start > *last);
        // Publish the completed bucket's tail before moving to the next bucket.
        let mut output = if rollover {
            self.flush(&trade.symbol, seconds, now, true)
        } else {
            Vec::new()
        };
        let b = self.buckets.entry(start).or_insert(Bucket {
            first: trade.timestamp,
            last: trade.timestamp,
            open: trade.price,
            high: trade.price,
            low: trade.price,
            close: trade.price,
            volume: 0,
            published: None,
            dirty: false,
        });
        if trade.timestamp < b.first {
            b.first = trade.timestamp;
            b.open = trade.price;
        }
        // Equal-time trades retain arrival order; upstream IDs are not in Trade yet.
        if trade.timestamp >= b.last {
            b.last = trade.timestamp;
            b.close = trade.price;
        }
        b.high = b.high.max(trade.price);
        b.low = b.low.min(trade.price);
        b.volume += trade.size;
        b.dirty = true;
        if b.published
            .is_none_or(|at| now.duration_since(at) >= PUBLISH_INTERVAL)
        {
            output.push(b.publish(&trade.symbol, start, seconds, now));
        }
        while self.buckets.len() > RETAINED_BUCKETS {
            self.buckets.pop_first();
        }
        output
    }

    /// Called even with no new market messages, so sparse/premarket tails flush.
    pub fn flush(
        &mut self,
        symbol: &str,
        seconds: i64,
        now: Instant,
        force: bool,
    ) -> Vec<ScanEvent> {
        self.buckets
            .iter_mut()
            .filter_map(|(start, b)| {
                (b.dirty
                    && (force
                        || b.published
                            .is_none_or(|at| now.duration_since(at) >= PUBLISH_INTERVAL)))
                .then(|| b.publish(symbol, *start, seconds, now))
            })
            .collect()
    }
}

impl Bucket {
    fn publish(
        &mut self,
        symbol: &str,
        timestamp: DateTime<Utc>,
        seconds: i64,
        now: Instant,
    ) -> ScanEvent {
        self.published = Some(now);
        self.dirty = false;
        // Only provider official/corrected minute bars claim authoritative finality.
        ScanEvent::BarUpdate {
            symbol: symbol.into(),
            timestamp,
            open: self.open,
            high: self.high,
            low: self.low,
            close: self.close,
            volume: self.volume,
            interval_secs: seconds as u32,
            is_final: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn trade(at: &str, price: f64, size: u64) -> Trade {
        Trade {
            symbol: "AUDIT".into(),
            timestamp: at.parse().unwrap(),
            price,
            size,
            conditions: vec![],
        }
    }
    fn values(e: &ScanEvent) -> (f64, f64, f64, f64, u64) {
        if let ScanEvent::BarUpdate {
            open,
            high,
            low,
            close,
            volume,
            ..
        } = e
        {
            (*open, *high, *low, *close, *volume)
        } else {
            panic!()
        }
    }
    #[test]
    fn sparse_premarket_tail_flushes_without_a_next_trade() {
        for seconds in [30, 60] {
            let mut bars = ChartBars::default();
            let now = Instant::now();
            assert_eq!(
                bars.on_trade(&trade("2026-09-18T08:00:00Z", 10., 2), seconds, now)
                    .len(),
                1
            );
            assert!(bars
                .on_trade(
                    &trade("2026-09-18T08:00:00.010Z", 12., 3),
                    seconds,
                    now + Duration::from_millis(10)
                )
                .is_empty());
            let flushed = bars.flush("AUDIT", seconds, now + PUBLISH_INTERVAL, false);
            assert_eq!(values(&flushed[0]), (10., 12., 10., 12., 5));
            assert!(bars
                .flush("AUDIT", seconds, now + PUBLISH_INTERVAL, false)
                .is_empty());
        }
    }
    #[test]
    fn rollover_keeps_tail_and_late_trade_does_not_rewind_current_bucket() {
        let mut bars = ChartBars::default();
        let now = Instant::now();
        bars.on_trade(&trade("2026-09-18T13:29:59.900Z", 10., 2), 30, now);
        bars.on_trade(
            &trade("2026-09-18T13:29:59.950Z", 15., 3),
            30,
            now + Duration::from_millis(1),
        );
        let out = bars.on_trade(
            &trade("2026-09-18T13:30:00Z", 20., 4),
            30,
            now + Duration::from_millis(2),
        );
        assert_eq!(out.len(), 2);
        assert_eq!(values(&out[0]), (10., 15., 10., 15., 5));
        bars.on_trade(
            &trade("2026-09-18T13:29:59.800Z", 8., 1),
            30,
            now + Duration::from_millis(3),
        );
        bars.on_trade(
            &trade("2026-09-18T13:30:01Z", 21., 5),
            30,
            now + Duration::from_millis(4),
        );
        let out = bars.flush("AUDIT", 30, now + Duration::from_millis(100), false);
        assert_eq!(values(&out[0]), (8., 15., 8., 15., 6));
        assert_eq!(values(&out[1]), (20., 21., 20., 21., 9));
    }
    #[test]
    fn summer_winter_premarket_and_regular_boundaries_are_exact() {
        for boundary in [
            "2026-09-18T08:00:00Z",
            "2026-01-09T09:00:00Z",
            "2026-09-18T13:30:00Z",
            "2026-01-09T14:30:00Z",
        ] {
            let at: DateTime<Utc> = boundary.parse().unwrap();
            for secs in [30, 60, 300] {
                assert_eq!(floor_to_interval(at, secs), at);
                assert_eq!(
                    floor_to_interval(at - chrono::Duration::nanoseconds(1), secs),
                    at - chrono::Duration::seconds(secs)
                );
            }
        }
    }
    #[test]
    fn correction_window_is_bounded_and_invalid_input_rejected() {
        let mut bars = ChartBars::default();
        let now = Instant::now();
        let mut t = trade("2026-09-18T08:00:00Z", 10., 1);
        for i in 0..25 {
            t.timestamp = floor_to_interval(t.timestamp, 30) + chrono::Duration::seconds(30);
            bars.on_trade(&t, 30, now + Duration::from_secs(i));
        }
        assert_eq!(bars.buckets.len(), RETAINED_BUCKETS);
        bars.on_trade(
            &trade("2026-09-18T08:00:00Z", 2., 1),
            30,
            now + Duration::from_secs(26),
        );
        assert_eq!(bars.rejected_old, 1);
        t.price = f64::NAN;
        bars.on_trade(&t, 30, now + Duration::from_secs(27));
        assert_eq!(bars.rejected_invalid, 1);
    }

    #[test]
    fn deterministic_ohlcv_matches_sorted_trade_oracle_for_all_intervals() {
        // Includes the final ns before the open and multiple trades with equal times.
        let inputs = [
            trade("2026-09-18T13:29:59.999999999Z", 7., 2),
            trade("2026-09-18T13:30:29Z", 14., 3),
            trade("2026-09-18T13:30:00Z", 10., 5),
            trade("2026-09-18T13:30:29Z", 13., 7),
            trade("2026-09-18T13:30:30Z", 20., 11),
            trade("2026-09-18T13:30:59.999999999Z", 9., 13),
            trade("2026-09-18T13:31:00Z", 11., 17),
            trade("2026-09-18T13:34:59.999999999Z", 30., 19),
            trade("2026-09-18T13:35:00Z", 25., 23),
        ];
        for secs in [30, 60, 300] {
            let mut actual = BTreeMap::new();
            let mut bars = ChartBars::default();
            let now = Instant::now();
            for (i, t) in inputs.iter().enumerate() {
                for event in bars.on_trade(t, secs, now + Duration::from_millis(i as u64)) {
                    if let ScanEvent::BarUpdate { timestamp, .. } = event {
                        actual.insert(timestamp, values(&event));
                    }
                }
            }
            for event in bars.flush("AUDIT", secs, now + Duration::from_secs(1), true) {
                if let ScanEvent::BarUpdate { timestamp, .. } = event {
                    actual.insert(timestamp, values(&event));
                }
            }
            let mut sorted = inputs.to_vec();
            sorted.sort_by_key(|t| t.timestamp);
            let mut expected: BTreeMap<DateTime<Utc>, (f64, f64, f64, f64, u64)> = BTreeMap::new();
            for t in sorted {
                let b = expected
                    .entry(floor_to_interval(t.timestamp, secs))
                    .or_insert((t.price, t.price, t.price, t.price, 0));
                b.1 = b.1.max(t.price);
                b.2 = b.2.min(t.price);
                b.3 = t.price;
                b.4 += t.size;
            }
            assert_eq!(actual, expected, "interval {secs}");
        }
    }
}
