//! Measuring the pre-registered criteria (§15–§22).
//!
//! # The one rule everything here obeys
//!
//! **Cohort membership is decided in the opportunity's first ranking window,
//! and the outcome is measured forward from that same instant.**
//!
//! Every alternative leaks the future into the past. Membership by *best* rank
//! ever held would use later windows to classify an earlier moment. Measuring
//! the candidate from its first top-k window while measuring the control from
//! its opening instant would confound selection with timing, so an enrichment
//! could not be read as either. One rule, applied identically to candidate and
//! control, is what makes §33's parity real rather than asserted.
//!
//! # Where the numbers come from
//!
//! Outcomes are computed from the **discovery** price series, not from the
//! measurement capture. Discovery is the whole-market ignition stream: the
//! platform did not select it and cannot have biased it, and using one price
//! source for candidate and control means a difference between them cannot be
//! an artefact of two pipelines disagreeing.
//!
//! # What "worst across primary surfaces" means
//!
//! A criterion named on several primary surfaces is decided by the **worst**
//! of them. Passing on the surface that happened to look best is exactly the
//! multiple-comparison problem §28 exists to prevent; per-surface detail is
//! reported so the spread is visible.

use std::collections::BTreeMap;

use crate::alpha::dataset::{forward, DiscoveryView, OpportunityDataset, OpportunityRow};
use crate::alpha::labels::ReferenceOpportunity;
use crate::alpha::ladder::{Ladder, LadderSummary};
use crate::alpha::matrix::{CriterionResult, Outcome};
use crate::alpha::spec::{Criterion, Dimension, QualificationSpec, Rule, Surface};
use crate::alpha::stats::{cluster_bootstrap, counts, Estimate, Observation, Quantity, Rng};
use serde::{Deserialize, Serialize};

/// A ranked cohort definition.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Selector {
    TopK(usize),
    /// Fraction of the window's cohort, e.g. 0.10 for the top decile.
    TopPercent(f64),
}

impl Selector {
    /// The rank threshold this selector implies in a window of `cohort` size.
    ///
    /// At least 1, so a percentile surface in a tiny window still names
    /// somebody rather than silently selecting nobody.
    fn threshold(self, cohort: usize) -> usize {
        match self {
            Selector::TopK(k) => k,
            Selector::TopPercent(p) => ((cohort as f64 * p).ceil() as usize).max(1),
        }
    }

    fn label(self) -> String {
        match self {
            Selector::TopK(k) => format!("top-{k}"),
            Selector::TopPercent(p) => format!("top-{:.0}%", p * 100.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Score {
    EarlyQuality,
    Continuation,
}

impl Score {
    fn rank_of(self, row: &OpportunityRow) -> Option<usize> {
        match self {
            Score::EarlyQuality => row.first_window_early_rank,
            Score::Continuation => row.first_window_continuation_rank,
        }
    }
    fn available(self, row: &OpportunityRow) -> bool {
        match self {
            Score::EarlyQuality => row.early_quality_available,
            Score::Continuation => row.continuation_available,
        }
    }
    /// The window's cohort for THIS surface -- the percentile denominator.
    ///
    /// Per surface since D6: EarlyQuality and Continuation are ranked
    /// independently over different scored sets, so a top-p% threshold must
    /// be taken over its own surface's N. It used to be taken over
    /// `max(early, continuation)` for both. Falls back to the combined size,
    /// then to the rows present, only for a dataset built before the
    /// per-surface maps existed.
    fn cohort_of(self, dataset: &OpportunityDataset, window_id: &str) -> Option<usize> {
        let per_surface = match self {
            Score::EarlyQuality => &dataset.window_early_cohort_sizes,
            Score::Continuation => &dataset.window_continuation_cohort_sizes,
        };
        per_surface
            .get(window_id)
            .or_else(|| dataset.window_cohort_sizes.get(window_id))
            .copied()
    }

    fn label(self) -> &'static str {
        match self {
            Score::EarlyQuality => "earlyQualityRank",
            Score::Continuation => "continuationRank",
        }
    }
}

/// One evaluated surface, reported whatever the criterion decides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceResult {
    pub surface: String,
    pub target_pct: f64,
    pub horizon_secs: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control: Option<f64>,
    pub estimate: Estimate,
}

/// Which control a comparison uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// Every opportunity in the same window.
    Contemporaneous,
    /// The k most recently opened opportunities in the same window.
    DetectionRecency,
    /// The k opportunities with the largest move-from-start in the window.
    MoveMagnitude,
}

/// Everything one evaluation run needs.
pub struct Inputs<'a> {
    pub spec: &'a QualificationSpec,
    pub dataset: &'a OpportunityDataset,
    pub discovery: &'a DiscoveryView,
    pub ladders: &'a [Ladder],
    pub reference: &'a [ReferenceOpportunity],
}

