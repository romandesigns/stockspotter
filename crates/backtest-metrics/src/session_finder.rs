//! Picks which historical (symbol, date) sessions are worth a full
//! intraday replay, instead of hand-picking dates. Every prior tuning
//! pass in this codebase used exactly one known session (SWVL's Aug 28
//! gap day) — enough to tune ignition (many tick-level signals per
//! session) but explicitly *not* enough to validate the fast funnel or
//! momentum scorer, which only produce a handful of session-level
//! signals each. Both of those need real contrast: quiet, non-qualifying
//! days alongside the big gappers, across more than one symbol — this
//! module is what selects that mix from real daily-bar data instead of
//! guessing.
//!
//! Two pure, independently-testable steps: `compute_day_signals` turns a
//! raw daily-bar series into per-day gap%/relative-volume, and
//! `pick_sessions` classifies+ranks days into `Hot` (candidate gap/surge
//! days, what the detectors are *supposed* to catch) and `Quiet`
//! (ordinary days, the negative control neither the funnel nor momentum
//! scorer should fire much on). `session_window_utc` is the one function
//! here that isn't pure — it needs `chrono-tz` to correctly place a
//! trading day's 9:30-16:00 ET regular session in UTC, which shifts by an
//! hour across the EST/EDT boundary.

use anyhow::{anyhow, Result};
use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::America::New_York;
use market_data::DailyBar;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DaySignal {
    pub date: NaiveDate,
    /// vs the prior trading day's close.
    pub gap_pct: f64,
    /// This day's volume vs the trailing (up to 20 prior days, excluding
    /// this one) average volume. `None` if there's no prior history yet
    /// to compare against (e.g. the very first days in the series).
    pub rel_volume: Option<f64>,
}

