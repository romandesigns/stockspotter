//! `detect()`/`confirm()` in `detect.rs`/`follow_through.rs` are pure —
//! deliberately so, same reasoning as the rest of this codebase. But
//! *something* has to own the rolling trade/quote history per ticker and
//! track "a candidate fired, now collecting price action to confirm it"
//! across ticks. That's what `IgnitionMonitor` is: the stateful wrapper a
//! live scan loop (or the replay engine later) actually talks to, one
//! instance per watched symbol.
//!
//! This is also where halt-lift resumption — the doc's fourth ignition
//! signal, and the only one that can't be computed fresh from a single
//! trade/quote window — actually lives. It's a *transition*: halted, then
//! not halted, tracked via `on_status()`. The resumption itself carries no
//! price (status updates don't include one), so opening a candidate has
//! to wait for the first trade that prints after the resume; `on_trade()`
//! handles that hand-off.

use std::collections::VecDeque;

use crate::detect::{detect, IgnitionSignals, IgnitionThresholds};
use crate::follow_through::{confirm, FollowThroughResult, FollowThroughThresholds};
use crate::tick::{Quote, Trade};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonitorConfig {
    /// How much trade/quote history to keep around. Bounded so memory
    /// doesn't grow unboundedly over a long-running session.
    pub max_trades: usize,
    pub max_quotes: usize,
    pub recent_window_secs: f64,
    pub baseline_window_secs: f64,
    pub spread_recent_n: usize,
    pub spread_baseline_n: usize,
    /// How many trades to collect after a candidate fires before running
    /// follow-through confirmation — the doc's "hundreds of ms to ~1s"
    /// delay, expressed as a trade count rather than wall-clock time
    /// since that's what's actually available tick-by-tick.
    pub confirmation_trade_count: usize,
    pub thresholds: IgnitionThresholds,
    pub follow_through: FollowThroughThresholds,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            max_trades: 500,
            max_quotes: 500,
            recent_window_secs: 1.0,
            baseline_window_secs: 20.0,
            spread_recent_n: 5,
            spread_baseline_n: 20,
            confirmation_trade_count: 10,
            thresholds: IgnitionThresholds::default(),
            follow_through: FollowThroughThresholds::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct PendingCandidate {
    breakout_level: f64,
    prices_after: Vec<f64>,
}

/// What happened as a result of feeding in one trade.
#[derive(Debug, Clone, PartialEq)]
pub enum MonitorEvent {
    /// Nothing notable — either no signal fired, or a candidate is still
    /// mid-confirmation and hasn't collected enough price action yet.
    None,
    /// A raw signal just crossed its threshold; now collecting
    /// `confirmation_trade_count` more trades before deciding.
    CandidateOpened(IgnitionSignals),
    /// Follow-through confirmation just finished for a prior candidate —
    /// `result.confirmed` is the real answer, not the raw signal alone.
    FollowThroughResolved(FollowThroughResult),
}

/// Owns the rolling trade/quote history for one ticker plus any
/// in-progress candidate. While a candidate is pending, new signal
/// detection is paused (only price-collection-for-confirmation runs) —
/// one candidate resolves before another can open, deliberately simple
/// rather than tracking overlapping candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusTransition {
    /// No change worth reporting (including the very first status ever
    /// seen for this symbol, which has nothing to transition from).
    Unchanged,
    Halted,
    /// A halt was just lifted — the next trade to print will open an
    /// ignition candidate, see `on_trade`.
    Resumed,
}

#[derive(Debug, Clone)]
pub struct IgnitionMonitor {
    config: MonitorConfig,
    trades: VecDeque<Trade>,
    quotes: VecDeque<Quote>,
    pending: Option<PendingCandidate>,
    last_status_halted: Option<bool>,
    resume_awaiting_first_trade: bool,
}

impl IgnitionMonitor {
    pub fn new(config: MonitorConfig) -> Self {
        Self {
            config,
            trades: VecDeque::new(),
            quotes: VecDeque::new(),
            pending: None,
            last_status_halted: None,
            resume_awaiting_first_trade: false,
        }
    }

    /// Quotes only ever feed the rolling window — they don't themselves
    /// trigger detection or advance a pending candidate (trades do both).
    pub fn on_quote(&mut self, quote: Quote) {
        self.quotes.push_back(quote);
        while self.quotes.len() > self.config.max_quotes {
            self.quotes.pop_front();
        }
    }

    /// Feeds in a trading-status update (Alpaca's `sc` field, e.g. "H").
    /// Only the halted -> not-halted transition matters here; everything
    /// else (first-ever status, halted -> halted, resumed -> resumed) is
    /// `Unchanged`.
    pub fn on_status(&mut self, status_code: &str) -> StatusTransition {
        let now_halted = is_halted(status_code);
        let transition = match self.last_status_halted {
            Some(true) if !now_halted => StatusTransition::Resumed,
            Some(false) if now_halted => StatusTransition::Halted,
            None if now_halted => StatusTransition::Halted,
            _ => StatusTransition::Unchanged,
        };
        if transition == StatusTransition::Resumed {
            self.resume_awaiting_first_trade = true;
        }
        self.last_status_halted = Some(now_halted);
        transition
    }

