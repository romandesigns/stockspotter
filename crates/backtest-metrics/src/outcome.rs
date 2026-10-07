//! Per-signal outcome evaluation — architecture doc section 8, step 2:
//! "For every signal fired..., log the actual subsequent price action."
//!
//! Methodology, made explicit rather than left implicit: a signal is a
//! "hit" if price moves up by `target_pct` before it drops by `stop_pct`,
//! within `lookforward_bars` bars — a simple target/stop model, not
//! anything more elaborate. This is a deliberately transparent starting
//! definition (the doc doesn't specify one), meant to be tuned once real
//! aggregate results exist, not treated as a final answer.

use serde::{Deserialize, Serialize};

use crate::signals::Strategy;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutcomeThresholds {
    /// A favorable move of at least this % (vs the signal price) counts
    /// as a hit.
    pub target_pct: f64,
    /// An adverse move of at least this % counts as a failure — checked
    /// bar-by-bar alongside the target, whichever comes first wins.
    pub stop_pct: f64,
    /// How many bars ahead to evaluate before giving up (neither target
    /// nor stop reached = not a hit, but not counted as a clean stop-out
    /// either — see `SignalOutcome::hit`).
    pub lookforward_bars: usize,
}

impl Default for OutcomeThresholds {
    /// A "swing" profile — reasonable for evaluating the fast funnel and
    /// momentum scorer, which are about sustained multi-minute
    /// qualification, not a single tick-level trigger.
    fn default() -> Self {
        Self {
            target_pct: 5.0,
            stop_pct: 3.0,
            lookforward_bars: 20,
        }
    }
}

impl OutcomeThresholds {
    /// A "scalp" profile, backtested for the ignition detector
    /// specifically (`backtest-metrics --bin tune`, 2026-08-30 against
    /// real SWVL data): evaluating ignition signals against `default()`'s
    /// swing bar produced only a 9.3% hit rate, but every monitor config
    /// tested did 3-4x better against this smaller/faster bar — ignition
    /// is fundamentally a fast microstructure signal, not a sustained
    /// swing one, and the outcome definition needs to match that or it's
    /// measuring the wrong thing. Confirmed best balance of hit rate vs.
    /// sample size: 35.8% hit rate on 316 signals (vs. 493 at the old
    /// confirmation_trade_count=10 / default() combination).
    ///
    /// **2026-09-06, on a real sample.** All of the above came from one
    /// symbol on one session. `--bin backtest_broad` now measures 45
    /// sessions across 9 symbols: 869 ignition signals, 38.6% hit rate,
    /// average winner +3.33%. Two things worth reading off that. The
    /// bracket's direction is validated — ignition really is a fast
    /// scalp signal, not a swing one. But 38.6% against a SYMMETRIC
    /// 2%/2% bracket is -0.46pp expectancy per signal, i.e. below
    /// breakeven unmanaged, and winners averaging +3.33% suggests the
    /// +2.0 target may be leaving real move on the table.
    ///
    /// Deliberately NOT retuned here. The log records
    /// `max_favorable_pct` but no max-adverse figure, so a different
    /// `stop_pct` cannot be evaluated post-hoc — changing this bracket
    /// honestly means re-running `backtest_broad` under it, not fitting
    /// a better-looking number to the sample already in hand. That
    /// distinction is the whole reason the broad sample was gathered.
    pub fn scalp() -> Self {
        Self {
            target_pct: 2.0,
            stop_pct: 2.0,
            lookforward_bars: 10,
        }
    }

    /// The right outcome bar depends on what kind of signal is being
    /// judged — a single blanket threshold for every strategy is exactly
    /// the mistake the tuning session above found. Ignition gets the
    /// scalp profile; funnel/momentum keep the swing default until
    /// they've had their own backtest pass to confirm or revise it.
    /// Consolidation breakout also starts on the swing default — an
    /// unverified starting choice (it enters on a held breakout close,
    /// not a tick-level trigger, so "swing" is the closer analogy of the
    /// two existing profiles) rather than a backtested one; revisit once
    /// it's been run through `backtest-metrics --bin tune_broad`.
    pub fn for_strategy(strategy: Strategy) -> Self {
        match strategy {
            // Micropullback gets ignition's fast scalp profile, not
            // consolidation-breakout's swing default -- it's built to
            // catch an "act within seconds" resumption (see
            // market_data::live::micropullback_config's own doc
            // comment), the same fast-microstructure character that
            // moved ignition off the swing default in the first place
            // (9.3% -> 35.8% hit rate once judged against the right bar,
            // 2026-08-30/31). Judging it against the slower swing bar
            // would repeat that exact mistake.
            Strategy::IgnitionDetector | Strategy::Micropullback => Self::scalp(),
            Strategy::FastFunnel | Strategy::MomentumScorer | Strategy::ConsolidationBreakout => Self::default(),
        }
    }
}

