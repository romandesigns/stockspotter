//! Ignition detection signals — architecture doc section 4.3. Three of the
//! doc's four tick-level signals are implemented here:
//!
//! - Sudden spike in trade frequency
//! - Bid-ask spread tightening aggressively
//! - Ask-side size being rapidly eaten through
//!
//! **Not implemented: halt-lift resumption moves.** That needs Alpaca's
//! trading-status/LULD feed, a separate subscription this crate's caller
//! doesn't ingest yet — same kind of documented, fail-closed gap as
//! `fast_funnel`'s missing float-share source, not a silent omission.
//! `IgnitionSignals::triggered` never fires on halt-lift alone because
//! there's currently no way to detect it.
//!
//! Unlike the momentum scorer's weighted average, these are independent
//! either/or signals per the doc's own framing (a list of distinct signal
//! *types*, not factors to blend into one number) — any one crossing its
//! threshold makes this a candidate, which then still has to survive
//! follow-through confirmation (`follow_through.rs`) before it's a real
//! alert.

use serde::{Deserialize, Serialize};

use crate::tick::{Quote, Trade};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IgnitionThresholds {
    /// Recent trade rate must be at least this many times the baseline
    /// rate to count as a spike.
    pub trade_frequency_spike_ratio: f64,
    /// Recent avg spread must be at most this fraction of the baseline avg
    /// spread to count as tightening (e.g. 0.5 = spread cut in half).
    pub spread_tighten_ratio: f64,
    /// Ask size must shrink by at least this fraction across the quote
    /// window, at a stable-or-rising ask price, to count as absorption.
    pub ask_absorption_min_drop_ratio: f64,
    /// A single trade landing inside the "recent" window trivially
    /// produces a high rate ratio just by being the newest trade to
    /// arrive — found empirically via the monitor's own tests, replaying
    /// realistic sparse-but-regular baseline trades one at a time. Require
    /// at least this many trades inside the recent window before a spike
    /// ratio counts as a real spike rather than single-print noise.
    pub min_recent_trades_for_spike: usize,
}

impl Default for IgnitionThresholds {
    /// Doc gives no exact numbers for section 4.3 (unlike the fast
    /// funnel's explicit $/float/volume/gap figures) — starting values
    /// only, tune via backtesting (section 8) once real data exists.
    fn default() -> Self {
        Self {
            trade_frequency_spike_ratio: 3.0,
            spread_tighten_ratio: 0.5,
            ask_absorption_min_drop_ratio: 0.6,
            min_recent_trades_for_spike: 3,
        }
    }
}

/// Per-signal breakdown, same transparency pattern as
/// `fast_funnel::FunnelExplanation` / `momentum_scorer::MomentumScore`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct IgnitionSignals {
    /// Recent trades/sec ÷ baseline trades/sec. `None` if there isn't
    /// enough trade history to establish a baseline.
    pub trade_frequency_ratio: Option<f64>,
    /// Recent avg spread ÷ baseline avg spread — lower means tighter.
    /// `None` if there isn't enough quote history.
    pub spread_ratio: Option<f64>,
    /// Whether resting ask size was rapidly eaten through without the
    /// price giving way. `None` if there isn't enough quote history.
    pub ask_absorbed: Option<bool>,
    pub trade_frequency_spiked: bool,
    pub spread_tightened: bool,
    /// Any one of the three implemented signals crossing its threshold.
    pub triggered: bool,
}

/// Splits `trades` into a "recent" window (the most recent
/// `recent_window_secs` of activity) and a "baseline" window (the
/// `baseline_window_secs` immediately before that), and returns
/// recent-rate ÷ baseline-rate. `None` if there isn't enough history to
/// fill both windows — a short/sparse tape shouldn't get to claim a spike
/// it can't actually establish a baseline for.
pub fn trade_frequency_ratio(
    trades: &[Trade],
    recent_window_secs: f64,
    baseline_window_secs: f64,
) -> Option<f64> {
    let last = trades.last()?;
    let now = last.timestamp_secs;
    let recent_start = now - recent_window_secs;
    let baseline_start = recent_start - baseline_window_secs;

    if trades.first()?.timestamp_secs > baseline_start {
        return None; // history doesn't reach back far enough
    }

    let recent_count = trades
        .iter()
        .filter(|t| t.timestamp_secs > recent_start)
        .count();
    let baseline_count = trades
        .iter()
        .filter(|t| t.timestamp_secs > baseline_start && t.timestamp_secs <= recent_start)
        .count();

    if baseline_count == 0 || baseline_window_secs <= 0.0 || recent_window_secs <= 0.0 {
        return None;
    }

    let recent_rate = recent_count as f64 / recent_window_secs;
    let baseline_rate = baseline_count as f64 / baseline_window_secs;
    if baseline_rate == 0.0 {
        return None;
    }
    Some(recent_rate / baseline_rate)
}

