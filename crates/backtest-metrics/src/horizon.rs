//! Multi-horizon outcome measurement with explicit censoring.
//!
//! Purely additive: `outcome::evaluate_outcome` and its target/stop model are
//! untouched and still authoritative for existing metrics. This module adds
//! the richer measurements Milestone A found missing — a forward-return grid,
//! MAE (absent entirely before), excursion timing, and a censoring
//! distinction.
//!
//! # The censoring problem this fixes
//!
//! `OutcomeKind::TimedOut` currently means two different things: "we watched
//! the full window and it never resolved" and "we ran out of data". Those are
//! not the same claim, and conflating them biases every hit rate downward by
//! an unknown amount concentrated in late-session signals. Here they are
//! separate values of `Observation`, and a censored horizon yields `None`
//! rather than a number.
//!
//! # Causal sampling rule
//!
//! For horizon *h*, the sampled price is the **first observation at or after
//! `signal_time + h`**. Never the nearest, never interpolated, never the last
//! before. If no such observation exists the horizon is censored. This rule is
//! stated once here and applied uniformly, so a return at 30s and a return at
//! 30m mean structurally the same thing.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

pub const HORIZON_SCHEMA_VERSION: u32 = 1;

/// The horizon grid, in seconds. Fixed here rather than passed in, so every
/// record in the corpus is comparable without carrying its own axis.
pub const HORIZON_SECS: [i64; 7] = [30, 60, 180, 300, 600, 900, 1800];

/// Target thresholds for time-to-target measurement, in percent.
pub const TARGET_PCTS: [f64; 3] = [2.0, 5.0, 10.0];

/// The longest horizon actually configured, derived from the grid rather than
/// restated. `const fn` so the settlement deadline below is a compile-time
/// consequence of `HORIZON_SECS` -- editing the grid moves the deadline with
/// it, and there is no second constant to remember.
pub const fn longest_horizon_secs() -> i64 {
    let mut longest = HORIZON_SECS[0];
    let mut index = 1;
    while index < HORIZON_SECS.len() {
        if HORIZON_SECS[index] > longest {
            longest = HORIZON_SECS[index];
        }
        index += 1;
    }
    longest
}

/// How long observation continues *past* the longest horizon before an episode
/// settles.
///
/// The sampling rule takes the first observation at or after `signal_at + h`,
/// so a deadline equal to `h` leaves the longest horizon needing a price at
/// exactly the instant collection stops -- observable only by a race, which is
/// what F1 measured: 76 of 135,716 episodes (0.06%) at 1800s against 51.5% at
/// 900s. The margin exists to receive a real observation after the final
/// target, not to extend retention: 120s matches `MAX_GAP_SECS`, so a path
/// that is dense enough to be gap-free at all is dense enough to land a point
/// inside it.
pub const OBSERVATION_MARGIN_SECS: i64 = 120;

/// When a closed episode's outcome is settled and written, measured from the
/// episode's own `signal_at`. Derived, never independently declared -- see
/// `longest_horizon_secs`.
pub const SETTLE_AFTER_SECS: i64 = longest_horizon_secs() + OBSERVATION_MARGIN_SECS;

/// The R1 invariant, enforced at compile time: **no configured horizon may
/// equal or exceed the settlement deadline.** If someone later adds a horizon
/// at or past `SETTLE_AFTER_SECS`, this fails the build rather than silently
/// reintroducing a structurally unobservable horizon.
const _: () = {
    let mut index = 0;
    while index < HORIZON_SECS.len() {
        assert!(
            HORIZON_SECS[index] < SETTLE_AFTER_SECS,
            "every horizon must be strictly shorter than SETTLE_AFTER_SECS; \
             a horizon at the settlement boundary is unobservable (see F1)"
        );
        index += 1;
    }
};

/// One (timestamp, price) observation of the forward path.
pub type PricePoint = (DateTime<Utc>, f64);