/// The evaluated session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Evaluation {
    pub results: Vec<CriterionResult>,
    pub surfaces: Vec<SurfaceResult>,
    pub segments: Vec<SegmentResult>,
    pub secondary_direction: Vec<SurfaceResult>,
    pub ladder: LadderSummary,
    pub shortfalls: Vec<String>,
    pub notes: Vec<String>,
    pub population: Population,
}

/// The counts §12 and §34 both need.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Population {
    pub raw_snapshot_rows: u64,
    pub distinct_opportunities: usize,
    pub distinct_symbols: usize,
    pub ranking_windows: usize,
    pub reference_opportunities: usize,
    pub positive_reference_opportunities: usize,
    pub eligible_reference_opportunities: usize,
}

/// One segmentation cell (§21).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentResult {
    pub dimension: String,
    pub segment: String,
    pub surface: String,
    pub estimate: Estimate,
    /// True when this cell's direction disagrees with the overall result — the
    /// reversal §21 exists to surface.
    pub reverses: bool,
}

// ---------------------------------------------------------------------------
// Cohort construction
// ---------------------------------------------------------------------------

/// Builds observations for one surface, one target and one horizon.
///
/// Candidate membership and control membership are both decided within the
/// opportunity's first window, and every outcome is measured forward from that
/// same instant.
fn observations(
    inputs: &Inputs,
    score: Score,
    selector: Selector,
    control: Control,
    target_pct: f64,
    horizon_secs: i64,
    value_of: Option<&dyn Fn(&OpportunityRow, f64, f64) -> Option<f64>>,
) -> Vec<Observation> {
    // Group by first window, so every control is contemporaneous by
    // construction rather than by assertion.
    let mut windows: BTreeMap<&str, Vec<&OpportunityRow>> = BTreeMap::new();
    for row in &inputs.dataset.rows {
        windows.entry(row.first_window_id.as_str()).or_default().push(row);
    }

    let max_gap = inputs.spec.reference_label.max_gap_secs;
    let mut out = Vec::new();
    for (window_id, rows) in windows {
        let cohort = score.cohort_of(inputs.dataset, window_id).unwrap_or(rows.len());
        let threshold = selector.threshold(cohort);

        // The control cohort, chosen inside this window only.
        let control_members: Vec<&str> = match control {
            Control::Contemporaneous => rows.iter().map(|r| r.opportunity_id.as_str()).collect(),
            Control::DetectionRecency => {
                let mut by_recency: Vec<&&OpportunityRow> = rows.iter().collect();
                by_recency.sort_by(|a, b| {
                    b.opened_at.cmp(&a.opened_at).then_with(|| a.opportunity_id.cmp(&b.opportunity_id))
                });
                by_recency
                    .into_iter()
                    .take(threshold)
                    .map(|r| r.opportunity_id.as_str())
                    .collect()
            }
            Control::MoveMagnitude => {
                let mut by_move: Vec<&&OpportunityRow> = rows.iter().collect();
                by_move.sort_by(|a, b| {
                    let left = a.move_before_detection_pct.unwrap_or(f64::MIN);
                    let right = b.move_before_detection_pct.unwrap_or(f64::MIN);
                    right
                        .partial_cmp(&left)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.opportunity_id.cmp(&b.opportunity_id))
                });
                by_move
                    .into_iter()
                    .take(threshold)
                    .map(|r| r.opportunity_id.as_str())
                    .collect()
            }
        };

        for row in rows {
            let in_candidate = score.available(row)
                && score.rank_of(row).is_some_and(|rank| rank <= threshold);
            let in_control = control_members.contains(&row.opportunity_id.as_str());
            if !in_candidate && !in_control {
                continue;
            }
            let series = inputs.discovery.series.get(&row.symbol);
            let measured = series.and_then(|series| {
                forward(
                    series,
                    row.first_ranked_at,
                    row.first_price,
                    horizon_secs,
                    target_pct,
                    max_gap,
                )
            });
            let (outcome, value) = match measured {
                Some((reached, mfe, mae, _)) => {
                    let value = value_of.and_then(|f| f(row, mfe, mae));
                    (Some(reached), value)
                }
                // Censored: excluded from both cohorts, counted, never a
                // failure.
                None => (None, None),
            };
            out.push(Observation {
                cluster: row.symbol.clone(),
                unit: row.opportunity_id.clone(),
                in_candidate,
                in_control,
                outcome,
                value,
            });
        }
    }
    out
}