/// How a signal's outcome actually resolved -- added 2026-09-06 to close
/// a real gap flagged during the auto-trader v4 near-miss investigation
/// (see `strategy_config`'s own doc comment): `hit`/`max_favorable_pct`
/// alone can't tell a clean stop-out apart from a signal that just ran
/// out its lookforward window never having moved much either way, so
/// every "miss" got silently treated as if it cost the full `stop_pct`
/// -- overstating real losses on anything that actually just timed out
/// flat.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum OutcomeKind {
    /// Reached `target_pct` before `stop_pct`, within the window.
    Hit,
    /// Reached `stop_pct` (adverse) before `target_pct`, within the
    /// window -- a real, clean stop-out, not just "didn't win".
    StoppedOut,
    /// Neither threshold was reached before the lookforward window ran
    /// out (or there was no following price data at all to evaluate).
    TimedOut,
    /// A signal logged before this field existed -- `data/
    /// backtest_log.jsonl` already has tens of thousands of these lines
    /// on the real deployed VPS, and `hit`/`max_favorable_pct`/
    /// `bars_to_target` are still fully valid for them. Only the finer
    /// stopped-out/timed-out/real-expectancy breakdown is unavailable
    /// for pre-existing data -- `#[serde(default)]` on `SignalOutcome`'s
    /// `kind` field means an old JSONL line missing this field
    /// deserializes to this variant rather than failing to parse at all.
    /// `evaluate_outcome()` itself never produces this -- it's only ever
    /// seen coming back out of already-persisted data.
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SignalOutcome {
    /// True if `target_pct` was reached before `stop_pct` within the
    /// lookforward window.
    pub hit: bool,
    /// Best favorable move seen within the window, as a percentage —
    /// recorded even on a miss, so "how close did it get" isn't lost.
    pub max_favorable_pct: f64,
    /// How many bars after the signal the target was reached, if it was.
    pub bars_to_target: Option<usize>,
    /// Which of the three ways this could resolve actually happened --
    /// see `OutcomeKind`. `#[serde(default)]` so every already-logged
    /// line on the real deployed VPS (which predates this field) still
    /// parses, just as `OutcomeKind::Unknown`.
    #[serde(default)]
    pub kind: OutcomeKind,
    /// The actual realized percentage move at the bar that resolved this
    /// signal (the target-crossing bar's real pct on a `Hit`, which can
    /// exceed `target_pct` if price gapped past it; the stop-crossing
    /// bar's real pct on a `StoppedOut`, which can be worse than
    /// `-stop_pct` the same way; the last-evaluated bar's real pct on a
    /// `TimedOut`, whatever that ended up being) -- the real number
    /// behind `OutcomeKind`, letting real average loss/timeout size (and
    /// a genuine evidence-based expectancy across every outcome, not
    /// just wins) be computed instead of assuming every non-hit cost
    /// exactly `stop_pct`. `#[serde(default)]` for the same
    /// backward-compatibility reason as `kind` -- defaults to `0.0` on
    /// old data, which is never read on its own without also checking
    /// `kind != Unknown` first (see `metrics::aggregate`).
    #[serde(default)]
    pub final_pct: f64,
}

