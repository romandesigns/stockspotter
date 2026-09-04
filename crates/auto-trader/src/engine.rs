//! The actual decision logic — entirely in-memory, zero real HTTP calls.
//! Deliberately I/O-free (no file writes happen in here) so it's testable
//! by feeding it a scripted `ScanEvent` sequence directly, the same way
//! `backtest_metrics::LiveSignalTracker::on_event` is tested without a
//! real broadcast channel. The caller (`main.rs`) is responsible for
//! appending whatever `JournalEntry`s come back to the actual journal
//! file via `journal::append`.

use std::collections::HashMap;

use backtest_metrics::{OutcomeThresholds, Strategy};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc, Weekday};
use market_data::{classify_session, ConsolidationEventKind, ConsolidationStrategy, ScanEvent, TradingSession};

use crate::config::Config;
use crate::journal::{ExitReason, JournalEntry, SkipReason};

/// Matches `apps/client/src/lib/momentumLabel.ts`'s `FACTOR_GOOD_THRESHOLD`
/// exactly, ported to Rust for the first time here (confirmed: no Rust
/// equivalent existed anywhere before this). Deliberately the SAME
/// number as the web/mobile micropullback alert's own gate
/// (`catalystConfirmation`) — "high confidence" should mean the
/// identical thing everywhere this feature family touches, not a
/// fourth, silently-different threshold.
const MOMENTUM_CONFIRM_THRESHOLD: f64 = 0.6;

#[derive(Debug, Clone)]
struct SimulatedPosition {
    qty: u64,
    entry_price: f64,
    entered_at: DateTime<Utc>,
    target_price: f64,
    stop_price: f64,
    max_hold_until: DateTime<Utc>,
}

#[derive(Debug, Clone, Default)]
pub struct RunningStats {
    pub trades: u32,
    pub wins: u32,
    pub losses: u32,
    pub cumulative_pnl_usd: f64,
}

pub struct Engine {
    config: Config,
    /// symbol -> (overall, volume_confirmation), updated on every
    /// `MomentumUpdate` — same shape the web/mobile alert hooks keep
    /// their own `momentumBySymbol` state in, just ported to Rust.
    momentum: HashMap<String, (f64, f64)>,
    open_positions: HashMap<String, SimulatedPosition>,
    /// symbol -> last entry date (UTC calendar date — an accepted
    /// simplification; the boundary lands at 8PM ET, well outside
    /// trading hours, so it can't split a real session in practice).
    entries_today: HashMap<String, NaiveDate>,
    pub stats: RunningStats,
}

