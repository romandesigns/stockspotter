//! Extracts discrete "a signal fired" moments from a `replay_engine`
//! result — architecture doc section 8 step 2 needs one clear moment per
//! signal to evaluate, not a whole timeline of bars where a condition
//! happens to hold.
//!
//! Funnel/momentum are *edge-triggered*: only the bar where
//! `passed()`/`qualifies()` flips from false to true counts as a new
//! signal — otherwise a strategy that stays qualified for 20 straight
//! bars would count as 20 signals instead of 1, wildly inflating hit
//! rate/signal-count stats. Ignition's `FollowThroughConfirmed` events are
//! already discrete per-occurrence signals, no dedup needed.

use replay_engine::{IgnitionEventKind, ReplayResult};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Strategy {
    FastFunnel,
    MomentumScorer,
    IgnitionDetector,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalMoment {
    pub strategy: Strategy,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub price: f64,
    /// Index into `ReplayResult::bar_events` at (or immediately after)
    /// this signal — used to slice out the following-prices window for
    /// outcome evaluation. All three strategies are evaluated against
    /// the same 1-min bar closes for a consistent methodology, even
    /// though ignition signals are technically tick-timed.
    pub bar_index: usize,
}

/// Extracts every edge-triggered funnel/momentum signal plus every
/// confirmed ignition signal from one replay result.
pub fn extract_signals(result: &ReplayResult) -> Vec<SignalMoment> {
    let mut signals = Vec::new();

    let mut funnel_was_passed = false;
    let mut momentum_was_qualified = false;
    for (i, event) in result.bar_events.iter().enumerate() {
        let passed = event.funnel.passed();
        if passed && !funnel_was_passed {
            signals.push(SignalMoment {
                strategy: Strategy::FastFunnel,
                timestamp: event.timestamp,
                price: event.price,
                bar_index: i,
            });
        }
        funnel_was_passed = passed;

        let qualifies = event
            .momentum
            .qualifies(momentum_scorer::DEFAULT_QUALIFY_THRESHOLD);
        if qualifies && !momentum_was_qualified {
            signals.push(SignalMoment {
                strategy: Strategy::MomentumScorer,
                timestamp: event.timestamp,
                price: event.price,
                bar_index: i,
            });
        }
        momentum_was_qualified = qualifies;
    }

    for event in &result.ignition_events {
        if !matches!(event.kind, IgnitionEventKind::FollowThroughConfirmed) {
            continue;
        }
        // Ignition fires on ticks, not bars — anchor it to the first bar
        // at or after the tick's timestamp so its following-prices window
        // comes from real subsequent bars, not ticks before the signal.
        let bar_index = result
            .bar_events
            .iter()
            .position(|b| b.timestamp >= event.timestamp)
            .unwrap_or(result.bar_events.len());
        signals.push(SignalMoment {
            strategy: Strategy::IgnitionDetector,
            timestamp: event.timestamp,
            price: event.price,
            bar_index,
        });
    }

    signals.sort_by_key(|s| s.timestamp);
    signals
}

/// The bar closes strictly after `signal.bar_index`, for outcome
/// evaluation.
pub fn following_prices(result: &ReplayResult, signal: &SignalMoment) -> Vec<f64> {
    result
        .bar_events
        .iter()
        .skip(signal.bar_index + 1)
        .map(|e| e.price)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fast_funnel::FunnelExplanation;
    use momentum_scorer::MomentumScore;
    use replay_engine::BarEvent;

    fn bar(timestamp_secs: i64, price: f64, funnel_passed: bool, momentum_high: bool) -> BarEvent {
        use chrono::TimeZone;
        BarEvent {
            timestamp: chrono::Utc.timestamp_opt(timestamp_secs, 0).unwrap(),
            price,
            gap_pct: 0.0,
            session_volume: 1000,
            funnel: FunnelExplanation {
                price_ok: funnel_passed,
                float_ok: funnel_passed,
                rel_vol_ok: funnel_passed,
                gap_ok: funnel_passed,
            },
            momentum: MomentumScore {
                volume_confirmation: if momentum_high { 1.0 } else { 0.0 },
                structure: if momentum_high { 1.0 } else { 0.0 },
                ma_slope: if momentum_high { 1.0 } else { 0.0 },
                wick_rejection: if momentum_high { 1.0 } else { 0.0 },
                overall: if momentum_high { 1.0 } else { 0.0 },
            },
        }
    }

    #[test]
    fn funnel_signal_is_edge_triggered_not_repeated_every_bar() {
        let result = ReplayResult {
            symbol: "TEST".to_string(),
            bar_events: vec![
                bar(0, 1.0, false, false),
                bar(60, 1.0, true, false),
                bar(120, 1.0, true, false), // still passed — should NOT re-signal
                bar(180, 1.0, true, false), // still passed — should NOT re-signal
            ],
            ignition_events: vec![],
        };
        let signals = extract_signals(&result);
        let funnel_signals: Vec<_> = signals
            .iter()
            .filter(|s| s.strategy == Strategy::FastFunnel)
            .collect();
        assert_eq!(funnel_signals.len(), 1);
        assert_eq!(funnel_signals[0].bar_index, 1);
    }

    #[test]
    fn funnel_signal_fires_again_after_dropping_and_requalifying() {
        let result = ReplayResult {
            symbol: "TEST".to_string(),
            bar_events: vec![
                bar(0, 1.0, true, false),
                bar(60, 1.0, false, false),
                bar(120, 1.0, true, false),
            ],
            ignition_events: vec![],
        };
        let signals = extract_signals(&result);
        let funnel_signals: Vec<_> = signals
            .iter()
            .filter(|s| s.strategy == Strategy::FastFunnel)
            .collect();
        assert_eq!(funnel_signals.len(), 2);
    }

    #[test]
    fn following_prices_excludes_the_signal_bar_itself() {
        let result = ReplayResult {
            symbol: "TEST".to_string(),
            bar_events: vec![bar(0, 1.0, false, false), bar(60, 2.0, true, false), bar(120, 3.0, true, false)],
            ignition_events: vec![],
        };
        let signal = SignalMoment {
            strategy: Strategy::FastFunnel,
            timestamp: result.bar_events[1].timestamp,
            price: 2.0,
            bar_index: 1,
        };
        assert_eq!(following_prices(&result, &signal), vec![3.0]);
    }
}