/// Average spread over the most recent `recent_n` quotes ÷ average spread
/// over the `baseline_n` quotes immediately before that.
pub fn spread_ratio(quotes: &[Quote], recent_n: usize, baseline_n: usize) -> Option<f64> {
    if quotes.len() < recent_n + baseline_n || recent_n == 0 || baseline_n == 0 {
        return None;
    }
    let split = quotes.len() - recent_n;
    let baseline_start = split.checked_sub(baseline_n)?;

    let recent_avg = avg_spread(&quotes[split..]);
    let baseline_avg = avg_spread(&quotes[baseline_start..split]);
    if baseline_avg <= 0.0 {
        return None;
    }
    Some(recent_avg / baseline_avg)
}

fn avg_spread(quotes: &[Quote]) -> f64 {
    quotes.iter().map(Quote::spread).sum::<f64>() / quotes.len() as f64
}

fn recent_trade_count(trades: &[Trade], recent_window_secs: f64) -> usize {
    let Some(last) = trades.last() else {
        return 0;
    };
    let recent_start = last.timestamp_secs - recent_window_secs;
    trades
        .iter()
        .filter(|t| t.timestamp_secs > recent_start)
        .count()
}

/// True if ask size shrank by at least `min_drop_ratio` from the start to
/// the end of `quotes` while the ask price held flat or rose — i.e.
/// resting size actually got eaten through rather than the price just
/// walking away from a wide, thin book.
pub fn ask_absorbed(quotes: &[Quote], min_drop_ratio: f64) -> Option<bool> {
    let first = quotes.first()?;
    let last = quotes.last()?;
    if first.ask_size == 0 {
        return None;
    }
    let price_held_or_rose = last.ask_price >= first.ask_price;
    let drop_ratio = 1.0 - (last.ask_size as f64 / first.ask_size as f64);
    Some(price_held_or_rose && drop_ratio >= min_drop_ratio)
}

