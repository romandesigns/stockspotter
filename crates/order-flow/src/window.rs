//! Rolling order-flow windows (specification sections 6 and 7): buy / sell
//! / unknown volume, delta, delta ratio, and trade / share / dollar
//! velocity with acceleration, over 1s, 5s, 15s, 30s, 1m and 5m.
//!
//! A window is the half-open interval `(now - w, now]`. Acceleration
//! compares it with the window immediately before it, `(now - 2w, now - w]`,
//! which is why trades are retained for twice the longest window.
//!
//! Snapshots are recomputed by scanning the retained trades. That is the
//! simple, obviously-correct form, and it is adequate for recording
//! observations on funnel-tracked symbols. If this ever runs on the hot
//! path for the whole market it needs incremental sums instead.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::classify::TradeSide;

/// Required resolutions, shortest first.
pub const WINDOWS_SECS: [f64; 6] = [1.0, 5.0, 15.0, 30.0, 60.0, 300.0];

const RETENTION_SECS: f64 = 2.0 * 300.0;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClassifiedTrade {
    pub timestamp_secs: f64,
    pub price: f64,
    pub size: u64,
    pub side: TradeSide,
    /// Large relative to this symbol's own recent prints (`large.rs`).
    pub large: bool,
}

/// Which cumulative-delta bucket a trade belongs to. Decided by the
/// ingestion layer, which owns the exchange calendar; this crate has no
/// clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Session {
    Premarket,
    Regular,
    AfterHours,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowStats {
    pub window_secs: f64,
    pub buy_volume: u64,
    pub sell_volume: u64,
    /// Kept apart from buy and sell on purpose: uncertain volume must stay
    /// visible instead of being folded into either side.
    pub unknown_volume: u64,
    pub delta: i64,
    /// `(buy - sell) / (buy + sell)`. `None` when nothing was classified,
    /// which is different from a balanced 0.0.
    pub delta_ratio: Option<f64>,
    pub trades: u64,
    pub shares: u64,
    pub dollars: f64,
    pub trades_per_sec: f64,
    pub shares_per_sec: f64,
    pub dollars_per_sec: f64,
    /// This window's share rate over the previous window's. `None` when
    /// the previous window was empty: "from nothing" is not a multiple.
    pub share_velocity_acceleration: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowSnapshot {
    pub as_of_secs: f64,
    /// One entry per `WINDOWS_SECS`, same order. The 5-minute entry's
    /// `delta` is the rolling cumulative delta.
    pub windows: Vec<WindowStats>,
    pub cumulative_delta_premarket: i64,
    pub cumulative_delta_regular: i64,
    pub cumulative_delta_after_hours: i64,
}

/// Per-symbol rolling state.
#[derive(Debug, Clone, Default)]
pub struct FlowTracker {
    trades: VecDeque<ClassifiedTrade>,
    cumulative_premarket: i64,
    cumulative_regular: i64,
    cumulative_after_hours: i64,
}

impl FlowTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on_trade(&mut self, trade: ClassifiedTrade, session: Session) {
        let signed = signed_size(&trade);
        match session {
            Session::Premarket => self.cumulative_premarket += signed,
            Session::Regular => self.cumulative_regular += signed,
            Session::AfterHours => self.cumulative_after_hours += signed,
        }
        let cutoff = trade.timestamp_secs - RETENTION_SECS;
        self.trades.push_back(trade);
        while self.trades.front().is_some_and(|t| t.timestamp_secs <= cutoff) {
            self.trades.pop_front();
        }
    }

    /// Clears everything for a new trading day.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Retained trades, oldest first (twice the longest window).
    pub fn trades(&self) -> impl Iterator<Item = &ClassifiedTrade> {
        self.trades.iter()
    }

    pub fn snapshot(&self, now_secs: f64) -> FlowSnapshot {
        FlowSnapshot {
            as_of_secs: now_secs,
            windows: WINDOWS_SECS.iter().map(|&w| self.window_stats(now_secs, w)).collect(),
            cumulative_delta_premarket: self.cumulative_premarket,
            cumulative_delta_regular: self.cumulative_regular,
            cumulative_delta_after_hours: self.cumulative_after_hours,
        }
    }

    fn window_stats(&self, now: f64, window: f64) -> WindowStats {
        let (mut buy, mut sell, mut unknown) = (0u64, 0u64, 0u64);
        let (mut trades, mut dollars, mut previous_shares) = (0u64, 0.0f64, 0u64);
        for t in &self.trades {
            let age = now - t.timestamp_secs;
            if age < 0.0 {
                continue;
            }
            if age < window {
                match t.side {
                    TradeSide::Buy => buy += t.size,
                    TradeSide::Sell => sell += t.size,
                    TradeSide::Unknown => unknown += t.size,
                }
                trades += 1;
                dollars += t.price * t.size as f64;
            } else if age < 2.0 * window {
                previous_shares += t.size;
            }
        }
        let shares = buy + sell + unknown;
        let classified = buy + sell;
        WindowStats {
            window_secs: window,
            buy_volume: buy,
            sell_volume: sell,
            unknown_volume: unknown,
            delta: buy as i64 - sell as i64,
            delta_ratio: (classified > 0).then(|| (buy as f64 - sell as f64) / classified as f64),
            trades,
            shares,
            dollars,
            trades_per_sec: trades as f64 / window,
            shares_per_sec: shares as f64 / window,
            dollars_per_sec: dollars / window,
            share_velocity_acceleration: (previous_shares > 0).then(|| shares as f64 / previous_shares as f64),
        }
    }
}

