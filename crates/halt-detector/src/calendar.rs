//! Regular NYSE holiday/early-close schedule. Exceptional closures require an override.
use chrono::{Datelike, Duration, NaiveDate, Weekday};

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}
fn observed(date: NaiveDate) -> NaiveDate {
    match date.weekday() {
        Weekday::Sat => date - Duration::days(1),
        Weekday::Sun => date + Duration::days(1),
        _ => date,
    }
}
fn nth(year: i32, month: u32, weekday: Weekday, n: i64) -> NaiveDate {
    let first = day(year, month, 1);
    first
        + Duration::days(
            (weekday.num_days_from_monday() as i64 - first.weekday().num_days_from_monday() as i64
                + 7)
                % 7
                + 7 * (n - 1),
        )
}
fn easter(year: i32) -> NaiveDate {
    let a = year % 19;
    let b = year / 100;
    let c = year % 100;
    let d = b / 4;
    let e = b % 4;
    let f = (b + 8) / 25;
    let g = (b - f + 1) / 3;
    let h = (19 * a + b - d - g + 15) % 30;
    let i = c / 4;
    let k = c % 4;
    let l = (32 + 2 * e + 2 * i - h - k) % 7;
    let m = (a + 11 * h + 22 * l) / 451;
    let value = h + l - 7 * m + 114;
    day(year, (value / 31) as u32, (value % 31 + 1) as u32)
}

/// Minutes after midnight New York; None means no regular session.
pub fn regular_close_minutes(date: NaiveDate) -> Option<u32> {
    if matches!(date.weekday(), Weekday::Sat | Weekday::Sun) {
        return None;
    }
    let y = date.year();
    let new_year = day(y, 1, 1);
    let new_year = if new_year.weekday() == Weekday::Sun {
        new_year + Duration::days(1)
    } else {
        new_year
    };
    let memorial = (0..7)
        .map(|i| day(y, 5, 31) - Duration::days(i))
        .find(|d| d.weekday() == Weekday::Mon)
        .unwrap();
    let thanksgiving = nth(y, 11, Weekday::Thu, 4);
    let holidays = [
        new_year,
        nth(y, 1, Weekday::Mon, 3),
        nth(y, 2, Weekday::Mon, 3),
        easter(y) - Duration::days(2),
        memorial,
        observed(day(y, 7, 4)),
        nth(y, 9, Weekday::Mon, 1),
        thanksgiving,
        observed(day(y, 12, 25)),
    ];
    if holidays.contains(&date) || (y >= 2022 && date == observed(day(y, 6, 19))) {
        return None;
    }
    if date == thanksgiving + Duration::days(1) || date == day(y, 12, 24) || date == day(y, 7, 3) {
        Some(780)
    } else {
        Some(960)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nyse_2026_holidays_and_short_sessions() {
        assert_eq!(regular_close_minutes(day(2026, 9, 7)), None);
        assert_eq!(regular_close_minutes(day(2026, 9, 8)), Some(960));
        assert_eq!(regular_close_minutes(day(2026, 11, 27)), Some(780));
        assert_eq!(regular_close_minutes(day(2026, 12, 24)), Some(780));
        assert_eq!(regular_close_minutes(day(2026, 7, 3)), None);
        assert_eq!(regular_close_minutes(day(2026, 4, 3)), None);
    }
}
