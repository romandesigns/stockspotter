//! The actual decision logic — entirely in-memory, zero real HTTP calls.
//! Deliberately I/O-free (no file writes happen in here) so it's testable
//! by feeding it a scripted `ScanEvent` sequence directly, the same way
//! `backtest_metrics::LiveSignalTracker::on_event` is tested without a
//! real broadcast channel. The caller (`main.rs`) is responsible for
//! appending whatever `JournalEntry`s come back to the actual journal
//! file via `journal::append`.
//!
//! v2 (2026-09-04, Roman's own recap/ask): a trailing stop that only
//! ever moves up, real use of two signals that arrived on this same WS
//! connection all along but were silently dropped (HaltWarning,
//! CatalystUpdate), a momentum-deterioration early exit, and a bounded,
//! honestly-gated self-adapting position size. All four are real risk-
//! reduction mechanisms, not speculative "AI" -- see each piece's own
//! doc comment for why.
//!
//! v3 (2026-09-04, Roman's own follow-up ask: "Micropullbacks can happen
//! on different time frames. We dont only want to target those entries.
//! Auto Trader should participate where ever it has identify
//! opportunities for profit by utilizing stockspotter resources,
//! context"): no longer Micropullback-only. `try_enter` now takes any
//! trigger source and looks up that strategy's own already-backtested
//! bracket via `OutcomeThresholds::for_strategy` -- every other gate
//! (regular-hours, halt-risk, momentum confirmation, one-per-day, max-
//! concurrent) already applied uniformly regardless of trigger source,
//! so this *is* "utilizing stockspotter's context" for real, not a new
//! mechanism. Which strategies actually get wired up as triggers is a
//! real evidence call, not "everything" -- see `on_event`'s own comment
//! on which three and why the other two (FastFunnel, MomentumScorer)
//! are deliberately left out for now. Anthropic /assess integration
//! (also named in the ask) is NOT wired in here -- that endpoint is
//! currently down on a billing issue (found and reported the same day,
//! see the ops memory log), and this engine stays deliberately I/O-free
//! (see the top of this doc comment); worth revisiting as a real
//! optional signal once the endpoint is back and once there's a design
//! for how a slow HTTP call fits an otherwise synchronous, in-memory
//! decision path.

use std::collections::{HashMap, VecDeque};

use backtest_metrics::{OutcomeThresholds, Strategy};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc, Weekday};
use market_data::{classify_session, ConsolidationEventKind, ConsolidationStrategy, HaltAlertLevel, IgnitionEventKind, ScanEvent, TradingSession};

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

/// Same "critical" tier boundary `MomentumScoreRow.tsx` already uses on
/// both frontends (`overall >= 0.6 good, >= 0.4 warning, else
/// critical`) — reused directly, not a new number invented for this
/// exit specifically.
const MOMENTUM_DETERIORATION_THRESHOLD: f64 = 0.4;

/// This project's own established "don't trust a sample smaller than
/// this" bar (see the live-efficiency benchmark's own reasoning) —
/// reused directly for gating when position-size adaptation is even
/// allowed to start, not a fresh number picked for this specifically.
const MIN_TRADES_BEFORE_ADAPTING_SIZE: usize = 20;
const ROLLING_WINDOW: usize = 20;
const CLOSED_TRADES_HISTORY_CAP: usize = 50;
const WIN_RATE_SCALE_UP_THRESHOLD: f64 = 0.55;
const WIN_RATE_SCALE_DOWN_THRESHOLD: f64 = 0.45;
const ADAPT_SIZE_UP_FACTOR: f64 = 1.1;
const ADAPT_SIZE_DOWN_FACTOR: f64 = 0.8;
const MIN_POSITION_SIZE_USD: f64 = 100.0;
const MAX_POSITION_SIZE_MULTIPLIER: f64 = 1.5;

