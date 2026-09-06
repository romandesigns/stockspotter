//! Aggregate metrics — architecture doc section 8 step 3: hit rate,
//! average move size on winners, timing accuracy, per strategy.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::outcome::{OutcomeKind, SignalOutcome};
use crate::signals::Strategy;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AggregateMetrics {
    pub total_signals: usize,
    pub hits: usize,
    pub hit_rate_pct: f64,
    /// Average of `max_favorable_pct` across winning signals only — a
    /// signal that missed doesn't get to contribute its (lesser)
    /// favorable move to "how big do winners run".
    pub avg_move_pct_on_winners: f64,
    /// Average bars-to-target across winning signals — "timing
    /// accuracy" per the doc, expressed as how early the signal caught
    /// the move rather than a wall-clock time.
    pub avg_bars_to_target_on_winners: f64,
    /// How many signals resolved as a clean stop-out (`OutcomeKind::
    /// StoppedOut`) — see `avg_loss_pct_on_stopped_out`'s own doc
    /// comment for why this breakdown exists.
    pub stopped_out: usize,
    /// How many signals ran out their whole lookforward window without
    /// hitting either threshold (`OutcomeKind::TimedOut`).
    pub timed_out: usize,
    /// The real average realized loss on signals that actually stopped
    /// out, added 2026-09-06 to close a gap flagged during the
    /// auto-trader v4 near-miss investigation (see `strategy_config`'s
    /// own doc comment): the existing naive expectancy formula there
    /// assumes every non-hit costs the full `stop_pct`, which overstates
    /// the real loss on anything that merely timed out flat rather than
    /// actually stopping out. `0.0` when there are no stopped-out
    /// signals yet (including when every signal predates `OutcomeKind`
    /// entirely — see `OutcomeKind::Unknown`).
    pub avg_loss_pct_on_stopped_out: f64,
    /// The average realized move on signals that timed out — often
    /// small, sometimes meaningfully positive or negative; tells the
    /// real story a plain hit/miss split can't ("how much was actually
    /// left on the table by not having an exit rule for this case").
    /// `0.0` when there are no timed-out signals yet.
    pub avg_final_pct_on_timed_out: f64,
    /// The real, evidence-based expectancy per signal: the mean of
    /// `final_pct` across every signal with known outcome-kind data
    /// (`kind != Unknown`), covering wins, stop-outs, AND timeouts in
    /// one number — directly comparable to (and a real check against)
    /// `strategy_config`'s own faster, cruder hit-rate-times-fixed-
    /// bracket approximation, without touching that formula itself.
    /// `None` only when there is no known-kind data at all yet (e.g.
    /// immediately after this shipped, before any signal has been
    /// freshly (re-)evaluated — every already-logged signal on the real
    /// deployed VPS predates `OutcomeKind` and reads back as `Unknown`
    /// until it's naturally superseded by fresh evaluations over time).
    pub real_expectancy_pct: Option<f64>,
}

pub fn aggregate(outcomes: &[SignalOutcome]) -> AggregateMetrics {
    let total_signals = outcomes.len();
    let winners: Vec<&SignalOutcome> = outcomes.iter().filter(|o| o.hit).collect();
    let hits = winners.len();

    let hit_rate_pct = if total_signals == 0 {
        0.0
    } else {
        hits as f64 / total_signals as f64 * 100.0
    };

    let avg_move_pct_on_winners = if winners.is_empty() {
        0.0
    } else {
        winners.iter().map(|o| o.max_favorable_pct).sum::<f64>() / winners.len() as f64
    };

    let bars_values: Vec<usize> = winners.iter().filter_map(|o| o.bars_to_target).collect();
    let avg_bars_to_target_on_winners = if bars_values.is_empty() {
        0.0
    } else {
        bars_values.iter().sum::<usize>() as f64 / bars_values.len() as f64
    };

    let stopped: Vec<&SignalOutcome> = outcomes.iter().filter(|o| o.kind == OutcomeKind::StoppedOut).collect();
    let stopped_out = stopped.len();
    let avg_loss_pct_on_stopped_out = if stopped.is_empty() {
        0.0
    } else {
        stopped.iter().map(|o| o.final_pct).sum::<f64>() / stopped.len() as f64
    };

    let timed: Vec<&SignalOutcome> = outcomes.iter().filter(|o| o.kind == OutcomeKind::TimedOut).collect();
    let timed_out = timed.len();
    let avg_final_pct_on_timed_out = if timed.is_empty() {
        0.0
    } else {
        timed.iter().map(|o| o.final_pct).sum::<f64>() / timed.len() as f64
    };

    let known: Vec<&SignalOutcome> = outcomes.iter().filter(|o| o.kind != OutcomeKind::Unknown).collect();
    let real_expectancy_pct = if known.is_empty() {
        None
    } else {
        Some(known.iter().map(|o| o.final_pct).sum::<f64>() / known.len() as f64)
    };

    AggregateMetrics {
        total_signals,
        hits,
        hit_rate_pct,
        avg_move_pct_on_winners,
        avg_bars_to_target_on_winners,
        stopped_out,
        timed_out,
        avg_loss_pct_on_stopped_out,
        avg_final_pct_on_timed_out,
        real_expectancy_pct,
    }
}

