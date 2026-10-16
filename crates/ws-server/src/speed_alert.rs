//! Server-side "speed alert" decision for the phone push (2026-10-16,
//! Roman: the Halt Early-Warning gauge is where most of his real profit
//! came from, read as a speed meter in premarket and regular hours alike).
//! The gauge is halt_detector's `proximity_ratio` -- |price - 5-minute
//! rolling reference| / LULD band width -- which is computed in every
//! session; only `level` is pinned to calm outside 09:30-16:00.
//!
//! This is a MIRROR of `packages/shared-types/src/haltSpeed.ts`
//! (`shouldSpeedAlert`, `SpeedTracker`, `addSpeedPoint`, `speedDelta1m`),
//! fixed to `SPEED_ALERT_DEFAULTS`. The web, desktop and phone panels run
//! the TypeScript copy; this one decides the push a locked phone receives,
//! so the two must agree or a push would arrive for an alert the panel
//! never showed (or the reverse). Change them together or not at all.
//!
//! Pure and synchronous on purpose: main.rs's push task feeds it every
//! `halt_warning` (one per trade on a tracked symbol, ~2,000/min across
//! the universe), so it must be cheap and must never await.

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};

/// Gauge percent the reading must rise THROUGH. Mirrors
/// `SPEED_ALERT_DEFAULTS.threshold` in haltSpeed.ts. Unmeasured starting
/// value, not a tuned one.
pub const SPEED_ALERT_THRESHOLD_PCT: u8 = 70;
/// Per-symbol quiet window after an alert, measured in READING time (the
/// event timestamp), not wall clock, exactly as the client measures it.
/// Mirrors `SPEED_ALERT_DEFAULTS.cooldownMs`. Unmeasured starting value.
pub const SPEED_ALERT_COOLDOWN_MS: i64 = 10 * 60_000;
/// Stored history per symbol. Mirrors `SPEED_HISTORY_WINDOW_MS`.
pub const SPEED_HISTORY_WINDOW_MS: i64 = 10 * 60_000;
/// One stored point per symbol per bucket; the newest reading in a bucket
/// replaces the older one. Mirrors `SPEED_SAMPLE_MS`.
pub const SPEED_SAMPLE_MS: i64 = 5_000;
/// Gauge points gained over the last minute that count as "filling fast".
/// Mirrors `FILLING_FAST_POINTS`. Unmeasured starting value.
pub const FILLING_FAST_POINTS: i32 = 15;
const FILLING_FAST_LOOKBACK_MS: i64 = 60_000;

/// Server-only (the client tracker lives for one app session, this one for
/// the process): a symbol not seen for this long, in reading time, is
/// forgotten. Its next reading is then a fresh baseline and cannot alert,
/// which is the conservative direction. Well past the 10-minute cooldown
/// and history window, so pruning never shortens either.
pub const SPEED_SYMBOL_IDLE_MS: i64 = 30 * 60_000;
/// How many observations between prune sweeps. A sweep is O(symbols), so
/// at ~2,000 readings/min this runs about every couple of minutes.
const PRUNE_EVERY: u32 = 4_096;

/// Gauge percent, clamped 0-100. `Math.round` in haltSpeed.ts is
/// floor(x + 0.5); written the same way here so a reading at exactly
/// x.5 rounds identically on both sides (ratio is non-negative, so this
/// also equals `f64::round`, but the mirror should not depend on that).
pub fn gauge_pct(proximity_ratio: f64) -> u8 {
    let ratio = if proximity_ratio.is_finite() { proximity_ratio } else { 0.0 };
    (ratio * 100.0 + 0.5).floor().clamp(0.0, 100.0) as u8
}