/// Turns a raw daily-bar series (oldest first, as Alpaca returns it) into
/// per-day gap%/relative-volume. The first bar in `bars` is consumed only
/// as "the prior close" for the second — it never becomes a `DaySignal`
/// itself, since gap% is undefined without a prior day.
pub fn compute_day_signals(bars: &[DailyBar]) -> Vec<DaySignal> {
    let mut out = Vec::new();
    for i in 1..bars.len() {
        let prior_close = bars[i - 1].close;
        let gap_pct = if prior_close > 0.0 {
            (bars[i].close - prior_close) / prior_close * 100.0
        } else {
            0.0
        };

        let lookback_start = i.saturating_sub(20);
        let trailing = &bars[lookback_start..i];
        let rel_volume = if trailing.is_empty() {
            None
        } else {
            let avg = trailing.iter().map(|b| b.volume).sum::<u64>() as f64 / trailing.len() as f64;
            if avg > 0.0 {
                Some(bars[i].volume as f64 / avg)
            } else {
                None
            }
        };

        out.push(DaySignal {
            date: bars[i].date,
            gap_pct,
            rel_volume,
        });
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionCategory {
    /// A real gap/surge day — what the fast funnel and ignition detector
    /// are supposed to catch.
    Hot,
    /// An ordinary day — the negative control. If the funnel or momentum
    /// scorer qualify just as readily on these, the thresholds are too
    /// loose, not just "working as designed".
    Quiet,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SessionPick {
    pub date: NaiveDate,
    pub category: SessionCategory,
    pub gap_pct: f64,
    pub rel_volume: f64,
}

const HOT_REL_VOLUME_THRESHOLD: f64 = 2.0;
const HOT_GAP_PCT_THRESHOLD: f64 = 10.0;
const QUIET_REL_VOLUME_RANGE: (f64, f64) = (0.3, 1.5);
const QUIET_GAP_PCT_MAX: f64 = 3.0;

/// Selects up to `max_hot` gap/surge days (ranked by `rel_volume *
/// |gap_pct|`, so a day needs to be unusual on *both* axes to rank top —
/// avoids picking a huge-gap-but-thin-volume day, or vice versa) and up
/// to `max_quiet` ordinary days (most recent first, since "ordinary" days
/// are plentiful and recency doesn't matter for the negative control) per
/// symbol.
pub fn pick_sessions(signals: &[DaySignal], max_hot: usize, max_quiet: usize) -> Vec<SessionPick> {
    let mut hot: Vec<SessionPick> = signals
        .iter()
        .filter(|s| {
            let rel_vol = s.rel_volume.unwrap_or(0.0);
            rel_vol >= HOT_REL_VOLUME_THRESHOLD || s.gap_pct.abs() >= HOT_GAP_PCT_THRESHOLD
        })
        .map(|s| SessionPick {
            date: s.date,
            category: SessionCategory::Hot,
            gap_pct: s.gap_pct,
            rel_volume: s.rel_volume.unwrap_or(0.0),
        })
        .collect();
    hot.sort_by(|a, b| {
        let score_a = a.rel_volume * a.gap_pct.abs();
        let score_b = b.rel_volume * b.gap_pct.abs();
        score_b.partial_cmp(&score_a).unwrap_or(std::cmp::Ordering::Equal)
    });
    hot.truncate(max_hot);

    let mut quiet: Vec<SessionPick> = signals
        .iter()
        .filter(|s| {
            let rel_vol = s.rel_volume.unwrap_or(0.0);
            rel_vol >= QUIET_REL_VOLUME_RANGE.0
                && rel_vol <= QUIET_REL_VOLUME_RANGE.1
                && s.gap_pct.abs() < QUIET_GAP_PCT_MAX
        })
        .map(|s| SessionPick {
            date: s.date,
            category: SessionCategory::Quiet,
            gap_pct: s.gap_pct,
            rel_volume: s.rel_volume.unwrap_or(0.0),
        })
        .collect();
    quiet.sort_by(|a, b| b.date.cmp(&a.date));
    quiet.truncate(max_quiet);

    hot.into_iter().chain(quiet).collect()
}

/// The regular 9:30 AM-4:00 PM Eastern trading session for `date`,
/// expressed in UTC. Genuinely needs `chrono-tz`, not a fixed offset —
/// the UTC hour of "9:30 AM Eastern" shifts across the EST/EDT boundary,
/// and a fixed-offset version would silently mis-window every session on
/// the wrong side of it (fetching pre-market/missing the close, or
/// vice versa).
pub fn session_window_utc(date: NaiveDate) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    let open_local = date.and_time(NaiveTime::from_hms_opt(9, 30, 0).unwrap());
    let close_local = date.and_time(NaiveTime::from_hms_opt(16, 0, 0).unwrap());

    let open_utc = New_York
        .from_local_datetime(&open_local)
        .single()
        .ok_or_else(|| anyhow!("ambiguous/nonexistent local open time for {date} (DST transition?)"))?
        .with_timezone(&Utc);
    let close_utc = New_York
        .from_local_datetime(&close_local)
        .single()
        .ok_or_else(|| anyhow!("ambiguous/nonexistent local close time for {date} (DST transition?)"))?
        .with_timezone(&Utc);

    Ok((open_utc, close_utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(date: NaiveDate, close: f64, volume: u64) -> DailyBar {
        DailyBar { date, close, volume }
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn compute_day_signals_skips_the_first_bar_and_computes_gap_correctly() {
        let bars = vec![
            bar(d(2026, 1, 5), 1.00, 100_000),
            bar(d(2026, 1, 6), 1.10, 200_000), // +10% gap
        ];
        let signals = compute_day_signals(&bars);
        assert_eq!(signals.len(), 1);
        assert!((signals[0].gap_pct - 10.0).abs() < 1e-9);
        assert_eq!(signals[0].date, d(2026, 1, 6));
    }

    #[test]
    fn compute_day_signals_rel_volume_uses_trailing_average_excluding_self() {
        let bars = vec![
            bar(d(2026, 1, 1), 1.0, 100_000),
            bar(d(2026, 1, 2), 1.0, 100_000),
            bar(d(2026, 1, 3), 1.0, 400_000), // 4x the trailing 2-day avg
        ];
        let signals = compute_day_signals(&bars);
        assert_eq!(signals.len(), 2);
        assert_eq!(signals[1].rel_volume, Some(4.0));
    }

    #[test]
    fn pick_sessions_classifies_hot_and_quiet_correctly() {
        let signals = vec![
            DaySignal { date: d(2026, 1, 2), gap_pct: 41.0, rel_volume: Some(6.0) }, // hot
            DaySignal { date: d(2026, 1, 3), gap_pct: 0.5, rel_volume: Some(1.0) },  // quiet
            DaySignal { date: d(2026, 1, 4), gap_pct: 15.0, rel_volume: Some(0.2) }, // hot (gap alone qualifies)
            DaySignal { date: d(2026, 1, 5), gap_pct: 50.0, rel_volume: None },      // no volume data — still hot via gap
        ];
        let picks = pick_sessions(&signals, 3, 1);
        let hot: Vec<_> = picks.iter().filter(|p| p.category == SessionCategory::Hot).collect();
        let quiet: Vec<_> = picks.iter().filter(|p| p.category == SessionCategory::Quiet).collect();
        assert_eq!(hot.len(), 3);
        assert_eq!(quiet.len(), 1);
        assert_eq!(quiet[0].date, d(2026, 1, 3));
    }

    #[test]
    fn pick_sessions_ranks_hot_days_by_combined_gap_and_volume() {
        let signals = vec![
            DaySignal { date: d(2026, 1, 2), gap_pct: 10.0, rel_volume: Some(2.0) }, // score 20
            DaySignal { date: d(2026, 1, 3), gap_pct: 40.0, rel_volume: Some(8.0) }, // score 320, ranks first
        ];
        let picks = pick_sessions(&signals, 1, 0);
        assert_eq!(picks.len(), 1);
        assert_eq!(picks[0].date, d(2026, 1, 3));
    }

    #[test]
    fn pick_sessions_respects_caps() {
        let signals: Vec<DaySignal> = (1..=30)
            .map(|day| DaySignal {
                date: d(2026, 1, day),
                gap_pct: 20.0,
                rel_volume: Some(5.0),
            })
            .collect();
        let picks = pick_sessions(&signals, 2, 0);
        assert_eq!(picks.len(), 2);
    }

    #[test]
    fn session_window_is_correct_across_the_edt_est_boundary() {
        // Same wall-clock 9:30 AM Eastern, different UTC hour depending
        // on daylight saving — this is exactly the bug a fixed-offset
        // version would get wrong.
        let (open_summer, _) = session_window_utc(d(2026, 7, 15)).unwrap(); // EDT, UTC-4
        let (open_winter, _) = session_window_utc(d(2026, 1, 15)).unwrap(); // EST, UTC-5
        assert_eq!(open_summer.format("%H:%M").to_string(), "13:30");
        assert_eq!(open_winter.format("%H:%M").to_string(), "14:30");
    }
}