/// Same aggregation, split out per strategy — the doc's whole point is
/// comparing strategies against each other, not just one blended number.
pub fn aggregate_by_strategy(
    entries: &[(Strategy, SignalOutcome)],
) -> HashMap<Strategy, AggregateMetrics> {
    let mut grouped: HashMap<Strategy, Vec<SignalOutcome>> = HashMap::new();
    for (strategy, outcome) in entries {
        grouped.entry(*strategy).or_default().push(*outcome);
    }
    grouped
        .into_iter()
        .map(|(strategy, outcomes)| (strategy, aggregate(&outcomes)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Defaults to `Unknown`/`0.0` for `kind`/`final_pct` -- every
    // pre-existing test below only ever asserted on hit-rate/winner
    // stats, which don't depend on either, so leaving them as the same
    // "predates OutcomeKind" placeholder every already-logged real
    // signal on the VPS reads back as is the most honest choice, not an
    // arbitrary one. Tests that actually exercise the new breakdown use
    // `outcome_with_kind` below instead.
    fn outcome(hit: bool, max_favorable_pct: f64, bars_to_target: Option<usize>) -> SignalOutcome {
        SignalOutcome {
            hit,
            max_favorable_pct,
            bars_to_target,
            kind: OutcomeKind::Unknown,
            final_pct: 0.0,
        }
    }

    fn outcome_with_kind(kind: OutcomeKind, final_pct: f64) -> SignalOutcome {
        SignalOutcome {
            hit: kind == OutcomeKind::Hit,
            max_favorable_pct: final_pct.max(0.0),
            bars_to_target: if kind == OutcomeKind::Hit { Some(1) } else { None },
            kind,
            final_pct,
        }
    }

    #[test]
    fn empty_outcomes_produce_zeroed_metrics_not_nan() {
        let m = aggregate(&[]);
        assert_eq!(m.total_signals, 0);
        assert_eq!(m.hit_rate_pct, 0.0);
        assert_eq!(m.avg_move_pct_on_winners, 0.0);
        assert_eq!(m.avg_loss_pct_on_stopped_out, 0.0);
        assert_eq!(m.avg_final_pct_on_timed_out, 0.0);
        assert_eq!(m.real_expectancy_pct, None);
    }

    #[test]
    fn hit_rate_computed_correctly() {
        let outcomes = vec![
            outcome(true, 6.0, Some(3)),
            outcome(true, 8.0, Some(5)),
            outcome(false, 1.0, None),
            outcome(false, -2.0, None),
        ];
        let m = aggregate(&outcomes);
        assert_eq!(m.total_signals, 4);
        assert_eq!(m.hits, 2);
        assert_eq!(m.hit_rate_pct, 50.0);
    }

    #[test]
    fn winner_stats_only_average_over_winners_not_all_signals() {
        let outcomes = vec![
            outcome(true, 10.0, Some(2)),
            outcome(false, 100.0, None), // a huge miss shouldn't inflate winner stats
        ];
        let m = aggregate(&outcomes);
        assert_eq!(m.avg_move_pct_on_winners, 10.0);
        assert_eq!(m.avg_bars_to_target_on_winners, 2.0);
    }

    #[test]
    fn real_expectancy_averages_final_pct_across_wins_stops_and_timeouts() {
        // Real-shaped mix: one clean +6% win, one clean -2% stop-out, one
        // timeout that drifted to +0.5% -- the naive hit-rate-times-
        // bracket formula elsewhere in this crate would score this
        // purely off the 1-hit-of-3 rate against a fixed bracket; this
        // metric instead averages what actually happened.
        let outcomes = vec![
            outcome_with_kind(OutcomeKind::Hit, 6.0),
            outcome_with_kind(OutcomeKind::StoppedOut, -2.0),
            outcome_with_kind(OutcomeKind::TimedOut, 0.5),
        ];
        let m = aggregate(&outcomes);
        assert_eq!(m.stopped_out, 1);
        assert_eq!(m.timed_out, 1);
        assert_eq!(m.avg_loss_pct_on_stopped_out, -2.0);
        assert_eq!(m.avg_final_pct_on_timed_out, 0.5);
        let expected = (6.0 + -2.0 + 0.5) / 3.0;
        assert!((m.real_expectancy_pct.unwrap() - expected).abs() < 1e-9);
    }

    #[test]
    fn unknown_kind_legacy_data_is_excluded_from_real_expectancy_but_not_from_hit_rate() {
        // Simulates the real deployed state right after this shipped:
        // every already-logged signal predates OutcomeKind and reads
        // back as Unknown. Hit-rate must still reflect them (nothing
        // about that changed); real_expectancy_pct must not silently
        // treat their placeholder final_pct of 0.0 as real evidence.
        let outcomes = vec![outcome(true, 6.0, Some(3)), outcome(false, -1.0, None)];
        let m = aggregate(&outcomes);
        assert_eq!(m.hit_rate_pct, 50.0);
        assert_eq!(m.real_expectancy_pct, None);
    }

    #[test]
    fn aggregate_by_strategy_keeps_strategies_separate() {
        let entries = vec![
            (Strategy::FastFunnel, outcome(true, 5.0, Some(1))),
            (Strategy::FastFunnel, outcome(false, 0.0, None)),
            (Strategy::IgnitionDetector, outcome(true, 20.0, Some(1))),
        ];
        let grouped = aggregate_by_strategy(&entries);
        assert_eq!(grouped[&Strategy::FastFunnel].total_signals, 2);
        assert_eq!(grouped[&Strategy::FastFunnel].hit_rate_pct, 50.0);
        assert_eq!(grouped[&Strategy::IgnitionDetector].total_signals, 1);
        assert_eq!(grouped[&Strategy::IgnitionDetector].hit_rate_pct, 100.0);
    }
}