fn signed_size(trade: &ClassifiedTrade) -> i64 {
    match trade.side {
        TradeSide::Buy => trade.size as i64,
        TradeSide::Sell => -(trade.size as i64),
        TradeSide::Unknown => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(timestamp_secs: f64, price: f64, size: u64, side: TradeSide) -> ClassifiedTrade {
        ClassifiedTrade { timestamp_secs, price, size, side, large: false }
    }

    fn window(snapshot: &FlowSnapshot, secs: f64) -> WindowStats {
        *snapshot.windows.iter().find(|w| w.window_secs == secs).unwrap()
    }

    #[test]
    fn delta_and_ratio_keep_unknown_volume_separate() {
        let mut tracker = FlowTracker::new();
        tracker.on_trade(t(100.0, 2.0, 700, TradeSide::Buy), Session::Regular);
        tracker.on_trade(t(100.2, 2.0, 100, TradeSide::Sell), Session::Regular);
        tracker.on_trade(t(100.4, 2.0, 200, TradeSide::Unknown), Session::Regular);
        let w = window(&tracker.snapshot(100.5), 1.0);
        assert_eq!((w.buy_volume, w.sell_volume, w.unknown_volume), (700, 100, 200));
        assert_eq!(w.delta, 600);
        // Unknown is excluded from the denominator: 600 / 800.
        assert!((w.delta_ratio.unwrap() - 0.75).abs() < 1e-9);
        assert_eq!(w.shares, 1000);
        assert_eq!(w.trades, 3);
        assert!((w.dollars - 2000.0).abs() < 1e-9);
    }

    #[test]
    fn empty_and_unclassified_windows_have_no_ratio() {
        let mut tracker = FlowTracker::new();
        assert_eq!(window(&tracker.snapshot(10.0), 5.0).delta_ratio, None);
        tracker.on_trade(t(10.0, 2.0, 500, TradeSide::Unknown), Session::Regular);
        let w = window(&tracker.snapshot(10.5), 5.0);
        assert_eq!(w.delta_ratio, None);
        assert_eq!(w.unknown_volume, 500);
        assert_eq!(w.share_velocity_acceleration, None);
    }

    #[test]
    fn windows_are_half_open_and_nest() {
        let mut tracker = FlowTracker::new();
        tracker.on_trade(t(94.0, 2.0, 10, TradeSide::Buy), Session::Regular);
        tracker.on_trade(t(99.0, 2.0, 20, TradeSide::Buy), Session::Regular);
        tracker.on_trade(t(99.8, 2.0, 40, TradeSide::Buy), Session::Regular);
        let snap = tracker.snapshot(100.0);
        assert_eq!(window(&snap, 1.0).shares, 40);
        // A trade exactly `w` old has left the window and is in the previous one.
        assert_eq!(window(&snap, 5.0).shares, 60);
        assert_eq!(window(&snap, 15.0).shares, 70);
    }

    #[test]
    fn velocity_and_acceleration_match_the_specification_example() {
        // Previous 30s at 18,000 shares/sec, current 30s at 74,000 -> 4.11x.
        let mut tracker = FlowTracker::new();
        for i in 0..30 {
            tracker.on_trade(t(i as f64 + 0.5, 2.0, 18_000, TradeSide::Buy), Session::Regular);
        }
        for i in 30..60 {
            tracker.on_trade(t(i as f64 + 0.5, 2.0, 74_000, TradeSide::Buy), Session::Regular);
        }
        let w = window(&tracker.snapshot(60.0), 30.0);
        assert!((w.shares_per_sec - 74_000.0).abs() < 1e-6);
        assert!((w.trades_per_sec - 1.0).abs() < 1e-9);
        assert!((w.dollars_per_sec - 148_000.0).abs() < 1e-6);
        assert!((w.share_velocity_acceleration.unwrap() - 74.0 / 18.0).abs() < 1e-9);
    }

    #[test]
    fn cumulative_delta_is_split_by_session_and_survives_eviction() {
        let mut tracker = FlowTracker::new();
        tracker.on_trade(t(0.0, 2.0, 300, TradeSide::Buy), Session::Premarket);
        tracker.on_trade(t(1.0, 2.0, 100, TradeSide::Sell), Session::Premarket);
        tracker.on_trade(t(2.0, 2.0, 999, TradeSide::Unknown), Session::Premarket);
        tracker.on_trade(t(5_000.0, 2.0, 50, TradeSide::Sell), Session::Regular);
        tracker.on_trade(t(30_000.0, 2.0, 70, TradeSide::Buy), Session::AfterHours);
        let snap = tracker.snapshot(30_000.0);
        assert_eq!(snap.cumulative_delta_premarket, 200);
        assert_eq!(snap.cumulative_delta_regular, -50);
        assert_eq!(snap.cumulative_delta_after_hours, 70);
        // Old trades are gone from the rolling windows but not the totals.
        assert_eq!(window(&snap, 300.0).shares, 70);
        assert_eq!(tracker.trades.len(), 1);
    }

    #[test]
    fn reset_clears_windows_and_cumulative_totals() {
        let mut tracker = FlowTracker::new();
        tracker.on_trade(t(1.0, 2.0, 300, TradeSide::Buy), Session::Regular);
        tracker.reset();
        let snap = tracker.snapshot(2.0);
        assert_eq!(snap.cumulative_delta_regular, 0);
        assert_eq!(window(&snap, 300.0).shares, 0);
    }

    #[test]
    fn snapshot_has_every_required_window_in_order() {
        let snap = FlowTracker::new().snapshot(0.0);
        let secs: Vec<f64> = snap.windows.iter().map(|w| w.window_secs).collect();
        assert_eq!(secs, vec![1.0, 5.0, 15.0, 30.0, 60.0, 300.0]);
    }
}
