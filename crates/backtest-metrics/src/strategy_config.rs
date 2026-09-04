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

/// The real judgment call, made recurring instead of static. `current`
/// is whatever's already on disk from the last decision (or, for a
/// strategy never decided before, `default_enabled`'s seed) --
/// insufficient data always falls back to it, never to "disabled by
/// default", so a strategy mid-evidence-gathering (Micropullback and
/// ConsolidationBreakout both sit well under 10 real signals as of this
/// writing) keeps trading and keeps accumulating real data instead of
/// being starved by its own thin sample the moment this ships.
///
/// **Auto-DISABLING an already-enabled strategy is deliberately never
/// automatic**, even with a decisive, sufficient-sample negative
/// expectancy -- only auto-ENABLING a currently-off one is. Real
/// incident that forced this asymmetry (2026-09-05, caught before it
/// took effect, not after): on this function's very first live run,
/// IgnitionDetector -- the auto-trader's main real trigger, actively
/// producing a small positive P&L in practice -- computed a decisively
/// NEGATIVE expectancy (-0.88pp) from `AggregateMetrics`' raw hit rate
/// (28.1%) against its naive 2%/2% bracket. That raw number is real, but
/// it measures the wrong thing for this decision: it judges the
/// UNMANAGED signal in isolation, while the auto-trader's actual trades
/// also get a trailing stop and an early momentum-deterioration exit
/// that measurably shrink real losses below the bracket's flat -2%
/// (confirmed the same day from the journal: momentum-deterioration
/// exits averaged -0.59%, not -2%) -- so the real managed strategy was
/// roughly breakeven-to-positive (34 trades, 16W/18L, +$37) at the exact
/// moment this metric said "decisively bad". Auto-disabling on this
/// signal would have silently regressed an already-shipped, working
/// strategy -- exactly what this project's own standing rule forbids
/// ("never ship a change that weakens or removes an already-tested,
/// currently-shipped strategy"). The fix: enabling stays fully
/// automatic (safe -- worst case, a new strategy gets a fair paper-
/// trading trial); disabling something already proven in practice now
/// only ever gets surfaced (`NegativeEvidenceNotActed`, real numbers
/// still shown) for a human to act on, never flipped by this function
/// itself.
pub fn decide_enabled_strategies(current: &HashMap<Strategy, bool>, metrics: &HashMap<Strategy, AggregateMetrics>) -> HashMap<Strategy, StrategyDecision> {
    ALL_STRATEGIES
        .iter()
        .map(|&strategy| {
            let actionable = ACTIONABLE_STRATEGIES.contains(&strategy);
            let currently_enabled = current.get(&strategy).copied().unwrap_or_else(|| default_enabled(strategy));

            let decision = match metrics.get(&strategy) {
                None => StrategyDecision { enabled: currently_enabled, sample_size: 0, expectancy_pct: None, actionable, reason: DecisionReason::InsufficientData },
                Some(m) => {
                    let thresholds = OutcomeThresholds::for_strategy(strategy);
                    let hit_rate = m.hit_rate_pct / 100.0;
                    let expectancy_pct = hit_rate * thresholds.target_pct - (1.0 - hit_rate) * thresholds.stop_pct;

                    if m.total_signals < MIN_SAMPLE_FOR_DECISION {
                        StrategyDecision { enabled: currently_enabled, sample_size: m.total_signals, expectancy_pct: Some(expectancy_pct), actionable, reason: DecisionReason::InsufficientData }
                    } else if expectancy_pct < -EXPECTANCY_MARGIN_PCT && currently_enabled {
                        // Never auto-disable something already live -- see
                        // this function's own doc comment.
                        StrategyDecision { enabled: true, sample_size: m.total_signals, expectancy_pct: Some(expectancy_pct), actionable, reason: DecisionReason::NegativeEvidenceNotActed }
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
        AggregateMetrics { total_signals, hits: (total_signals as f64 * hit_rate_pct / 100.0).round() as usize, hit_rate_pct, avg_move_pct_on_winners: 0.0, avg_bars_to_target_on_winners: 0.0 }
    }

    #[test]
    fn first_run_with_no_prior_decision_seeds_to_todays_hardcoded_defaults() {
        // No config file on disk yet, no evaluated signals yet -- shipping
        // this must not silently change live trading behavior before any
        // new evidence has actually been evaluated.
        let decisions = decide_enabled_strategies(&HashMap::new(), &HashMap::new());
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
        let decisions = decide_enabled_strategies(&current, &m);
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
        let decisions = decide_enabled_strategies(&current, &m);
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
        let decisions = decide_enabled_strategies(&current, &m);
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
        let decisions = decide_enabled_strategies(&current, &m);
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
        let decisions = decide_enabled_strategies(&current, &m);
        let d = decisions[&Strategy::IgnitionDetector];
        assert!(d.enabled, "must NOT auto-disable an already-shipped, currently-enabled strategy");
        assert_eq!(d.reason, DecisionReason::NegativeEvidenceNotActed);
        assert!(d.expectancy_pct.unwrap() < 0.0); // the real negative number is still shown, just not acted on
    }

    #[test]
    fn marginal_expectancy_in_the_dead_band_makes_no_change() {
        // Exactly 50% hit rate on a symmetric 2%/2% bracket -> expectancy
        // = 0.0, inside the +/-0.25 dead-band -- must not flip either way.
        let mut current = HashMap::new();
        current.insert(Strategy::IgnitionDetector, true);
        let mut m = HashMap::new();
        m.insert(Strategy::IgnitionDetector, metrics(500, 50.0));
        let decisions = decide_enabled_strategies(&current, &m);
        let d = decisions[&Strategy::IgnitionDetector];
        assert!(d.enabled); // unchanged from `current`
        assert_eq!(d.reason, DecisionReason::NoChangeMarginal);

        // And from the other starting state too -- the dead-band doesn't
        // just happen to favor "stay enabled".
        current.insert(Strategy::IgnitionDetector, false);
        let decisions = decide_enabled_strategies(&current, &m);
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
        let decisions = decide_enabled_strategies(&HashMap::new(), &m);
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
        let decisions = decide_enabled_strategies(&HashMap::new(), &m);
        let d = decisions[&Strategy::ConsolidationBreakout];
        assert_eq!(d.reason, DecisionReason::PositiveExpectancy);
        assert!(d.enabled);
    }
}
