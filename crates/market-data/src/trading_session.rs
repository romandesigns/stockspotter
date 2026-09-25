//! Which US equity trading session a given instant falls in — Premarket /
//! Regular / After-Hours / Overnight. Built for `movers.rs`'s rolling 24h
//! "best reading" tracker (Top Gainers / Highly Trading), so a stock's
//! peak reading can be labeled with when it happened, not just what it
//! was.
//!
//! Named `trading_session` rather than `session` to avoid colliding with
//! this crate's existing `session.rs` (`SessionTracker`, an unrelated
//! per-symbol OHLCV/gap tracker for the live funnel path).
//!
//! Boundaries (US market convention, matches this app's own "opening at
//! 4AM" framing): Premarket 4:00-9:30 ET, Regular 9:30-16:00 ET,
//! After-Hours 16:00-20:00 ET, Overnight 20:00-4:00 ET (next day).
//!
//! `classify_session` converts an unambiguous UTC instant into its New
//! York wall-clock time via `chrono-tz` (same library
//! `backtest-metrics::session_finder::session_window_utc` already uses
//! for the reverse direction) -- this direction needs no DST
//! ambiguity handling at all: a `DateTime<Utc>` is always a single,
//! well-defined instant, `with_timezone` just reads off whichever local
//! offset (EST/EDT) actually applied to it.
//!
//! Overnight is included for completeness even though real overnight ATS
//! trading (~8PM-4AM ET, e.g. Blue Ocean) isn't visible through Alpaca at
//! all (see stockspotter-open-tasks memory) -- so in practice a symbol's
//! already-recorded Premarket/Regular/After-Hours best will almost always
//! still be the rolling-24h max once Overnight rolls around, since
//! nothing new is happening from Alpaca's point of view. Expected, not a
//! bug.

use chrono::{DateTime, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::America::New_York;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TradingSession {
    Premarket,
    Regular,
    AfterHours,
    Overnight,
}

impl TradingSession {
    /// The same snake_case name `Serialize` writes, for records that store the
    /// session as a plain string (`backtest_metrics::context::MarketFeatures`).
    pub fn as_str(self) -> &'static str {
        match self {
            TradingSession::Premarket => "premarket",
            TradingSession::Regular => "regular",
            TradingSession::AfterHours => "after_hours",
            TradingSession::Overnight => "overnight",
        }
    }
}

/// Hours after New York midnight at which a market day begins: 04:00 ET, the
/// premarket open. See `market_day`.
const MARKET_DAY_START_HOUR: u32 = 4;

/// The **market day** an instant belongs to: the New York calendar date of
/// `t - 4h`, so a market day runs from 04:00 ET to 04:00 ET the next morning.
///
/// Defined once, here, because every per-day research quantity (the
/// pre-detection baseline and the funnel/market freshness in
/// `backtest_metrics::context::FeatureCache`) must agree on where one day ends
/// and the next begins -- measurement-correctness contract of 2026-09-25,
/// "Market day". Why 04:00 ET and not midnight or UTC:
///
/// * 04:00 ET is where this app's data actually starts: the premarket open,
///   `rest::fetch_session_bars`'s backfill start, and the de facto live-scan
///   rebuild on the first new-date minute bar (`live.rs`).
/// * 20:00-04:00 ET belongs to the day it follows. Alpaca shows nothing there
///   today; if overnight (e.g. Blue Ocean) prints ever appear, they attach to
///   the session they follow rather than resetting a baseline at midnight in
///   the middle of it.
/// * The UTC date is wrong: in EDT an after-hours print at 20:30 ET is 00:30Z
///   on the *next* UTC date, and in EST the whole final after-hours hour lands
///   there. UTC `sessionDate` stays the file/identity partition key elsewhere;
///   the market day is a separate quantity, carried as its own field.
///
/// DST-safe by construction: the offset applied is whichever one was in force
/// at `t` (a `DateTime<Utc>` is one well-defined instant), and US transitions
/// happen at 02:00 local on a Sunday, so the 04:00 boundary itself is never
/// skipped or repeated. Weekends and holidays need no special case: consumers
/// reset on market-day *inequality*, so Friday -> Monday resets exactly once.
/// A Saturday instant simply gets Saturday's date.
pub fn market_day(t: DateTime<Utc>) -> NaiveDate {
    (t.with_timezone(&New_York).naive_local() - Duration::hours(i64::from(MARKET_DAY_START_HOUR)))
        .date()
}

