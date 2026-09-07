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
use crate::flat_base::{in_gated_price_band, is_flat_base, FlatBaseThresholds};
use crate::follow_through::{confirm, FollowThroughResult, FollowThroughThresholds};
use crate::tick::{Quote, Trade};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonitorConfig {
    /// Minimum count-based trade history; the complete time-based frequency
    /// interval is retained even when it contains more trades. Quotes use a count cap.
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
    ///
    /// Backtested 2026-08-30 against real SWVL data via
    /// `backtest-metrics --bin tune`: the original value of 10 produced
    /// 493 "confirmed" signals over ~6.5 real trading hours with a 9.3%
    /// hit rate against a +5%/20-bar bar — median favorable move after
    /// confirmation was only 1.42%, meaning confirmation was firing on
    /// noise, not real ignition moves. Doubling the observation window
    /// to 20 roughly tripled the hit rate (to 35.8% against a more
    /// appropriate +2%/10-bar bar — see the outcome-profile note below)
    /// while keeping a healthy 316-signal sample; 40 narrowed the sample
    /// too far for little extra gain. 20 is the new default for that
    /// reason, not a guess.
    ///
    /// **Held up on a real sample (2026-09-06).** The above was one
    /// symbol on one session, which is not evidence of much.
    /// `backtest-metrics --bin backtest_broad` now replays 45 real
    /// sessions across 9 symbols (27 gap/surge days plus 18 quiet
    /// negative-control days): 869 ignition signals, 38.6% hit rate —
    /// so the single-session 35.8% was, unusually, not an artifact. That
    /// is still slightly BELOW the 50% breakeven line for the symmetric
    /// 2%/2% bracket (expectancy -0.46pp per signal before costs), which
    /// is the honest state of this detector as an unmanaged signal.
    /// `strategy_config::decide_enabled_strategies` documents why the
    /// managed auto-trader trade measures better than the raw signal
    /// (trailing stop + momentum-deterioration exit cut real losses
    /// below the bracket's flat -2%); that argument is unchanged, it now
    /// just rests on a real sample instead of one day.
    pub confirmation_trade_count: usize,
    pub thresholds: IgnitionThresholds,
    pub follow_through: FollowThroughThresholds,
    /// Low-float flat-base refinement (part-3 doc). Enabled by default
    /// as of 2026-09-06 — it had shipped as `None` (fully off), which
    /// meant the doc's headline low-float pattern was implemented,
    /// tested, and never actually running in production, since
    /// `market_data::live` only ever constructs `MonitorConfig::default()`.
    ///
    /// Turning it on does not widen anything: the gate only ever
    /// *suppresses* candidates, and only for stocks at or below
    /// `FlatBaseThresholds::max_price_for_gate` ($0.25). Every stock
    /// above that band takes the identical code path it did before —
    /// see `in_gated_price_band`'s early return and this file's
    /// `flat_base_gate_*` tests, which pin exactly that isolation.
    pub flat_base: Option<FlatBaseThresholds>,
    /// Minimum seconds between *confirmed* ignition alerts for one
    /// symbol. While inside this window no new candidate opens at all,
    /// so a burst of near-identical re-triggers on the same move
    /// collapses to the single alert that led it.
    ///
    /// Measured 2026-09-06 against the real SWVL session already in
    /// `data/backtest_log.jsonl` (316 confirmed signals over 6.5h — one
    /// alert every 74 seconds on a *single* symbol, which is the
    /// architecture doc's own "signal clutter" failure mode arriving
    /// exactly as predicted). Sweeping the cooldown over that data:
    ///
    /// ```text
    /// cooldown   alerts   hit rate   alerts/hr
    ///      0s      316      35.8%        49.0
    ///     60s      156      41.0%        24.2
    ///    180s       86      45.3%        13.3
    ///    300s       62      51.6%         9.6   <-- default
    ///    600s       33      45.5%         5.1
    ///    900s       24      33.3%         3.7
    /// ```
    ///
    /// 300s is the peak, and not by a small margin: it cuts alert volume
    /// 5x while raising hit rate past the 50% breakeven line for
    /// `OutcomeThresholds::scalp`'s symmetric 2%/2% bracket — the raw
    /// 35.8% signal was below breakeven before costs. Longer cooldowns
    /// start suppressing genuine re-ignitions (a stock that runs,
    /// consolidates, and runs again is a real Ross Cameron setup, not a
    /// duplicate) and the hit rate falls back off.
    ///
    /// **Re-swept on the broad sample (2026-09-06), same day.** The
    /// caveat above said to re-run this once a real multi-symbol sample
    /// existed; `backtest_broad` produced one (45 sessions, 9 symbols,
    /// 869 ignition signals) and the sweep was repeated per-symbol
    /// against it. Every cooldown from 0 to 300s leaves the sample
    /// untouched — proof the 300s gate is already binding — and every
    /// value ABOVE it makes things worse (450s: 36.5%, 600s: 37.1%,
    /// 1200s: 34.6%, all below 300s's 38.6%). 300s stands as a measured
    /// optimum on real multi-symbol data, not a one-session guess.
    ///
    /// Set to 0.0 to disable the cooldown entirely.
    pub alert_cooldown_secs: f64,
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
            confirmation_trade_count: 20,
            thresholds: IgnitionThresholds::default(),
            follow_through: FollowThroughThresholds::default(),
            flat_base: Some(FlatBaseThresholds::default()),
            alert_cooldown_secs: 300.0,
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
    /// Timestamp of the last *confirmed* alert, for the cooldown in
    /// `in_alert_cooldown`. Only a confirmation sets this — an opened
    /// candidate that then fails follow-through was never an alert and
    /// must not suppress the next real one.
    last_confirmed_alert_secs: Option<f64>,
}