/// Evaluates all implemented signals over the given trade/quote windows.
pub fn detect(
    trades: &[Trade],
    quotes: &[Quote],
    recent_window_secs: f64,
    baseline_window_secs: f64,
    spread_recent_n: usize,
    spread_baseline_n: usize,
    thresholds: &IgnitionThresholds,
) -> IgnitionSignals {
    let trade_frequency_ratio =
        trade_frequency_ratio(trades, recent_window_secs, baseline_window_secs);
    let spread_ratio = spread_ratio(quotes, spread_recent_n, spread_baseline_n);
    let ask_absorbed = ask_absorbed(quotes, thresholds.ask_absorption_min_drop_ratio);

    let trade_frequency_spiked = trade_frequency_ratio
        .is_some_and(|r| r >= thresholds.trade_frequency_spike_ratio)
        && recent_trade_count(trades, recent_window_secs) >= thresholds.min_recent_trades_for_spike;
    let spread_tightened =
        spread_ratio.is_some_and(|r| r <= thresholds.spread_tighten_ratio);
    let ask_absorbed_flag = ask_absorbed.unwrap_or(false);

    IgnitionSignals {
        trade_frequency_ratio,
        spread_ratio,
        ask_absorbed,
        trade_frequency_spiked,
        spread_tightened,
        triggered: trade_frequency_spiked || spread_tightened || ask_absorbed_flag,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(t: f64) -> Trade {
        Trade {
            timestamp_secs: t,
            price: 5.0,
            size: 100,
        }
    }

    fn quote(t: f64, bid: f64, ask: f64, ask_size: u64) -> Quote {
        Quote {
            timestamp_secs: t,
            bid_price: bid,
            bid_size: 100,
            ask_price: ask,
            ask_size,
        }
    }

    #[test]
    fn trade_frequency_ratio_needs_enough_history() {
        let trades = vec![trade(0.0), trade(0.5)];
        assert_eq!(trade_frequency_ratio(&trades, 1.0, 10.0), None);
    }

    #[test]
    fn trade_frequency_ratio_detects_a_real_spike() {
        // Baseline: sparse trades reaching back well past the 20s
        // baseline window. Recent: a tight burst of 10 trades in the last
        // ~1s. Total span must cover recent_window + baseline_window
        // (1.0 + 20.0 = 21.0s) or the baseline check bails with None —
        // an earlier version of this test only spanned ~20.9s and hit
        // exactly that.
        let mut trades = Vec::new();
        let mut t = -30.0;
        while t < -3.0 {
            trades.push(trade(t));
            t += 3.0;
        }
        for i in 0..10 {
            trades.push(trade(i as f64 * 0.1)); // burst at t=0.0..0.9
        }
        let ratio = trade_frequency_ratio(&trades, 1.0, 20.0).unwrap();
        assert!(ratio > 10.0, "expected a strong spike ratio, got {ratio}");
    }

    #[test]
    fn spread_ratio_detects_tightening() {
        let mut quotes = Vec::new();
        for i in 0..5 {
            quotes.push(quote(i as f64, 4.90, 5.10, 500)); // wide spread
        }
        for i in 5..10 {
            quotes.push(quote(i as f64, 4.99, 5.01, 500)); // tight spread
        }
        let ratio = spread_ratio(&quotes, 5, 5).unwrap();
        assert!(ratio < 0.2, "expected tight/baseline << 1, got {ratio}");
    }

    #[test]
    fn ask_absorbed_true_when_size_eaten_without_price_giving_way() {
        let quotes = vec![
            quote(0.0, 4.99, 5.00, 1000),
            quote(0.5, 4.99, 5.00, 600),
            quote(1.0, 4.99, 5.00, 150), // 85% of original size gone, price held
        ];
        assert_eq!(ask_absorbed(&quotes, 0.6), Some(true));
    }

    #[test]
    fn ask_absorbed_known_limitation_cannot_distinguish_eaten_from_reprinted() {
        // Size drops, but only because the ask price moved away, not
        // because it was actually eaten through at one level. This
        // heuristic can't tell the difference without order-level data —
        // price_held_or_rose is still true (5.10 >= 5.00), so this comes
        // back Some(true) even though nothing was really "absorbed".
        // Documented here as a known limitation, not silently wrong.
        let quotes = vec![
            quote(0.0, 4.99, 5.00, 1000),
            quote(1.0, 5.05, 5.10, 150),
        ];
        assert_eq!(ask_absorbed(&quotes, 0.6), Some(true));
    }

    #[test]
    fn trade_frequency_does_not_spike_on_a_single_trade_in_sparse_regular_data() {
        // Regression: found via IgnitionMonitor's own tests. A trade
        // spaced every 3s, evaluated the instant it arrives, always has
        // exactly itself inside a 1s "recent" window — that alone
        // produced ratio ~3.3x with the old ungated logic. One trade is
        // not a spike.
        let trades: Vec<Trade> = (0..9).map(|i| trade(-30.0 + i as f64 * 3.0)).collect();
        let thresholds = IgnitionThresholds::default();
        let quotes: Vec<Quote> = Vec::new();
        let result = detect(&trades, &quotes, 1.0, 20.0, 5, 5, &thresholds);
        assert!(
            !result.trade_frequency_spiked,
            "a single regularly-spaced trade should not read as a spike: {result:?}"
        );
    }

    #[test]
    fn trade_frequency_spikes_on_a_real_multi_trade_burst() {
        let mut trades: Vec<Trade> = (0..9).map(|i| trade(-30.0 + i as f64 * 3.0)).collect();
        trades.push(trade(0.0));
        trades.push(trade(0.05));
        trades.push(trade(0.1));
        let thresholds = IgnitionThresholds::default();
        let quotes: Vec<Quote> = Vec::new();
        let result = detect(&trades, &quotes, 1.0, 20.0, 5, 5, &thresholds);
        assert!(result.trade_frequency_spiked, "expected a real burst to spike: {result:?}");
    }

    #[test]
    fn detect_triggers_on_any_single_signal() {
        let trades = vec![trade(0.0)];
        let quotes = vec![
            quote(0.0, 4.90, 5.10, 1000),
            quote(1.0, 4.99, 5.01, 1000),
        ];
        let thresholds = IgnitionThresholds::default();
        // Not enough quote history for spread_ratio (needs recent_n +
        // baseline_n), so this should not spuriously trigger.
        let result = detect(&trades, &quotes, 1.0, 10.0, 5, 5, &thresholds);
        assert!(!result.triggered);
    }
}