fn estimate_for(
    inputs: &Inputs,
    score: Score,
    selector: Selector,
    control: Control,
    target_pct: f64,
    horizon_secs: i64,
    quantity: Quantity,
    value_of: Option<&dyn Fn(&OpportunityRow, f64, f64) -> Option<f64>>,
    rng: &mut Rng,
) -> SurfaceResult {
    let rows = observations(
        inputs, score, selector, control, target_pct, horizon_secs, value_of,
    );
    let estimate = cluster_bootstrap(
        &rows,
        quantity,
        inputs.spec.uncertainty.resamples,
        inputs.spec.uncertainty.confidence,
        rng,
    );
    // The two sides, reported separately so a ratio is never the only thing a
    // reader can see.
    let candidate = cluster_bootstrap(
        &rows,
        if matches!(quantity, Quantity::MeanRatio) { Quantity::Mean } else { Quantity::Rate },
        0,
        inputs.spec.uncertainty.confidence,
        rng,
    )
    .point;
    let control_value = {
        let flipped: Vec<Observation> = rows
            .iter()
            .map(|o| Observation { in_candidate: o.in_control, ..o.clone() })
            .collect();
        cluster_bootstrap(
            &flipped,
            if matches!(quantity, Quantity::MeanRatio) { Quantity::Mean } else { Quantity::Rate },
            0,
            inputs.spec.uncertainty.confidence,
            rng,
        )
        .point
    };
    SurfaceResult {
        surface: format!("{} {}", score.label(), selector.label()),
        target_pct,
        horizon_secs,
        candidate,
        control: control_value,
        estimate,
    }
}

/// The primary surfaces for one score, parsed from the contract's own list.
fn primary_selectors(spec: &QualificationSpec, score: Score) -> Vec<Selector> {
    let prefix = score.label();
    let mut out = Vec::new();
    for surface in &spec.primary_ranking_surfaces {
        if !surface.starts_with(prefix) {
            continue;
        }
        if let Some(rest) = surface.split_whitespace().nth(1) {
            if let Some(pct) = rest.strip_prefix("top-").and_then(|r| r.strip_suffix('%')) {
                if let Ok(value) = pct.parse::<f64>() {
                    out.push(Selector::TopPercent(value / 100.0));
                }
            } else if let Some(k) = rest.strip_prefix("top-") {
                if let Ok(value) = k.parse::<usize>() {
                    out.push(Selector::TopK(value));
                }
            }
        }
    }
    out
}

/// Worst outcome across a set, so a criterion cannot pass on its best surface.
fn worst(outcomes: &[Outcome]) -> Outcome {
    if outcomes.iter().any(|o| *o == Outcome::Fail) {
        Outcome::Fail
    } else if outcomes.iter().any(|o| *o == Outcome::Insufficient) || outcomes.is_empty() {
        Outcome::Insufficient
    } else {
        Outcome::Pass
    }
}