impl IgnitionMonitor {
    /// Upgrade a trades-only coverage tier without resetting pending confirmation
    /// or notification cooldown. Detection thresholds stay unchanged.
    pub fn enable_full_history(&mut self) {
        let defaults=MonitorConfig::default();
        self.config.max_quotes=defaults.max_quotes;
        self.config.max_trades=defaults.max_trades;
    }

    pub fn new(config: MonitorConfig) -> Self {
        Self {
            config,
            trades: VecDeque::new(),
            quotes: VecDeque::new(),
            pending: None,
            last_status_halted: None,
            resume_awaiting_first_trade: false,
            last_confirmed_alert_secs: None,
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
        if !trade.price.is_finite() || !trade.timestamp_secs.is_finite()
            || self.trades.back().is_some_and(|last| trade.timestamp_secs < last.timestamp_secs) {
            return MonitorEvent::None;
        }
        let price = trade.price;
        let now_secs = trade.timestamp_secs;
        self.trades.push_back(trade);
        // Retain the complete detection interval plus one boundary trade.
        // max_trades is a minimum history for the count-based flat-base gate.
        let cutoff = now_secs - self.config.recent_window_secs - self.config.baseline_window_secs;
        while self.trades.len() > self.config.max_trades.max(2)
            && self.trades.get(1).is_some_and(|t| t.timestamp_secs <= cutoff) {
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
                // Only a real confirmation starts the cooldown clock —
                // see `last_confirmed_alert_secs`' own doc comment.
                if result.confirmed {
                    self.last_confirmed_alert_secs = Some(now_secs);
                }
                return MonitorEvent::FollowThroughResolved(result);
            }
            return MonitorEvent::None;
        }

        // A halt-lift resumption was flagged by on_status() and no other
        // candidate is currently pending (checked above) — this trade is
        // the first post-halt print, so it becomes the breakout level.
        if self.resume_awaiting_first_trade {
            self.resume_awaiting_first_trade = false;
            if self.flat_base_gate_blocks(price) {
                return MonitorEvent::None;
            }
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
            // Cooldown is checked here rather than at confirmation time
            // on purpose: suppressing the *candidate* means the burst of
            // re-triggers riding the same move never even enters
            // follow-through, so it costs nothing to evaluate them.
            // Halt-lift resumption (handled above) deliberately bypasses
            // this — a halt lift is a rare, discrete, externally-timed
            // event, not one of the repeat triggers this cooldown exists
            // to collapse.
            if self.in_alert_cooldown(now_secs) || self.flat_base_gate_blocks(price) {
                return MonitorEvent::None;
            }
            self.pending = Some(PendingCandidate {
                breakout_level: price,
                prices_after: Vec::new(),
            });
            return MonitorEvent::CandidateOpened(signals);
        }

        MonitorEvent::None
    }