/// `following_prices` is the price series *after* the signal fired,
/// chronological, as many bars as are available (may be shorter than
/// `lookforward_bars` near the end of a replay window — evaluated over
/// whatever exists, not padded or extrapolated).
pub fn evaluate_outcome(
    signal_price: f64,
    following_prices: &[f64],
    thresholds: &OutcomeThresholds,
) -> SignalOutcome {
    let mut max_favorable_pct = 0.0_f64;
    let mut hit = false;
    let mut bars_to_target = None;
    let mut kind = OutcomeKind::TimedOut;
    let mut final_pct = 0.0_f64;

    if signal_price <= 0.0 {
        return SignalOutcome {
            hit: false,
            max_favorable_pct: 0.0,
            bars_to_target: None,
            kind: OutcomeKind::TimedOut,
            final_pct: 0.0,
        };
    }

    for (i, &price) in following_prices
        .iter()
        .take(thresholds.lookforward_bars)
        .enumerate()
    {
        let pct = (price - signal_price) / signal_price * 100.0;
        max_favorable_pct = max_favorable_pct.max(pct);
        final_pct = pct;

        if pct >= thresholds.target_pct {
            hit = true;
            bars_to_target = Some(i + 1);
            kind = OutcomeKind::Hit;
            break;
        }
        if pct <= -thresholds.stop_pct {
            kind = OutcomeKind::StoppedOut;
            break;
        }
    }

    SignalOutcome {
        hit,
        max_favorable_pct,
        bars_to_target,
        kind,
        final_pct,
    }
}

/// Re-evaluates a stored `LoggedSignal::forward_path_pct` under any
/// bracket, without re-fetching anything.
///
/// This is the same decision `evaluate_outcome` makes, expressed over
/// percentages instead of raw prices — target and stop are both checked
/// on every bar in order, and whichever is crossed FIRST decides the
/// outcome. That ordering is the whole reason the path is stored rather
/// than a max-favorable summary: a signal that ran +3% then -4% and one
/// that ran -4% then +3% have identical max-favorable figures and
/// opposite outcomes.
///
/// Kept as its own function rather than folding into `evaluate_outcome`
/// because the inputs genuinely differ (percentages already relative to
/// the signal, vs raw prices needing a divide), and because a single
/// shared body would have to re-derive prices from percentages just to
/// divide them back out again. Their agreement is pinned by
/// `path_evaluation_matches_price_evaluation` below.
pub fn evaluate_outcome_from_path(path_pct: &[f64], thresholds: &OutcomeThresholds) -> SignalOutcome {
    let mut max_favorable_pct = 0.0_f64;
    let mut final_pct = 0.0_f64;

    for (i, &pct) in path_pct.iter().take(thresholds.lookforward_bars).enumerate() {
        max_favorable_pct = max_favorable_pct.max(pct);
        final_pct = pct;

        if pct >= thresholds.target_pct {
            return SignalOutcome {
                hit: true,
                max_favorable_pct,
                bars_to_target: Some(i + 1),
                kind: OutcomeKind::Hit,
                final_pct,
            };
        }
        if pct <= -thresholds.stop_pct {
            return SignalOutcome {
                hit: false,
                max_favorable_pct,
                bars_to_target: None,
                kind: OutcomeKind::StoppedOut,
                final_pct,
            };
        }
    }

    SignalOutcome {
        hit: false,
        max_favorable_pct,
        bars_to_target: None,
        kind: OutcomeKind::TimedOut,
        final_pct,
    }
}