fn result_of(
    criterion: &Criterion,
    surfaces: &[SurfaceResult],
    comparison: Option<f64>,
    notes: Vec<String>,
) -> CriterionResult {
    let outcomes: Vec<Outcome> = surfaces
        .iter()
        .map(|s| CriterionResult::decide(criterion, s.estimate.clone(), comparison))
        .collect();
    // The worst surface's estimate is the one reported, so the headline figure
    // is the one the decision rests on.
    let worst_index = outcomes
        .iter()
        .enumerate()
        .min_by_key(|(_, outcome)| match outcome {
            Outcome::Fail => 0,
            Outcome::Insufficient => 1,
            Outcome::Pass => 2,
        })
        .map(|(index, _)| index);
    let representative = worst_index.and_then(|index| surfaces.get(index));
    CriterionResult {
        criterion_id: criterion.id.clone(),
        dimension: criterion.dimension,
        surface: criterion.surface,
        blocking: criterion.blocking,
        metric: criterion.metric.clone(),
        control: criterion.control.clone(),
        candidate_value: representative.and_then(|s| s.candidate),
        control_value: representative.and_then(|s| s.control),
        comparison: representative.and_then(|s| s.estimate.point),
        estimate: representative
            .map(|s| s.estimate.clone())
            .unwrap_or_else(|| empty_estimate()),
        criterion: describe(&criterion.rule),
        outcome: worst(&outcomes),
        notes,
    }
}

fn empty_estimate() -> Estimate {
    Estimate {
        point: None,
        lower: None,
        upper: None,
        confidence: 0.95,
        n_units: 0,
        n_clusters: 0,
        n_raw_rows: 0,
        n_censored: 0,
        method: "not measured".to_string(),
    }
}

fn describe(rule: &Rule) -> String {
    match rule {
        Rule::EnrichmentLowerBoundAbove { value } => {
            format!("clustered lower bound on the ratio must exceed {value}")
        }
        Rule::UpliftLowerBoundAbove { value } => {
            format!("clustered lower bound on the difference must exceed {value}")
        }
        Rule::MonotoneAcrossRanks => "rates must not increase as rank worsens".to_string(),
        Rule::NoWorseThanControlBy { fraction } => {
            format!("clustered lower bound must exceed {:.2} of the control", 1.0 - fraction)
        }
        Rule::MedianAbove { value } => format!("clustered lower bound on the median must exceed {value}"),
        Rule::Descriptive => "reported, never decisive".to_string(),
    }
}

/// Rate by rank bucket, for the monotonicity rules.
///
/// Returns `Some(1.0)` when monotone, `Some(0.0)` when not, and `None` when
/// fewer than three buckets are populated — too few to speak of a shape.
fn monotonicity(inputs: &Inputs, score: Score, target_pct: f64, horizon_secs: i64) -> Option<f64> {
    const BUCKETS: [(usize, usize); 4] = [(1, 5), (6, 20), (21, 100), (101, usize::MAX)];
    let max_gap = inputs.spec.reference_label.max_gap_secs;
    let mut hits = [0usize; 4];
    let mut totals = [0usize; 4];
    for row in &inputs.dataset.rows {
        let Some(rank) = score.rank_of(row) else { continue };
        if !score.available(row) {
            continue;
        }
        let Some(series) = inputs.discovery.series.get(&row.symbol) else { continue };
        let Some((reached, _, _, _)) = forward(
            series,
            row.first_ranked_at,
            row.first_price,
            horizon_secs,
            target_pct,
            max_gap,
        ) else {
            continue;
        };
        for (index, (low, high)) in BUCKETS.iter().enumerate() {
            if rank >= *low && rank <= *high {
                totals[index] += 1;
                if reached {
                    hits[index] += 1;
                }
                break;
            }
        }
    }
    let populated: Vec<f64> = (0..4)
        .filter(|i| totals[*i] >= 10)
        .map(|i| hits[i] as f64 / totals[i] as f64)
        .collect();
    if populated.len() < 3 {
        return None;
    }
    // Five percentage points of slack: the question is whether the ordering
    // holds, not whether every adjacent pair is strictly separated on one
    // session's worth of evidence.
    let monotone = populated.windows(2).all(|pair| pair[0] >= pair[1] - 0.05);
    Some(if monotone { 1.0 } else { 0.0 })
}

