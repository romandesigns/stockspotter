//! Aggregate metrics — architecture doc section 8 step 3: hit rate,
//! average move size on winners, timing accuracy, per strategy.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::outcome::SignalOutcome;
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

    AggregateMetrics {
        total_signals,
        hits,
        hit_rate_pct,
        avg_move_pct_on_winners,
        avg_bars_to_target_on_winners,
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

    fn outcome(hit: bool, max_favorable_pct: f64, bars_to_target: Option<usize>) -> SignalOutcome {
        SignalOutcome {
            hit,
            max_favorable_pct,
            bars_to_target,
        }
    }

    #[test]
    fn empty_outcomes_produce_zeroed_metrics_not_nan() {
        let m = aggregate(&[]);
        assert_eq!(m.total_signals, 0);
        assert_eq!(m.hit_rate_pct, 0.0);
        assert_eq!(m.avg_move_pct_on_winners, 0.0);
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