/// Turns a post-signal price series into the percentage path stored on
/// `LoggedSignal::forward_path_pct`. Truncated to
/// `log::MAX_FORWARD_PATH_BARS`; an invalid signal price yields an empty
/// path (which the sweep skips rather than scoring as flat).
pub fn forward_path_pct(signal_price: f64, following_prices: &[f64]) -> Vec<f64> {
    if signal_price <= 0.0 {
        return Vec::new();
    }
    following_prices
        .iter()
        .take(crate::log::MAX_FORWARD_PATH_BARS)
        .map(|p| (p - signal_price) / signal_price * 100.0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_evaluation_matches_price_evaluation() {
        // The two evaluators must agree, or every sweep result is
        // measuring something subtly different from what the backtest
        // logged. Checked across a spread of real-shaped paths: a clean
        // win, a clean stop, a timeout, and the two orderings that a
        // max-favorable summary cannot tell apart.
        let t = OutcomeThresholds { target_pct: 2.0, stop_pct: 2.0, lookforward_bars: 10 };
        let cases: Vec<Vec<f64>> = vec![
            vec![100.0, 101.0, 103.0],          // runs to target
            vec![100.0, 99.0, 97.0],            // runs to stop
            vec![100.0, 100.5, 99.7, 100.2],    // neither, times out
            vec![100.0, 103.0, 96.0],           // up first, then down -> Hit
            vec![100.0, 96.0, 103.0],           // down first, then up -> StoppedOut
            vec![100.0],                        // no following bars at all
        ];
        for prices in cases {
            let signal_price = prices[0];
            let following = &prices[1..];
            let from_prices = evaluate_outcome(signal_price, following, &t);
            let path = forward_path_pct(signal_price, following);
            let from_path = evaluate_outcome_from_path(&path, &t);
            assert_eq!(from_prices.hit, from_path.hit, "hit disagreed on {prices:?}");
            assert_eq!(from_prices.kind, from_path.kind, "kind disagreed on {prices:?}");
            assert_eq!(from_prices.bars_to_target, from_path.bars_to_target, "bars disagreed on {prices:?}");
            assert!(
                (from_prices.max_favorable_pct - from_path.max_favorable_pct).abs() < 1e-9,
                "max favorable disagreed on {prices:?}"
            );
            assert!((from_prices.final_pct - from_path.final_pct).abs() < 1e-9, "final pct disagreed on {prices:?}");
        }
    }

    #[test]
    fn ordering_decides_the_outcome_not_the_extremes() {
        // The exact case a max-favorable/max-adverse summary cannot
        // resolve, and the reason the full path is stored: both paths
        // touch +3% and -4%, in opposite order, and resolve oppositely.
        let t = OutcomeThresholds { target_pct: 2.0, stop_pct: 2.0, lookforward_bars: 10 };
        assert_eq!(evaluate_outcome_from_path(&[3.0, -4.0], &t).kind, OutcomeKind::Hit);
        assert_eq!(evaluate_outcome_from_path(&[-4.0, 3.0], &t).kind, OutcomeKind::StoppedOut);
    }

    #[test]
    fn a_wider_lookforward_can_change_a_timeout_into_a_hit() {
        // What the sweep exists to discover: the same stored evidence,
        // judged over a longer hold, resolves differently.
        let path = [0.5, 0.9, 1.2, 1.6, 2.4];
        let short = OutcomeThresholds { target_pct: 2.0, stop_pct: 2.0, lookforward_bars: 3 };
        let long = OutcomeThresholds { target_pct: 2.0, stop_pct: 2.0, lookforward_bars: 10 };
        assert_eq!(evaluate_outcome_from_path(&path, &short).kind, OutcomeKind::TimedOut);
        assert_eq!(evaluate_outcome_from_path(&path, &long).kind, OutcomeKind::Hit);
    }

    #[test]
    fn forward_path_is_capped_and_rejects_a_bad_signal_price() {
        let prices: Vec<f64> = (0..100).map(|i| 100.0 + i as f64).collect();
        assert_eq!(forward_path_pct(100.0, &prices).len(), crate::log::MAX_FORWARD_PATH_BARS);
        assert!(forward_path_pct(0.0, &prices).is_empty());
        assert!(forward_path_pct(-1.0, &prices).is_empty());
    }

    fn thresholds() -> OutcomeThresholds {
        OutcomeThresholds {
            target_pct: 5.0,
            stop_pct: 3.0,
            lookforward_bars: 10,
        }
    }

    #[test]
    fn for_strategy_gives_ignition_the_scalp_profile() {
        let t = OutcomeThresholds::for_strategy(Strategy::IgnitionDetector);
        assert_eq!(t, OutcomeThresholds::scalp());
        assert_ne!(t, OutcomeThresholds::default());
    }

    #[test]
    fn for_strategy_gives_micropullback_the_scalp_profile_not_the_swing_default() {
        // Same reasoning as ignition -- see for_strategy's own doc
        // comment on why judging a fast microstructure signal against
        // the slower swing bar would repeat the exact mistake that
        // moved ignition off it in the first place.
        let t = OutcomeThresholds::for_strategy(Strategy::Micropullback);
        assert_eq!(t, OutcomeThresholds::scalp());
        assert_ne!(t, OutcomeThresholds::default());
    }

    #[test]
    fn for_strategy_gives_funnel_and_momentum_the_swing_default() {
        assert_eq!(
            OutcomeThresholds::for_strategy(Strategy::FastFunnel),
            OutcomeThresholds::default()
        );
        assert_eq!(
            OutcomeThresholds::for_strategy(Strategy::MomentumScorer),
            OutcomeThresholds::default()
        );
    }

    #[test]
    fn hits_when_target_reached_before_stop() {
        let prices = [1.01, 1.02, 1.03, 1.06]; // +6% on the 4th bar
        let outcome = evaluate_outcome(1.00, &prices, &thresholds());
        assert!(outcome.hit);
        assert_eq!(outcome.bars_to_target, Some(4));
        assert!(outcome.max_favorable_pct >= 5.0);
        assert_eq!(outcome.kind, OutcomeKind::Hit);
        // The real crossing bar's pct (+6%), not the flat +5% threshold --
        // a real win can (and here does) run past the target it cleared.
        assert!((outcome.final_pct - 6.0).abs() < 1e-9);
    }

    #[test]
    fn misses_when_stopped_out_before_target() {
        let prices = [0.99, 0.98, 0.965, 1.10]; // -3.5% before the later +10%
        let outcome = evaluate_outcome(1.00, &prices, &thresholds());
        assert!(!outcome.hit);
        assert_eq!(outcome.bars_to_target, None);
        assert_eq!(outcome.kind, OutcomeKind::StoppedOut);
        assert!((outcome.final_pct - (-3.5)).abs() < 1e-9);
    }

    #[test]
    fn misses_when_lookforward_window_runs_out() {
        let prices = [1.01, 1.02, 1.03]; // never reaches +5%, never stops out
        let outcome = evaluate_outcome(1.00, &prices, &thresholds());
        assert!(!outcome.hit);
        assert!((outcome.max_favorable_pct - 3.0).abs() < 1e-9);
        assert_eq!(outcome.kind, OutcomeKind::TimedOut);
        // The last bar evaluated, not the best favorable move seen --
        // here they happen to be the same value, unlike a timeout that
        // pulled back from an earlier high.
        assert!((outcome.final_pct - 3.0).abs() < 1e-9);
    }

    #[test]
    fn timed_out_final_pct_reflects_the_last_bar_not_the_best_one_seen() {
        // Ran up to +4% then pulled back to +1% by the time the window
        // runs out (never reaching the +5% target, never stopping out) --
        // max_favorable_pct should still show the +4% high-water mark,
        // but final_pct (what a real evidence-based expectancy should
        // average) must reflect where it actually ended up, not the peak.
        let prices = [1.02, 1.04, 1.03, 1.01];
        let outcome = evaluate_outcome(1.00, &prices, &thresholds());
        assert_eq!(outcome.kind, OutcomeKind::TimedOut);
        assert!((outcome.max_favorable_pct - 4.0).abs() < 1e-9);
        assert!((outcome.final_pct - 1.0).abs() < 1e-9);
    }

    #[test]
    fn old_json_missing_kind_and_final_pct_deserializes_as_unknown_not_a_parse_error() {
        // Real backward-compatibility requirement: data/backtest_log.jsonl
        // on the deployed VPS has tens of thousands of lines written
        // before this field existed -- they must keep parsing.
        let old_json = r#"{"hit":true,"max_favorable_pct":6.0,"bars_to_target":3}"#;
        let parsed: SignalOutcome = serde_json::from_str(old_json).expect("old-shaped JSON must still parse");
        assert!(parsed.hit);
        assert_eq!(parsed.kind, OutcomeKind::Unknown);
        assert_eq!(parsed.final_pct, 0.0);
    }

    #[test]
    fn respects_lookforward_bars_cap() {
        let mut prices = vec![1.001; 5];
        prices.push(1.10); // +10%, but past the 5-bar cap below
        let thresholds = OutcomeThresholds {
            lookforward_bars: 5,
            ..thresholds()
        };
        let outcome = evaluate_outcome(1.00, &prices, &thresholds);
        assert!(!outcome.hit);
    }

    #[test]
    fn zero_or_negative_signal_price_does_not_divide_by_zero() {
        let outcome = evaluate_outcome(0.0, &[1.0, 2.0], &thresholds());
        assert!(!outcome.hit);
        assert_eq!(outcome.max_favorable_pct, 0.0);
    }
}