/// Move vs the 5-minute rolling reference, in percent. None when the
/// reference is unusable. Mirrors `speedMovePct`.
pub fn speed_move_pct(reference_price: f64, current_price: f64) -> Option<f64> {
    // NaN reference is unusable too, as `!(ref > 0)` treats it in TS.
    if reference_price.is_nan() || reference_price <= 0.0 || !current_price.is_finite() {
        return None;
    }
    Some((current_price - reference_price) / reference_price * 100.0)
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpeedAlert {
    pub symbol: String,
    pub at: DateTime<Utc>,
    pub price: f64,
    pub pct: u8,
    pub move_pct: Option<f64>,
    pub relative_volume: Option<f64>,
    pub filling_fast: bool,
}

#[derive(Debug, Clone, Copy)]
struct SpeedPoint {
    at_ms: i64,
    pct: u8,
}

#[derive(Debug, Default)]
struct SymbolState {
    /// Sampled gauge history, oldest first, pruned to the window.
    points: VecDeque<SpeedPoint>,
    last_seen_ms: i64,
    last_pct: u8,
    last_alert_ms: Option<i64>,
}

impl SymbolState {
    /// Mirrors `addSpeedPoint`. Out-of-order readings never reach here
    /// (observe drops them first).
    fn add_point(&mut self, at_ms: i64, pct: u8) {
        let point = SpeedPoint { at_ms, pct };
        match self.points.back_mut() {
            Some(last) if last.at_ms.div_euclid(SPEED_SAMPLE_MS) == at_ms.div_euclid(SPEED_SAMPLE_MS) => *last = point,
            _ => self.points.push_back(point),
        }
        let cutoff = at_ms - SPEED_HISTORY_WINDOW_MS;
        while self.points.len() > 1 && self.points.front().is_some_and(|p| p.at_ms < cutoff) {
            self.points.pop_front();
        }
    }

    /// Mirrors `speedDelta1m`: newest pct minus the newest point at least
    /// 60 s older; None until the history reaches back a minute.
    fn delta_1m(&self) -> Option<i32> {
        let last = self.points.back()?;
        self.points
            .iter()
            .rev()
            .skip(1)
            .find(|p| last.at_ms - p.at_ms >= FILLING_FAST_LOOKBACK_MS)
            .map(|p| i32::from(last.pct) - i32::from(p.pct))
    }

    fn filling_fast(&self) -> bool {
        self.delta_1m().is_some_and(|d| d >= FILLING_FAST_POINTS)
    }
}

/// Process-lifetime per-symbol state behind the speed push. See the module
/// doc for what it mirrors.
#[derive(Debug, Default)]
pub struct SpeedAlertTracker {
    symbols: HashMap<String, SymbolState>,
    since_prune: u32,
}

impl SpeedAlertTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one `halt_warning` reading; returns the alert it raised, if
    /// any. Mirrors `SpeedTracker.observe` with `shouldSpeedAlert` at the
    /// fixed defaults: up only, premarket included (so `luld_in_effect` is
    /// deliberately not an input), first reading per symbol is a baseline
    /// (a reconnect or snapshot replay cannot fire a burst), and a reading
    /// at or before the last one seen for its symbol is ignored.
    pub fn observe(
        &mut self,
        symbol: &str,
        timestamp: DateTime<Utc>,
        reference_price: f64,
        current_price: f64,
        proximity_ratio: f64,
        relative_volume: Option<f64>,
    ) -> Option<SpeedAlert> {
        let at_ms = timestamp.timestamp_millis();
        self.since_prune += 1;
        if self.since_prune >= PRUNE_EVERY {
            self.since_prune = 0;
            self.prune(at_ms);
        }

        let pct = gauge_pct(proximity_ratio);
        let state = match self.symbols.get_mut(symbol) {
            Some(state) => state,
            None => {
                // Baseline: record it, never alert on it.
                let mut state = SymbolState { last_seen_ms: at_ms, last_pct: pct, ..SymbolState::default() };
                state.add_point(at_ms, pct);
                self.symbols.insert(symbol.to_string(), state);
                return None;
            }
        };
        if at_ms <= state.last_seen_ms {
            return None;
        }
        state.last_seen_ms = at_ms;
        state.add_point(at_ms, pct);
        let previous_pct = std::mem::replace(&mut state.last_pct, pct);

        let crossed = previous_pct < SPEED_ALERT_THRESHOLD_PCT && pct >= SPEED_ALERT_THRESHOLD_PCT;
        let bullish = current_price >= reference_price;
        let cooled = state.last_alert_ms.is_none_or(|last| at_ms - last >= SPEED_ALERT_COOLDOWN_MS);
        if !(crossed && bullish && cooled) {
            return None;
        }
        state.last_alert_ms = Some(at_ms);
        Some(SpeedAlert {
            symbol: symbol.to_string(),
            at: timestamp,
            price: current_price,
            pct,
            move_pct: speed_move_pct(reference_price, current_price),
            relative_volume,
            filling_fast: state.filling_fast(),
        })
    }

    /// Drops symbols idle for longer than `SPEED_SYMBOL_IDLE_MS` relative to
    /// `now_ms` (a reading time). Readings are near-monotonic across the
    /// universe, so the latest reading is a fair "now".
    fn prune(&mut self, now_ms: i64) {
        self.symbols.retain(|_, s| now_ms - s.last_seen_ms <= SPEED_SYMBOL_IDLE_MS);
    }

    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.symbols.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_790_000_000 + secs, 0).unwrap()
    }

    /// Up move (price above reference) at the given gauge ratio.
    fn up(tracker: &mut SpeedAlertTracker, symbol: &str, secs: i64, ratio: f64) -> Option<SpeedAlert> {
        tracker.observe(symbol, t(secs), 1.00, 1.20, ratio, Some(5.2))
    }

    #[test]
    fn gauge_pct_matches_the_client_rounding_and_clamp() {
        assert_eq!(gauge_pct(0.696), 70);
        assert_eq!(gauge_pct(0.694), 69);
        assert_eq!(gauge_pct(0.005), 1);
        assert_eq!(gauge_pct(1.7), 100);
        assert_eq!(gauge_pct(-0.2), 0);
        assert_eq!(gauge_pct(f64::NAN), 0);
        assert_eq!(gauge_pct(f64::INFINITY), 0);
    }

    #[test]
    fn rising_through_the_threshold_fires_with_the_reading_s_details() {
        let mut tracker = SpeedAlertTracker::new();
        assert!(up(&mut tracker, "ABC", 0, 0.50).is_none());
        let alert = up(&mut tracker, "ABC", 1, 0.72).expect("crossing must fire");
        assert_eq!(alert.symbol, "ABC");
        assert_eq!(alert.at, t(1));
        assert_eq!(alert.pct, 72);
        assert!((alert.price - 1.20).abs() < 1e-12);
        assert!((alert.move_pct.unwrap() - 20.0).abs() < 1e-9);
        assert_eq!(alert.relative_volume, Some(5.2));
        assert!(!alert.filling_fast, "history does not reach back a minute yet");
    }

    #[test]
    fn exactly_the_threshold_counts_and_69_does_not() {
        let mut tracker = SpeedAlertTracker::new();
        up(&mut tracker, "ABC", 0, 0.10);
        assert!(up(&mut tracker, "ABC", 1, 0.69).is_none());
        assert!(up(&mut tracker, "ABC", 2, 0.70).is_some());
    }

    #[test]
    fn the_first_reading_is_a_baseline_even_far_above_the_threshold() {
        let mut tracker = SpeedAlertTracker::new();
        assert!(up(&mut tracker, "ABC", 0, 0.95).is_none());
    }

    #[test]
    fn staying_above_the_threshold_does_not_fire_again() {
        let mut tracker = SpeedAlertTracker::new();
        up(&mut tracker, "ABC", 0, 0.80);
        assert!(up(&mut tracker, "ABC", 1, 0.85).is_none());
        assert!(up(&mut tracker, "ABC", 2, 0.99).is_none());
    }

    #[test]
    fn a_down_move_through_the_threshold_is_skipped() {
        let mut tracker = SpeedAlertTracker::new();
        tracker.observe("ABC", t(0), 1.00, 0.85, 0.50, None);
        assert!(tracker.observe("ABC", t(1), 1.00, 0.80, 0.75, None).is_none());
        // And the down reading still became the previous pct: a later up
        // reading at 0.80 is not a crossing.
        assert!(up(&mut tracker, "ABC", 2, 0.80).is_none());
    }

    #[test]
    fn price_equal_to_reference_counts_as_up_like_the_client() {
        let mut tracker = SpeedAlertTracker::new();
        tracker.observe("ABC", t(0), 1.00, 1.00, 0.10, None);
        assert!(tracker.observe("ABC", t(1), 1.00, 1.00, 0.75, None).is_some());
    }

    #[test]
    fn cooldown_is_ten_minutes_of_reading_time() {
        let mut tracker = SpeedAlertTracker::new();
        up(&mut tracker, "ABC", 0, 0.10);
        assert!(up(&mut tracker, "ABC", 1, 0.75).is_some());
        // Drop and re-cross at 9 minutes: still cooling down.
        up(&mut tracker, "ABC", 1 + 9 * 60 - 1, 0.10);
        assert!(up(&mut tracker, "ABC", 1 + 9 * 60, 0.75).is_none());
        // Drop and re-cross at exactly 10 minutes: allowed.
        up(&mut tracker, "ABC", 1 + 10 * 60 - 1, 0.10);
        assert!(up(&mut tracker, "ABC", 1 + 10 * 60, 0.75).is_some());
    }

    #[test]
    fn cooldown_is_per_symbol() {
        let mut tracker = SpeedAlertTracker::new();
        up(&mut tracker, "ABC", 0, 0.10);
        up(&mut tracker, "XYZ", 0, 0.10);
        assert!(up(&mut tracker, "ABC", 1, 0.75).is_some());
        assert!(up(&mut tracker, "XYZ", 1, 0.75).is_some());
    }

    #[test]
    fn out_of_order_and_repeated_readings_are_ignored() {
        let mut tracker = SpeedAlertTracker::new();
        up(&mut tracker, "ABC", 10, 0.10);
        // Older reading crossing the threshold: ignored entirely.
        assert!(up(&mut tracker, "ABC", 5, 0.90).is_none());
        // Same timestamp: ignored too.
        assert!(up(&mut tracker, "ABC", 10, 0.90).is_none());
        // The ignored readings did not move the previous pct, so a real
        // newer crossing still fires.
        assert!(up(&mut tracker, "ABC", 11, 0.90).is_some());
    }

    #[test]
    fn filling_fast_when_the_gauge_gained_15_points_in_a_minute() {
        let mut tracker = SpeedAlertTracker::new();
        up(&mut tracker, "ABC", 0, 0.50);
        up(&mut tracker, "ABC", 30, 0.60);
        let alert = up(&mut tracker, "ABC", 60, 0.70).expect("crossing");
        // 70 - 50 = 20 points in 60 s.
        assert!(alert.filling_fast);
    }

    #[test]
    fn not_filling_fast_when_the_gain_is_under_15_points() {
        let mut tracker = SpeedAlertTracker::new();
        up(&mut tracker, "ABC", 0, 0.58);
        up(&mut tracker, "ABC", 30, 0.65);
        let alert = up(&mut tracker, "ABC", 60, 0.71).expect("crossing");
        // 71 - 58 = 13 points.
        assert!(!alert.filling_fast);
    }

    #[test]
    fn filling_fast_compares_against_the_newest_point_a_minute_older() {
        let mut tracker = SpeedAlertTracker::new();
        // 0 s: 30, 20 s: 60, 80 s: 72. The newest point >= 60 s older than
        // 80 s is the one at 20 s (60), so the delta is 12, not 42.
        up(&mut tracker, "ABC", 0, 0.30);
        up(&mut tracker, "ABC", 20, 0.60);
        let alert = up(&mut tracker, "ABC", 80, 0.72).expect("crossing");
        assert!(!alert.filling_fast);
    }

    #[test]
    fn one_point_per_five_second_bucket_newest_wins() {
        let mut state = SymbolState::default();
        state.add_point(0, 10);
        state.add_point(1_000, 20);
        state.add_point(4_999, 30);
        state.add_point(5_000, 40);
        let pcts: Vec<u8> = state.points.iter().map(|p| p.pct).collect();
        assert_eq!(pcts, vec![30, 40]);
    }

    #[test]
    fn history_is_pruned_to_ten_minutes() {
        let mut state = SymbolState::default();
        for i in 0..=150 {
            state.add_point(i * SPEED_SAMPLE_MS, 10);
        }
        let newest = state.points.back().unwrap().at_ms;
        assert!(state.points.iter().all(|p| newest - p.at_ms <= SPEED_HISTORY_WINDOW_MS));
        assert_eq!(state.points.len(), (SPEED_HISTORY_WINDOW_MS / SPEED_SAMPLE_MS + 1) as usize);
    }

    #[test]
    fn idle_symbols_are_pruned_and_come_back_as_a_baseline() {
        let mut tracker = SpeedAlertTracker::new();
        up(&mut tracker, "OLD", 0, 0.10);
        // Keep another symbol busy past the idle window and past a sweep.
        let start = SPEED_SYMBOL_IDLE_MS / 1000 + 1;
        for i in 0..i64::from(PRUNE_EVERY) {
            up(&mut tracker, "HOT", start + i, 0.10);
        }
        assert_eq!(tracker.tracked(), 1, "OLD should have been dropped");
        // OLD returns above the threshold: a fresh baseline, not an alert.
        let back = start + i64::from(PRUNE_EVERY);
        assert!(up(&mut tracker, "OLD", back, 0.90).is_none());
    }

    #[test]
    fn recently_seen_symbols_survive_a_sweep() {
        let mut tracker = SpeedAlertTracker::new();
        up(&mut tracker, "ABC", 0, 0.10);
        for i in 0..i64::from(PRUNE_EVERY) {
            up(&mut tracker, "HOT", 1 + i / 100, 0.10);
        }
        assert_eq!(tracker.tracked(), 2);
        assert!(up(&mut tracker, "ABC", 100, 0.75).is_some());
    }
}