    pub fn on_trade(&mut self, trade: Trade) -> MonitorEvent {
        let price = trade.price;
        self.trades.push_back(trade);
        while self.trades.len() > self.config.max_trades {
            self.trades.pop_front();
        }

        if let Some(pending) = self.pending.as_mut() {
            pending.prices_after.push(price);
            if pending.prices_after.len() >= self.config.confirmation_trade_count {
                let pending = self
                    .pending
                    .take()
                    .expect("just matched Some on self.pending above");
                let result = confirm(
                    pending.breakout_level,
                    &pending.prices_after,
                    &self.config.follow_through,
                );
                return MonitorEvent::FollowThroughResolved(result);
            }
            return MonitorEvent::None;
        }

        // A halt-lift resumption was flagged by on_status() and no other
        // candidate is currently pending (checked above) — this trade is
        // the first post-halt print, so it becomes the breakout level.
        if self.resume_awaiting_first_trade {
            self.resume_awaiting_first_trade = false;
            self.pending = Some(PendingCandidate {
                breakout_level: price,
                prices_after: Vec::new(),
            });
            return MonitorEvent::CandidateOpened(IgnitionSignals {
                trade_frequency_ratio: None,
                spread_ratio: None,
                ask_absorbed: None,
                trade_frequency_spiked: false,
                spread_tightened: false,
                halt_lift: true,
                triggered: true,
            });
        }

        let trades_slice: &[Trade] = self.trades.make_contiguous();
        let quotes_slice: &[Quote] = self.quotes.make_contiguous();
        let cfg = self.config;
        let signals = detect(
            trades_slice,
            quotes_slice,
            cfg.recent_window_secs,
            cfg.baseline_window_secs,
            cfg.spread_recent_n,
            cfg.spread_baseline_n,
            &cfg.thresholds,
        );

        if signals.triggered {
            self.pending = Some(PendingCandidate {
                breakout_level: price,
                prices_after: Vec::new(),
            });
            return MonitorEvent::CandidateOpened(signals);
        }

        MonitorEvent::None
    }
}

