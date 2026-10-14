//! Does any of this predict what price does next? (Specification steps 5
//! and 6: validate the features and measure their value BEFORE anything is
//! allowed to feed ranking.)
//!
//! This module is the measuring half, kept pure so it can be tested
//! without market data: given a time series of observations (what the
//! engine said, and the price then), it reports, for each state of each
//! feature, how price moved over the following one and five minutes.
//!
//! It deliberately reports plain descriptive numbers -- count, mean
//! forward return, share of positive outcomes -- and no significance
//! claim. A table that says "flow confirming: +0.4% over 5 minutes, n=31"
//! is an invitation to look harder, not a result. Small n, overlapping
//! samples from one session, and one symbol at a time all make naive
//! significance testing here misleading, so none is offered.
//!
//! Getting real observations to feed it means replaying real trades and
//! comparing against real outcomes; that is the job of the
//! `order_flow_validate` binary in `replay-engine`, and running it against
//! live sessions is a research decision this crate takes no part in.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::engine::{ExhaustionLevel, FlowRead, LargeTradeRead, Level, OrderFlowSummary, PressureSide};
use crate::levels::AbsorptionSide;

/// One sample: the engine's read at an instant, and the price then.
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    pub at_secs: f64,
    pub price: f64,
    /// (feature name, state) pairs, e.g. ("flow", "confirming").
    pub states: Vec<(&'static str, String)>,
}

impl Observation {
    /// The categorical reads worth testing, taken from a summary. Each is
    /// one feature with a small set of states, so a table of them is
    /// readable and no state is carved out of thin air.
    pub fn from_summary(at_secs: f64, price: f64, summary: &OrderFlowSummary) -> Self {
        let read = &summary.interpretation;
        let pressure = match read.pressure_side {
            PressureSide::Balanced => "balanced".to_string(),
            side => format!("{}_{}", if side == PressureSide::Buy { "buy" } else { "sell" }, level_name(read.pressure_level)),
        };
        let absorption = match (summary.absorption.side, summary.absorption.level) {
            (Some(side), level) if level >= Level::Moderate => {
                format!("{}_{}", if side == AbsorptionSide::SellSide { "sell_side" } else { "buy_side" }, level_name(level))
            }
            _ => "none_or_low".to_string(),
        };
        let states = vec![
            ("pressure", pressure),
            (
                "flow",
                match read.flow {
                    FlowRead::Confirming => "confirming",
                    FlowRead::Diverging => "diverging",
                    FlowRead::Neutral => "neutral",
                }
                .to_string(),
            ),
            ("absorption", absorption),
            (
                "large_trades",
                match read.large_trades {
                    LargeTradeRead::BuyDominant => "buy_dominant",
                    LargeTradeRead::SellDominant => "sell_dominant",
                    LargeTradeRead::Balanced => "balanced",
                    LargeTradeRead::None => "none",
                }
                .to_string(),
            ),
            ("sweeps", level_name(summary.sweeps.state).to_string()),
            (
                "exhaustion",
                match summary.exhaustion.level {
                    ExhaustionLevel::None => "none",
                    ExhaustionLevel::Possible => "possible",
                    ExhaustionLevel::Likely => "likely",
                }
                .to_string(),
            ),
        ];
        Self { at_secs, price, states }
    }
}

fn level_name(level: Level) -> &'static str {
    match level {
        Level::None => "none",
        Level::Low => "low",
        Level::Moderate => "moderate",
        Level::High => "high",
        Level::Extreme => "extreme",
    }
}

/// How price moved after every observation that showed one state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateOutcome {
    pub feature: String,
    pub state: String,
    /// Observations in this state that had a price at the horizon.
    pub count_1m: usize,
    pub mean_return_1m_pct: Option<f64>,
    pub share_positive_1m: Option<f64>,
    pub count_5m: usize,
    pub mean_return_5m_pct: Option<f64>,
    pub share_positive_5m: Option<f64>,
}

const HORIZONS_SECS: [f64; 2] = [60.0, 300.0];

/// Percent change from `observations[from]` to the first later
/// observation at least `horizon` seconds on. `None` when the series ends
/// first, or when the next sample is so much later (more than twice the
/// horizon) that it is really a different question -- a halt or a dead
/// tape must not be scored as "the five-minute return".
fn forward_return(observations: &[Observation], from: usize, horizon: f64) -> Option<f64> {
    let start = &observations[from];
    if start.price <= 0.0 {
        return None;
    }
    let later = observations[from + 1..].iter().find(|o| o.at_secs - start.at_secs >= horizon)?;
    (later.at_secs - start.at_secs <= 2.0 * horizon).then(|| (later.price - start.price) / start.price * 100.0)
}