/// The UTC instant market day `day` opens: 04:00 America/New_York on that
/// date -- 08:00Z under EDT, 09:00Z under EST.
///
/// 04:00 local exists exactly once on every US calendar day (transitions are
/// at 02:00), so `from_local_datetime` is unambiguous. The fallback exists
/// only so this can never panic on a malformed tz database; it assumes EST,
/// the larger offset, which errs towards calling a baseline truncated rather
/// than complete.
pub fn market_day_open(day: NaiveDate) -> DateTime<Utc> {
    let local = day.and_time(NaiveTime::from_hms_opt(MARKET_DAY_START_HOUR, 0, 0).unwrap());
    New_York
        .from_local_datetime(&local)
        .earliest()
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|| Utc.from_utc_datetime(&(local + Duration::hours(5))))
}

pub fn classify_session(now_utc: DateTime<Utc>) -> TradingSession {
    let local_time = now_utc.with_timezone(&New_York).time();

    let premarket_start = NaiveTime::from_hms_opt(4, 0, 0).unwrap();
    let regular_start = NaiveTime::from_hms_opt(9, 30, 0).unwrap();
    let close = halt_detector::calendar::regular_close_minutes(now_utc.with_timezone(&New_York).date_naive());
    let Some(close) = close else { return TradingSession::Overnight; };
    let regular_end = NaiveTime::from_hms_opt(close / 60, close % 60, 0).unwrap();
    let after_hours_end = NaiveTime::from_hms_opt(20, 0, 0).unwrap();

    if local_time >= premarket_start && local_time < regular_start {
        TradingSession::Premarket
    } else if local_time >= regular_start && local_time < regular_end {
        TradingSession::Regular
    } else if local_time >= regular_end && local_time < after_hours_end {
        TradingSession::AfterHours
    } else {
        TradingSession::Overnight
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    fn utc_s(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, s).unwrap()
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    // --- market_day (contract D3-T1..T3, brief tests 12-14) ---------------

    #[test]
    fn market_day_starts_at_0400_new_york_in_edt() {
        // 2026-09-22 is EDT (UTC-4): 04:00 ET = 08:00Z.
        assert_eq!(market_day(utc_s(2026, 9, 22, 7, 59, 59)), day(2026, 9, 21));
        assert_eq!(market_day(utc_s(2026, 9, 22, 8, 0, 0)), day(2026, 9, 22));
        assert_eq!(market_day_open(day(2026, 9, 22)), utc(2026, 9, 22, 8, 0));
    }

    #[test]
    fn market_day_starts_at_0400_new_york_in_est() {
        // 2026-01-15 is EST (UTC-5): 04:00 ET = 09:00Z. The same 08:30Z that
        // is already the new day in summer is still the previous one here.
        assert_eq!(market_day(utc_s(2026, 1, 15, 8, 59, 59)), day(2026, 1, 14));
        assert_eq!(market_day(utc_s(2026, 1, 15, 9, 0, 0)), day(2026, 1, 15));
        assert_eq!(market_day(utc(2026, 1, 15, 8, 30)), day(2026, 1, 14));
        assert_eq!(market_day_open(day(2026, 1, 15)), utc(2026, 1, 15, 9, 0));
    }

    #[test]
    fn market_day_is_unambiguous_on_both_dst_transition_sundays() {
        // Spring forward, Sunday 2026-03-08: 02:00 EST -> 03:00 EDT, so 04:00
        // local is already EDT (08:00Z). Saturday's 04:00 was still EST.
        assert_eq!(market_day_open(day(2026, 3, 7)), utc(2026, 3, 7, 9, 0));
        assert_eq!(market_day_open(day(2026, 3, 8)), utc(2026, 3, 8, 8, 0));
        assert_eq!(market_day_open(day(2026, 3, 9)), utc(2026, 3, 9, 8, 0));
        assert_eq!(market_day(utc_s(2026, 3, 8, 7, 59, 59)), day(2026, 3, 7));
        assert_eq!(market_day(utc(2026, 3, 8, 8, 0)), day(2026, 3, 8));
        // 07:30Z is 03:30 EDT, just after the skipped 02:xx hour: no panic,
        // and still the previous market day.
        assert_eq!(market_day(utc(2026, 3, 8, 7, 30)), day(2026, 3, 7));

        // Fall back, Sunday 2026-11-01: 02:00 EDT -> 01:00 EST, so 04:00
        // local is EST (09:00Z); the repeated 01:xx hour is before it.
        assert_eq!(market_day_open(day(2026, 10, 31)), utc(2026, 10, 31, 8, 0));
        assert_eq!(market_day_open(day(2026, 11, 1)), utc(2026, 11, 1, 9, 0));
        assert_eq!(market_day_open(day(2026, 11, 2)), utc(2026, 11, 2, 9, 0));
        assert_eq!(market_day(utc_s(2026, 11, 1, 8, 59, 59)), day(2026, 10, 31));
        assert_eq!(market_day(utc(2026, 11, 1, 9, 0)), day(2026, 11, 1));
        // Both instants of the repeated 01:30 local hour (05:30Z EDT and
        // 06:30Z EST) belong to the previous market day.
        assert_eq!(market_day(utc(2026, 11, 1, 5, 30)), day(2026, 10, 31));
        assert_eq!(market_day(utc(2026, 11, 1, 6, 30)), day(2026, 10, 31));
    }

    #[test]
    fn crossing_utc_midnight_after_hours_is_not_a_new_market_day() {
        // 20:30 ET on 2026-09-22 (EDT) is 00:30Z on 09-23: a new UTC date,
        // the same market day.
        assert_eq!(market_day(utc(2026, 9, 23, 0, 30)), day(2026, 9, 22));
        // EST: 19:30 ET on 2026-01-15 is 00:30Z on 01-16.
        assert_eq!(market_day(utc(2026, 1, 16, 0, 30)), day(2026, 1, 15));
    }

    #[test]
    fn every_market_day_open_of_2026_round_trips() {
        let mut d = day(2026, 1, 1);
        while d <= day(2026, 12, 31) {
            let open = market_day_open(d);
            assert_eq!(market_day(open), d, "the open of {d} must belong to {d}");
            assert_eq!(
                market_day(open - Duration::seconds(1)),
                d.pred_opt().unwrap(),
                "one second before the open of {d} is the previous market day"
            );
            d = d.succ_opt().unwrap();
        }
    }

    #[test]
    fn session_names_match_the_serialized_form() {
        for s in [
            TradingSession::Premarket,
            TradingSession::Regular,
            TradingSession::AfterHours,
            TradingSession::Overnight,
        ] {
            assert_eq!(serde_json::to_value(s).unwrap(), serde_json::json!(s.as_str()));
        }
    }

    // Jan 15 2026 is EST (UTC-5): 9:30 AM ET = 14:30 UTC.
    #[test]
    fn classifies_all_four_sessions_in_est() {
        assert_eq!(classify_session(utc(2026, 1, 15, 9, 0)), TradingSession::Premarket); // 4:00 ET
        assert_eq!(classify_session(utc(2026, 1, 15, 12, 0)), TradingSession::Premarket); // 7:00 ET
        assert_eq!(classify_session(utc(2026, 1, 15, 14, 30)), TradingSession::Regular); // 9:30 ET
        assert_eq!(classify_session(utc(2026, 1, 15, 18, 0)), TradingSession::Regular); // 1:00 PM ET
        assert_eq!(classify_session(utc(2026, 1, 15, 21, 0)), TradingSession::AfterHours); // 4:00 PM ET
        assert_eq!(classify_session(utc(2026, 1, 15, 23, 30)), TradingSession::AfterHours); // 6:30 PM ET
        assert_eq!(classify_session(utc(2026, 1, 16, 1, 0)), TradingSession::Overnight); // 8:00 PM ET
        assert_eq!(classify_session(utc(2026, 1, 15, 8, 59)), TradingSession::Overnight); // 3:59 AM ET
    }

    // Jul 15 2026 is EDT (UTC-4): the same 9:30 AM ET wall-clock moment
    // lands on a DIFFERENT UTC hour than the EST test above -- this is
    // exactly the case a fixed-offset version would get wrong.
    #[test]
    fn classifies_correctly_across_the_edt_est_boundary() {
        // The exact same UTC instant (14:30 UTC) that was Regular-session
        // 9:30 AM ET under EST in the test above is 10:30 AM ET here under
        // EDT -- still Regular, but for a different reason. The real proof
        // this needs chrono-tz and not a fixed offset: 14:30 UTC alone is
        // ambiguous without knowing which offset applies to *this* date.
        assert_eq!(classify_session(utc(2026, 7, 15, 13, 30)), TradingSession::Regular); // 9:30 ET (EDT)
        assert_eq!(classify_session(utc(2026, 7, 15, 8, 0)), TradingSession::Premarket); // 4:00 ET (EDT)
        assert_eq!(classify_session(utc(2026, 7, 15, 7, 59)), TradingSession::Overnight); // 3:59 AM ET (EDT)
    }
}
