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
    let usable: Vec<PricePoint> = path
        .iter()
        .copied()
        .filter(|(t, p)| *t >= signal_at && p.is_finite() && *p > 0.0)
        .collect();

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
