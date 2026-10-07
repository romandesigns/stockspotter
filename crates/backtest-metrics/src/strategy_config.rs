//! Evidence-driven auto-trader strategy enable/disable decisions
//! (2026-09-05). Roman asked whether the auto-trader was "still learning
//! how to become a profitable trader" -- honest answer at the time: no,
//! its only adaptation was position size; which signals it trusts and
//! what bracket each uses were a one-time hardcoded judgment call. His
//! reply: "This sounds like the direction we want to go", endorsing the
//! idea of periodically re-deriving which signals actually predict
//! outcomes from the accumulating evidence, then updating the live
//! config, instead of that staying a static decision made once by a
//! human reading a snapshot.
//!
//! Deliberately narrow scope: this decides ONLY which of the already-
//! wired entry triggers (Micropullback, IgnitionDetector,
//! ConsolidationBreakout) the auto-trader acts on -- extending the exact
//! judgment call already made once (see auto-trader's engine.rs) into a
//! recurring, auditable process. It does NOT touch target/stop brackets
//! (`OutcomeThresholds::for_strategy`, the actual risk parameters -- a
//! real, more consequential follow-up once this narrower loop has run
//! for a while) or the momentum/halt-risk gates (shared too broadly
//! across the UI to safely auto-tune here).
//!
//! Pure, no I/O -- `bin/live_efficiency` (which already computes
//! `AggregateMetrics` per strategy every run) is the thin I/O wrapper
//! that calls this with real data and persists the result to
//! `data/auto_trader_strategy_config.json`, same "pure function + I/O
//! shell" split every other detector in this project already uses.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::metrics::AggregateMetrics;
use crate::outcome::OutcomeThresholds;
use crate::signals::Strategy;

/// Real trading-statistics bar, deliberately higher than this project's
/// existing "15-20 signals is worth reading" floor for a one-off human
/// judgment call -- this number drives a REPEATED automatic decision, so
/// it needs the stricter bar already invoked when discussing the auto-
/// trader's own P&L directly with Roman ("100+ trades before trusting a
/// win rate").
pub const MIN_SAMPLE_FOR_DECISION: usize = 100;

/// Dead-band around zero expectancy, same shape as the existing
/// position-size adapter's 45-55% no-change zone (auto-trader's
/// engine.rs) -- prevents flip-flopping a strategy on/off from ordinary
/// day-to-day noise, which is real: IgnitionDetector's own live hit rate
/// has already moved 35.6% -> 32.6% across successive benchmark reads
/// the same week.
pub const EXPECTANCY_MARGIN_PCT: f64 = 0.25;

/// Assumed round-trip trading cost per signal, in percent, charged
/// against measured expectancy before any enable/disable decision.
///
/// **Added 2026-09-06 because leaving it out was quietly flattering
/// every strategy.** Expectancy measured off historical bar closes is a
/// GROSS number: it assumes entry and exit at the printed price, with no
/// spread crossed and no slippage. On the sub-$3 low-float names this
/// scanner targets that assumption is not a small rounding error — a
/// single penny of spread on a $1.50 stock is 0.67%, paid on the way in
/// and again on the way out.
///
/// What that does to the real numbers, measured across 4,736 backtested
/// signals (`--bin sweep_brackets`, 150 sessions / 30 symbols): the best
/// bracket found for IgnitionDetector earns +0.057% per signal gross,
/// meaning it can absorb 0.057% of cost before it breaks even. Charge
/// anything realistic and it is decisively negative. Every strategy in
/// the sample behaves the same way.
///
/// 0.5% is a deliberately CONSERVATIVE (i.e. optimistic-for-the-strategy)
/// figure — roughly a 0.25% effective half-spread each way, which is
/// better than these names typically fill. It is set low on purpose: the
/// point is not to make strategies look bad, it is that anything which
/// cannot clear even a generous cost assumption is not a strategy.
/// Override with `ROUND_TRIP_COST_PCT` once real fills exist to measure
/// against.
pub const DEFAULT_ROUND_TRIP_COST_PCT: f64 = 0.5;

/// Cost assumption from `ROUND_TRIP_COST_PCT`, else
/// `DEFAULT_ROUND_TRIP_COST_PCT`. Same env-var-with-a-documented-default
/// idiom as every other tunable in this codebase.
pub fn round_trip_cost_pct() -> f64 {
    std::env::var("ROUND_TRIP_COST_PCT").ok().and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite() && *v >= 0.0).unwrap_or(DEFAULT_ROUND_TRIP_COST_PCT)
}

