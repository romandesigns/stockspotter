//! Offline Step-4 analysis: common-pool selection, the +2% / 300 s outcome
//! evaluator, censoring, the session statistic and exact inference.
//!
//! **Offline and evidence-driven only.** Nothing here fetches anything. The
//! outcome *evaluator* takes normalised, already-authenticated trade and status
//! evidence; the *fetcher* that would produce real trade evidence is separate
//! and separately gated, so network or API behaviour can never shape outcome
//! semantics. Every function is deterministic in its inputs.
//!
//! The contracts implemented here are the Step 4A proposals as accepted for
//! implementation (clauses 7-9); every numeric choice that is still open is a
//! parameter, not a constant, so the freeze decides it.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use chrono::{DateTime, Duration, TimeZone, Utc};

use super::*;

// ===========================================================================
// Price representation
// ===========================================================================

/// Prices as integer **micro-dollars** (1e-6 USD).
///
/// US equity prices carry at most 4 decimal places on the consolidated tape
/// (sub-dollar) and 2 above a dollar; micro-dollars represent every such price
/// exactly, and rounding an `f64` feed price to the nearest micro recovers the
/// decimal it was parsed from. The target test is then exact integer
/// arithmetic in i128, with no float comparison anywhere.
pub type Micros = i64;

pub fn to_micros(price: f64) -> Option<Micros> {
    if !price.is_finite() || price <= 0.0 || price > 1.0e9 {
        return None;
    }
    Some((price * 1_000_000.0).round() as i64)
}

/// `trade >= anchor * (1 + target_bp / 10_000)`, exactly.
pub fn reaches_target(trade: Micros, anchor: Micros, target_bp: i64) -> bool {
    i128::from(trade) * 10_000 >= i128::from(anchor) * i128::from(10_000 + target_bp)
}

/// Primary target: +2%.
pub const PRIMARY_TARGET_BP: i64 = 200;
/// Primary horizon.
pub const HORIZON_SECS: i64 = 300;

// ===========================================================================
// Primary scope: the whole horizon inside the regular session
// ===========================================================================

/// True when `(t0, t0 + 300 s]` lies entirely inside `t0`'s regular session,
/// using the project's NYSE calendar (holidays: no session; early closes:
/// that day's close). The horizon's inclusive end must be strictly before
/// the close, so a closing-auction print at the bell is never inside it.
pub fn in_primary_scope(t0: DateTime<Utc>) -> bool {
    // The market day (04:00 America/New_York boundary) equals the New York
    // date for every instant from 04:00 on; an earlier instant maps to the
    // previous day, whose session has closed, so it falls out of scope.
    let day = market_data::trading_session::market_day(t0);
    let (Some(open), Some(close)) = (
        market_data::trading_session::regular_session_open(day),
        market_data::trading_session::regular_session_close(day),
    ) else {
        return false;
    };
    t0 >= open && t0 + Duration::seconds(HORIZON_SECS) < close
}

// ===========================================================================
// Session extract: bounded, from a certified run
// ===========================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct CandidateExtract {
    pub opportunity_id: String,
    pub symbol: String,
    pub opened_at: DateTime<Utc>,
    pub confirmation_sequence: Option<u64>,
    /// Clause-3 anchor price (the provenance price).
    pub anchor_price: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct WindowExtract {
    pub window_id: String,
    pub anchor_at: DateTime<Utc>,
    /// Complete and not invalidated (lag, ambiguity, truncation).
    pub valid: bool,
    /// Eligible candidates only; others are counted, not kept.
    pub eligible: Vec<CandidateExtract>,
    pub open_count: u64,
}

/// Halt knowledge for one session.
#[derive(Debug, Clone, Default)]
pub struct StatusEvidence {
    /// The run's status evidence is loss-free and attached: the tap reported
    /// its totals and dropped nothing, and the run certified.
    pub loss_free: bool,
    /// Intervals during which full-market status delivery was live.
    pub coverage: Vec<(DateTime<Utc>, DateTime<Utc>)>,
    /// Per symbol, halt intervals `[start, end)`; `end` `None` = never resumed
    /// within the run.
    pub halts: BTreeMap<String, Vec<(DateTime<Utc>, Option<DateTime<Utc>>)>>,
}