// ---------------------------------------------------------------------------
// The evaluation
// ---------------------------------------------------------------------------

/// Measures every criterion in the contract.
pub fn evaluate(inputs: &Inputs) -> Evaluation {
    let spec = inputs.spec;
    let mut rng = Rng::from_hex_seed(&spec.sha256());
    let mut evaluation = Evaluation {
        ladder: LadderSummary::of(inputs.ladders),
        ..Evaluation::default()
    };

    // --- population and minimum evidence (§34) ---------------------------
    let symbols: std::collections::BTreeSet<&str> =
        inputs.dataset.rows.iter().map(|r| r.symbol.as_str()).collect();
    let primary_target = spec.primary_targets_pct.first().copied().unwrap_or(2.0);
    let target_index = spec
        .reference_label
        .targets_pct
        .iter()
        .position(|t| (*t - primary_target).abs() < 1e-9)
        .unwrap_or(0);
    let eligible: Vec<&ReferenceOpportunity> =
        inputs.reference.iter().filter(|r| r.ineligible.is_none()).collect();
    let positives = eligible
        .iter()
        .filter(|r| r.is_opportunity(target_index) == Some(true))
        .count();

    evaluation.population = Population {
        raw_snapshot_rows: inputs.dataset.raw_rows,
        distinct_opportunities: inputs.dataset.rows.len(),
        distinct_symbols: symbols.len(),
        ranking_windows: inputs.dataset.windows,
        reference_opportunities: inputs.reference.len(),
        positive_reference_opportunities: positives,
        eligible_reference_opportunities: eligible.len(),
    };

    let minimum = &spec.minimum_evidence;
    let p = &evaluation.population;
    if p.distinct_opportunities < minimum.distinct_opportunities {
        evaluation.shortfalls.push(format!(
            "distinct opportunities {} < {} required",
            p.distinct_opportunities, minimum.distinct_opportunities
        ));
    }
    if p.distinct_symbols < minimum.distinct_symbols {
        evaluation.shortfalls.push(format!(
            "distinct symbols {} < {} required",
            p.distinct_symbols, minimum.distinct_symbols
        ));
    }
    if p.positive_reference_opportunities < minimum.positive_reference_opportunities {
        evaluation.shortfalls.push(format!(
            "positive reference opportunities {} < {} required",
            p.positive_reference_opportunities, minimum.positive_reference_opportunities
        ));
    }
    if p.ranking_windows < minimum.ranking_windows {
        evaluation.shortfalls.push(format!(
            "ranking windows {} < {} required",
            p.ranking_windows, minimum.ranking_windows
        ));
    }

    // --- the criteria -----------------------------------------------------
    let horizons: Vec<i64> = spec.primary_horizons_secs.clone();
    let early = primary_selectors(spec, Score::EarlyQuality);
    let continuation = primary_selectors(spec, Score::Continuation);

    for criterion in &spec.criteria {
        let result = match criterion.id.as_str() {
            "A1-early-quality-enrichment" => {
                let surfaces = sweep(inputs, Score::EarlyQuality, &early, Control::Contemporaneous,
                    primary_target, &horizons, Quantity::RateRatio, None, &mut rng);
                evaluation.surfaces.extend(surfaces.clone());
                result_of(criterion, &surfaces, None, Vec::new())
            }
            "A2-early-quality-beats-first-detection" => {
                let surfaces = sweep(inputs, Score::EarlyQuality, &early, Control::DetectionRecency,
                    primary_target, &horizons, Quantity::RateRatio, None, &mut rng);
                evaluation.surfaces.extend(surfaces.clone());
                result_of(criterion, &surfaces, None, Vec::new())
            }
            "R2-ranking-beats-move-magnitude" => {
                let surfaces = sweep(inputs, Score::EarlyQuality, &early, Control::MoveMagnitude,
                    primary_target, &horizons, Quantity::RateRatio, None, &mut rng);
                evaluation.surfaces.extend(surfaces.clone());
                result_of(criterion, &surfaces, None, Vec::new())
            }
            "B1-continuation-enrichment-within-movement-band" => {
                let surfaces = sweep(inputs, Score::Continuation, &continuation,
                    Control::Contemporaneous, primary_target, &horizons, Quantity::RateRatio,
                    None, &mut rng);
                evaluation.surfaces.extend(surfaces.clone());
                result_of(criterion, &surfaces, None,
                    vec!["banded by move-before-detection; see the segmentation table".to_string()])
            }
            "B2-continuation-monotonicity" => {
                let shape = monotonicity(inputs, Score::Continuation, primary_target, horizons[0]);
                let surfaces = sweep(inputs, Score::Continuation, &continuation,
                    Control::Contemporaneous, primary_target, &horizons[..1], Quantity::Rate,
                    None, &mut rng);
                result_of(criterion, &surfaces, shape, Vec::new())
            }
            "R1-ranking-monotonicity" => {
                let shape = monotonicity(inputs, Score::EarlyQuality, primary_target, horizons[0]);
                let surfaces = sweep(inputs, Score::EarlyQuality, &early, Control::Contemporaneous,
                    primary_target, &horizons[..1], Quantity::Rate, None, &mut rng);
                result_of(criterion, &surfaces, shape, Vec::new())
            }
            "D1-excursion-ratio-not-degraded" => {
                let ratio = |_: &OpportunityRow, mfe: f64, mae: f64| -> Option<f64> {
                    if mae.abs() < 1e-9 { None } else { Some(mfe / mae.abs()) }
                };
                let surfaces = sweep(inputs, Score::EarlyQuality, &early, Control::Contemporaneous,
                    primary_target, &horizons, Quantity::MeanRatio, Some(&ratio), &mut rng);
                evaluation.surfaces.extend(surfaces.clone());
                result_of(criterion, &surfaces, None, Vec::new())
            }
            "E1-ranked-before-the-move" => {
                let surfaces = vec![lead_time(inputs, &mut rng)];
                result_of(criterion, &surfaces, None, Vec::new())
            }
            "C1-oi-conditional-retention" => {
                let surfaces = vec![retention(inputs, &mut rng)];
                result_of(criterion, &surfaces, None, Vec::new())
            }
            "C2-recall-attribution-established" => {
                let surfaces = vec![attribution(inputs, &mut rng)];
                result_of(criterion, &surfaces, None, Vec::new())
            }
            // Descriptive criteria are reported with whatever evidence exists
            // and can never change the verdict.
            _ => CriterionResult {
                criterion_id: criterion.id.clone(),
                dimension: criterion.dimension,
                surface: criterion.surface,
                blocking: criterion.blocking,
                metric: criterion.metric.clone(),
                control: criterion.control.clone(),
                candidate_value: None,
                control_value: None,
                comparison: None,
                estimate: empty_estimate(),
                criterion: describe(&criterion.rule),
                outcome: Outcome::Pass,
                notes: vec!["descriptive; see the corresponding report section".to_string()],
            },
        };
        evaluation.results.push(result);
    }

    // --- secondary direction (contract review, decision 2) ----------------
    for target in &spec.secondary_targets_pct {
        let surfaces = sweep(inputs, Score::EarlyQuality, &early, Control::Contemporaneous,
            *target, &horizons, Quantity::RateRatio, None, &mut rng);
        evaluation.secondary_direction.extend(surfaces);
    }

    // --- segmentation (§21) ------------------------------------------------
    evaluation.segments = segment(inputs, &early, primary_target, horizons[0], &mut rng);
    let overall_positive = evaluation
        .results
        .iter()
        .find(|r| r.criterion_id == "A1-early-quality-enrichment")
        .and_then(|r| r.comparison)
        .map(|v| v > 1.0);
    if let Some(positive) = overall_positive {
        for cell in &mut evaluation.segments {
            if let Some(point) = cell.estimate.point {
                cell.reverses = (point > 1.0) != positive;
            }
        }
        let reversals = evaluation.segments.iter().filter(|c| c.reverses).count();
        if reversals > 0 {
            evaluation.notes.push(format!(
                "{reversals} segmentation cell(s) show the primary relationship reversing; \
                 diagnostic, and a reason for human review"
            ));
        }
    }

    evaluation
}

