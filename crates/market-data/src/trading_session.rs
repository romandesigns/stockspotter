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

use chrono::{DateTime, NaiveTime, Utc};
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

pub fn classify_session(now_utc: DateTime<Utc>) -> TradingSession {
    let local_time = now_utc.with_timezone(&New_York).time();

    let premarket_start = NaiveTime::from_hms_opt(4, 0, 0).unwrap();
    let regular_start = NaiveTime::from_hms_opt(9, 30, 0).unwrap();
    let regular_end = NaiveTime::from_hms_opt(16, 0, 0).unwrap();
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