impl StatusEvidence {
    /// Halt knowledge is complete over `[from, to]`: loss-free, and one
    /// full-market coverage interval spans it. Absence of a halt record is
    /// evidence of no halt **only** under this condition.
    pub fn complete_over(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> bool {
        self.loss_free && self.coverage.iter().any(|(a, b)| *a <= from && to <= *b)
    }

    /// The earliest halt start overlapping `(from, to]`, if any.
    pub fn first_halt_overlapping(&self, symbol: &str, from: DateTime<Utc>, to: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.halts
            .get(symbol)?
            .iter()
            .filter(|(s, e)| *s <= to && e.map_or(true, |e| e > from))
            .map(|(s, _)| *s)
            .min()
    }
}

/// The halted-status predicate, identical to the ignition monitor's
/// (`ignition_detector::monitor::is_halted`): status code `H`.
pub fn is_halt_code(code: &str) -> bool {
    code == "H"
}

#[derive(Debug, Clone)]
pub struct SessionExtract {
    pub run_id: String,
    pub first_window_id: Option<String>,
    /// Every lifecycle open in the run's first window (left-censored).
    pub first_window_open: HashSet<String>,
    pub first_receipt_market_at: Option<DateTime<Utc>>,
    /// Every lifecycle seen that meets the left-censoring rule, whatever its
    /// eligibility: recorded for diagnostics, never pooled.
    pub left_censored_seen: BTreeSet<String>,
    pub windows: Vec<WindowExtract>,
    pub status: StatusEvidence,
    /// The streaming certificate the extract was read under (`None` only for
    /// hand-built extracts in tests).
    pub certificate: Option<Certificate>,
}

/// Reads a run, which must certify PASS by the streaming certifier, into the
/// bounded extract the analysis needs.
pub fn extract_session(run_dir: &Path) -> Result<SessionExtract, String> {
    let (verdict, _) = stream::assess_streaming(run_dir);
    let certificate = match verdict {
        CaptureVerdict::Pass(c) => *c,
        other => return Err(format!("run does not certify: {}", other.label())),
    };
    let mut first_window_id = None;
    let mut first_window_open = HashSet::new();
    let mut first_receipt_market_at = None;
    let mut left_censored_seen = BTreeSet::new();
    let mut windows = Vec::new();
    let mut pending: Option<WindowExtract> = None;
    let mut coverage = Vec::new();
    let mut open_coverage: Option<DateTime<Utc>> = None;
    let mut halts: BTreeMap<String, Vec<(DateTime<Utc>, Option<DateTime<Utc>>)>> = BTreeMap::new();
    let mut run_end_at = None;
    stream::for_each_record(run_dir, |record| match record {
        ObservationRecord::Receipt { market_at: Some(m), .. } if first_receipt_market_at.is_none() => {
            first_receipt_market_at = Some(m);
        }
        ObservationRecord::WindowBegin { window_id, .. } => {
            pending = Some(WindowExtract {
                window_id,
                anchor_at: DateTime::<Utc>::MIN_UTC,
                valid: false,
                eligible: Vec::new(),
                open_count: 0,
            });
        }
        ObservationRecord::Candidate {
            window_id, opportunity_id, symbol, opened_at, provenance, confirmation_sequence, eligibility, ..
        } => {
            if first_window_id.is_none() || first_window_id.as_deref() == Some(window_id.as_str()) {
                first_window_id = Some(window_id.clone());
                first_window_open.insert(opportunity_id.clone());
                left_censored_seen.insert(opportunity_id.clone());
            }
            if first_receipt_market_at.map_or(true, |t| opened_at < t) {
                left_censored_seen.insert(opportunity_id.clone());
            }
            if let Some(w) = pending.as_mut() {
                w.open_count += 1;
                if eligibility.eligible {
                    w.eligible.push(CandidateExtract {
                        opportunity_id,
                        symbol,
                        opened_at,
                        confirmation_sequence,
                        anchor_price: provenance.map(|p| p.price),
                    });
                }
            }
        }
        ObservationRecord::WindowClose { window_id, rank_completed_at, source_lag_invalid, mapping_ambiguous, cohort_truncated, .. } => {
            if first_window_id.is_none() {
                first_window_id = Some(window_id.clone());
            }
            if let Some(mut w) = pending.take() {
                w.anchor_at = rank_completed_at;
                w.valid = !source_lag_invalid && !mapping_ambiguous && !cohort_truncated;
                windows.push(w);
            }
        }
        ObservationRecord::StatusStream { event, full_market, at, .. } => {
            if let Some(start) = open_coverage.take() {
                coverage.push((start, at));
            }
            if full_market && (event == "run_start_state" || event == "started") {
                open_coverage = Some(at);
            }
        }
        ObservationRecord::Status { symbol, status_code, market_at, .. } => {
            let list = halts.entry(symbol).or_default();
            let halted_now = list.last().is_some_and(|(_, e)| e.is_none());
            if is_halt_code(&status_code) {
                if !halted_now {
                    list.push((market_at, None));
                }
            } else if halted_now {
                if let Some(last) = list.last_mut() {
                    last.1 = Some(market_at);
                }
            }
        }
        ObservationRecord::RunEnd { ended_at, .. } => run_end_at = Some(ended_at),
        _ => {}
    })?;
    if let (Some(start), Some(end)) = (open_coverage, run_end_at) {
        coverage.push((start, end));
    }
    let loss_free = certificate.status.tap_attached && certificate.status.tap_dropped == 0;
    Ok(SessionExtract {
        run_id: certificate.run_id.clone(),
        certificate: Some(certificate.clone()),
        first_window_id,
        first_window_open,
        first_receipt_market_at,
        left_censored_seen,
        windows,
        status: StatusEvidence { loss_free, coverage, halts },
    })
}

// ===========================================================================
// Clause 7: common-pool selection
// ===========================================================================

/// Arm-B evidence: `earlyQualityRank` per `(windowId, opportunityId)` from the
/// authenticated opportunity-intelligence stream. A present row with no rank
/// is `Some(None)`; an absent row is not in the map.
#[derive(Debug, Clone, Default)]
pub struct OiRanks {
    /// The OI stream for the session has zero known loss (its writer's
    /// counters and markers were checked by the caller).
    pub zero_loss_established: bool,
    pub rows: HashMap<(String, String), Option<u64>>,
}

#[derive(Debug, Clone, Copy)]
pub struct SelectionConfig {
    /// Per-arm budget: `k = min(budget, |P(W)|)`.
    pub budget: usize,
    /// A window is discriminating when `|P(W)| >= min_pool`.
    pub min_pool: usize,
}

impl Default for SelectionConfig {
    fn default() -> Self {
        Self { budget: 5, min_pool: 6 }
    }
}

#[derive(Debug, Clone)]
pub struct PoolWindow {
    pub window_id: String,
    pub anchor_at: DateTime<Utc>,
    pub pool: Vec<CandidateExtract>,
    pub k: usize,
    pub arm_a: Vec<String>,
    pub arm_b: Vec<String>,
    pub discriminating: bool,
    /// Pool members with no OI row: arm B cannot be formed for this window.
    pub oi_missing: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Selection {
    pub windows: Vec<PoolWindow>,
    /// Lifecycles excluded as left-censored (recorded, never pooled).
    pub left_censored: BTreeSet<String>,
    /// Invalid windows skipped (no pool).
    pub invalid_windows: u64,
    /// Set when the whole comparison cannot be made (e.g. OI loss not
    /// excluded). Pools are still recorded for diagnostics.
    pub comparison_indeterminate: Option<String>,
}

/// Frozen clause 7, exactly:
///
/// * F(L) = the first valid window in which L is eligible and not
///   left-censored; P(W) = every L with F(L) = W.
/// * Left-censored: open in the run's first window, or opened before the run's
///   first receipt market time. Recorded; never pooled.
/// * Every P(W) member retires after W, selected or not. No refill; a retired
///   lifecycle never re-enters. A new lifecycle of the same symbol is a new
///   candidate.
/// * Arm A: confirmation receive sequence ascending, then opportunityId.
/// * Arm B: earlyQualityRank ascending, a missing rank after every rank,
///   then opportunityId.
pub fn select(extract: &SessionExtract, oi: &OiRanks, cfg: SelectionConfig) -> Selection {
    let mut out = Selection { left_censored: extract.left_censored_seen.clone(), ..Selection::default() };
    if !oi.zero_loss_established {
        out.comparison_indeterminate = Some("opportunity-intelligence evidence not established loss-free".into());
    }
    let mut retired: HashSet<String> = HashSet::new();
    for w in &extract.windows {
        if !w.valid {
            out.invalid_windows += 1;
            continue;
        }
        let mut pool = Vec::new();
        for c in &w.eligible {
            if retired.contains(&c.opportunity_id) {
                continue;
            }
            let censored = extract.first_window_open.contains(&c.opportunity_id)
                || extract.first_receipt_market_at.map_or(true, |t| c.opened_at < t);
            if censored || c.confirmation_sequence.is_none() || c.anchor_price.is_none() {
                if censored {
                    out.left_censored.insert(c.opportunity_id.clone());
                }
                retired.insert(c.opportunity_id.clone());
                continue;
            }
            pool.push(c.clone());
        }
        for c in &pool {
            retired.insert(c.opportunity_id.clone());
        }
        let k = cfg.budget.min(pool.len());
        let mut a = pool.clone();
        a.sort_by(|x, y| {
            (x.confirmation_sequence, &x.opportunity_id).cmp(&(y.confirmation_sequence, &y.opportunity_id))
        });
        let mut oi_missing = Vec::new();
        let mut b: Vec<(u8, u64, String)> = Vec::with_capacity(pool.len());
        for c in &pool {
            match oi.rows.get(&(w.window_id.clone(), c.opportunity_id.clone())) {
                None => oi_missing.push(c.opportunity_id.clone()),
                Some(Some(rank)) => b.push((0, *rank, c.opportunity_id.clone())),
                Some(None) => b.push((1, 0, c.opportunity_id.clone())),
            }
        }
        b.sort();
        out.windows.push(PoolWindow {
            window_id: w.window_id.clone(),
            anchor_at: w.anchor_at,
            discriminating: pool.len() >= cfg.min_pool,
            arm_a: a.iter().take(k).map(|c| c.opportunity_id.clone()).collect(),
            arm_b: if oi_missing.is_empty() { b.iter().take(k).map(|x| x.2.clone()).collect() } else { Vec::new() },
            k,
            pool,
            oi_missing,
        });
    }
    out
}

// ===========================================================================
// Trade-condition policy (versioned, bound by SHA)
// ===========================================================================

pub const CONDITION_POLICY_SCHEMA: &str = "trade-condition-policy-v1";

#[derive(Debug, Clone)]
pub struct ConditionPolicy {
    pub version: String,
    /// SHA-256 of the artifact's RFC 8785 canonical bytes.
    pub sha256: String,
    pub included: BTreeSet<String>,
    pub excluded: BTreeSet<String>,
}

impl ConditionPolicy {
    /// Loads and identifies a policy artifact. Codes must be classified
    /// exactly once (disjoint included/excluded lists).
    pub fn bind(bytes: &[u8]) -> Result<Self, String> {
        let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if v["schema"] != CONDITION_POLICY_SCHEMA {
            return Err(format!("schema is not {CONDITION_POLICY_SCHEMA}"));
        }
        let list = |k: &str| -> Result<BTreeSet<String>, String> {
            v[k].as_array()
                .ok_or(format!("{k} missing"))?
                .iter()
                .map(|x| x.as_str().map(|s| s.to_string()).ok_or(format!("{k}: non-string code")))
                .collect()
        };
        let (included, excluded) = (list("included")?, list("excluded")?);
        if let Some(both) = included.intersection(&excluded).next() {
            return Err(format!("code {both:?} both included and excluded"));
        }
        let canonical = prereg::canonicalize(&v).map_err(|e| e.to_string())?;
        Ok(Self {
            version: v["version"].as_str().unwrap_or_default().to_string(),
            sha256: prereg::sha256_hex(&canonical),
            included,
            excluded,
        })
    }
}

// ===========================================================================
// Outcome evaluator (+2% / 300 s)
// ===========================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct Trade {
    pub exchange_at: DateTime<Utc>,
    pub price: Micros,
    pub conditions: Vec<String>,
}

/// Normalised trade evidence for one symbol, as produced by an authenticated
/// fetcher. The evaluator never looks behind it.
#[derive(Debug, Clone, PartialEq)]
pub struct TradeEvidence {
    pub symbol: String,
    /// The interval the fetch requested and received.
    pub covered_from: DateTime<Utc>,
    pub covered_to: DateTime<Utc>,
    /// The response was complete (all pages, no truncation).
    pub response_complete: bool,
    /// Known feed gaps inside the covered interval.
    pub gaps: Vec<(DateTime<Utc>, DateTime<Utc>)>,
    pub trades: Vec<Trade>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CensorReason {
    MissingTradeEvidence,
    IncompleteResponse,
    PartialHorizon,
    FeedGap,
    /// Halt knowledge not certified complete over the horizon.
    StatusUnknown,
    /// A halt overlaps the horizon and the target was not reached before it.
    HaltOverlap,
    /// A trade carried a condition code the policy does not classify.
    UnknownCondition,
    InvalidAnchorPrice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success { at: DateTime<Utc> },
    Failure,
    Censored(CensorReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvaluationError {
    /// The policy is not the one the preregistration binds.
    PolicyIdentity { expected: String, found: String },
}

/// Evaluates one candidate. Horizon `(t0, t0 + 300 s]` in exchange time;
/// success iff an included trade reaches `anchor * 1.02` (inclusive), and --
/// where a halt overlaps the horizon -- only if it did so **before** the halt
/// began. Missing, incomplete, partial or gapped evidence, uncertified halt
/// knowledge, and unclassifiable conditions are censored (unknown), never
/// failure. A complete horizon with no qualifying trade is a valid failure.
pub fn evaluate(
    t0: DateTime<Utc>,
    anchor_price: f64,
    symbol: &str,
    trades: Option<&TradeEvidence>,
    status: &StatusEvidence,
    policy: &ConditionPolicy,
    expected_policy_sha: &str,
) -> Result<Outcome, EvaluationError> {
    if policy.sha256 != expected_policy_sha {
        return Err(EvaluationError::PolicyIdentity {
            expected: expected_policy_sha.to_string(),
            found: policy.sha256.clone(),
        });
    }
    let end = t0 + Duration::seconds(HORIZON_SECS);
    let Some(anchor) = to_micros(anchor_price) else {
        return Ok(Outcome::Censored(CensorReason::InvalidAnchorPrice));
    };
    let Some(ev) = trades else {
        return Ok(Outcome::Censored(CensorReason::MissingTradeEvidence));
    };
    if !ev.response_complete {
        return Ok(Outcome::Censored(CensorReason::IncompleteResponse));
    }
    if ev.covered_from > t0 || ev.covered_to < end {
        return Ok(Outcome::Censored(CensorReason::PartialHorizon));
    }
    if ev.gaps.iter().any(|(a, b)| *a < end && *b > t0) {
        return Ok(Outcome::Censored(CensorReason::FeedGap));
    }
    if !status.complete_over(t0, end) {
        return Ok(Outcome::Censored(CensorReason::StatusUnknown));
    }
    let halt = status.first_halt_overlapping(symbol, t0, end);
    let mut in_horizon: Vec<&Trade> = ev.trades.iter().filter(|t| t.exchange_at > t0 && t.exchange_at <= end).collect();
    in_horizon.sort_by_key(|t| t.exchange_at);
    for t in in_horizon {
        if let Some(h) = halt {
            if t.exchange_at >= h {
                break;
            }
        }
        if t.conditions.iter().any(|c| !policy.included.contains(c) && !policy.excluded.contains(c)) {
            return Ok(Outcome::Censored(CensorReason::UnknownCondition));
        }
        if t.conditions.iter().any(|c| policy.excluded.contains(c)) {
            continue;
        }
        if reaches_target(t.price, anchor, PRIMARY_TARGET_BP) {
            return Ok(Outcome::Success { at: t.exchange_at });
        }
    }
    Ok(match halt {
        Some(_) => Outcome::Censored(CensorReason::HaltOverlap),
        None => Outcome::Failure,
    })
}

// ===========================================================================
// Censoring and the session statistic
// ===========================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct CensorEntry {
    pub window_id: String,
    pub opportunity_id: String,
    pub reason: CensorReason,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionStatistic {
    pub run_id: String,
    pub hits_a: u64,
    pub hits_b: u64,
    pub total_k: u64,
    pub windows_used: u64,
    /// Discriminating windows inside the primary scope (the censoring-rate
    /// denominator).
    pub discriminating_in_scope: u64,
    pub censored_windows: u64,
    pub out_of_scope_discriminating: u64,
    pub non_discriminating: u64,
    pub oi_missing_windows: u64,
    /// `(hits_a - hits_b) / total_k`; `None` when no window was usable.
    pub d: Option<f64>,
    pub censor_log: Vec<CensorEntry>,
}

impl SessionStatistic {
    /// Censored discriminating windows / all in-scope discriminating windows.
    pub fn censoring_rate(&self) -> Option<f64> {
        (self.discriminating_in_scope > 0).then(|| self.censored_windows as f64 / self.discriminating_in_scope as f64)
    }

    /// Against a proposed LIMITED threshold (per mille), which stays a
    /// parameter until the freeze.
    pub fn exceeds_censoring_limit(&self, limit_permille: u64) -> Option<bool> {
        self.censoring_rate().map(|r| r * 1000.0 > limit_permille as f64)
    }
}

/// Computes the session statistic over discriminating, in-scope, uncensored,
/// paired windows. **Paired censoring:** a window in which any selected
/// candidate (either arm) is censored is excluded from both arms and logged.
pub fn session_statistic(
    selection: &Selection,
    run_id: &str,
    mut outcome: impl FnMut(&PoolWindow, &CandidateExtract) -> Outcome,
) -> SessionStatistic {
    let mut s = SessionStatistic { run_id: run_id.to_string(), ..SessionStatistic::default() };
    for w in &selection.windows {
        if !w.discriminating {
            s.non_discriminating += 1;
            continue;
        }
        if !in_primary_scope(w.anchor_at) {
            s.out_of_scope_discriminating += 1;
            continue;
        }
        s.discriminating_in_scope += 1;
        if !w.oi_missing.is_empty() {
            s.oi_missing_windows += 1;
            continue;
        }
        let selected: BTreeSet<&String> = w.arm_a.iter().chain(w.arm_b.iter()).collect();
        let mut results: HashMap<&String, Outcome> = HashMap::new();
        let mut censored = false;
        for id in selected {
            let c = w.pool.iter().find(|c| &c.opportunity_id == id).expect("selected from pool");
            let o = outcome(w, c);
            if let Outcome::Censored(reason) = o {
                censored = true;
                s.censor_log.push(CensorEntry { window_id: w.window_id.clone(), opportunity_id: id.clone(), reason });
            }
            results.insert(id, o);
        }
        if censored {
            s.censored_windows += 1;
            continue;
        }
        let hit = |id: &String| matches!(results.get(id), Some(Outcome::Success { .. }));
        s.hits_a += w.arm_a.iter().filter(|id| hit(id)).count() as u64;
        s.hits_b += w.arm_b.iter().filter(|id| hit(id)).count() as u64;
        s.total_k += w.k as u64;
        s.windows_used += 1;
    }
    s.d = (s.total_k > 0).then(|| (s.hits_a as f64 - s.hits_b as f64) / s.total_k as f64);
    s
}

// ===========================================================================
// Inference
// ===========================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct SignFlip {
    pub sessions: usize,
    pub observed_sum: f64,
    /// Sign patterns with |sum| >= |observed sum| (tolerance-inclusive).
    pub extreme: u64,
    pub total: u64,
    /// Exact two-sided p-value: `extreme / total`.
    pub p_value: f64,
}

/// Exact two-sided sign-flip test over session statistics.
///
/// Enumerates all `2^S` sign patterns (S is capped by the campaign at 20, so
/// at most ~1M patterns). The observed pattern is always counted, so
/// `p >= 1 / 2^S`. Ties are judged with a relative tolerance of 1e-9 of the
/// total absolute mass: D values are ratios of small integers, and an exact
/// tie must not be split by floating-point summation order.
pub fn sign_flip_exact(ds: &[f64]) -> SignFlip {
    let s = ds.len();
    assert!(s <= 30, "sign-flip enumeration is bounded to 30 sessions");
    let observed: f64 = ds.iter().sum();
    let scale: f64 = ds.iter().map(|d| d.abs()).sum::<f64>().max(1.0);
    let tol = 1e-9 * scale;
    let total = 1u64 << s;
    let mut extreme = 0u64;
    for mask in 0..total {
        let mut sum = 0.0;
        for (i, d) in ds.iter().enumerate() {
            sum += if mask & (1 << i) != 0 { -d } else { *d };
        }
        if sum.abs() >= observed.abs() - tol {
            extreme += 1;
        }
    }
    SignFlip { sessions: s, observed_sum: observed, extreme, total, p_value: extreme as f64 / total as f64 }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PrimaryInference {
    /// Fewer qualifying sessions than the (parameterised) minimum.
    Indeterminate { sessions: usize, minimum: usize },
    Computed(SignFlip),
}

pub fn primary_inference(ds: &[f64], minimum_sessions: usize) -> PrimaryInference {
    if ds.len() < minimum_sessions {
        return PrimaryInference::Indeterminate { sessions: ds.len(), minimum: minimum_sessions };
    }
    PrimaryInference::Computed(sign_flip_exact(ds))
}

/// Two-sided 97.5% Student-t quantiles, df 1..=30.
const T975: [f64; 30] = [
    12.706, 4.303, 3.182, 2.776, 2.571, 2.447, 2.365, 2.306, 2.262, 2.228, 2.201, 2.179, 2.160, 2.145, 2.131,
    2.120, 2.110, 2.101, 2.093, 2.086, 2.080, 2.074, 2.069, 2.064, 2.060, 2.056, 2.052, 2.048, 2.045, 2.042,
];

/// **DESCRIPTIVE** mean and 95% t interval (df = S - 1). It does not decide
/// the primary test; the exact sign-flip test does.
#[derive(Debug, Clone, PartialEq)]
pub struct Descriptive {
    pub label: &'static str,
    pub sessions: usize,
    pub mean: f64,
    pub sd: f64,
    pub df: usize,
    pub t_critical: f64,
    pub low: f64,
    pub high: f64,
}

pub fn descriptive_t(ds: &[f64]) -> Option<Descriptive> {
    let n = ds.len();
    if !(2..=31).contains(&n) {
        return None;
    }
    let mean = ds.iter().sum::<f64>() / n as f64;
    let var = ds.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / (n as f64 - 1.0);
    let sd = var.sqrt();
    let t = T975[n - 2];
    let half = t * sd / (n as f64).sqrt();
    Some(Descriptive {
        label: "DESCRIPTIVE",
        sessions: n,
        mean,
        sd,
        df: n - 1,
        t_critical: t,
        low: mean - half,
        high: mean + half,
    })
}

// ===========================================================================
// Secondary: symbol-clustered sensitivity -- data shape and interface only
// ===========================================================================

/// One selected candidate's result, the unit a symbol-clustered sensitivity
/// (CR2 or wild cluster bootstrap) consumes. Produced here; estimated by a
/// later, separately reviewed offline module behind `ClusterSensitivity`.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterRow {
    pub session: String,
    pub window_id: String,
    pub symbol: String,
    pub arm: char,
    pub hit: bool,
}

/// The interface the secondary estimator will implement. Deliberately not
/// implemented here: it would add numerical code (or a dependency) whose
/// review is separate from the primary, which is the session-level exact test.
pub trait ClusterSensitivity {
    fn estimate(&self, rows: &[ClusterRow]) -> Result<(f64, f64, f64), String>;
}

/// Cluster rows for the used (uncensored, discriminating, in-scope) windows.
pub fn cluster_rows(
    selection: &Selection,
    session: &str,
    mut outcome: impl FnMut(&PoolWindow, &CandidateExtract) -> Outcome,
) -> Vec<ClusterRow> {
    let mut rows = Vec::new();
    for w in selection.windows.iter().filter(|w| w.discriminating && in_primary_scope(w.anchor_at) && w.oi_missing.is_empty()) {
        let outcomes: Vec<(char, &String, Outcome)> = w
            .arm_a
            .iter()
            .map(|id| ('A', id))
            .chain(w.arm_b.iter().map(|id| ('B', id)))
            .map(|(arm, id)| {
                let c = w.pool.iter().find(|c| &c.opportunity_id == id).expect("from pool");
                (arm, id, outcome(w, c))
            })
            .collect();
        if outcomes.iter().any(|(_, _, o)| matches!(o, Outcome::Censored(_))) {
            continue;
        }
        for (arm, id, o) in outcomes {
            let c = w.pool.iter().find(|c| &c.opportunity_id == id).expect("from pool");
            rows.push(ClusterRow {
                session: session.to_string(),
                window_id: w.window_id.clone(),
                symbol: c.symbol.clone(),
                arm,
                hit: matches!(o, Outcome::Success { .. }),
            });
        }
    }
    rows
}

/// Convenience for tests and tooling: a fixed UTC instant.
pub fn utc(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, mi, s).unwrap()
}