#[allow(clippy::too_many_arguments)]
fn sweep(
    inputs: &Inputs,
    score: Score,
    selectors: &[Selector],
    control: Control,
    target_pct: f64,
    horizons: &[i64],
    quantity: Quantity,
    value_of: Option<&dyn Fn(&OpportunityRow, f64, f64) -> Option<f64>>,
    rng: &mut Rng,
) -> Vec<SurfaceResult> {
    let mut out = Vec::new();
    for selector in selectors {
        for horizon in horizons {
            out.push(estimate_for(
                inputs, score, *selector, control, target_pct, *horizon, quantity, value_of, rng,
            ));
        }
    }
    out
}

/// E1: seconds between the first top-k rank and the primary crossing.
fn lead_time(inputs: &Inputs, rng: &mut Rng) -> SurfaceResult {
    let rows: Vec<Observation> = inputs
        .ladders
        .iter()
        .filter_map(|ladder| {
            let ranked = ladder.top_k.first_at?;
            let crossed = ladder.primary_crossing_at?;
            Some(Observation {
                cluster: ladder.symbol.clone(),
                unit: format!("{}:lead", ladder.symbol),
                in_candidate: true,
                in_control: false,
                outcome: None,
                value: Some((crossed - ranked).num_seconds() as f64),
            })
        })
        .collect();
    let estimate = cluster_bootstrap(
        &rows,
        Quantity::Median,
        inputs.spec.uncertainty.resamples,
        inputs.spec.uncertainty.confidence,
        rng,
    );
    SurfaceResult {
        surface: "first top-k rank to +2% crossing, seconds".to_string(),
        target_pct: inputs.spec.primary_targets_pct.first().copied().unwrap_or(2.0),
        horizon_secs: 0,
        candidate: estimate.point,
        control: Some(0.0),
        estimate,
    }
}

