//! The still-forming chart candle built from raw trade prints, and the rule
//! for which prints may move it.
//!
//! Two defects this replaces (diagnosis 2026-10-16):
//!
//! - **Late prints reset the candle.** The old update compared buckets with
//!   `!=`, so one print from the previous minute (late reports arrive all
//!   session) restarted the bar at that minute with a single price, and the
//!   next current print restarted it again: the open jumped, the wicks
//!   vanished and the volume started over. Only a NEWER bucket may start a
//!   bar now; an older one is ignored, because Alpaca's official minute bar
//!   already includes whatever correction it carries.
//! - **Every print counted.** Odd lots, average-price reports, prior-reference
//!   and out-of-sequence prints moved the high and low, which the official bar
//!   never does. `trade_updates` applies the same per-condition table Alpaca
//!   aggregates its own minute bars with.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use crate::events::ScanEvent;

/// Whether one print may move a minute bar's price (open/high/low/close)
/// and whether it adds to the bar's volume.
///
/// Source: Alpaca's Market Data FAQ, "How are bars aggregated?"
/// (<https://docs.alpaca.markets/docs/market-data-faq#how-are-bars-aggregated>,
/// read 2026-10-16), minute-bar columns. It follows the CTS/UTP guidance and
/// Alpaca states it is the authoritative list for its own minute bars; for
/// minute bars O/C and H/L always agree, so one flag covers all four.
/// "Multiple conditions follow the strictest rule", so a print updates a
/// field only if every one of its conditions allows it.
///
/// | Updates           | Conditions                                                        |
/// |-------------------|-------------------------------------------------------------------|
/// | nothing           | M official close, Q official open, 9 corrected consolidated close |
/// | volume only       | B average price (CTA), C cash, G bunched sold, H price variation, I odd lot, N next day, P prior reference price, R seller, U extended trading hours, V contingent, W average price, Z sold out of sequence, 4 derivatively priced, 7 qualified contingent |
/// | price and volume  | everything else, including regular sale (blank/@), F ISO, O opening, 5 reopening, 6 closing, T extended hours, X cross |
///
/// `B` is the one code whose meaning depends on the tape: average-price on
/// CTA (tapes A/B, volume only) but bunched trade on UTP (tape C, counts in
/// full). `Trade` does not carry the tape, so the UTP/OTC regular-sale code
/// `@`, which CTA never uses, stands in for it; a `B` without `@` is taken as
/// CTA's average-price report, the conservative reading.
pub fn trade_updates(conditions: &[String]) -> (bool, bool) {
    let utp = conditions.iter().any(|c| c.trim() == "@");
    let mut ohlc = true;
    let mut volume = true;
    for condition in conditions {
        match condition.trim() {
            "M" | "Q" | "9" => {
                ohlc = false;
                volume = false;
            }
            "B" if utp => {}
            "B" | "C" | "G" | "H" | "I" | "N" | "P" | "R" | "U" | "V" | "W" | "Z" | "4" | "7" => ohlc = false,
            _ => {}
        }
    }
    (ohlc, volume)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Ohlc {
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
}

/// Running OHLCV for one symbol's current bucket of `interval_secs`.
#[derive(Debug, Clone)]
pub(crate) struct FormingBar {
    pub bucket_start: DateTime<Utc>,
    /// `None` until a price-eligible print arrives: a bucket opened by an odd
    /// lot has volume but no price yet, and is never sent like that (Alpaca
    /// emits no bar with a zero field either).
    pub ohlc: Option<Ohlc>,
    pub volume: u64,
    pub last_broadcast: Instant,
    /// Changed since it was last sent.
    pub dirty: bool,
    /// Closed: sent as final, or superseded by the official bar. Prints for a
    /// closed bucket are late and ignored like any older-bucket print.
    pub closed: bool,
}

/// What one print did to a series.
#[derive(Debug)]
pub(crate) enum TradeStep {
    /// Late (older bucket or a closed one) or excluded by its conditions.
    Ignored,
    /// Moved the current bucket.
    Updated,
    /// Opened a newer bucket; the one it replaced is returned so the caller
    /// can close it.
    Rolled(FormingBar),
}

/// One symbol's forming bar at one interval.
#[derive(Debug, Default)]
pub(crate) struct BarSeries {
    pub bar: Option<FormingBar>,
}

pub(crate) fn floor_to_interval(t: DateTime<Utc>, interval_secs: i64) -> DateTime<Utc> {
    let secs = t.timestamp();
    let floored = secs - secs.rem_euclid(interval_secs);
    DateTime::from_timestamp(floored, 0).unwrap_or(t)
}

impl FormingBar {
    fn new(bucket_start: DateTime<Utc>, last_broadcast: Instant) -> Self {
        Self { bucket_start, ohlc: None, volume: 0, last_broadcast, dirty: false, closed: false }
    }

    fn apply(&mut self, price: f64, size: u64, ohlc: bool, volume: bool) {
        if ohlc {
            self.ohlc = Some(match self.ohlc {
                None => Ohlc { open: price, high: price, low: price, close: price },
                Some(o) => Ohlc { open: o.open, high: o.high.max(price), low: o.low.min(price), close: price },
            });
        }
        if volume {
            self.volume += size;
        }
        self.dirty = true;
    }

    /// Has unsent changes and the throttle allows another send.
    pub fn due(&self, now: Instant, throttle: Duration) -> bool {
        self.dirty && !self.closed && now.saturating_duration_since(self.last_broadcast) >= throttle
    }

    /// The bucket has ended by `wall` plus a grace for prints still in flight.
    pub fn ended(&self, interval_secs: i64, wall: DateTime<Utc>, grace: chrono::Duration) -> bool {
        wall >= self.bucket_start + chrono::Duration::seconds(interval_secs) + grace
    }

    /// The wire frame, or `None` while the bar has no price.
    pub fn event(&self, symbol: &str, interval_secs: i64, is_final: bool) -> Option<ScanEvent> {
        let o = self.ohlc?;
        Some(ScanEvent::BarUpdate {
            symbol: symbol.to_string(),
            timestamp: self.bucket_start,
            open: o.open,
            high: o.high,
            low: o.low,
            close: o.close,
            volume: self.volume,
            interval_secs: interval_secs as u32,
            is_final,
        })
    }

    /// Marks a send at `now`.
    pub fn sent(&mut self, now: Instant) {
        self.last_broadcast = now;
        self.dirty = false;
    }
}

impl BarSeries {
    /// Applies one print. `throttle` backdates a brand-new series so its first
    /// print is sent at once instead of after a full throttle interval.
    #[allow(clippy::too_many_arguments)]
    pub fn on_trade(
        &mut self,
        interval_secs: i64,
        at: DateTime<Utc>,
        price: f64,
        size: u64,
        conditions: &[String],
        now: Instant,
        throttle: Duration,
    ) -> TradeStep {
        let (ohlc, volume) = trade_updates(conditions);
        if !ohlc && !volume {
            return TradeStep::Ignored;
        }
        let bucket = floor_to_interval(at, interval_secs);
        match &mut self.bar {
            None => {
                let mut bar = FormingBar::new(bucket, now.checked_sub(throttle).unwrap_or(now));
                bar.apply(price, size, ohlc, volume);
                self.bar = Some(bar);
                TradeStep::Updated
            }
            Some(bar) if bucket < bar.bucket_start => TradeStep::Ignored,
            Some(bar) if bucket == bar.bucket_start => {
                if bar.closed {
                    return TradeStep::Ignored;
                }
                bar.apply(price, size, ohlc, volume);
                TradeStep::Updated
            }
            Some(bar) => {
                let mut next = FormingBar::new(bucket, bar.last_broadcast);
                next.apply(price, size, ohlc, volume);
                TradeStep::Rolled(std::mem::replace(bar, next))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const THROTTLE: Duration = Duration::from_millis(500);

    fn at(h: u32, m: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 15, h, m, s).unwrap()
    }

    fn c(codes: &[&str]) -> Vec<String> {
        codes.iter().map(|s| s.to_string()).collect()
    }

    fn ohlc(series: &BarSeries) -> Ohlc {
        series.bar.as_ref().unwrap().ohlc.unwrap()
    }

    #[test]
    fn regular_sales_update_everything() {
        assert_eq!(trade_updates(&[]), (true, true));
        assert_eq!(trade_updates(&c(&["@"])), (true, true));
        assert_eq!(trade_updates(&c(&[" "])), (true, true));
        assert_eq!(trade_updates(&c(&["@", "F", "T"])), (true, true));
        assert_eq!(trade_updates(&c(&["@", "5", "X"])), (true, true), "a reopening print sets the price");
    }

    #[test]
    fn volume_only_conditions_never_move_the_price() {
        for code in ["C", "G", "H", "I", "N", "P", "R", "U", "V", "W", "Z", "4", "7"] {
            assert_eq!(trade_updates(&c(&["@", code])), (false, true), "{code}");
        }
        assert_eq!(trade_updates(&c(&["@", "F", "I"])), (false, true), "the strictest condition wins");
    }

    #[test]
    fn informational_prints_update_nothing() {
        for code in ["M", "Q", "9"] {
            assert_eq!(trade_updates(&c(&[code])), (false, false), "{code}");
            assert_eq!(trade_updates(&c(&["@", code, "I"])), (false, false), "{code}");
        }
    }

    #[test]
    fn b_is_a_bunched_trade_on_utp_and_an_average_price_on_cta() {
        assert_eq!(trade_updates(&c(&["@", "B"])), (true, true));
        assert_eq!(trade_updates(&c(&["B"])), (false, true));
    }

    #[test]
    fn a_late_print_from_the_previous_minute_does_not_reset_the_bar() {
        let now = Instant::now();
        let mut s = BarSeries::default();
        s.on_trade(60, at(14, 31, 5), 10.0, 100, &c(&["@"]), now, THROTTLE);
        s.on_trade(60, at(14, 31, 20), 10.5, 100, &c(&["@"]), now, THROTTLE);
        s.on_trade(60, at(14, 31, 30), 9.8, 100, &c(&["@"]), now, THROTTLE);
        // Reported now, executed during 14:30.
        assert!(matches!(s.on_trade(60, at(14, 30, 58), 7.0, 5_000, &c(&["@"]), now, THROTTLE), TradeStep::Ignored));
        let bar = s.bar.as_ref().unwrap();
        assert_eq!(bar.bucket_start, at(14, 31, 0));
        assert_eq!(bar.ohlc, Some(Ohlc { open: 10.0, high: 10.5, low: 9.8, close: 9.8 }));
        assert_eq!(bar.volume, 300);
        // And the next current print simply continues it.
        s.on_trade(60, at(14, 31, 40), 10.1, 100, &c(&["@"]), now, THROTTLE);
        assert_eq!(ohlc(&s).open, 10.0);
        assert_eq!(s.bar.as_ref().unwrap().volume, 400);
    }

    #[test]
    fn a_newer_minute_starts_a_new_bar_and_hands_back_the_old_one() {
        let now = Instant::now();
        let mut s = BarSeries::default();
        s.on_trade(60, at(14, 31, 5), 10.0, 100, &c(&["@"]), now, THROTTLE);
        let step = s.on_trade(60, at(14, 32, 1), 11.0, 200, &c(&["@"]), now, THROTTLE);
        let TradeStep::Rolled(previous) = step else { panic!("expected a roll, got {step:?}") };
        assert_eq!(previous.bucket_start, at(14, 31, 0));
        assert_eq!(previous.volume, 100);
        let bar = s.bar.as_ref().unwrap();
        assert_eq!(bar.bucket_start, at(14, 32, 0));
        assert_eq!(bar.ohlc, Some(Ohlc { open: 11.0, high: 11.0, low: 11.0, close: 11.0 }));
        assert_eq!(bar.volume, 200);
    }

    #[test]
    fn the_same_rules_hold_for_thirty_second_bars() {
        let now = Instant::now();
        let mut s = BarSeries::default();
        s.on_trade(30, at(14, 31, 35), 10.0, 100, &c(&["@"]), now, THROTTLE);
        assert!(matches!(s.on_trade(30, at(14, 31, 29), 1.0, 100, &c(&["@"]), now, THROTTLE), TradeStep::Ignored));
        assert_eq!(s.bar.as_ref().unwrap().bucket_start, at(14, 31, 30));
        assert!(matches!(s.on_trade(30, at(14, 32, 0), 10.2, 100, &c(&["@"]), now, THROTTLE), TradeStep::Rolled(_)));
    }

    #[test]
    fn an_odd_lot_adds_volume_but_never_moves_the_price() {
        let now = Instant::now();
        let mut s = BarSeries::default();
        s.on_trade(60, at(14, 31, 5), 10.0, 100, &c(&["@"]), now, THROTTLE);
        s.on_trade(60, at(14, 31, 6), 12.0, 37, &c(&["@", "I"]), now, THROTTLE);
        assert_eq!(ohlc(&s), Ohlc { open: 10.0, high: 10.0, low: 10.0, close: 10.0 });
        assert_eq!(s.bar.as_ref().unwrap().volume, 137);
    }

    #[test]
    fn a_bucket_opened_by_an_odd_lot_has_no_price_and_is_not_sent() {
        let now = Instant::now();
        let mut s = BarSeries::default();
        s.on_trade(60, at(14, 31, 5), 10.0, 37, &c(&["@", "I"]), now, THROTTLE);
        let bar = s.bar.as_ref().unwrap();
        assert!(bar.ohlc.is_none());
        assert_eq!(bar.volume, 37);
        assert!(bar.event("AAA", 60, false).is_none());
        s.on_trade(60, at(14, 31, 6), 10.1, 100, &c(&["@"]), now, THROTTLE);
        assert_eq!(ohlc(&s).open, 10.1);
        assert_eq!(s.bar.as_ref().unwrap().volume, 137);
    }

    #[test]
    fn excluded_conditions_are_ignored_entirely() {
        let now = Instant::now();
        let mut s = BarSeries::default();
        s.on_trade(60, at(14, 31, 5), 10.0, 100, &c(&["@"]), now, THROTTLE);
        for codes in [&["@", "Z"][..], &["@", "4"], &["@", "W"], &["@", "P"], &["@", "V"], &["@", "7"], &["@", "C"], &["@", "N"]] {
            s.on_trade(60, at(14, 31, 10), 50.0, 10, &c(codes), now, THROTTLE);
        }
        assert_eq!(ohlc(&s), Ohlc { open: 10.0, high: 10.0, low: 10.0, close: 10.0 }, "no excluded print moved the price");
        assert!(matches!(s.on_trade(60, at(14, 31, 11), 50.0, 10, &c(&["M"]), now, THROTTLE), TradeStep::Ignored));
        assert!(matches!(s.on_trade(60, at(14, 33, 0), 50.0, 10, &c(&["9"]), now, THROTTLE), TradeStep::Ignored), "an informational print cannot roll the bar either");
        assert_eq!(s.bar.as_ref().unwrap().bucket_start, at(14, 31, 0));
    }

    #[test]
    fn a_closed_bucket_ignores_further_prints() {
        let now = Instant::now();
        let mut s = BarSeries::default();
        s.on_trade(60, at(14, 31, 5), 10.0, 100, &c(&["@"]), now, THROTTLE);
        s.bar.as_mut().unwrap().closed = true;
        assert!(matches!(s.on_trade(60, at(14, 31, 59), 11.0, 100, &c(&["@"]), now, THROTTLE), TradeStep::Ignored));
        assert!(matches!(s.on_trade(60, at(14, 32, 0), 11.0, 100, &c(&["@"]), now, THROTTLE), TradeStep::Rolled(_)));
    }

    #[test]
    fn the_first_print_is_due_at_once_and_then_throttled() {
        let now = Instant::now();
        let mut s = BarSeries::default();
        s.on_trade(60, at(14, 31, 5), 10.0, 100, &c(&["@"]), now, THROTTLE);
        let bar = s.bar.as_mut().unwrap();
        assert!(bar.due(now, THROTTLE));
        bar.sent(now);
        assert!(!bar.due(now, THROTTLE), "nothing new to send");
        s.on_trade(60, at(14, 31, 6), 10.1, 100, &c(&["@"]), now, THROTTLE);
        let bar = s.bar.as_ref().unwrap();
        assert!(!bar.due(now + Duration::from_millis(100), THROTTLE));
        assert!(bar.due(now + THROTTLE, THROTTLE), "a trailing change is sent once the throttle allows");
    }

    #[test]
    fn a_bucket_ends_after_its_interval_plus_grace() {
        let now = Instant::now();
        let mut s = BarSeries::default();
        s.on_trade(30, at(14, 31, 5), 10.0, 100, &c(&["@"]), now, THROTTLE);
        let bar = s.bar.as_ref().unwrap();
        let grace = chrono::Duration::seconds(2);
        assert!(!bar.ended(30, at(14, 31, 31), grace));
        assert!(bar.ended(30, at(14, 31, 32), grace));
    }

    #[test]
    fn the_wire_frame_is_the_existing_bar_update() {
        let now = Instant::now();
        let mut s = BarSeries::default();
        s.on_trade(30, at(14, 31, 5), 10.0, 100, &c(&["@"]), now, THROTTLE);
        let json = serde_json::to_value(s.bar.as_ref().unwrap().event("AAA", 30, true).unwrap()).unwrap();
        assert_eq!(json["type"], "bar_update");
        assert_eq!(json["intervalSecs"], 30);
        assert_eq!(json["isFinal"], true);
        assert_eq!(json["timestamp"], "2026-10-15T14:31:00Z");
    }
}