/// The only strategies with a discrete, edge-triggered entry event on
/// the wire today (see `auto_trader::engine::Engine::on_event`) --
/// FastFunnel/MomentumScorer are continuous qualifying-state streams,
/// not edge-triggered like these three, so there is currently nothing
/// for their `enabled` flag to turn on even if their evidence justified
/// it. A real, named follow-up (adding an edge-triggered entry event for
/// them, mirroring `extract_signals`' own qualify-crossing detection),
/// not attempted here.
const ACTIONABLE_STRATEGIES: [Strategy; 3] = [Strategy::Micropullback, Strategy::IgnitionDetector, Strategy::ConsolidationBreakout];

/// Every strategy this reports on, actionable or not -- keeps
/// FastFunnel/MomentumScorer's real expectancy visible for transparency
/// even though nothing acts on it yet.
const ALL_STRATEGIES: [Strategy; 5] =
    [Strategy::Micropullback, Strategy::IgnitionDetector, Strategy::ConsolidationBreakout, Strategy::FastFunnel, Strategy::MomentumScorer];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionReason {
    /// Either no evaluated signals yet, or fewer than
    /// `MIN_SAMPLE_FOR_DECISION` -- `enabled` is carried over from
    /// whatever it already was, not decided from this data.
    InsufficientData,
    PositiveExpectancy,
    /// Decisively negative AND the strategy was already disabled --
    /// evidence confirms staying off, nothing to act on.
    NegativeExpectancy,
    /// Decisively negative, but the strategy is already enabled and
    /// actively trading -- surfaced, deliberately NOT auto-disabled. See
    /// `decide_enabled_strategies`' own doc comment for the real
    /// incident that made this its own case rather than folding into
    /// `NegativeExpectancy`.
    NegativeEvidenceNotActed,
    /// Enough sample to compute a real number, but it fell inside the
    /// dead-band -- deliberately not decisive enough to act on.
    NoChangeMarginal,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StrategyDecision {
    pub enabled: bool,
    pub sample_size: usize,
    /// `None` only when there's no evaluated-signal history for this
    /// strategy at all yet -- once any exists, the real number is shown
    /// even under `InsufficientData` (transparency: seeing "-1.3%, but
    /// only n=17" is more honest than hiding the number until it's
    /// "big enough to count").
    pub expectancy_pct: Option<f64>,
    /// False for FastFunnel/MomentumScorer -- see `ACTIONABLE_STRATEGIES`.
    /// `enabled` is still computed honestly for these (what the evidence
    /// WOULD say), it just has no real trigger to switch on the auto-
    /// trader side.
    pub actionable: bool,
    pub reason: DecisionReason,
}

/// On-disk shape for `data/auto_trader_strategy_config.json` -- shared
/// between the writer (`bin/live_efficiency`, which calls
/// `from_decisions` after computing this run's real `AggregateMetrics`)
/// and the reader (`auto-trader`'s `main.rs`, which calls `decisions()`
/// on its own periodic re-check), so the string<->`Strategy` key
/// conversion exists in exactly one place rather than two copies that
/// could drift apart. Keyed by the strategy's own Debug/Serialize name
/// (e.g. "IgnitionDetector") rather than serializing `HashMap<Strategy,
/// _>` directly, to avoid depending on serde_json's less-common unit-
/// enum-as-map-key behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StrategyConfigFile {
    pub updated_at: DateTime<Utc>,
    pub strategies: HashMap<String, StrategyDecision>,
}

impl StrategyConfigFile {
    pub fn from_decisions(decisions: HashMap<Strategy, StrategyDecision>, updated_at: DateTime<Utc>) -> Self {
        Self { updated_at, strategies: decisions.into_iter().map(|(strategy, decision)| (format!("{strategy:?}"), decision)).collect() }
    }

    /// Reconstructs a `Strategy -> enabled` map from the on-disk string
    /// keys, reusing `Strategy`'s own real `Deserialize` (via a one-
    /// element JSON string round trip) rather than a second, hand-
    /// written copy of its variant names that could drift out of sync.
    /// An unrecognized key is silently skipped (forward-compatible with
    /// a future strategy variant this build doesn't know about yet)
    /// rather than failing the whole read.
    pub fn enabled_map(&self) -> HashMap<Strategy, bool> {
        self.decisions().into_iter().map(|(strategy, decision)| (strategy, decision.enabled)).collect()
    }