/// Alpaca's trading-status codes follow the UTP/CTA convention; "H"
/// (Halted) is the one confirmed via Alpaca's own docs/examples. The full
/// code set isn't enumerated anywhere we could confirm — extend this if
/// real halt data surfaces other codes that should also count.
fn is_halted(status_code: &str) -> bool {
    status_code == "H"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(t: f64, price: f64) -> Trade {
        Trade {
            timestamp_secs: t,
            price,
            size: 100,
        }
    }

    #[test]
    fn insufficient_history_never_triggers() {
        let mut monitor = IgnitionMonitor::new(MonitorConfig::default());
        let event = monitor.on_trade(trade(0.0, 5.0));
        assert_eq!(event, MonitorEvent::None);
    }

    #[test]
    fn full_lifecycle_candidate_opens_then_confirms() {
        let config = MonitorConfig {
            confirmation_trade_count: 3,
            ..MonitorConfig::default()
        };
        let mut monitor = IgnitionMonitor::new(config);

        // Sparse baseline reaching back well past recent+baseline window
        // (1.0 + 20.0 = 21.0s), same construction as detect.rs's own test.
        let mut t = -30.0;
        while t < -3.0 {
            let event = monitor.on_trade(trade(t, 5.00));
            assert_eq!(event, MonitorEvent::None, "baseline trades shouldn't trigger");
            t += 3.0;
        }

        // Burst: min_recent_trades_for_spike (3, the default) means the
        // first two rapid trades just accumulate — only the 3rd, with
        // 3 trades now inside the 1s recent window, actually opens a
        // candidate at breakout_level = its own price.
        assert_eq!(monitor.on_trade(trade(0.0, 5.00)), MonitorEvent::None);
        assert_eq!(monitor.on_trade(trade(0.05, 5.01)), MonitorEvent::None);
        let opened = monitor.on_trade(trade(0.1, 5.02));
        match opened {
            MonitorEvent::CandidateOpened(signals) => assert!(signals.trade_frequency_spiked),
            other => panic!("expected CandidateOpened, got {other:?}"),
        }

        // Next 2 trades (confirmation_trade_count=3) just accumulate...
        assert_eq!(monitor.on_trade(trade(0.15, 5.04)), MonitorEvent::None);
        assert_eq!(monitor.on_trade(trade(0.2, 5.06)), MonitorEvent::None);
        // ...and the 3rd resolves follow-through. Price only went up, so
        // this should confirm.
        match monitor.on_trade(trade(0.25, 5.08)) {
            MonitorEvent::FollowThroughResolved(result) => assert!(result.confirmed),
            other => panic!("expected FollowThroughResolved, got {other:?}"),
        }
    }

    #[test]
    fn candidate_rejected_when_price_air_pockets_after_opening() {
        let config = MonitorConfig {
            confirmation_trade_count: 3,
            ..MonitorConfig::default()
        };
        let mut monitor = IgnitionMonitor::new(config);

        let mut t = -30.0;
        while t < -3.0 {
            monitor.on_trade(trade(t, 5.00));
            t += 3.0;
        }
        monitor.on_trade(trade(0.0, 5.00));
        monitor.on_trade(trade(0.05, 5.01));
        let opened = monitor.on_trade(trade(0.1, 5.02));
        assert!(matches!(opened, MonitorEvent::CandidateOpened(_)));

        monitor.on_trade(trade(0.15, 4.98));
        monitor.on_trade(trade(0.2, 4.95));
        match monitor.on_trade(trade(0.25, 4.90)) {
            MonitorEvent::FollowThroughResolved(result) => assert!(!result.confirmed),
            other => panic!("expected FollowThroughResolved, got {other:?}"),
        }
    }

    #[test]
    fn status_transition_only_fires_on_halted_to_resumed() {
        let mut monitor = IgnitionMonitor::new(MonitorConfig::default());
        assert_eq!(monitor.on_status("T"), StatusTransition::Unchanged); // normal trading, first-ever status
        assert_eq!(monitor.on_status("H"), StatusTransition::Halted);
        assert_eq!(monitor.on_status("H"), StatusTransition::Unchanged); // still halted
        assert_eq!(monitor.on_status("T"), StatusTransition::Resumed);
        assert_eq!(monitor.on_status("T"), StatusTransition::Unchanged); // still trading
    }

    #[test]
    fn halt_lift_opens_a_candidate_on_the_first_post_halt_trade() {
        let config = MonitorConfig {
            confirmation_trade_count: 2,
            ..MonitorConfig::default()
        };
        let mut monitor = IgnitionMonitor::new(config);

        monitor.on_status("H");
        assert_eq!(monitor.on_status("T"), StatusTransition::Resumed);

        // Status updates carry no price — the resumption itself opens
        // nothing yet. The next trade is what actually opens a candidate,
        // using its own price as the breakout level.
        let opened = monitor.on_trade(trade(0.0, 5.00));
        match opened {
            MonitorEvent::CandidateOpened(signals) => {
                assert!(signals.halt_lift);
                assert!(signals.triggered);
            }
            other => panic!("expected CandidateOpened, got {other:?}"),
        }

        monitor.on_trade(trade(0.1, 5.05));
        match monitor.on_trade(trade(0.2, 5.10)) {
            MonitorEvent::FollowThroughResolved(result) => assert!(result.confirmed),
            other => panic!("expected FollowThroughResolved, got {other:?}"),
        }
    }

    #[test]
    fn halt_lift_does_not_open_a_second_candidate_while_one_is_pending() {
        let config = MonitorConfig {
            confirmation_trade_count: 5,
            ..MonitorConfig::default()
        };
        let mut monitor = IgnitionMonitor::new(config);

        let mut t = -30.0;
        while t < -3.0 {
            monitor.on_trade(trade(t, 5.00));
            t += 3.0;
        }
        monitor.on_trade(trade(0.0, 5.00));
        monitor.on_trade(trade(0.05, 5.01));
        let opened = monitor.on_trade(trade(0.1, 5.02));
        assert!(matches!(opened, MonitorEvent::CandidateOpened(_)));

        // A halt-lift flagged while a candidate is already pending should
        // just wait its turn, not interrupt the in-progress one.
        monitor.on_status("H");
        monitor.on_status("T");
        let event = monitor.on_trade(trade(0.15, 5.03));
        assert_eq!(event, MonitorEvent::None);
    }

    #[test]
    fn no_new_candidate_opens_while_one_is_pending() {
        let config = MonitorConfig {
            confirmation_trade_count: 5,
            ..MonitorConfig::default()
        };
        let mut monitor = IgnitionMonitor::new(config);

        let mut t = -30.0;
        while t < -3.0 {
            monitor.on_trade(trade(t, 5.00));
            t += 3.0;
        }
        monitor.on_trade(trade(0.0, 5.00));
        monitor.on_trade(trade(0.05, 5.01));
        let opened = monitor.on_trade(trade(0.1, 5.02));
        assert!(matches!(opened, MonitorEvent::CandidateOpened(_)));

        // Even though this next trade would itself look like a huge
        // spike in isolation, a candidate is already pending — it should
        // just accumulate toward that one's confirmation, not open a
        // second candidate.
        let event = monitor.on_trade(trade(0.11, 5.20));
        assert_eq!(event, MonitorEvent::None);
    }
}