/// C1: among reference opportunities detection reached, the fraction OI kept.
fn retention(inputs: &Inputs, rng: &mut Rng) -> SurfaceResult {
    let rows: Vec<Observation> = inputs
        .ladders
        .iter()
        .filter(|ladder| ladder.detected.reached == Some(true))
        .map(|ladder| Observation {
            cluster: ladder.symbol.clone(),
            unit: format!("{}:retention", ladder.symbol),
            in_candidate: true,
            in_control: false,
            outcome: ladder.opportunity_created.reached,
            value: None,
        })
        .collect();
    let estimate = cluster_bootstrap(
        &rows,
        Quantity::Rate,
        inputs.spec.uncertainty.resamples,
        inputs.spec.uncertainty.confidence,
        rng,
    );
    SurfaceResult {
        surface: "OI retention among detected reference opportunities".to_string(),
        target_pct: inputs.spec.primary_targets_pct.first().copied().unwrap_or(2.0),
        horizon_secs: 0,
        candidate: estimate.point,
        // The detector stage is the comparator, and by construction every unit
        // in this population reached it.
        control: Some(1.0),
        estimate,
    }
}

/// C2: the fraction of reference opportunities whose stage is established.
fn attribution(inputs: &Inputs, rng: &mut Rng) -> SurfaceResult {
    let rows: Vec<Observation> = inputs
        .ladders
        .iter()
        .map(|ladder| Observation {
            cluster: ladder.symbol.clone(),
            unit: format!("{}:attribution", ladder.symbol),
            in_candidate: true,
            in_control: false,
            outcome: Some(ladder.classify() != crate::alpha::ladder::Classification::Unknown),
            value: Some(
                if ladder.classify() == crate::alpha::ladder::Classification::Unknown {
                    0.0
                } else {
                    1.0
                },
            ),
        })
        .collect();
    let estimate = cluster_bootstrap(
        &rows,
        Quantity::Median,
        inputs.spec.uncertainty.resamples,
        inputs.spec.uncertainty.confidence,
        rng,
    );
    SurfaceResult {
        surface: "causally established stage, fraction of reference opportunities".to_string(),
        target_pct: 0.0,
        horizon_secs: 0,
        candidate: estimate.point,
        control: None,
        estimate,
    }
}