#[derive(Debug, Clone)]
struct SimulatedPosition {
    qty: u64,
    entry_price: f64,
    entered_at: DateTime<Utc>,
    target_price: f64,
    /// Mutable now (v2) — trails up behind `highest_price_since_entry`,
    /// never moves down. Starts equal to the old fixed 2%-below-entry
    /// value, so a trade that never goes anywhere behaves identically
    /// to before.
    stop_price: f64,
    highest_price_since_entry: f64,
    max_hold_until: DateTime<Utc>,
    /// Which strategy's trigger opened this position (v3) — needed so
    /// the trailing-stop recompute in `on_bar` uses the SAME bracket
    /// (`OutcomeThresholds::for_strategy`) the position was actually
    /// opened and sized with, not a hardcoded one. A symbol can only
    /// ever have one open position at a time (the one-per-day gate), so
    /// this never needs to disambiguate between two simultaneous
    /// strategies on the same symbol.
    strategy: Strategy,
}

/// Just enough to compute a rolling win rate — see `position_size_usd`.
#[derive(Debug, Clone, Copy)]
struct TradeOutcome {
    pnl_usd: f64,
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
    /// symbol -> latest known halt-proximity level, updated on every
    /// `HaltWarning` — real signal this process always received but
    /// dropped until v2.
    halt_level: HashMap<String, HaltAlertLevel>,
    /// symbol -> latest known catalyst tags, updated on every
    /// `CatalystUpdate` — informational context on `Entered`, not a
    /// gate (see try_enter's own comment on why).
    catalyst_tags: HashMap<String, Vec<String>>,
    /// symbol -> most recent known close, from the 60s BarUpdate stream
    /// — needed as a real exit price when a momentum-deterioration exit
    /// is triggered by a MomentumUpdate event, which carries no price of
    /// its own (same reason LiveSignalTracker keeps an identical map).
    last_price: HashMap<String, f64>,
    open_positions: HashMap<String, SimulatedPosition>,
    /// symbol -> last entry date (UTC calendar date — an accepted
    /// simplification; the boundary lands at 8PM ET, well outside
    /// trading hours, so it can't split a real session in practice).
    entries_today: HashMap<String, NaiveDate>,
    /// Capped rolling history of this engine's own real closed trades —
    /// the only input `position_size_usd` uses to adapt. Resets on
    /// process restart, same as every other in-memory field here.
    closed_trades: VecDeque<TradeOutcome>,
    pub stats: RunningStats,
}

