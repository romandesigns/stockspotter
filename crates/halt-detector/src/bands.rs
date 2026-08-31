//! LULD band width — architecture doc part-3's price-tier lookup table
//! for Tier 2 securities (the vast majority of tickers this scanner
//! watches; this crate doesn't attempt Tier 1 handling since the doc
//! scopes the system to Tier 2), plus the 3:35-4:00 PM ET closing-window
//! doubling rule.

use chrono::{DateTime, Timelike, Utc};
use chrono_tz::America::New_York;

/// Dollar band width around `reference_price` a trade must stay within to
/// avoid crossing into halt territory — *not* a percentage, since the
/// sub-$0.75 tier is defined as a flat dollar amount (or a percentage,
/// whichever is smaller), and expressing everything in dollars from the
/// start avoids any lossy back-and-forth conversion between the tiers.
///
/// Tiers, straight from the doc:
/// - above $3.00: 10% of reference price
/// - $0.75-$3.00: 20% of reference price
/// - below $0.75: the lesser of $0.15 or 75% of reference price
///
/// `doubled` applies the doc's closing-window rule — call `band_doubles`
/// to determine that, don't decide it here (keeps this function pure and
/// clock-independent, easy to test exhaustively).
pub fn band_width_dollars(reference_price: f64, doubled: bool) -> f64 {
    if reference_price <= 0.0 {
        return 0.0;
    }
    let base = if reference_price > 3.00 {
        reference_price * 0.10
    } else if reference_price >= 0.75 {
        reference_price * 0.20
    } else {
        (0.15_f64).min(reference_price * 0.75)
    };
    if doubled {
        base * 2.0
    } else {
        base
    }
}

/// True during 3:35 PM - 4:00 PM ET (inclusive of :35, exclusive of the
/// 4:00 PM close itself) — the doc's window during which bands double for
/// Tier 1 stocks and Tier 2 stocks at or below $3.00. Handles EST/EDT
/// correctly via `chrono-tz`'s IANA database rather than a hardcoded UTC
/// offset, which would be wrong for half the year.
pub fn is_closing_window(utc_time: DateTime<Utc>) -> bool {
    let et = utc_time.with_timezone(&New_York);
    let minutes_since_midnight = et.hour() * 60 + et.minute();
    (935..960).contains(&minutes_since_midnight) // 15:35 .. 16:00
}

/// Whether the doubling rule actually applies for a given
/// `reference_price`, given whether it's currently the closing window.
/// Per the doc: doubling applies to Tier 1 stocks (out of scope here) and
/// Tier 2 stocks at or below $3.00 — *not* the >$3.00 (10% band) tier.
pub fn band_doubles(reference_price: f64, in_closing_window: bool) -> bool {
    in_closing_window && reference_price <= 3.00
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn above_3_dollars_gets_the_10_percent_band() {
        let band = band_width_dollars(5.00, false);
        assert!((band - 0.50).abs() < 1e-9);
    }

    #[test]
    fn between_75_cents_and_3_dollars_gets_the_20_percent_band() {
        let band = band_width_dollars(1.00, false);
        assert!((band - 0.20).abs() < 1e-9);
    }

    #[test]
    fn low_price_uses_the_flat_15_cents_once_75_percent_exceeds_it() {
        // 0.75 * 0.50 = 0.375, well above 0.15 -> the flat amount wins.
        let band = band_width_dollars(0.50, false);
        assert!((band - 0.15).abs() < 1e-9);
    }

    #[test]
    fn very_low_price_uses_75_percent_once_it_undercuts_15_cents() {
        // 0.75 * 0.10 = 0.075, below 0.15 -> the percentage wins.
        let band = band_width_dollars(0.10, false);
        assert!((band - 0.075).abs() < 1e-9);
    }

    #[test]
    fn the_two_low_tier_terms_cross_over_at_20_cents() {
        // 0.75 * 0.20 = 0.15 exactly -> both terms agree at this price,
        // matching the doc's own note that the $0.15-$0.25 range
        // "collapses to essentially the flat $0.15 move".
        let band = band_width_dollars(0.20, false);
        assert!((band - 0.15).abs() < 1e-9);
    }

    #[test]
    fn doubling_multiplies_the_band_by_two() {
        let normal = band_width_dollars(5.00, false);
        let doubled = band_width_dollars(5.00, true);
        assert!((doubled - normal * 2.0).abs() < 1e-9);
    }

    #[test]
    fn zero_or_negative_price_returns_a_zero_band_not_garbage() {
        assert_eq!(band_width_dollars(0.0, false), 0.0);
        assert_eq!(band_width_dollars(-1.0, false), 0.0);
    }

    #[test]
    fn band_doubles_only_for_tier2_at_or_below_3_dollars() {
        assert!(band_doubles(3.00, true));
        assert!(band_doubles(0.50, true));
        assert!(!band_doubles(3.01, true), "the >$3 tier should not double");
        assert!(!band_doubles(1.00, false), "no doubling outside the closing window");
    }

    #[test]
    fn closing_window_detected_correctly_in_standard_time() {
        // Jan 15 2026 is EST (UTC-5): 3:35 PM ET = 20:35 UTC.
        let just_before = Utc.with_ymd_and_hms(2026, 1, 15, 20, 34, 59).unwrap();
        let at_open = Utc.with_ymd_and_hms(2026, 1, 15, 20, 35, 0).unwrap();
        let mid_window = Utc.with_ymd_and_hms(2026, 1, 15, 20, 50, 0).unwrap();
        let at_close = Utc.with_ymd_and_hms(2026, 1, 15, 21, 0, 0).unwrap();

        assert!(!is_closing_window(just_before));
        assert!(is_closing_window(at_open));
        assert!(is_closing_window(mid_window));
        assert!(!is_closing_window(at_close), "4:00 PM ET itself is market close, not still-open window");
    }

    #[test]
    fn closing_window_detected_correctly_in_daylight_time() {
        // Jul 15 2026 is EDT (UTC-4): 3:35 PM ET = 19:35 UTC. This is the
        // exact scenario a hardcoded UTC offset would get wrong for half
        // the year — real reason chrono-tz is used instead of a fixed
        // offset.
        let just_before = Utc.with_ymd_and_hms(2026, 7, 15, 19, 34, 59).unwrap();
        let at_open = Utc.with_ymd_and_hms(2026, 7, 15, 19, 35, 0).unwrap();

        assert!(!is_closing_window(just_before));
        assert!(is_closing_window(at_open));
    }
}
