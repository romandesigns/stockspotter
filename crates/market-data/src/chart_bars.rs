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

    /// CHARACTERIZATION, not an assertion that the behaviour is correct.
    ///
    /// Live witness, 2026-09-21: DDC's 15:37 UTC minute published a
    /// provisional volume of 96,108 against an authoritative 119,482 --
    /// 23,374 shares / 19.6% missing -- with a silent tail of only ~0.3s
    /// before the boundary. 23,374 shares cannot trade in 300ms, so the
    /// rollover-tail mechanism cannot explain it. The remaining explanation
    /// is that chart coverage of that bucket STARTED LATE and the partial
    /// state was published as though it described the whole minute.
    ///
    /// This test pins down exactly what the aggregator does in that case, so
    /// the contract discussion has a fixed reference and any future semantic
    /// change has something to break.
    ///
    /// What it shows today:
    ///   - `Bucket.first` knows the coverage began mid-bucket.
    ///   - The published BarUpdate carries no field that can express it.
    ///   - So a consumer cannot distinguish a full bucket from a partial one.
    #[test]
    fn mid_bucket_coverage_start_publishes_a_partial_bar_indistinguishable_from_a_full_one() {
        let mut full = ChartBars::default();
        let mut late = ChartBars::default();
        let now = Instant::now();

        // The whole minute, as an observer present from the start would see.
        let whole = [
            ("2026-09-21T15:37:00.100Z", 10.0, 40_000u64),
            ("2026-09-21T15:37:20.000Z", 11.0, 40_000),
            ("2026-09-21T15:37:45.000Z", 12.0, 39_482),
        ];
        for (t, p, sz) in whole {
            full.on_trade(&trade(t, p, sz), 60, now);
        }
        // The same minute, observed only from 15:37:45 -- coverage started late.
        late.on_trade(&trade("2026-09-21T15:37:45.000Z", 12.0, 39_482), 60, now);

        let key = floor_to_interval("2026-09-21T15:37:30Z".parse().unwrap(), 60);
        let f = full.buckets.get(&key).unwrap();
        let l = late.buckets.get(&key).unwrap();

        // The aggregator itself is not wrong about what it saw.
        assert_eq!(f.volume, 119_482);
        assert_eq!(l.volume, 39_482);
        // And it does know coverage began late: `first` differs by 44.9s.
        assert_eq!(f.first.to_rfc3339_opts(chrono::SecondsFormat::Millis, true), "2026-09-21T15:37:00.100Z");
        assert_eq!(l.first.to_rfc3339_opts(chrono::SecondsFormat::Millis, true), "2026-09-21T15:37:45.000Z");
        assert!((l.first - key).num_seconds() >= 44);

        // But the published events are structurally identical in shape: the
        // only difference is the numbers, with nothing to say one describes
        // 60 seconds and the other 15. THIS is the defect.
        //
        // Replay both through the same path production uses -- on_trade
        // returns the publications, and a forced flush drains any tail -- and
        // take the last BarUpdate each one emitted for this bucket.
        let last_bar = |bars: &mut ChartBars, trades: &[(&str, f64, u64)]| {
            let mut out: Vec<ScanEvent> = Vec::new();
            let t0 = Instant::now();
            for (i, (t, p, sz)) in trades.iter().enumerate() {
                // Advance past PUBLISH_INTERVAL so every trade is publishable,
                // mirroring a real stream rather than one instant.
                out.extend(bars.on_trade(&trade(t, *p, *sz), 60, t0 + PUBLISH_INTERVAL * (i as u32 + 1)));
            }
            out.extend(bars.flush("AUDIT", 60, t0 + PUBLISH_INTERVAL * 100, true));
            out.into_iter()
                .filter_map(|e| match e {
                    ScanEvent::BarUpdate { volume, is_final, interval_secs, .. } => Some((volume, is_final, interval_secs)),
                    _ => None,
                })
                .last()
                .expect("expected at least one BarUpdate")
        };

        let mut full2 = ChartBars::default();
        let mut late2 = ChartBars::default();
        let (vf, ff, ivf) = last_bar(&mut full2, &whole);
        let (vl, fl, ivl) = last_bar(&mut late2, &[("2026-09-21T15:37:45.000Z", 12.0, 39_482)]);

        assert_eq!(vf, 119_482, "full-coverage volume");
        assert_eq!(vl, 39_482, "partial-coverage volume -- 67% short, exactly the DDC shape");
        assert_eq!(ivf, 60);
        assert_eq!(ivl, 60);
        // Both merely "provisional". Neither can say "partial coverage", which
        // is what leaves the client unable to tell them apart.
        assert!(!ff);
        assert!(!fl);
    }

    /// 30-SECOND FINALITY CONTRACT.
    ///
    /// The 2026-09-21 session measured 2,394 sub-minute updates and **zero**
    /// carried `isFinal`. That is correct and must stay correct: Alpaca
    /// publishes official minute bars, and there is no authoritative
    /// sub-minute source to be final against. A 30s candle is a reduction of
    /// the raw trades this client happened to receive, nothing more.
    #[test]
    fn a_sub_minute_bar_is_never_marked_final() {
        let mut bars = ChartBars::default();
        let now = Instant::now();
        for s in ["00", "07", "14", "21", "28", "35", "42", "49", "56"] {
            bars.on_trade(&trade(&format!("2026-09-21T15:37:{s}Z"), 10.0, 100), 30, now);
        }
        // Cross a boundary so a rollover publication happens too.
        let mut events = bars.on_trade(&trade("2026-09-21T15:38:02Z", 11.0, 100), 30, now + PUBLISH_INTERVAL * 4);
        events.extend(bars.flush("AUDIT", 30, now + PUBLISH_INTERVAL * 8, true));
        assert!(!events.is_empty(), "expected at least one sub-minute publication");
        for e in &events {
            if let ScanEvent::BarUpdate { interval_secs, is_final, .. } = e {
                assert_eq!(*interval_secs, 30);
                assert!(!*is_final, "a 30s bar must never claim authoritative finality");
            }
        }
    }

    /// The same guarantee for every interval this aggregator serves: nothing
    /// it produces is authoritative, because it aggregates received trades
    /// rather than provider bars. Finality is the provider's word alone.
    #[test]
    fn no_locally_aggregated_bar_of_any_interval_claims_finality() {
        for seconds in [30_i64, 60, 300] {
            let mut bars = ChartBars::default();
            let now = Instant::now();
            let mut evs = bars.on_trade(&trade("2026-09-21T15:37:01Z", 10.0, 100), seconds, now);
            evs.extend(bars.on_trade(&trade("2026-09-21T15:39:01Z", 11.0, 100), seconds, now + PUBLISH_INTERVAL * 2));
            evs.extend(bars.flush("AUDIT", seconds, now + PUBLISH_INTERVAL * 4, true));
            assert!(!evs.is_empty(), "interval {seconds}s produced no publication");
            for e in evs {
                if let ScanEvent::BarUpdate { is_final, .. } = e {
                    assert!(!is_final, "interval {seconds}s claimed finality");
                }
            }
        }
    }
}