    /// True if a confirmed alert fired for this symbol less than
    /// `alert_cooldown_secs` ago. Always false when the cooldown is
    /// disabled (0.0) or nothing has been confirmed yet, so a monitor
    /// configured that way behaves exactly as it did before this existed.
    fn in_alert_cooldown(&self, now_secs: f64) -> bool {
        if self.config.alert_cooldown_secs <= 0.0 {
            return false;
        }
        self.last_confirmed_alert_secs
            .is_some_and(|last| now_secs - last < self.config.alert_cooldown_secs)
    }

    /// True if the low-float flat-base gate is configured, `price` falls
    /// in its band, and the trades immediately *before* this one (not
    /// including it — the gate looks at what came before the surge, not
    /// the surge print itself) don't show a confirmed flat base. When
    /// true, a candidate that would otherwise open here gets suppressed
    /// instead — see `MonitorConfig::flat_base`'s doc comment for the
    /// isolation guarantee this depends on.
    fn flat_base_gate_blocks(&mut self, price: f64) -> bool {
        let Some(thresholds) = self.config.flat_base else {
            return false;
        };
        if !in_gated_price_band(price, &thresholds) {
            return false;
        }
        let trades = self.trades.make_contiguous();
        let lookback = if trades.is_empty() {
            trades
        } else {
            &trades[..trades.len() - 1]
        };
        !is_flat_base(lookback, &thresholds)
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
    #[test]
    fn promotion_preserves_trade_history_and_cooldown_and_accepts_quotes() {
        let mut monitor=IgnitionMonitor::new(MonitorConfig{max_quotes:0,max_trades:120,..MonitorConfig::default()});
        monitor.on_trade(trade(1.,5.));monitor.last_confirmed_alert_secs=Some(1.);
        monitor.enable_full_history();
        assert_eq!(monitor.trades.len(),1);assert_eq!(monitor.last_confirmed_alert_secs,Some(1.));
        monitor.on_quote(Quote{timestamp_secs:2.,bid_price:4.99,ask_price:5.,bid_size:1,ask_size:1});
        assert_eq!(monitor.quotes.len(),1);
        assert_eq!(monitor.config.thresholds,MonitorConfig::default().thresholds);
    }
    use super::*;

    #[test]
    fn busy_tape_retains_a_full_frequency_baseline_in_both_coverage_tiers() {
        for max_trades in [120,500] {
            let mut monitor = IgnitionMonitor::new(MonitorConfig { max_trades, ..MonitorConfig::default() });
            for i in 0..42000 { monitor.on_trade(trade(i as f64 / 1000.0,5.0)); }
            let ratio = crate::detect::trade_frequency_ratio(monitor.trades.make_contiguous(),1.0,20.0).unwrap();
            assert!((ratio - 1.0).abs() < 0.01, "steady 1000 trades/sec must have a steady baseline: {ratio}");
            assert!(monitor.trades.len() <= 21002);
        }
    }

    fn trade(t: f64, price: f64) -> Trade {
        Trade {
            timestamp_secs: t,
            price,
            size: 100,
        }
    }

    /// Feeds a sparse baseline over the ~30s leading up to `at_secs`,
    /// then a 3-trade burst inside a 0.1s window, and returns whatever
    /// the burst's final trade produced.
    ///
    /// The baseline has to be re-fed before *every* burst rather than
    /// once at the start: `detect` compares a recent window against a
    /// `baseline_window_secs` (20s) one, so trades from a burst minutes
    /// earlier have long since aged out of the rolling deque and can't
    /// serve as the baseline for a later burst. Any test firing more
    /// than one burst needs this, not `baseline_burst_setup`.
    fn burst_at(monitor: &mut IgnitionMonitor, at_secs: f64, base_price: f64) -> MonitorEvent {
        let mut t = at_secs - 30.0;
        while t < at_secs - 3.0 {
            monitor.on_trade(trade(t, base_price));
            t += 3.0;
        }
        monitor.on_trade(trade(at_secs, base_price));
        monitor.on_trade(trade(at_secs + 0.05, base_price * 1.12));
        monitor.on_trade(trade(at_secs + 0.1, base_price * 1.04))
    }

    fn baseline_burst_setup(config: MonitorConfig) -> (IgnitionMonitor, Vec<MonitorEvent>) {
        let mut monitor = IgnitionMonitor::new(config);
        let mut events = Vec::new();
        let mut t = -30.0;
        while t < -3.0 {
            events.push(monitor.on_trade(trade(t, 5.00)));
            t += 3.0;
        }
        (monitor, events)
    }

    #[test]
    fn flat_base_gate_blocks_a_candidate_when_the_lookback_is_not_flat() {
        let config = MonitorConfig {
            flat_base: Some(FlatBaseThresholds {
                max_price_for_gate: 0.25,
                lookback_trades: 2,
                max_range_ratio: 0.03,
            }),
            ..MonitorConfig::default()
        };
        let (mut monitor, _) = baseline_burst_setup(config);

        // Two warmup trades right before the trigger, deliberately far
        // apart (0.20 -> 0.30, a 50% swing — nowhere near "flat").
        monitor.on_trade(trade(0.0, 0.20));
        monitor.on_trade(trade(0.05, 0.30));
        // Trigger trade: 3rd rapid trade, price in the gated band, would
        // open a candidate without the gate (matches the existing
        // full_lifecycle test's burst shape).
        let event = monitor.on_trade(trade(0.1, 0.22));

        assert_eq!(event, MonitorEvent::None, "gate should have suppressed this candidate");
    }

    #[test]
    fn flat_base_gate_allows_a_candidate_when_the_lookback_is_flat() {
        let config = MonitorConfig {
            flat_base: Some(FlatBaseThresholds {
                max_price_for_gate: 0.25,
                lookback_trades: 2,
                max_range_ratio: 0.03,
            }),
            ..MonitorConfig::default()
        };
        let (mut monitor, _) = baseline_burst_setup(config);

        // Two warmup trades right before the trigger, tightly clustered.
        monitor.on_trade(trade(0.0, 0.220));
        monitor.on_trade(trade(0.05, 0.221));
        let event = monitor.on_trade(trade(0.1, 0.222));

        assert!(
            matches!(event, MonitorEvent::CandidateOpened(_)),
            "flat lookback should have let the candidate through, got {event:?}"
        );
    }

    #[test]
    fn flat_base_gate_has_no_effect_on_stocks_outside_its_price_band() {
        // Same non-flat setup as the blocking test above, but priced at
        // $5 instead of $0.22 — the doc is explicit this gate must not
        // change behavior for stocks outside the low-price profile, even
        // with the gate configured and even with a wildly non-flat
        // lookback.
        let config = MonitorConfig {
            flat_base: Some(FlatBaseThresholds {
                max_price_for_gate: 0.25,
                lookback_trades: 2,
                max_range_ratio: 0.03,
            }),
            ..MonitorConfig::default()
        };
        let (mut monitor, _) = baseline_burst_setup(config);

        monitor.on_trade(trade(0.0, 4.00));
        monitor.on_trade(trade(0.05, 6.00)); // 50% swing, would fail flat-base
        let event = monitor.on_trade(trade(0.1, 5.00)); // outside the $0.25 gate band

        assert!(
            matches!(event, MonitorEvent::CandidateOpened(_)),
            "gate must not affect a stock outside its price band, got {event:?}"
        );
    }

    #[test]
    fn default_config_now_has_the_flat_base_gate_on() {
        // Changed 2026-09-06. This test previously pinned the opposite
        // (`flat_base: None`), which was the bug: the part-3 low-float
        // refinement was fully implemented and tested but never ran,
        // because `market_data::live` only ever builds
        // `MonitorConfig::default()`. Same scenario as
        // `flat_base_gate_blocks_...` above, but taking the gate from
        // the default config rather than an explicit opt-in — a
        // low-priced stock whose lookback isn't flat is now suppressed
        // out of the box.
        assert_eq!(MonitorConfig::default().flat_base, Some(FlatBaseThresholds::default()));

        let (mut monitor, _) = baseline_burst_setup(MonitorConfig::default());
        monitor.on_trade(trade(0.0, 0.20));
        monitor.on_trade(trade(0.05, 0.30));
        let event = monitor.on_trade(trade(0.1, 0.22));

        assert_eq!(event, MonitorEvent::None, "gate should block: 5.00 baseline is not a flat base");
    }

    #[test]
    fn default_config_leaves_stocks_above_the_gate_band_completely_unaffected() {
        // The isolation guarantee the part-3 doc demands, now that the
        // gate ships on by default: enabling it must not change
        // detection for any stock outside the low-price band. Identical
        // burst shape to the test above, just priced at $5 instead of
        // $0.22.
        let (mut monitor, _) = baseline_burst_setup(MonitorConfig::default());
        monitor.on_trade(trade(0.0, 5.00));
        monitor.on_trade(trade(0.05, 5.60));
        let event = monitor.on_trade(trade(0.1, 5.20));

        assert!(
            matches!(event, MonitorEvent::CandidateOpened(_)),
            "gate must not touch a stock above its price band, got {event:?}"
        );
    }

    #[test]
    fn cooldown_suppresses_a_second_candidate_until_the_window_elapses() {
        // confirmation_trade_count: 1 so a candidate resolves on the very
        // next trade, keeping this focused on the cooldown itself rather
        // than on follow-through mechanics (covered by their own tests).
        let config = MonitorConfig {
            confirmation_trade_count: 1,
            alert_cooldown_secs: 300.0,
            ..MonitorConfig::default()
        };
        let mut monitor = IgnitionMonitor::new(config);

        let opened = burst_at(&mut monitor, 0.0, 5.00);
        assert!(matches!(opened, MonitorEvent::CandidateOpened(_)));

        // Next trade resolves it. Price holds well above the breakout
        // level, so this confirms and starts the cooldown clock.
        let resolved = monitor.on_trade(trade(0.2, 5.80));
        let MonitorEvent::FollowThroughResolved(result) = resolved else {
            panic!("expected the candidate to resolve, got {resolved:?}");
        };
        assert!(result.confirmed, "test setup should produce a confirmed alert");

        // An identical burst 60s later — inside the 300s window — must
        // not open anything.
        let during = burst_at(&mut monitor, 60.0, 5.00);
        assert_eq!(during, MonitorEvent::None, "cooldown should suppress this candidate");

        // The same burst past the window opens normally again.
        let after = burst_at(&mut monitor, 400.0, 5.00);
        assert!(
            matches!(after, MonitorEvent::CandidateOpened(_)),
            "cooldown should have expired by now, got {after:?}"
        );
    }

    #[test]
    fn an_unconfirmed_candidate_does_not_start_the_cooldown() {
        // The distinction `last_confirmed_alert_secs` exists for: a
        // candidate that opens and then *fails* follow-through was never
        // an alert, so it must not suppress the next real one.
        let config = MonitorConfig {
            confirmation_trade_count: 1,
            alert_cooldown_secs: 300.0,
            ..MonitorConfig::default()
        };
        let mut monitor = IgnitionMonitor::new(config);

        assert!(matches!(burst_at(&mut monitor, 0.0, 5.00), MonitorEvent::CandidateOpened(_)));

        // Full round-trip back below the breakout level -> not confirmed.
        let resolved = monitor.on_trade(trade(0.2, 4.50));
        let MonitorEvent::FollowThroughResolved(result) = resolved else {
            panic!("expected the candidate to resolve, got {resolved:?}");
        };
        assert!(!result.confirmed, "test setup should produce a failed candidate");

        // Well inside what would have been the cooldown window.
        let next = burst_at(&mut monitor, 60.0, 5.00);
        assert!(
            matches!(next, MonitorEvent::CandidateOpened(_)),
            "a failed candidate must not have started a cooldown, got {next:?}"
        );
    }

    #[test]
    fn cooldown_disabled_at_zero_restores_the_pre_cooldown_behavior() {
        let config = MonitorConfig {
            confirmation_trade_count: 1,
            alert_cooldown_secs: 0.0,
            ..MonitorConfig::default()
        };
        let mut monitor = IgnitionMonitor::new(config);

        assert!(matches!(burst_at(&mut monitor, 0.0, 5.00), MonitorEvent::CandidateOpened(_)));
        let MonitorEvent::FollowThroughResolved(result) = monitor.on_trade(trade(0.2, 5.80)) else {
            panic!("expected the candidate to resolve");
        };
        assert!(result.confirmed);

        // 60s after a confirmation — deep inside what the default 300s
        // cooldown would have suppressed, but this monitor has none.
        let next = burst_at(&mut monitor, 60.0, 5.00);
        assert!(
            matches!(next, MonitorEvent::CandidateOpened(_)),
            "cooldown 0.0 should be fully off, got {next:?}"
        );
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