impl Engine {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            momentum: HashMap::new(),
            open_positions: HashMap::new(),
            entries_today: HashMap::new(),
            stats: RunningStats::default(),
        }
    }

    /// Processes one `ScanEvent`, returning whatever journal-worthy
    /// decisions it produced (0 or 1 in every real case: a
    /// `MomentumUpdate` never produces one, a `ConsolidationEvent`
    /// produces exactly one `Entered`/`Skipped`, a `BarUpdate` produces
    /// at most one `Exited` since it's symbol-specific).
    pub fn on_event(&mut self, event: &ScanEvent) -> Vec<JournalEntry> {
        match event {
            ScanEvent::MomentumUpdate { symbol, overall, volume_confirmation, .. } => {
                self.momentum.insert(symbol.clone(), (*overall, *volume_confirmation));
                Vec::new()
            }
            ScanEvent::ConsolidationEvent {
                symbol,
                timestamp,
                price,
                kind: ConsolidationEventKind::EntryTriggered,
                strategy: ConsolidationStrategy::Micropullback,
            } => vec![self.try_enter(symbol, *price, *timestamp)],
            ScanEvent::BarUpdate { symbol, timestamp, close, interval_secs: 60, .. } => {
                match self.check_exit(symbol, *close, *timestamp) {
                    Some(entry) => vec![entry],
                    None => Vec::new(),
                }
            }
            // Every other variant (FunnelSignal, IgnitionEvent, HaltWarning,
            // CatalystUpdate, the 30s BarUpdate stream, non-micropullback or
            // non-EntryTriggered ConsolidationEvents) is real information
            // this service simply doesn't act on -- not an oversight, this
            // is Micropullback-only by Roman's own explicit scoping.
            _ => Vec::new(),
        }
    }

    fn try_enter(&mut self, symbol: &str, price: f64, timestamp: DateTime<Utc>) -> JournalEntry {
        let session = classify_session(timestamp);
        let is_weekday = !matches!(timestamp.weekday(), Weekday::Sat | Weekday::Sun);
        if session != TradingSession::Regular || !is_weekday {
            return JournalEntry::Skipped {
                symbol: symbol.to_string(),
                reason: SkipReason::OutsideRegularHours,
                at: timestamp,
                detail: format!("session={session:?} weekday={:?}", timestamp.weekday()),
            };
        }

        let Some(&(overall, volume_confirmation)) = self.momentum.get(symbol) else {
            return JournalEntry::Skipped {
                symbol: symbol.to_string(),
                reason: SkipReason::MomentumGateFailed,
                at: timestamp,
                detail: "no momentum data yet for this symbol".to_string(),
            };
        };
        if overall < MOMENTUM_CONFIRM_THRESHOLD || volume_confirmation < MOMENTUM_CONFIRM_THRESHOLD {
            return JournalEntry::Skipped {
                symbol: symbol.to_string(),
                reason: SkipReason::MomentumGateFailed,
                at: timestamp,
                detail: format!(
                    "overall={overall:.2} volumeConfirmation={volume_confirmation:.2}, need >= {MOMENTUM_CONFIRM_THRESHOLD}"
                ),
            };
        }

        if self.open_positions.len() >= self.config.max_concurrent_positions {
            return JournalEntry::Skipped {
                symbol: symbol.to_string(),
                reason: SkipReason::MaxConcurrentPositions,
                at: timestamp,
                detail: format!("{} of {} slots already open", self.open_positions.len(), self.config.max_concurrent_positions),
            };
        }

        let today = timestamp.date_naive();
        if self.entries_today.get(symbol) == Some(&today) {
            return JournalEntry::Skipped {
                symbol: symbol.to_string(),
                reason: SkipReason::AlreadyEnteredToday,
                at: timestamp,
                detail: format!("already entered {symbol} on {today}"),
            };
        }

        if price <= 0.0 {
            return JournalEntry::Skipped {
                symbol: symbol.to_string(),
                reason: SkipReason::ZeroQuantity,
                at: timestamp,
                detail: format!("non-positive signal price {price}"),
            };
        }
        let qty = (self.config.position_size_usd / price).floor() as u64;
        if qty == 0 {
            return JournalEntry::Skipped {
                symbol: symbol.to_string(),
                reason: SkipReason::ZeroQuantity,
                at: timestamp,
                detail: format!("price {price} exceeds the ${} position budget", self.config.position_size_usd),
            };
        }

        // The already-backtested "scalp" profile, not re-hardcoded here --
        // see OutcomeThresholds::for_strategy's own doc comment for why
        // Micropullback shares Ignition's fast-microstructure profile.
        let thresholds = OutcomeThresholds::for_strategy(Strategy::Micropullback);
        let target_price = price * (1.0 + thresholds.target_pct / 100.0);
        let stop_price = price * (1.0 - thresholds.stop_pct / 100.0);
        let max_hold_until = timestamp + Duration::minutes(thresholds.lookforward_bars as i64);

        self.open_positions.insert(
            symbol.to_string(),
            SimulatedPosition { qty, entry_price: price, entered_at: timestamp, target_price, stop_price, max_hold_until },
        );
        self.entries_today.insert(symbol.to_string(), today);

        JournalEntry::Entered {
            symbol: symbol.to_string(),
            entry_price: price,
            qty,
            position_size_usd: self.config.position_size_usd,
            target_price,
            stop_price,
            entered_at: timestamp,
            momentum_overall: overall,
            momentum_volume_confirmation: volume_confirmation,
        }
    }

    fn check_exit(&mut self, symbol: &str, close: f64, timestamp: DateTime<Utc>) -> Option<JournalEntry> {
        let reason = {
            let position = self.open_positions.get(symbol)?;
            if close >= position.target_price {
                ExitReason::TargetHit
            } else if close <= position.stop_price {
                ExitReason::StopHit
            } else if timestamp >= position.max_hold_until {
                ExitReason::Timeout
            } else {
                return None;
            }
        };

        let position = self.open_positions.remove(symbol).expect("presence just confirmed above");
        let pnl_usd = (close - position.entry_price) * position.qty as f64;
        let pnl_pct = (close - position.entry_price) / position.entry_price * 100.0;

        self.stats.trades += 1;
        if pnl_usd > 0.0 {
            self.stats.wins += 1;
        } else {
            self.stats.losses += 1;
        }
        self.stats.cumulative_pnl_usd += pnl_usd;

        Some(JournalEntry::Exited {
            symbol: symbol.to_string(),
            exit_price: close,
            exit_reason: reason,
            pnl_usd,
            pnl_pct,
            qty: position.qty,
            entered_at: position.entered_at,
            exited_at: timestamp,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn cfg() -> Config {
        Config {
            ws_url: "ws://localhost:8787".to_string(),
            position_size_usd: 500.0,
            max_concurrent_positions: 4,
            journal_path: "unused-in-tests.jsonl".to_string(),
        }
    }

    // Wed Sep 2 2026, 14:30 UTC = 10:30 AM EDT -- squarely regular session.
    fn regular_session_ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 2, 14, 30, 0).unwrap()
    }

    // Same wall-clock date, but 11:00 UTC = 7:00 AM EDT -- premarket.
    fn premarket_ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 2, 11, 0, 0).unwrap()
    }

    fn momentum_update(symbol: &str, overall: f64, volume_confirmation: f64, ts: DateTime<Utc>) -> ScanEvent {
        ScanEvent::MomentumUpdate {
            symbol: symbol.to_string(),
            timestamp: ts,
            volume_confirmation,
            structure: 0.0,
            ma_slope: 0.0,
            wick_rejection: 0.0,
            overall,
            qualifies: true,
        }
    }

    fn entry_triggered(symbol: &str, price: f64, ts: DateTime<Utc>) -> ScanEvent {
        ScanEvent::ConsolidationEvent {
            symbol: symbol.to_string(),
            timestamp: ts,
            price,
            kind: ConsolidationEventKind::EntryTriggered,
            strategy: ConsolidationStrategy::Micropullback,
        }
    }

    fn bar_60s(symbol: &str, close: f64, ts: DateTime<Utc>) -> ScanEvent {
        ScanEvent::BarUpdate { symbol: symbol.to_string(), timestamp: ts, open: close, high: close, low: close, close, volume: 1000, interval_secs: 60 }
    }

    #[test]
    fn skips_when_no_momentum_data_exists_yet() {
        let mut engine = Engine::new(cfg());
        let entries = engine.on_event(&entry_triggered("SWVL", 3.00, regular_session_ts()));
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0], JournalEntry::Skipped { reason: SkipReason::MomentumGateFailed, .. }));
    }

    #[test]
    fn skips_when_momentum_is_below_threshold() {
        let mut engine = Engine::new(cfg());
        engine.on_event(&momentum_update("SWVL", 0.5, 0.5, regular_session_ts()));
        let entries = engine.on_event(&entry_triggered("SWVL", 3.00, regular_session_ts()));
        assert!(matches!(entries[0], JournalEntry::Skipped { reason: SkipReason::MomentumGateFailed, .. }));
    }

    #[test]
    fn enters_when_momentum_confirms_and_session_is_regular() {
        let mut engine = Engine::new(cfg());
        engine.on_event(&momentum_update("SWVL", 0.72, 0.68, regular_session_ts()));
        let entries = engine.on_event(&entry_triggered("SWVL", 3.00, regular_session_ts()));
        match &entries[0] {
            JournalEntry::Entered { qty, target_price, stop_price, .. } => {
                assert_eq!(*qty, 166); // floor(500 / 3.00)
                assert!((*target_price - 3.06).abs() < 1e-9); // +2%
                assert!((*stop_price - 2.94).abs() < 1e-9); // -2%
            }
            other => panic!("expected Entered, got {other:?}"),
        }
    }

    #[test]
    fn skips_outside_regular_session_hours_even_with_confirmed_momentum() {
        let mut engine = Engine::new(cfg());
        engine.on_event(&momentum_update("SWVL", 0.9, 0.9, premarket_ts()));
        let entries = engine.on_event(&entry_triggered("SWVL", 3.00, premarket_ts()));
        assert!(matches!(entries[0], JournalEntry::Skipped { reason: SkipReason::OutsideRegularHours, .. }));
    }

    #[test]
    fn skips_zero_quantity_when_price_exceeds_position_budget() {
        let mut engine = Engine::new(cfg());
        engine.on_event(&momentum_update("BRKA", 0.9, 0.9, regular_session_ts()));
        let entries = engine.on_event(&entry_triggered("BRKA", 600_000.0, regular_session_ts()));
        assert!(matches!(entries[0], JournalEntry::Skipped { reason: SkipReason::ZeroQuantity, .. }));
    }

    #[test]
    fn enforces_max_concurrent_positions() {
        let mut config = cfg();
        config.max_concurrent_positions = 1;
        let mut engine = Engine::new(config);
        engine.on_event(&momentum_update("AAA", 0.9, 0.9, regular_session_ts()));
        engine.on_event(&momentum_update("BBB", 0.9, 0.9, regular_session_ts()));
        let first = engine.on_event(&entry_triggered("AAA", 3.00, regular_session_ts()));
        assert!(matches!(first[0], JournalEntry::Entered { .. }));
        let second = engine.on_event(&entry_triggered("BBB", 3.00, regular_session_ts()));
        assert!(matches!(second[0], JournalEntry::Skipped { reason: SkipReason::MaxConcurrentPositions, .. }));
    }

    #[test]
    fn enforces_one_entry_per_symbol_per_day() {
        let mut engine = Engine::new(cfg());
        engine.on_event(&momentum_update("SWVL", 0.9, 0.9, regular_session_ts()));
        let first = engine.on_event(&entry_triggered("SWVL", 3.00, regular_session_ts()));
        assert!(matches!(first[0], JournalEntry::Entered { .. }));

        // Exit it first (position occupies the symbol slot otherwise the
        // second EntryTriggered wouldn't even reach the day-dedup check).
        engine.on_event(&bar_60s("SWVL", 3.10, regular_session_ts() + Duration::minutes(1)));

        let second = engine.on_event(&entry_triggered("SWVL", 3.00, regular_session_ts() + Duration::minutes(2)));
        assert!(matches!(second[0], JournalEntry::Skipped { reason: SkipReason::AlreadyEnteredToday, .. }));
    }

    #[test]
    fn exits_on_target_hit() {
        let mut engine = Engine::new(cfg());
        engine.on_event(&momentum_update("SWVL", 0.9, 0.9, regular_session_ts()));
        engine.on_event(&entry_triggered("SWVL", 3.00, regular_session_ts()));
        let entries = engine.on_event(&bar_60s("SWVL", 3.06, regular_session_ts() + Duration::minutes(2)));
        match &entries[0] {
            JournalEntry::Exited { exit_reason: ExitReason::TargetHit, pnl_usd, .. } => {
                assert!(*pnl_usd > 0.0);
            }
            other => panic!("expected Exited/TargetHit, got {other:?}"),
        }
        assert_eq!(engine.stats.wins, 1);
    }

    #[test]
    fn exits_on_stop_hit() {
        let mut engine = Engine::new(cfg());
        engine.on_event(&momentum_update("SWVL", 0.9, 0.9, regular_session_ts()));
        engine.on_event(&entry_triggered("SWVL", 3.00, regular_session_ts()));
        let entries = engine.on_event(&bar_60s("SWVL", 2.94, regular_session_ts() + Duration::minutes(2)));
        assert!(matches!(entries[0], JournalEntry::Exited { exit_reason: ExitReason::StopHit, .. }));
        assert_eq!(engine.stats.losses, 1);
    }

    #[test]
    fn exits_on_timeout_when_neither_target_nor_stop_hit_within_the_hold_window() {
        let mut engine = Engine::new(cfg());
        engine.on_event(&momentum_update("SWVL", 0.9, 0.9, regular_session_ts()));
        engine.on_event(&entry_triggered("SWVL", 3.00, regular_session_ts()));
        // Still between stop and target, but past the 10-minute scalp window.
        let entries = engine.on_event(&bar_60s("SWVL", 3.01, regular_session_ts() + Duration::minutes(11)));
        assert!(matches!(entries[0], JournalEntry::Exited { exit_reason: ExitReason::Timeout, .. }));
    }

    #[test]
    fn stays_open_when_price_is_between_stop_and_target_and_still_within_the_hold_window() {
        let mut engine = Engine::new(cfg());
        engine.on_event(&momentum_update("SWVL", 0.9, 0.9, regular_session_ts()));
        engine.on_event(&entry_triggered("SWVL", 3.00, regular_session_ts()));
        let entries = engine.on_event(&bar_60s("SWVL", 3.01, regular_session_ts() + Duration::minutes(2)));
        assert!(entries.is_empty());
    }

    #[test]
    fn ignores_30s_sub_minute_bars_for_exit_monitoring() {
        let mut engine = Engine::new(cfg());
        engine.on_event(&momentum_update("SWVL", 0.9, 0.9, regular_session_ts()));
        engine.on_event(&entry_triggered("SWVL", 3.00, regular_session_ts()));
        let bar_30s = ScanEvent::BarUpdate {
            symbol: "SWVL".to_string(),
            timestamp: regular_session_ts() + Duration::minutes(2),
            open: 3.06,
            high: 3.06,
            low: 3.06,
            close: 3.06, // would hit target on a 60s bar
            volume: 1000,
            interval_secs: 30,
        };
        let entries = engine.on_event(&bar_30s);
        assert!(entries.is_empty(), "30s bars must not drive exit decisions -- thresholds were calibrated against 1-minute bars");
    }

    /// End-to-end scripted sequence: confirm -> enter -> exit, asserting
    /// the exact resulting JournalEntry sequence, mirroring how
    /// `LiveSignalTracker::on_event` is itself tested without any real
    /// WS connection.
    #[test]
    fn full_scripted_sequence_produces_the_expected_journal() {
        let mut engine = Engine::new(cfg());
        let t0 = regular_session_ts();

        let mut produced = Vec::new();
        produced.extend(engine.on_event(&momentum_update("SWVL", 0.75, 0.70, t0)));
        produced.extend(engine.on_event(&entry_triggered("SWVL", 3.00, t0)));
        produced.extend(engine.on_event(&bar_60s("SWVL", 3.01, t0 + Duration::minutes(1))));
        produced.extend(engine.on_event(&bar_60s("SWVL", 3.07, t0 + Duration::minutes(3))));

        // MomentumUpdate produced nothing, the mid-flight bar stayed open
        // (nothing produced), only the entry and the final exit did.
        assert_eq!(produced.len(), 2);
        assert!(matches!(produced[0], JournalEntry::Entered { .. }));
        assert!(matches!(produced[1], JournalEntry::Exited { exit_reason: ExitReason::TargetHit, .. }));
        assert!(engine.open_positions.is_empty());
    }
}