/// `observations` must be in time order. Rows come back sorted by feature
/// then state, so two runs are comparable line for line.
pub fn outcomes_by_state(observations: &[Observation]) -> Vec<StateOutcome> {
    let mut returns: BTreeMap<(String, String), [Vec<f64>; 2]> = BTreeMap::new();
    for (index, observation) in observations.iter().enumerate() {
        let forward = HORIZONS_SECS.map(|h| forward_return(observations, index, h));
        for (feature, state) in &observation.states {
            let slot = returns.entry((feature.to_string(), state.clone())).or_default();
            for (k, value) in forward.iter().enumerate() {
                if let Some(value) = value {
                    slot[k].push(*value);
                }
            }
        }
    }
    let mean = |v: &[f64]| (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64);
    let positive = |v: &[f64]| (!v.is_empty()).then(|| v.iter().filter(|r| **r > 0.0).count() as f64 / v.len() as f64);
    returns
        .into_iter()
        .map(|((feature, state), [one, five])| StateOutcome {
            feature,
            state,
            count_1m: one.len(),
            mean_return_1m_pct: mean(&one),
            share_positive_1m: positive(&one),
            count_5m: five.len(),
            mean_return_5m_pct: mean(&five),
            share_positive_5m: positive(&five),
        })
        .collect()
}

/// The outcomes as a Markdown table, for a research note.
pub fn to_markdown(outcomes: &[StateOutcome]) -> String {
    let pct = |v: Option<f64>| v.map_or("—".to_string(), |x| format!("{x:+.2}%"));
    let share = |v: Option<f64>| v.map_or("—".to_string(), |x| format!("{:.0}%", x * 100.0));
    let mut out = String::from("| Feature | State | n (1m) | Mean 1m | Up 1m | n (5m) | Mean 5m | Up 5m |\n|---|---|---|---|---|---|---|---|\n");
    for o in outcomes {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
            o.feature,
            o.state,
            o.count_1m,
            pct(o.mean_return_1m_pct),
            share(o.share_positive_1m),
            o.count_5m,
            pct(o.mean_return_5m_pct),
            share(o.share_positive_5m),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(at_secs: f64, price: f64, flow: &str) -> Observation {
        Observation { at_secs, price, states: vec![("flow", flow.to_string())] }
    }

    #[test]
    fn forward_returns_are_grouped_by_the_state_at_the_time() {
        // Every "confirming" sample is followed by +1% a minute later,
        // every "diverging" one by -1%.
        let series = vec![obs(0.0, 100.0, "confirming"), obs(60.0, 101.0, "diverging"), obs(120.0, 99.99, "confirming"), obs(180.0, 100.9899, "neutral")];
        let out = outcomes_by_state(&series);
        let row = |state: &str| out.iter().find(|o| o.state == state).unwrap();
        assert_eq!(row("confirming").count_1m, 2);
        assert!((row("confirming").mean_return_1m_pct.unwrap() - 1.0).abs() < 1e-6);
        assert_eq!(row("confirming").share_positive_1m, Some(1.0));
        assert_eq!(row("diverging").count_1m, 1);
        assert!((row("diverging").mean_return_1m_pct.unwrap() + 1.0).abs() < 1e-6);
        assert_eq!(row("diverging").share_positive_1m, Some(0.0));
        // The last sample has nothing after it.
        assert_eq!(row("neutral").count_1m, 0);
        assert_eq!(row("neutral").mean_return_1m_pct, None);
    }

    #[test]
    fn a_horizon_the_series_does_not_reach_is_not_scored() {
        let series = vec![obs(0.0, 100.0, "confirming"), obs(60.0, 102.0, "confirming")];
        let out = outcomes_by_state(&series);
        assert_eq!(out[0].count_1m, 1);
        assert_eq!(out[0].count_5m, 0);
        assert_eq!(out[0].mean_return_5m_pct, None);
    }

    #[test]
    fn a_gap_far_longer_than_the_horizon_is_not_scored_as_that_horizon() {
        // Next print is 40 minutes later (a halt): not "the 1-minute return".
        let series = vec![obs(0.0, 100.0, "confirming"), obs(2_400.0, 150.0, "confirming")];
        let out = outcomes_by_state(&series);
        assert_eq!((out[0].count_1m, out[0].count_5m), (0, 0));
    }

    #[test]
    fn rows_are_sorted_and_the_markdown_table_has_one_line_per_row() {
        let series = vec![obs(0.0, 100.0, "neutral"), obs(60.0, 101.0, "confirming"), obs(120.0, 100.0, "neutral")];
        let out = outcomes_by_state(&series);
        assert_eq!(out.iter().map(|o| o.state.as_str()).collect::<Vec<_>>(), vec!["confirming", "neutral"]);
        let table = to_markdown(&out);
        assert_eq!(table.lines().count(), 2 + out.len());
        assert!(table.contains("| flow | confirming | 1 | -0.99% | 0% |"));
    }

    #[test]
    fn an_observation_from_a_summary_names_every_feature_once() {
        let mut engine = crate::engine::OrderFlowEngine::default();
        engine.on_trade(crate::tick::Trade { timestamp_secs: 1.0, price: 2.0, size: 100 }, crate::window::Session::Regular, crate::engine::TradeFlags::default());
        let o = Observation::from_summary(2.0, 2.0, &engine.summary(2.0));
        let features: Vec<&str> = o.states.iter().map(|s| s.0).collect();
        assert_eq!(features, vec!["pressure", "flow", "absorption", "large_trades", "sweeps", "exhaustion"]);
        assert_eq!(o.states[0].1, "balanced");
        assert_eq!(o.states[2].1, "none_or_low");
    }
}