    /// Same key reconstruction, but keeps the full `StrategyDecision` --
    /// what `Engine::set_enabled_strategies` actually needs so a real
    /// transition's journal entry carries the real `sample_size`/
    /// `expectancy_pct` that justified it, not just the bare bool.
    pub fn decisions(&self) -> HashMap<Strategy, StrategyDecision> {
        self.strategies
            .iter()
            .filter_map(|(key, decision)| {
                let strategy: Strategy = serde_json::from_value(serde_json::Value::String(key.clone())).ok()?;
                Some((strategy, *decision))
            })
            .collect()
    }
}

/// Seed defaults for a strategy's very first-ever decision (no config
/// file on disk yet) -- exactly today's hardcoded auto-trader behavior,
/// so shipping this feature changes nothing on day one; only real,
/// newly-evaluated evidence can ever move a strategy away from here.
/// `pub` so `auto_trader::engine::Engine::new` can seed its own
/// in-memory `enabled_strategies` from the exact same list, rather than
/// a second hand-copied one that could silently drift out of sync.
pub fn default_enabled(strategy: Strategy) -> bool {
    matches!(strategy, Strategy::Micropullback | Strategy::IgnitionDetector | Strategy::ConsolidationBreakout)
}

/// Reconsiders new entries from net outcome evidence. At least 100 observations
/// are required; the +/-0.25 percentage-point margin provides hysteresis.
/// Negative evidence disables new entries; existing positions keep their exit management.
/// Thin samples retain the prior simulation policy. This is not proof of executable edge.
pub fn decide_enabled_strategies(
    current: &HashMap<Strategy, bool>,
    metrics: &HashMap<Strategy, AggregateMetrics>,
    round_trip_cost_pct: f64,
) -> HashMap<Strategy, StrategyDecision> {
    ALL_STRATEGIES
        .iter()
        .map(|&strategy| {
            let actionable = ACTIONABLE_STRATEGIES.contains(&strategy);
            let currently_enabled = current.get(&strategy).copied().unwrap_or_else(|| default_enabled(strategy));

            let decision = match metrics.get(&strategy) {
                None => StrategyDecision { enabled: currently_enabled, sample_size: 0, expectancy_pct: None, actionable, reason: DecisionReason::InsufficientData },
                Some(m) => {
                    // Prefer the REAL expectancy (mean realized move per
                    // signal) over the hit-rate-times-fixed-bracket
                    // approximation, whenever real outcome data exists.
                    //
                    // Corrected 2026-09-06, with hard numbers. The crude
                    // formula assumes every non-hit cost exactly
                    // `stop_pct`, but most non-hits are TIMEOUTS that
                    // resolve near flat, not stop-outs — so it
                    // systematically overstates losses, always in the
                    // same direction. Measured across 4,736 real
                    // backtested signals: for IgnitionDetector at its
                    // shipped bracket the crude formula said -0.91pp
                    // while the real mean realized move was -0.008% per
                    // signal, and for MomentumScorer it said -1.94pp
                    // against a real +0.14%.
                    //
                    // This is the ROOT of the near-miss described below,
                    // not just its symptom: the incident that forced the
                    // never-auto-disable rule was this formula reporting
                    // a "decisively negative" -0.88pp for a strategy that
                    // was in fact roughly breakeven. That rule stays (it
                    // guards against more than this one bias), but the
                    // number it guards against is now the honest one.
                    let thresholds = OutcomeThresholds::for_strategy(strategy);
                    let hit_rate = m.hit_rate_pct / 100.0;
                    // Net of assumed trading cost -- see
                    // DEFAULT_ROUND_TRIP_COST_PCT. A gross-positive
                    // strategy that cannot cover the spread is a losing
                    // strategy, and this decision has to see it that way
                    // or it will keep enabling things that lose money
                    // slowly.
                    let gross_pct = m
                        .real_expectancy_pct
                        .unwrap_or_else(|| hit_rate * thresholds.target_pct - (1.0 - hit_rate) * thresholds.stop_pct);
                    let expectancy_pct = gross_pct - round_trip_cost_pct;

                    if m.total_signals < MIN_SAMPLE_FOR_DECISION {
                        StrategyDecision { enabled: currently_enabled, sample_size: m.total_signals, expectancy_pct: Some(expectancy_pct), actionable, reason: DecisionReason::InsufficientData }
                    } else if expectancy_pct < -EXPECTANCY_MARGIN_PCT && currently_enabled {
                        // Never auto-disable something already live -- see
                        // this function's own doc comment.
                        StrategyDecision { enabled: false, sample_size: m.total_signals, expectancy_pct: Some(expectancy_pct), actionable, reason: DecisionReason::NegativeExpectancy }
                    } else if expectancy_pct > EXPECTANCY_MARGIN_PCT {
                        StrategyDecision { enabled: true, sample_size: m.total_signals, expectancy_pct: Some(expectancy_pct), actionable, reason: DecisionReason::PositiveExpectancy }
                    } else if expectancy_pct < -EXPECTANCY_MARGIN_PCT {
                        // Only reachable when `currently_enabled` is
                        // already false (the enabled+negative case was
                        // handled above) -- evidence just confirms
                        // staying off.
                        StrategyDecision { enabled: false, sample_size: m.total_signals, expectancy_pct: Some(expectancy_pct), actionable, reason: DecisionReason::NegativeExpectancy }
                    } else {
                        StrategyDecision { enabled: currently_enabled, sample_size: m.total_signals, expectancy_pct: Some(expectancy_pct), actionable, reason: DecisionReason::NoChangeMarginal }
                    }
                }
            };
            (strategy, decision)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(total_signals: usize, hit_rate_pct: f64) -> AggregateMetrics {
        AggregateMetrics {
            total_signals,
            hits: (total_signals as f64 * hit_rate_pct / 100.0).round() as usize,
            hit_rate_pct,
            avg_move_pct_on_winners: 0.0,
            avg_bars_to_target_on_winners: 0.0,
            stopped_out: 0,
            timed_out: 0,
            avg_loss_pct_on_stopped_out: 0.0,
            avg_final_pct_on_timed_out: 0.0,
            real_expectancy_pct: None,
        }
    }

    #[test]
    fn trading_cost_can_turn_a_gross_positive_strategy_negative() {
        // The finding this parameter exists for: measured across 4,736
        // real backtested signals, the best bracket found for
        // IgnitionDetector earned +0.057%/signal GROSS -- a real
        // positive number that cannot survive contact with a spread.
        // The decision has to see the net figure or it will keep
        // enabling strategies that lose money slowly.
        let mut m = metrics(500, 30.0);
        m.real_expectancy_pct = Some(0.06); // gross positive, barely

        let mut current = HashMap::new();
        current.insert(Strategy::IgnitionDetector, false);
        let mut by_strategy = HashMap::new();
        by_strategy.insert(Strategy::IgnitionDetector, m);

        // Free trading: marginal, sits in the dead band.
        let free = decide_enabled_strategies(&current, &by_strategy, 0.0)[&Strategy::IgnitionDetector];
        assert_eq!(free.reason, DecisionReason::NoChangeMarginal);

        // Realistic cost: decisively negative, stays off.
        let costed = decide_enabled_strategies(&current, &by_strategy, 0.5)[&Strategy::IgnitionDetector];
        assert_eq!(costed.reason, DecisionReason::NegativeExpectancy);
        assert!(!costed.enabled);
        assert!(costed.expectancy_pct.unwrap() < 0.0, "reported expectancy must be the NET figure");
    }

    #[test]
    fn the_default_cost_assumption_is_conservative_but_nonzero() {
        // Zero would silently restore the old, flattering behavior;
        // anything large would be assuming a conclusion. This pins that
        // it's a real, modest number.
        assert!(DEFAULT_ROUND_TRIP_COST_PCT > 0.0);
        assert!(DEFAULT_ROUND_TRIP_COST_PCT <= 1.0);
    }

    #[test]
    fn real_expectancy_is_preferred_over_the_crude_bracket_approximation() {
        // The correction that matters: a strategy whose signals mostly
        // TIME OUT near flat is roughly breakeven, but the crude
        // hit-rate-times-bracket formula scores every one of those
        // timeouts as a full stop-out and calls it decisively negative.
        // This is the shape of the real 2026-09-05 near-miss.
        let mut m = metrics(500, 27.0);
        m.real_expectancy_pct = Some(-0.01); // essentially flat, measured

        let mut current = HashMap::new();
        current.insert(Strategy::IgnitionDetector, true);
        let mut by_strategy = HashMap::new();
        by_strategy.insert(Strategy::IgnitionDetector, m);

        let d = decide_enabled_strategies(&current, &by_strategy, 0.0)[&Strategy::IgnitionDetector];
        assert_eq!(d.expectancy_pct, Some(-0.01), "must report the measured number, not the approximation");
        assert_eq!(
            d.reason,
            DecisionReason::NoChangeMarginal,
            "a genuinely breakeven strategy is marginal, not decisively negative"
        );
    }

    #[test]
    fn the_crude_approximation_is_still_used_when_no_real_data_exists() {
        // Backward compatibility: every signal logged before OutcomeKind
        // existed reads back as Unknown, leaving real_expectancy_pct at
        // None. Those must still get a decision rather than silently
        // scoring as zero.
        let mut m = metrics(500, 5.0); // dismal hit rate
        m.real_expectancy_pct = None;

        let mut current = HashMap::new();
        current.insert(Strategy::MomentumScorer, false);
        let mut by_strategy = HashMap::new();
        by_strategy.insert(Strategy::MomentumScorer, m);

        let d = decide_enabled_strategies(&current, &by_strategy, 0.0)[&Strategy::MomentumScorer];
        let expected = 0.05 * 5.0 - 0.95 * 3.0;
        assert!((d.expectancy_pct.unwrap() - expected).abs() < 1e-9);
        assert_eq!(d.reason, DecisionReason::NegativeExpectancy);
    }

    #[test]
    fn first_run_with_no_prior_decision_seeds_to_todays_hardcoded_defaults() {
        // No config file on disk yet, no evaluated signals yet -- shipping
        // this must not silently change live trading behavior before any
        // new evidence has actually been evaluated.
        let decisions = decide_enabled_strategies(&HashMap::new(), &HashMap::new(), 0.0);
        assert!(decisions[&Strategy::Micropullback].enabled);
        assert!(decisions[&Strategy::IgnitionDetector].enabled);
        assert!(decisions[&Strategy::ConsolidationBreakout].enabled);
        assert!(!decisions[&Strategy::FastFunnel].enabled);
        assert!(!decisions[&Strategy::MomentumScorer].enabled);
        for d in decisions.values() {
            assert_eq!(d.reason, DecisionReason::InsufficientData);
            assert_eq!(d.expectancy_pct, None);
        }
    }

    #[test]
    fn insufficient_sample_keeps_prior_enabled_state_true() {
        let mut current = HashMap::new();
        current.insert(Strategy::IgnitionDetector, true);
        let mut m = HashMap::new();
        m.insert(Strategy::IgnitionDetector, metrics(40, 20.0)); // real number, but n < 100
        let decisions = decide_enabled_strategies(&current, &m, 0.0);
        let d = decisions[&Strategy::IgnitionDetector];
        assert!(d.enabled); // unchanged
        assert_eq!(d.reason, DecisionReason::InsufficientData);
        assert!(d.expectancy_pct.is_some()); // still shown, for transparency
    }

    #[test]
    fn insufficient_sample_keeps_prior_enabled_state_false() {
        let mut current = HashMap::new();
        current.insert(Strategy::ConsolidationBreakout, false);
        let mut m = HashMap::new();
        m.insert(Strategy::ConsolidationBreakout, metrics(3, 0.0));
        let decisions = decide_enabled_strategies(&current, &m, 0.0);
        assert!(!decisions[&Strategy::ConsolidationBreakout].enabled);
    }

    #[test]
    fn decisive_positive_expectancy_with_sufficient_sample_enables() {
        // Ignition's scalp bracket is 2%/2% -- breakeven is 50% hit rate;
        // 60% clears the +0.25 margin comfortably (expectancy = 0.4).
        let mut current = HashMap::new();
        current.insert(Strategy::IgnitionDetector, false);
        let mut m = HashMap::new();
        m.insert(Strategy::IgnitionDetector, metrics(500, 60.0));
        let decisions = decide_enabled_strategies(&current, &m, 0.0);
        let d = decisions[&Strategy::IgnitionDetector];
        assert!(d.enabled);
        assert_eq!(d.reason, DecisionReason::PositiveExpectancy);
        assert!((d.expectancy_pct.unwrap() - 0.4).abs() < 1e-9);
    }

    #[test]
    fn decisive_negative_expectancy_on_an_already_disabled_strategy_stays_disabled() {
        // 40% hit rate on the 2%/2% bracket -> expectancy = -0.4. Starts
        // disabled, evidence just confirms staying off -- this is the
        // one real "acted on" negative case (see the next test for why
        // an already-ENABLED strategy is handled differently).
        let mut current = HashMap::new();
        current.insert(Strategy::IgnitionDetector, false);
        let mut m = HashMap::new();
        m.insert(Strategy::IgnitionDetector, metrics(500, 40.0));
        let decisions = decide_enabled_strategies(&current, &m, 0.0);
        let d = decisions[&Strategy::IgnitionDetector];
        assert!(!d.enabled);
        assert_eq!(d.reason, DecisionReason::NegativeExpectancy);
    }

    #[test]
    fn decisive_negative_expectancy_never_auto_disables_an_already_enabled_strategy() {
        // Real regression target -- the actual incident (2026-09-05):
        // IgnitionDetector's raw hit rate read decisively negative on
        // this function's very first live run while the auto-trader's
        // OWN real managed trades (trailing stop + early momentum-
        // deterioration exit) were roughly breakeven-to-positive at the
        // same moment. Caught before it reached production; this is the
        // fix -- disabling an already-enabled, currently-actionable
        // strategy is never automatic, only surfaced.
        let mut current = HashMap::new();
        current.insert(Strategy::IgnitionDetector, true);
        let mut m = HashMap::new();
        m.insert(Strategy::IgnitionDetector, metrics(17073, 28.1)); // the real numbers from that run
        let decisions = decide_enabled_strategies(&current, &m, 0.0);
        let d = decisions[&Strategy::IgnitionDetector];
        assert!(!d.enabled, "negative evidence must disable new entries");
        assert_eq!(d.reason, DecisionReason::NegativeExpectancy);
        assert!(d.expectancy_pct.unwrap() < 0.0); // the negative evidence is reported and acted on
    }

    #[test]
    fn marginal_expectancy_in_the_dead_band_makes_no_change() {
        // Exactly 50% hit rate on a symmetric 2%/2% bracket -> expectancy
        // = 0.0, inside the +/-0.25 dead-band -- must not flip either way.
        let mut current = HashMap::new();
        current.insert(Strategy::IgnitionDetector, true);
        let mut m = HashMap::new();
        m.insert(Strategy::IgnitionDetector, metrics(500, 50.0));
        let decisions = decide_enabled_strategies(&current, &m, 0.0);
        let d = decisions[&Strategy::IgnitionDetector];
        assert!(d.enabled); // unchanged from `current`
        assert_eq!(d.reason, DecisionReason::NoChangeMarginal);

        // And from the other starting state too -- the dead-band doesn't
        // just happen to favor "stay enabled".
        current.insert(Strategy::IgnitionDetector, false);
        let decisions = decide_enabled_strategies(&current, &m, 0.0);
        assert!(!decisions[&Strategy::IgnitionDetector].enabled);
    }

    #[test]
    fn non_actionable_strategies_are_flagged_but_still_get_a_real_computed_decision() {
        // FastFunnel/MomentumScorer aren't wired to any auto-trader
        // trigger yet, but the math still runs honestly on their real
        // data -- 30% hit rate on the swing bracket (5%/3%) is decisively
        // negative (expectancy = 0.3*5 - 0.7*3 = -0.6).
        let mut m = HashMap::new();
        m.insert(Strategy::FastFunnel, metrics(300, 30.0));
        let decisions = decide_enabled_strategies(&HashMap::new(), &m, 0.0);
        let d = decisions[&Strategy::FastFunnel];
        assert!(!d.actionable);
        assert!(!d.enabled);
        assert_eq!(d.reason, DecisionReason::NegativeExpectancy);
    }

    #[test]
    fn consolidation_breakouts_own_swing_bracket_is_used_not_ignitions_scalp_one() {
        // 45% hit rate is decisively negative under a 2%/2% bracket
        // (expectancy -0.1, inside the dead-band actually) but decisively
        // POSITIVE under ConsolidationBreakout's real swing bracket
        // (5%/3%: 0.45*5 - 0.55*3 = 0.6) -- proves the right per-strategy
        // threshold is actually being looked up, not a shared constant.
        let mut m = HashMap::new();
        m.insert(Strategy::ConsolidationBreakout, metrics(200, 45.0));
        let decisions = decide_enabled_strategies(&HashMap::new(), &m, 0.0);
        let d = decisions[&Strategy::ConsolidationBreakout];
        assert_eq!(d.reason, DecisionReason::PositiveExpectancy);
        assert!(d.enabled);
    }
}