/// Why a measurement is unavailable. Every one of these means *we do not
/// know*, and must never be aggregated as though it meant *it did not
/// happen*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CensorReason {
    /// The session ended before the horizon elapsed.
    SessionEnded,
    /// Observation stopped for our reasons — process restart, capture ended.
    CaptureEnded,
    /// The path simply does not extend far enough.
    InsufficientForwardData,
    /// A gap in the middle of the window larger than tolerated, so the
    /// extremes inside it are unknown.
    DataGap,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Observation<T> {
    Observed(T),
    Censored(CensorReason),
}

impl<T> Observation<T> {
    pub fn observed(self) -> Option<T> {
        match self {
            Self::Observed(value) => Some(value),
            Self::Censored(_) => None,
        }
    }

    pub fn is_censored(&self) -> bool {
        matches!(self, Self::Censored(_))
    }
}

/// One horizon's forward return.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HorizonReturn {
    pub horizon_secs: i64,
    pub outcome: Observation<f64>,
}

/// Excursion extremes over the measured window.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Excursion {
    /// Maximum favorable excursion, percent.
    pub mfe_pct: f64,
    /// Maximum adverse excursion, percent (negative or zero). Absent from the
    /// existing outcome model entirely; without it no risk-adjusted statement
    /// about signal quality is possible.
    pub mae_pct: f64,
    pub seconds_to_mfe: i64,
    pub seconds_to_mae: i64,
    /// Worst drawdown occurring *before* MFE was reached — the heat a position
    /// had to endure to capture the favorable move.
    pub drawdown_before_mfe_pct: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HorizonOutcome {
    pub schema_version: u32,
    pub signal_price: f64,
    pub signal_at: DateTime<Utc>,
    pub returns: Vec<HorizonReturn>,
    pub excursion: Observation<Excursion>,
    /// Seconds to first touch of each target in `TARGET_PCTS`.
    pub time_to_target: Vec<TargetTiming>,
    /// How far the observed path actually extended. Makes partial coverage
    /// visible instead of implicit.
    pub observed_span_secs: i64,
    pub observation_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetTiming {
    pub target_pct: f64,
    /// `Observed(Some(secs))` = reached. `Observed(None)` = genuinely not
    /// reached within a fully-observed window. `Censored` = unknown.
    pub outcome: Observation<Option<i64>>,
}

/// Largest tolerated gap between consecutive observations before the extremes
/// spanning it are treated as unknown.
pub const MAX_GAP_SECS: i64 = 120;

/// Evaluates a forward path.
///
/// `path` must be chronological and contain only observations at or after
/// `signal_at`. `session_end` marks the last instant the session could have
/// been observed; horizons past it are censored `SessionEnded` rather than
/// `InsufficientForwardData`, because the distinction matters for whether
/// more data collection would help.
pub fn evaluate_horizons(
    signal_price: f64,
    signal_at: DateTime<Utc>,
    path: &[PricePoint],
    session_end: Option<DateTime<Utc>>,
) -> HorizonOutcome {
    let mut usable: Vec<PricePoint> = path
        .iter()
        .copied()
        .filter(|(t, p)| *t >= signal_at && p.is_finite() && *p > 0.0)
        .collect();
    // The sampling rule ("first observation at or after the target") is only
    // correct on a chronological path. Since R2, points reach a path stamped
    // from two different clocks -- exchange time for trade- and bar-close-
    // derived prices, receipt time for in-progress buckets -- so arrival order
    // no longer implies time order. Sorting here keeps the rule sound at the
    // one place it is applied, rather than relying on every producer.
    usable.sort_by_key(|(t, _)| *t);

    let span = usable
        .last()
        .map(|(t, _)| (*t - signal_at).num_seconds())
        .unwrap_or(0);

    let returns = HORIZON_SECS
        .iter()
        .map(|&secs| HorizonReturn {
            horizon_secs: secs,
            outcome: sample_at(signal_price, signal_at, &usable, secs, span, session_end),
        })
        .collect();

    let excursion = compute_excursion(signal_price, signal_at, &usable);

    let time_to_target = TARGET_PCTS
        .iter()
        .map(|&target| TargetTiming {
            target_pct: target,
            outcome: time_to(signal_price, signal_at, &usable, target, span, session_end),
        })
        .collect();

    HorizonOutcome {
        schema_version: HORIZON_SCHEMA_VERSION,
        signal_price,
        signal_at,
        returns,
        excursion,
        time_to_target,
        observed_span_secs: span,
        observation_count: usable.len(),
    }
}

/// The causal sampling rule: first observation at or after the horizon.
fn sample_at(
    signal_price: f64,
    signal_at: DateTime<Utc>,
    path: &[PricePoint],
    horizon_secs: i64,
    span: i64,
    session_end: Option<DateTime<Utc>>,
) -> Observation<f64> {
    let target_time = signal_at + Duration::seconds(horizon_secs);
    if let Some((_, price)) = path.iter().find(|(t, _)| *t >= target_time) {
        return Observation::Observed((price - signal_price) / signal_price * 100.0);
    }
    Observation::Censored(censor_reason(target_time, span, horizon_secs, session_end))
}

fn censor_reason(
    target_time: DateTime<Utc>,
    span: i64,
    horizon_secs: i64,
    session_end: Option<DateTime<Utc>>,
) -> CensorReason {
    match session_end {
        Some(end) if target_time > end => CensorReason::SessionEnded,
        _ if span < horizon_secs => CensorReason::InsufficientForwardData,
        _ => CensorReason::CaptureEnded,
    }
}

fn compute_excursion(
    signal_price: f64,
    signal_at: DateTime<Utc>,
    path: &[PricePoint],
) -> Observation<Excursion> {
    if path.is_empty() {
        return Observation::Censored(CensorReason::InsufficientForwardData);
    }
    // A gap wider than tolerance means the extremes inside it are unknown, so
    // reporting a maximum over the visible parts would understate the true
    // excursion while looking authoritative.
    if path
        .windows(2)
        .any(|w| (w[1].0 - w[0].0).num_seconds() > MAX_GAP_SECS)
    {
        return Observation::Censored(CensorReason::DataGap);
    }

    let pct = |p: f64| (p - signal_price) / signal_price * 100.0;
    let mut mfe = f64::NEG_INFINITY;
    let mut mae = f64::INFINITY;
    let mut secs_to_mfe = 0;
    let mut secs_to_mae = 0;
    for (t, price) in path {
        let change = pct(*price);
        if change > mfe {
            mfe = change;
            secs_to_mfe = (*t - signal_at).num_seconds();
        }
        if change < mae {
            mae = change;
            secs_to_mae = (*t - signal_at).num_seconds();
        }
    }
    // Drawdown before MFE: the deepest adverse excursion strictly before the
    // favorable peak was set.
    let drawdown_before_mfe = path
        .iter()
        .take_while(|(t, _)| (*t - signal_at).num_seconds() < secs_to_mfe)
        .map(|(_, p)| pct(*p))
        .fold(0.0_f64, f64::min);

    Observation::Observed(Excursion {
        mfe_pct: if mfe.is_finite() { mfe } else { 0.0 },
        mae_pct: if mae.is_finite() { mae.min(0.0) } else { 0.0 },
        seconds_to_mfe: secs_to_mfe,
        seconds_to_mae: secs_to_mae,
        drawdown_before_mfe_pct: drawdown_before_mfe.min(0.0),
    })
}

fn time_to(
    signal_price: f64,
    signal_at: DateTime<Utc>,
    path: &[PricePoint],
    target_pct: f64,
    span: i64,
    session_end: Option<DateTime<Utc>>,
) -> Observation<Option<i64>> {
    let threshold = signal_price * (1.0 + target_pct / 100.0);
    if let Some((t, _)) = path.iter().find(|(_, p)| *p >= threshold) {
        return Observation::Observed(Some((*t - signal_at).num_seconds()));
    }
    // Not reached. Only a *fully observed* longest horizon lets us say
    // "genuinely not reached"; otherwise the answer is unknown.
    let longest = *HORIZON_SECS.last().unwrap();
    if span >= longest {
        Observation::Observed(None)
    } else {
        let target_time = signal_at + Duration::seconds(longest);
        Observation::Censored(censor_reason(target_time, span, longest, session_end))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_757_000_000 + secs, 0).unwrap()
    }

    /// A path sampled every 10s for `secs` seconds, following `f`.
    fn path(secs: i64, f: impl Fn(i64) -> f64) -> Vec<PricePoint> {
        (0..=secs).step_by(10).map(|s| (at(s), f(s))).collect()
    }

    // --- R1: settlement must outlast the horizon grid (defect F1) ---

    #[test]
    fn settlement_deadline_exceeds_every_configured_horizon() {
        // Also asserted at compile time; restated here so the intent is
        // visible to a reader of the test suite.
        for horizon in HORIZON_SECS {
            assert!(
                horizon < SETTLE_AFTER_SECS,
                "horizon {horizon} is not strictly shorter than the settlement \
                 deadline {SETTLE_AFTER_SECS}"
            );
        }
    }

    #[test]
    fn the_settlement_deadline_tracks_the_horizon_grid_automatically() {
        // The F1 bug was two constants that had to agree and silently stopped
        // agreeing. There is now exactly one source: the grid.
        assert_eq!(longest_horizon_secs(), 1800);
        assert_eq!(
            SETTLE_AFTER_SECS,
            longest_horizon_secs() + OBSERVATION_MARGIN_SECS,
            "the deadline must be derived, never independently declared"
        );
    }

    #[test]
    fn a_path_reaching_the_longest_horizon_plus_margin_observes_every_horizon() {
        let p = path(SETTLE_AFTER_SECS, |_| 100.0);
        let out = evaluate_horizons(100.0, at(0), &p, None);
        for r in &out.returns {
            assert!(
                r.outcome.observed().is_some(),
                "horizon {} must be observable once collection runs to the \
                 settlement deadline",
                r.horizon_secs
            );
        }
    }

    #[test]
    fn settling_exactly_at_the_longest_horizon_censors_it() {
        // Regression case for F1, kept deliberately: this is the old
        // behaviour. A path that stops one step *before* the longest horizon
        // cannot observe it, which is exactly what production did when the
        // settle deadline equalled the horizon.
        let p = path(longest_horizon_secs() - 10, |_| 100.0);
        let out = evaluate_horizons(100.0, at(0), &p, None);
        let longest = out
            .returns
            .iter()
            .find(|r| r.horizon_secs == longest_horizon_secs())
            .unwrap();
        assert!(
            longest.outcome.observed().is_none(),
            "a path stopping before the longest horizon must censor it"
        );
    }

    #[test]
    fn observed_coverage_is_monotone_non_increasing_in_horizon() {
        // Whatever the path, a longer horizon can never be observable when a
        // shorter one is not -- the sampling rule would have to skip backwards.
        for stop in [0, 45, 250, 700, 1500, SETTLE_AFTER_SECS] {
            let p = path(stop, |s| 100.0 + s as f64 / 100.0);
            let out = evaluate_horizons(100.0, at(0), &p, None);
            let mut previous_observed = true;
            for r in &out.returns {
                let observed = r.outcome.observed().is_some();
                assert!(
                    !(observed && !previous_observed),
                    "horizon {} observed after a shorter one was censored (stop={stop})",
                    r.horizon_secs
                );
                previous_observed = observed;
            }
        }
    }

    // --- R2: causality of the path itself ---

    #[test]
    fn out_of_order_points_are_sorted_before_sampling() {
        // Since R2 a path carries two clocks (exchange time for trade- and
        // bar-close-derived prices, receipt time for in-progress buckets), so
        // arrival order no longer implies time order.
        let ordered = vec![(at(0), 100.0), (at(30), 110.0), (at(60), 120.0)];
        let shuffled = vec![(at(60), 120.0), (at(0), 100.0), (at(30), 110.0)];
        let a = evaluate_horizons(100.0, at(0), &ordered, None);
        let b = evaluate_horizons(100.0, at(0), &shuffled, None);
        assert_eq!(
            a.returns[0].outcome.observed(),
            b.returns[0].outcome.observed(),
            "sampling must not depend on arrival order"
        );
        assert_eq!(a.returns[0].outcome.observed(), Some(10.0));
    }

    #[test]
    fn excursion_cannot_include_a_price_from_before_the_signal() {
        // A price stamped before `signal_at` is not something we could have
        // acted on; MFE/MAE must ignore it rather than report a better
        // excursion than really existed.
        let p = vec![
            (at(-60), 50.0),  // far below -- would dominate MAE if admitted
            (at(0), 100.0),
            (at(30), 101.0),
            (at(60), 102.0),
        ];
        let out = evaluate_horizons(100.0, at(0), &p, None);
        let excursion = out.excursion.observed().expect("path is gap-free");
        assert!(
            excursion.mae_pct > -50.0,
            "pre-signal price leaked into MAE: {}",
            excursion.mae_pct
        );
        assert_eq!(out.observation_count, 3, "only forward points are usable");
    }

    #[test]
    fn a_known_path_produces_exact_horizon_returns() {
        // +1% per 60s, linear on the 10s grid.
        let p = path(1800, |s| 100.0 * (1.0 + s as f64 / 6000.0));
        let out = evaluate_horizons(100.0, at(0), &p, None);

        let got = |h: i64| {
            out.returns.iter().find(|r| r.horizon_secs == h).unwrap().outcome.observed().unwrap()
        };
        assert!((got(30) - 0.5).abs() < 1e-9, "30s: {}", got(30));
        assert!((got(60) - 1.0).abs() < 1e-9, "60s: {}", got(60));
        assert!((got(300) - 5.0).abs() < 1e-9, "5m: {}", got(300));
        assert!((got(1800) - 30.0).abs() < 1e-9, "30m: {}", got(1800));
    }

    #[test]
    fn mfe_mae_and_drawdown_before_mfe_are_exact_on_a_known_path() {
        // Down to -4%, up to +12%, back to +6%.
        let mut p = vec![(at(0), 100.0)];
        p.push((at(60), 96.0));   // MAE -4%
        p.push((at(120), 104.0));
        p.push((at(180), 112.0)); // MFE +12%
        p.push((at(240), 106.0));
        let out = evaluate_horizons(100.0, at(0), &p, None);
        let ex = out.excursion.observed().expect("fully observed path");
        assert!((ex.mfe_pct - 12.0).abs() < 1e-9);
        assert!((ex.mae_pct - -4.0).abs() < 1e-9);
        assert_eq!(ex.seconds_to_mfe, 180);
        assert_eq!(ex.seconds_to_mae, 60);
        assert!(
            (ex.drawdown_before_mfe_pct - -4.0).abs() < 1e-9,
            "the -4% dip preceded the +12% peak: {}",
            ex.drawdown_before_mfe_pct
        );
    }

    #[test]
    fn time_to_target_is_the_first_touch_at_or_after_the_threshold() {
        let p = path(1800, |s| 100.0 * (1.0 + s as f64 / 6000.0));
        let out = evaluate_horizons(100.0, at(0), &p, None);
        let secs = |target: f64| {
            out.time_to_target
                .iter()
                .find(|t| t.target_pct == target)
                .unwrap()
                .outcome
                .observed()
                .unwrap()
        };
        assert_eq!(secs(2.0), Some(120));
        assert_eq!(secs(5.0), Some(300));
        assert_eq!(secs(10.0), Some(600));
    }

    #[test]
    fn an_unreached_target_on_a_complete_window_is_a_real_miss_not_censoring() {
        let p = path(1800, |_| 100.0); // flat for the full longest horizon
        let out = evaluate_horizons(100.0, at(0), &p, None);
        let t10 = out.time_to_target.iter().find(|t| t.target_pct == 10.0).unwrap();
        assert_eq!(t10.outcome, Observation::Observed(None), "fully observed and not reached");
    }

    #[test]
    fn an_incomplete_window_censors_rather_than_reporting_a_miss() {
        let p = path(120, |_| 100.0); // only two minutes of data
        let out = evaluate_horizons(100.0, at(0), &p, None);

        let five_min = out.returns.iter().find(|r| r.horizon_secs == 300).unwrap();
        assert!(five_min.outcome.is_censored(), "5m horizon is unknown, not zero");
        assert_eq!(
            five_min.outcome,
            Observation::Censored(CensorReason::InsufficientForwardData)
        );

        let t10 = out.time_to_target.iter().find(|t| t.target_pct == 10.0).unwrap();
        assert!(t10.outcome.is_censored(), "must not count as a failure to reach target");

        // The horizons that WERE observed are still reported.
        let one_min = out.returns.iter().find(|r| r.horizon_secs == 60).unwrap();
        assert!(!one_min.outcome.is_censored());
    }

    #[test]
    fn a_session_ending_is_distinguished_from_a_capture_ending() {
        let p = path(120, |_| 100.0);
        let session_end = at(150);
        let out = evaluate_horizons(100.0, at(0), &p, Some(session_end));
        let five_min = out.returns.iter().find(|r| r.horizon_secs == 300).unwrap();
        assert_eq!(
            five_min.outcome,
            Observation::Censored(CensorReason::SessionEnded),
            "more collection would not help; the session was over"
        );
    }

    #[test]
    fn a_mid_window_gap_censors_the_excursion() {
        let p = vec![(at(0), 100.0), (at(30), 101.0), (at(30 + MAX_GAP_SECS + 10), 104.0)];
        let out = evaluate_horizons(100.0, at(0), &p, None);
        assert_eq!(
            out.excursion,
            Observation::Censored(CensorReason::DataGap),
            "extremes inside an unobserved gap are unknown"
        );
    }

    #[test]
    fn observations_before_the_signal_are_ignored() {
        let mut p = vec![(at(-300), 50.0), (at(-60), 60.0)];
        p.extend(path(300, |_| 100.0));
        let out = evaluate_horizons(100.0, at(0), &p, None);
        let ex = out.excursion.observed().unwrap();
        assert!((ex.mae_pct - 0.0).abs() < 1e-9, "pre-signal prices must not become MAE");
        assert_eq!(out.observation_count, path(300, |_| 100.0).len());
    }

    #[test]
    fn an_empty_forward_path_censors_everything() {
        let out = evaluate_horizons(100.0, at(0), &[], None);
        assert!(out.excursion.is_censored());
        assert!(out.returns.iter().all(|r| r.outcome.is_censored()));
        assert_eq!(out.observed_span_secs, 0);
    }

    #[test]
    fn a_horizon_outcome_round_trips_through_json() {
        let p = path(600, |s| 100.0 + s as f64 * 0.01);
        let out = evaluate_horizons(100.0, at(0), &p, None);
        let json = serde_json::to_string(&out).unwrap();
        let back: HorizonOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(back, out);
    }

    #[test]
    fn future_prices_beyond_a_horizon_do_not_change_that_horizons_return() {
        let base = path(300, |s| 100.0 + s as f64 * 0.01);
        let mut extended = base.clone();
        extended.extend((310..=1800).step_by(10).map(|s| (at(s), 500.0)));

        let a = evaluate_horizons(100.0, at(0), &base, None);
        let b = evaluate_horizons(100.0, at(0), &extended, None);
        let pick = |o: &HorizonOutcome, h: i64| {
            o.returns.iter().find(|r| r.horizon_secs == h).unwrap().outcome
        };
        assert_eq!(pick(&a, 30), pick(&b, 30));
        assert_eq!(pick(&a, 60), pick(&b, 60));
        assert_eq!(pick(&a, 300), pick(&b, 300));
    }
}