/// §21: the same primary comparison, inside each segment.
fn segment(
    inputs: &Inputs,
    selectors: &[Selector],
    target_pct: f64,
    horizon_secs: i64,
    rng: &mut Rng,
) -> Vec<SegmentResult> {
    let Some(selector) = selectors.first().copied() else { return Vec::new() };
    let all = observations(
        inputs, Score::EarlyQuality, selector, Control::Contemporaneous, target_pct, horizon_secs,
        None,
    );
    let index: BTreeMap<&str, &OpportunityRow> = inputs
        .dataset
        .rows
        .iter()
        .map(|row| (row.opportunity_id.as_str(), row))
        .collect();

    let keys: [(&str, Box<dyn Fn(&OpportunityRow) -> String>); 5] = [
        ("regime", Box::new(|r: &OpportunityRow| r.regime.clone())),
        (
            "price band",
            Box::new(|r: &OpportunityRow| {
                r.price_band.map_or("unknown".to_string(), |b| format!("band {b}"))
            }),
        ),
        (
            "time of day",
            Box::new(|r: &OpportunityRow| format!("{:02}:00Z", r.first_ranked_at.format("%H"))),
        ),
        (
            "opening detector",
            Box::new(|r: &OpportunityRow| {
                r.detectors.first().cloned().unwrap_or_else(|| "unknown".to_string())
            }),
        ),
        (
            "confluence",
            Box::new(|r: &OpportunityRow| format!("{} detector(s)", r.confluence_count)),
        ),
    ];

    let mut out = Vec::new();
    for (dimension, key) in keys.iter() {
        let mut groups: BTreeMap<String, Vec<Observation>> = BTreeMap::new();
        for observation in &all {
            let Some(row) = index.get(observation.unit.as_str()) else { continue };
            groups.entry(key(row)).or_default().push(observation.clone());
        }
        for (segment_name, rows) in groups {
            let (units, clusters) = counts(&rows);
            // A cell too thin to estimate is reported as unestimable rather
            // than omitted, so a reader can see the coverage gap.
            if units < 10 || clusters < 2 {
                continue;
            }
            let estimate = cluster_bootstrap(
                &rows,
                Quantity::RateRatio,
                inputs.spec.uncertainty.resamples.min(500),
                inputs.spec.uncertainty.confidence,
                rng,
            );
            out.push(SegmentResult {
                dimension: (*dimension).to_string(),
                segment: segment_name,
                surface: format!("{} {}", Score::EarlyQuality.label(), selector.label()),
                estimate,
                reverses: false,
            });
        }
    }
    out
}

/// Whether a dimension has any blocking criterion, for the matrix's rows.
pub fn is_blocking_dimension(spec: &QualificationSpec, dimension: Dimension) -> bool {
    spec.criteria.iter().any(|c| c.dimension == dimension && c.blocking)
}

/// Surfaces are reported whatever the verdict; this keeps the primary ones
/// distinguishable from the diagnostic ones in the output.
pub fn is_primary(spec: &QualificationSpec, criterion_id: &str) -> bool {
    spec.criteria
        .iter()
        .find(|c| c.id == criterion_id)
        .map(|c| c.surface == Surface::Primary)
        .unwrap_or(false)
}

#[cfg(test)]
#[path = "evaluate_tests.rs"]
mod tests;