impl Engine {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            momentum: HashMap::new(),
            halt_level: HashMap::new(),
            catalyst_tags: HashMap::new(),
            last_price: HashMap::new(),
            open_positions: HashMap::new(),
            entries_today: HashMap::new(),
            closed_trades: VecDeque::new(),
            stats: RunningStats::default(),
        }
    }

    /// Processes one `ScanEvent`, returning whatever journal-worthy
    /// decisions it produced. No longer always 0-or-1 as of v2: a single
    /// 60s bar can now produce BOTH a `StopAdjusted` (the trailing stop
    /// ratcheting up) and an `Exited` on the same call.
    pub fn on_event(&mut self, event: &ScanEvent) -> Vec<JournalEntry> {
        match event {
            ScanEvent::MomentumUpdate { symbol, timestamp, overall, volume_confirmation, .. } => {
                self.momentum.insert(symbol.clone(), (*overall, *volume_confirmation));
                self.check_momentum_deterioration(symbol, *overall, *timestamp)
            }
            ScanEvent::HaltWarning { symbol, level, .. } => {
                self.halt_level.insert(symbol.clone(), *level);
                Vec::new()
            }
            ScanEvent::CatalystUpdate { symbol, catalyst_tags, .. } => {
                self.catalyst_tags.insert(symbol.clone(), catalyst_tags.clone());
                Vec::new()
            }
            // Three real entry triggers now (v3), not one -- each maps to
            // its own already-backtested bracket via
            // `OutcomeThresholds::for_strategy`:
            //
            // - Micropullback (ConsolidationEvent/EntryTriggered): the
            //   original, fast "act within seconds" resumption. Scalp
            //   bracket. Real evidence is still thin (2 live signals ever
            //   as of this morning's benchmark) -- kept because it's
            //   already the reason this engine exists, not because it's
            //   proven.
            // - IgnitionDetector (IgnitionEvent/FollowThroughConfirmed):
            //   the strongest real evidence this platform has (5,879
            //   live-evaluated signals, 35.6% hit rate, same scalp
            //   bracket already backtested specifically for it -- see
            //   OutcomeThresholds::for_strategy's own doc comment). The
            //   clear, evidence-backed reason to broaden past
            //   Micropullback-only.
            // - ConsolidationBreakout (ConsolidationEvent/EntryTriggered,
            //   the slower/2-candle-minimum sibling config): swing
            //   bracket. Real evidence is thinner still (2 live signals
            //   ever) -- added for the same reason Micropullback was kept
            //   at 2: this is a dry-run paper journal, the right way to
            //   let real evidence accumulate is to let it trade on paper,
            //   not to wait for a sample size that can only grow by
            //   watching.
            //
            // Deliberately NOT wired up as standalone triggers:
            // FastFunnel and MomentumScorer. Neither has a discrete
            // "entry" event on the wire (both are continuous qualifying-
            // state streams, not edge-triggered signals like the three
            // above) -- MomentumScorer already participates as this
            // engine's own entry gate below, just not as an independent
            // trigger. MomentumScorer's real live numbers (213 signals,
            // 10.8% hit rate against a 5%-target/3%-stop swing bracket)
            // also don't clear this project's own bar: expectancy under
            // that bracket needs >37.5% hits to break even on target/stop
            // size alone, well above what's actually been observed.
            ScanEvent::ConsolidationEvent {
                symbol,
                timestamp,
                price,
                kind: ConsolidationEventKind::EntryTriggered,
                strategy: ConsolidationStrategy::Micropullback,
            } => vec![self.try_enter(symbol, *price, *timestamp, Strategy::Micropullback)],
            ScanEvent::ConsolidationEvent {
                symbol,
                timestamp,
                price,
                kind: ConsolidationEventKind::EntryTriggered,
                strategy: ConsolidationStrategy::ConsolidationBreakout,
            } => vec![self.try_enter(symbol, *price, *timestamp, Strategy::ConsolidationBreakout)],
            ScanEvent::IgnitionEvent { symbol, timestamp, price, kind: IgnitionEventKind::FollowThroughConfirmed } => {
                vec![self.try_enter(symbol, *price, *timestamp, Strategy::IgnitionDetector)]
            }
            ScanEvent::BarUpdate { symbol, timestamp, close, interval_secs: 60, .. } => {
                self.last_price.insert(symbol.clone(), *close);
                self.on_bar(symbol, *close, *timestamp)
            }
            // Every other variant (FunnelSignal, the 30s BarUpdate
            // stream, IgnitionEvent's own CandidateOpened/
            // FollowThroughRejected kinds, SurgeDetected/
            // ConsolidationConfirmed) is real information this service
            // simply doesn't act on -- informational/precursor states,
            // not entry moments.
            _ => Vec::new(),
        }
    }

    /// Real momentum breakdown, not just price, while a position is
    /// open — exits before the trailing stop eventually catches up.
    /// Silently skipped (documented, accepted gap, same as
    /// LiveSignalTracker's own identical situation) if no price is known
    /// yet for this symbol — MomentumUpdate itself carries no price.
    fn check_momentum_deterioration(&mut self, symbol: &str, overall: f64, timestamp: DateTime<Utc>) -> Vec<JournalEntry> {
        if overall >= MOMENTUM_DETERIORATION_THRESHOLD || !self.open_positions.contains_key(symbol) {
            return Vec::new();
        }
        let Some(&price) = self.last_price.get(symbol) else {
            return Vec::new();
        };
        self.close_position(symbol, price, ExitReason::MomentumDeteriorated, timestamp).into_iter().collect()
    }

    fn try_enter(&mut self, symbol: &str, price: f64, timestamp: DateTime<Utc>, strategy: Strategy) -> JournalEntry {
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

        // Real added risk a plain momentum reading doesn't capture --
        // don't open a fresh position on something already heating
        // toward a halt band. Missing halt data fails OPEN (treated as
        // calm) -- a data gap shouldn't silently block an otherwise-good
        // entry, matching this project's own fail-open convention.
        let halt_level = self.halt_level.get(symbol).copied();
        if matches!(halt_level, Some(HaltAlertLevel::Amber) | Some(HaltAlertLevel::Red)) {
            return JournalEntry::Skipped {
                symbol: symbol.to_string(),
                reason: SkipReason::HaltRiskTooHigh,
                at: timestamp,
                detail: format!("halt level is {halt_level:?}, too risky to open a fresh position"),
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
        let position_size_usd = self.position_size_usd();
        let qty = (position_size_usd / price).floor() as u64;
        if qty == 0 {
            return JournalEntry::Skipped {
                symbol: symbol.to_string(),
                reason: SkipReason::ZeroQuantity,
                at: timestamp,
                detail: format!("price {price} exceeds the ${position_size_usd} position budget"),
            };
        }

        // The already-backtested bracket for WHICHEVER strategy actually
        // triggered this entry (v3) -- not hardcoded to Micropullback
        // anymore, see on_event's own comment on which strategies trigger
        // and OutcomeThresholds::for_strategy's doc comment for why each
        // gets the bracket it gets.
        let thresholds = OutcomeThresholds::for_strategy(strategy);
        let target_price = price * (1.0 + thresholds.target_pct / 100.0);
        let stop_price = price * (1.0 - thresholds.stop_pct / 100.0);
        let max_hold_until = timestamp + Duration::minutes(thresholds.lookforward_bars as i64);

        self.open_positions.insert(
            symbol.to_string(),
            SimulatedPosition {
                qty,
                entry_price: price,
                entered_at: timestamp,
                target_price,
                stop_price,
                highest_price_since_entry: price,
                max_hold_until,
                strategy,
            },
        );
        self.entries_today.insert(symbol.to_string(), today);

        JournalEntry::Entered {
            symbol: symbol.to_string(),
            strategy,
            entry_price: price,
            qty,
            position_size_usd,
            target_price,
            stop_price,
            entered_at: timestamp,
            momentum_overall: overall,
            momentum_volume_confirmation: volume_confirmation,
            catalyst_tags: self.catalyst_tags.get(symbol).cloned().unwrap_or_default(),
        }
    }

    /// Handles one 60s bar for an open position: trails the stop up
    /// first (so a bar that both makes a new high AND immediately
    /// reverses through the OLD stop level still gets judged against the
    /// freshly-trailed one, not a stale one), then checks the real exit
    /// conditions. Can return 0, 1 (a StopAdjusted OR an Exited), or 2
    /// entries (both, on the same bar) -- e.g. a bar that ratchets the
    /// stop up and then still closes below target/stop/timeout produces
    /// only the StopAdjusted; one that ratchets up AND then hits target
    /// on the very same close produces both.
    fn on_bar(&mut self, symbol: &str, close: f64, timestamp: DateTime<Utc>) -> Vec<JournalEntry> {
        let mut out = Vec::new();

        if let Some(position) = self.open_positions.get_mut(symbol) {
            if close > position.highest_price_since_entry {
                position.highest_price_since_entry = close;
                // The SAME bracket this position was actually opened
                // with (v3) -- trailing a Micropullback position's stop
                // by a swing-sized distance (or vice versa) would silently
                // change its risk profile mid-trade.
                let thresholds = OutcomeThresholds::for_strategy(position.strategy);
                let new_stop_price = position.highest_price_since_entry * (1.0 - thresholds.stop_pct / 100.0);
                if new_stop_price > position.stop_price {
                    let previous_stop_price = position.stop_price;
                    position.stop_price = new_stop_price;
                    out.push(JournalEntry::StopAdjusted {
                        symbol: symbol.to_string(),
                        previous_stop_price,
                        new_stop_price,
                        trigger_price: close,
                        at: timestamp,
                    });
                }
            }
        }

        let reason = {
            let Some(position) = self.open_positions.get(symbol) else {
                return out;
            };
            if close >= position.target_price {
                ExitReason::TargetHit
            } else if close <= position.stop_price {
                ExitReason::StopHit
            } else if timestamp >= position.max_hold_until {
                ExitReason::Timeout
            } else {
                return out;
            }
        };

        if let Some(exited) = self.close_position(symbol, close, reason, timestamp) {
            out.push(exited);
        }
        out
    }

    /// Shared close-out bookkeeping — pnl, running stats, and the
    /// rolling closed-trade history `position_size_usd` reads. Used by
    /// both the price/time-based exits (`on_bar`) and the momentum-
    /// deterioration exit, so there's exactly one place a trade actually
    /// gets closed, not two slightly-different copies.
    fn close_position(&mut self, symbol: &str, exit_price: f64, reason: ExitReason, timestamp: DateTime<Utc>) -> Option<JournalEntry> {
        let position = self.open_positions.remove(symbol)?;
        let pnl_usd = (exit_price - position.entry_price) * position.qty as f64;
        let pnl_pct = (exit_price - position.entry_price) / position.entry_price * 100.0;

        self.stats.trades += 1;
        if pnl_usd > 0.0 {
            self.stats.wins += 1;
        } else {
            self.stats.losses += 1;
        }
        self.stats.cumulative_pnl_usd += pnl_usd;

        self.closed_trades.push_back(TradeOutcome { pnl_usd });
        if self.closed_trades.len() > CLOSED_TRADES_HISTORY_CAP {
            self.closed_trades.pop_front();
        }

        Some(JournalEntry::Exited {
            symbol: symbol.to_string(),
            exit_price,
            exit_reason: reason,
            pnl_usd,
            pnl_pct,
            qty: position.qty,
            entered_at: position.entered_at,
            exited_at: timestamp,
        })
    }

    /// Real, bounded adaptation from this engine's own closed-trade
    /// history — not the entry gate itself (that risks a runaway
    /// feedback loop tightening into never-trading or loosening into
    /// recklessness), position size only. Stays at the configured
    /// default until there's a real, evidentially-meaningful sample
    /// (this project's own established 20-trade bar) — before that,
    /// this is honestly unchanged, not silently approximated from too
    /// little data.
    fn position_size_usd(&self) -> f64 {
        if self.closed_trades.len() < MIN_TRADES_BEFORE_ADAPTING_SIZE {
            return self.config.position_size_usd;
        }
        let recent: Vec<&TradeOutcome> = self.closed_trades.iter().rev().take(ROLLING_WINDOW).collect();
        let wins = recent.iter().filter(|t| t.pnl_usd > 0.0).count();
        let win_rate = wins as f64 / recent.len() as f64;
        let base = self.config.position_size_usd;
        if win_rate > WIN_RATE_SCALE_UP_THRESHOLD {
            (base * ADAPT_SIZE_UP_FACTOR).min(base * MAX_POSITION_SIZE_MULTIPLIER)
        } else if win_rate < WIN_RATE_SCALE_DOWN_THRESHOLD {
            (base * ADAPT_SIZE_DOWN_FACTOR).max(MIN_POSITION_SIZE_USD)
        } else {
            base
        }
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

    fn breakout_entry_triggered(symbol: &str, price: f64, ts: DateTime<Utc>) -> ScanEvent {
        ScanEvent::ConsolidationEvent {
            symbol: symbol.to_string(),
            timestamp: ts,
            price,
            kind: ConsolidationEventKind::EntryTriggered,
            strategy: ConsolidationStrategy::ConsolidationBreakout,
        }
    }

    fn ignition_follow_through(symbol: &str, price: f64, ts: DateTime<Utc>) -> ScanEvent {
        ScanEvent::IgnitionEvent { symbol: symbol.to_string(), timestamp: ts, price, kind: IgnitionEventKind::FollowThroughConfirmed }
    }

    fn bar_60s(symbol: &str, close: f64, ts: DateTime<Utc>) -> ScanEvent {
        ScanEvent::BarUpdate { symbol: symbol.to_string(), timestamp: ts, open: close, high: close, low: close, close, volume: 1000, interval_secs: 60 }
    }

    fn halt_warning(symbol: &str, level: HaltAlertLevel, ts: DateTime<Utc>) -> ScanEvent {
        ScanEvent::HaltWarning {
            symbol: symbol.to_string(),
            timestamp: ts,
            reference_price: 3.0,
            current_price: 3.0,
            band_width_dollars: 0.6,
            band_doubled: false,
            proximity_ratio: 0.5,
            relative_volume: None,
            level,
        }
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
            JournalEntry::Entered { qty, target_price, stop_price, catalyst_tags, .. } => {
                assert_eq!(*qty, 166); // floor(500 / 3.00)
                assert!((*target_price - 3.06).abs() < 1e-9); // +2%
                assert!((*stop_price - 2.94).abs() < 1e-9); // -2%
                assert!(catalyst_tags.is_empty());
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
        let exited = entries.iter().find(|e| matches!(e, JournalEntry::Exited { .. })).unwrap();
        match exited {
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
        assert!(entries.iter().any(|e| matches!(e, JournalEntry::Exited { exit_reason: ExitReason::StopHit, .. })));
        assert_eq!(engine.stats.losses, 1);
    }

    #[test]
    fn exits_on_timeout_when_neither_target_nor_stop_hit_within_the_hold_window() {
        let mut engine = Engine::new(cfg());
        engine.on_event(&momentum_update("SWVL", 0.9, 0.9, regular_session_ts()));
        engine.on_event(&entry_triggered("SWVL", 3.00, regular_session_ts()));
        // Still between stop and target, but past the 10-minute scalp window.
        let entries = engine.on_event(&bar_60s("SWVL", 3.01, regular_session_ts() + Duration::minutes(11)));
        assert!(entries.iter().any(|e| matches!(e, JournalEntry::Exited { exit_reason: ExitReason::Timeout, .. })));
    }

    #[test]
    fn stays_open_when_price_is_between_stop_and_target_and_still_within_the_hold_window() {
        let mut engine = Engine::new(cfg());
        engine.on_event(&momentum_update("SWVL", 0.9, 0.9, regular_session_ts()));
        engine.on_event(&entry_triggered("SWVL", 3.00, regular_session_ts()));
        // Below entry (2.99, not a new high) so the trailing-stop logic
        // produces nothing either -- isolates "no exit" from the
        // separate trailing-stop behavior, covered by its own test below.
        let entries = engine.on_event(&bar_60s("SWVL", 2.99, regular_session_ts() + Duration::minutes(2)));
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
        produced.extend(engine.on_event(&bar_60s("SWVL", 2.99, t0 + Duration::minutes(1)))); // below entry, not a new high
        produced.extend(engine.on_event(&bar_60s("SWVL", 3.07, t0 + Duration::minutes(3))));

        // MomentumUpdate produced nothing, the mid-flight bar stayed open
        // and wasn't a new high either (nothing produced), the final bar
        // both trails the stop (a new high) and hits target on the same
        // close -- two entries.
        assert_eq!(produced.len(), 3);
        assert!(matches!(produced[0], JournalEntry::Entered { .. }));
        assert!(matches!(produced[1], JournalEntry::StopAdjusted { .. }));
        assert!(matches!(produced[2], JournalEntry::Exited { exit_reason: ExitReason::TargetHit, .. }));
        assert!(engine.open_positions.is_empty());
    }

    #[test]
    fn trailing_stop_ratchets_up_on_a_new_high_and_never_moves_back_down() {
        let mut engine = Engine::new(cfg());
        let t0 = regular_session_ts();
        engine.on_event(&momentum_update("SWVL", 0.9, 0.9, t0));
        engine.on_event(&entry_triggered("SWVL", 3.00, t0)); // stop starts at 2.94

        // Price runs up -- stop should trail behind the new high.
        let up = engine.on_event(&bar_60s("SWVL", 3.02, t0 + Duration::minutes(1)));
        assert!(matches!(up[0], JournalEntry::StopAdjusted { new_stop_price, .. } if (new_stop_price - 2.9596).abs() < 1e-9));

        // A pullback bar (still above the new stop) must NOT move the
        // stop back down.
        let pullback = engine.on_event(&bar_60s("SWVL", 2.98, t0 + Duration::minutes(2)));
        assert!(pullback.is_empty(), "a pullback that doesn't make a new high must not touch the stop");

        // Confirm the stop is now genuinely tighter than the original
        // fixed 2.94 -- a close that would have survived the OLD stop
        // now hits the trailed one instead.
        let exit = engine.on_event(&bar_60s("SWVL", 2.95, t0 + Duration::minutes(3)));
        assert!(exit.iter().any(|e| matches!(e, JournalEntry::Exited { exit_reason: ExitReason::StopHit, .. })));
    }

    #[test]
    fn skips_entry_when_halt_risk_is_amber_or_red() {
        let mut engine = Engine::new(cfg());
        let t0 = regular_session_ts();
        engine.on_event(&momentum_update("SWVL", 0.9, 0.9, t0));
        engine.on_event(&halt_warning("SWVL", HaltAlertLevel::Red, t0));
        let entries = engine.on_event(&entry_triggered("SWVL", 3.00, t0));
        assert!(matches!(entries[0], JournalEntry::Skipped { reason: SkipReason::HaltRiskTooHigh, .. }));
    }

    #[test]
    fn enters_when_halt_level_is_calm_or_unknown() {
        let mut engine = Engine::new(cfg());
        let t0 = regular_session_ts();
        engine.on_event(&momentum_update("SWVL", 0.9, 0.9, t0));
        engine.on_event(&halt_warning("SWVL", HaltAlertLevel::Calm, t0));
        let entries = engine.on_event(&entry_triggered("SWVL", 3.00, t0));
        assert!(matches!(entries[0], JournalEntry::Entered { .. }));

        // No HaltWarning at all yet for a second symbol -- fails open.
        engine.on_event(&momentum_update("OTHR", 0.9, 0.9, t0));
        let entries2 = engine.on_event(&entry_triggered("OTHR", 3.00, t0));
        assert!(matches!(entries2[0], JournalEntry::Entered { .. }));
    }

    #[test]
    fn exits_early_on_momentum_deterioration() {
        let mut engine = Engine::new(cfg());
        let t0 = regular_session_ts();
        engine.on_event(&momentum_update("SWVL", 0.9, 0.9, t0));
        engine.on_event(&entry_triggered("SWVL", 3.00, t0));
        engine.on_event(&bar_60s("SWVL", 3.01, t0 + Duration::minutes(1))); // seeds last_price

        let entries = engine.on_event(&momentum_update("SWVL", 0.35, 0.35, t0 + Duration::minutes(2)));
        assert!(matches!(entries[0], JournalEntry::Exited { exit_reason: ExitReason::MomentumDeteriorated, .. }));
        assert!(engine.open_positions.is_empty());
    }

    #[test]
    fn momentum_deterioration_is_a_no_op_without_an_open_position() {
        let mut engine = Engine::new(cfg());
        let entries = engine.on_event(&momentum_update("SWVL", 0.1, 0.1, regular_session_ts()));
        assert!(entries.is_empty());
    }

    #[test]
    fn position_size_stays_at_default_under_twenty_closed_trades() {
        let mut engine = Engine::new(cfg());
        // 5 losing round trips -- nowhere near the 20-trade bar.
        for i in 0..5 {
            let t = regular_session_ts() + Duration::weeks(i);
            engine.on_event(&momentum_update("SWVL", 0.9, 0.9, t));
            engine.on_event(&entry_triggered("SWVL", 3.00, t));
            engine.on_event(&bar_60s("SWVL", 2.94, t + Duration::minutes(2)));
        }
        assert_eq!(engine.position_size_usd(), 500.0);
    }

    #[test]
    fn position_size_scales_down_after_a_poor_rolling_record() {
        let mut engine = Engine::new(cfg());
        // 20 losing round trips -- win rate 0%, well under the 45% floor.
        for i in 0..20 {
            let t = regular_session_ts() + Duration::weeks(i);
            engine.on_event(&momentum_update("SWVL", 0.9, 0.9, t));
            engine.on_event(&entry_triggered("SWVL", 3.00, t));
            engine.on_event(&bar_60s("SWVL", 2.94, t + Duration::minutes(2)));
        }
        assert_eq!(engine.position_size_usd(), 400.0); // 500 * 0.8
    }

    #[test]
    fn enters_on_ignition_follow_through_confirmed_not_just_micropullback() {
        // v3: IgnitionDetector is a real, separately-wired trigger now,
        // not folded silently into the momentum gate -- and it shares
        // Micropullback's exact scalp bracket (OutcomeThresholds::
        // for_strategy), so a +/-2% bracket here is the same evidence-
        // backed profile this project already trusts for the fast-
        // microstructure signals.
        let mut engine = Engine::new(cfg());
        engine.on_event(&momentum_update("SWVL", 0.72, 0.68, regular_session_ts()));
        let entries = engine.on_event(&ignition_follow_through("SWVL", 3.00, regular_session_ts()));
        match &entries[0] {
            JournalEntry::Entered { strategy, target_price, stop_price, .. } => {
                assert_eq!(*strategy, Strategy::IgnitionDetector);
                assert!((*target_price - 3.06).abs() < 1e-9); // +2%, the scalp bracket
                assert!((*stop_price - 2.94).abs() < 1e-9); // -2%
            }
            other => panic!("expected Entered, got {other:?}"),
        }
    }

    #[test]
    fn enters_on_consolidation_breakout_using_its_own_swing_bracket() {
        // ConsolidationBreakout (the slower sibling of Micropullback)
        // gets the swing default (5%/3%), a real, different bracket from
        // the scalp one above -- proves try_enter actually looks up the
        // triggering strategy's own thresholds, not a hardcoded value.
        let mut engine = Engine::new(cfg());
        engine.on_event(&momentum_update("SWVL", 0.72, 0.68, regular_session_ts()));
        let entries = engine.on_event(&breakout_entry_triggered("SWVL", 3.00, regular_session_ts()));
        match &entries[0] {
            JournalEntry::Entered { strategy, target_price, stop_price, .. } => {
                assert_eq!(*strategy, Strategy::ConsolidationBreakout);
                assert!((*target_price - 3.15).abs() < 1e-9); // +5%
                assert!((*stop_price - 2.91).abs() < 1e-9); // -3%
            }
            other => panic!("expected Entered, got {other:?}"),
        }
    }

    #[test]
    fn trailing_stop_on_a_consolidation_breakout_position_uses_its_own_swing_distance() {
        // Real regression target for the on_bar fix -- before v3, the
        // trailing-stop recompute was hardcoded to Micropullback's 2%
        // distance regardless of which strategy actually opened the
        // position. A ConsolidationBreakout position's stop must trail
        // by its own 3%, not 2%.
        let mut engine = Engine::new(cfg());
        let t0 = regular_session_ts();
        engine.on_event(&momentum_update("SWVL", 0.9, 0.9, t0));
        engine.on_event(&breakout_entry_triggered("SWVL", 3.00, t0)); // stop starts at 2.91 (-3%)

        let up = engine.on_event(&bar_60s("SWVL", 3.30, t0 + Duration::minutes(1)));
        let adjusted = up.iter().find(|e| matches!(e, JournalEntry::StopAdjusted { .. })).unwrap();
        match adjusted {
            JournalEntry::StopAdjusted { new_stop_price, .. } => {
                assert!((*new_stop_price - 3.201).abs() < 1e-9); // 3.30 * (1 - 3%), not 2%
            }
            other => panic!("expected StopAdjusted, got {other:?}"),
        }
    }

    #[test]
    fn position_size_scales_up_after_a_strong_rolling_record() {
        let mut engine = Engine::new(cfg());
        // 20 winning round trips -- win rate 100%, well over the 55% bar.
        for i in 0..20 {
            let t = regular_session_ts() + Duration::weeks(i);
            engine.on_event(&momentum_update("SWVL", 0.9, 0.9, t));
            engine.on_event(&entry_triggered("SWVL", 3.00, t));
            engine.on_event(&bar_60s("SWVL", 3.06, t + Duration::minutes(2)));
        }
        assert_eq!(engine.position_size_usd(), 550.0); // 500 * 1.1
    }
}
