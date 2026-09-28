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
use serde::Serialize;

use super::campaign::OutcomeAccess;
pub use super::policy::{ConditionPolicy, PrintVerdict, StatusClass, StatusPolicy};
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

/// One persisted SIP trading-status message, uninterpreted. Meaning is
/// assigned only at evaluation time, by the bound [`StatusPolicy`], so the
/// extract never bakes a classification into the evidence.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusEvent {
    pub market_at: DateTime<Utc>,
    pub tape: Option<String>,
    pub code: String,
    pub reason: Option<String>,
}

/// Why execution may have been unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptionKind {
    /// A status the policy classifies HALT / PAUSE / NON_TRADABLE.
    Halted,
    /// A status the policy does not classify: execution availability unknown.
    Unclassified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interruption {
    pub start: DateTime<Utc>,
    /// `None`: not ended within the run.
    pub end: Option<DateTime<Utc>>,
    pub kind: InterruptionKind,
}

/// Halt knowledge for one session.
#[derive(Debug, Clone, Default)]
pub struct StatusEvidence {
    /// The run's status evidence is loss-free and attached: the tap reported
    /// its totals and dropped nothing, and the run certified.
    pub loss_free: bool,
    /// Intervals during which full-market status delivery was live.
    pub coverage: Vec<(DateTime<Utc>, DateTime<Utc>)>,
    /// Per symbol, every status message in record order.
    pub events: BTreeMap<String, Vec<StatusEvent>>,
}

impl StatusEvidence {
    /// Halt knowledge is complete over `[from, to]`: loss-free, and one
    /// full-market coverage interval spans it. Absence of a halt record is
    /// evidence of no halt **only** under this condition.
    pub fn complete_over(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> bool {
        self.loss_free && self.coverage.iter().any(|(a, b)| *a <= from && to <= *b)
    }

    /// Reconstructs a symbol's interruptions under `policy`:
    ///
    /// * HALT / PAUSE / NON_TRADABLE opens (or continues) an interruption;
    ///   one arriving during an `Unclassified` interval upgrades it;
    /// * RESUME ends whatever interruption is open;
    /// * INFORMATIONAL changes nothing -- an imbalance or price indication
    ///   during a halt does **not** resume trading;
    /// * an unclassified status opens an `Unclassified` interval that only a
    ///   classified RESUME / interruption ends.
    pub fn interruptions(&self, symbol: &str, policy: &StatusPolicy) -> Vec<Interruption> {
        let mut out = Vec::new();
        let Some(events) = self.events.get(symbol) else {
            return out;
        };
        let mut ordered: Vec<&StatusEvent> = events.iter().collect();
        ordered.sort_by_key(|e| e.market_at); // stable: record order breaks ties
        let mut open: Option<(DateTime<Utc>, InterruptionKind)> = None;
        for e in ordered {
            match policy.classify(e.tape.as_deref(), &e.code) {
                Some(c) if c.interrupts() => match open {
                    None => open = Some((e.market_at, InterruptionKind::Halted)),
                    Some((s, InterruptionKind::Unclassified)) => {
                        out.push(Interruption { start: s, end: Some(e.market_at), kind: InterruptionKind::Unclassified });
                        open = Some((e.market_at, InterruptionKind::Halted));
                    }
                    Some(_) => {}
                },
                Some(StatusClass::Resume) => {
                    if let Some((s, kind)) = open.take() {
                        out.push(Interruption { start: s, end: Some(e.market_at), kind });
                    }
                }
                Some(_) => {}
                None => {
                    if open.is_none() {
                        open = Some((e.market_at, InterruptionKind::Unclassified));
                    }
                }
            }
        }
        if let Some((s, kind)) = open {
            out.push(Interruption { start: s, end: None, kind });
        }
        out
    }

    /// The earliest interruption overlapping `(from, to]`, if any.
    pub fn first_interruption(
        &self,
        symbol: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        policy: &StatusPolicy,
    ) -> Option<Interruption> {
        self.interruptions(symbol, policy)
            .into_iter()
            .filter(|i| i.start <= to && i.end.map_or(true, |e| e > from))
            .min_by_key(|i| i.start)
    }
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
    let mut events: BTreeMap<String, Vec<StatusEvent>> = BTreeMap::new();
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
        ObservationRecord::Status { symbol, status_code, reason_code, tape, market_at, .. } => {
            events.entry(symbol).or_default().push(StatusEvent { market_at, tape, code: status_code, reason: reason_code });
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
        status: StatusEvidence { loss_free, coverage, events },
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
    /// Arm-A selection: the first `k` of `arm_a_order`.
    pub arm_a: Vec<String>,
    /// Arm-B selection: the first `k` of `arm_b_order`; empty when any pool
    /// member has no OI row.
    pub arm_b: Vec<String>,
    /// The complete arm orderings over the pool (audit).
    pub arm_a_order: Vec<String>,
    pub arm_b_order: Vec<String>,
    pub discriminating: bool,
    /// Pool members with no OI row: arm B cannot be formed for this window.
    pub oi_missing: Vec<String>,
    /// Every identity retired at this window: the pool, plus eligible
    /// lifecycles excluded here (they cannot re-enter later).
    pub retired: Vec<String>,
    /// Eligible lifecycles seen here but not pooled, with the reason.
    pub exclusions: Vec<(String, &'static str)>,
}

#[derive(Debug, Clone, Default)]
pub struct Selection {
    pub windows: Vec<PoolWindow>,
    /// Lifecycles excluded as left-censored (recorded, never pooled).
    pub left_censored: BTreeSet<String>,
    /// Invalid windows skipped (no pool).
    pub invalid_windows: u64,
    pub invalid_window_ids: Vec<String>,
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
            out.invalid_window_ids.push(w.window_id.clone());
            continue;
        }
        let mut pool = Vec::new();
        let mut exclusions = Vec::new();
        let mut retired_here = Vec::new();
        for c in &w.eligible {
            if retired.contains(&c.opportunity_id) {
                exclusions.push((c.opportunity_id.clone(), "retired-earlier"));
                continue;
            }
            let censored = extract.first_window_open.contains(&c.opportunity_id)
                || extract.first_receipt_market_at.map_or(true, |t| c.opened_at < t);
            let reason = if censored {
                Some("left-censored")
            } else if c.confirmation_sequence.is_none() {
                Some("no-single-confirmation-sequence")
            } else if c.anchor_price.is_none() {
                Some("no-anchor-price")
            } else {
                None
            };
            if let Some(reason) = reason {
                if censored {
                    out.left_censored.insert(c.opportunity_id.clone());
                }
                retired.insert(c.opportunity_id.clone());
                retired_here.push(c.opportunity_id.clone());
                exclusions.push((c.opportunity_id.clone(), reason));
                continue;
            }
            pool.push(c.clone());
        }
        for c in &pool {
            retired.insert(c.opportunity_id.clone());
            retired_here.push(c.opportunity_id.clone());
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
        let arm_a_order: Vec<String> = a.iter().map(|c| c.opportunity_id.clone()).collect();
        let arm_b_order: Vec<String> = if oi_missing.is_empty() { b.into_iter().map(|x| x.2).collect() } else { Vec::new() };
        out.windows.push(PoolWindow {
            window_id: w.window_id.clone(),
            anchor_at: w.anchor_at,
            discriminating: pool.len() >= cfg.min_pool,
            arm_a: arm_a_order.iter().take(k).cloned().collect(),
            arm_b: arm_b_order.iter().take(k).cloned().collect(),
            arm_a_order,
            arm_b_order,
            k,
            pool,
            oi_missing,
            retired: retired_here,
            exclusions,
        });
    }
    out
}

// ===========================================================================
// Arm-B join authentication
// ===========================================================================

/// One normalised `earlyQualityRank` row from the session's OI artifact.
#[derive(Debug, Clone, PartialEq)]
pub struct OiRankRow {
    pub session: String,
    pub window_id: String,
    pub opportunity_id: String,
    pub early_quality_rank: Option<u64>,
    /// When the rank was computed: must not be after the window's anchor.
    pub computed_at: DateTime<Utc>,
}

/// A session's normalised OI rank artifact (produced by
/// [`super::oi_extract::extract`]): NDJSON, one `oi_rank_artifact` header and
/// `oi_rank` rows. Identified by the SHA-256 of its exact bytes.
#[derive(Debug, Clone, Default)]
pub struct OiArtifact {
    pub sha256: String,
    pub session: Option<String>,
    pub known_loss: Option<u64>,
    pub schema: Option<String>,
    pub extraction_contract: Option<String>,
    pub implementation_sha: Option<String>,
    pub sources: Option<serde_json::Value>,
    pub completeness_established: Option<bool>,
    /// Defects the extractor found in the *source* (header-reported).
    pub source_malformed_rows: Option<u64>,
    /// Defects in the normalised artifact itself.
    pub malformed_rows: u64,
    pub rows: Vec<OiRankRow>,
}

impl OiArtifact {
    pub fn parse(bytes: &[u8]) -> Self {
        let mut a = OiArtifact { sha256: prereg::sha256_hex(bytes), ..OiArtifact::default() };
        for line in bytes.split(|b| *b == b'\n').filter(|l| !l.iter().all(u8::is_ascii_whitespace)) {
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
                a.malformed_rows += 1;
                continue;
            };
            match v["kind"].as_str() {
                Some("oi_rank_artifact") if a.session.is_none() => {
                    let s = |k: &str| v[k].as_str().map(str::to_string);
                    a.session = s("session");
                    a.known_loss = v["knownLoss"].as_u64();
                    a.schema = s("schema");
                    a.extraction_contract = s("extractionContract");
                    a.implementation_sha = s("implementationSha");
                    a.sources = Some(v["sources"].clone()).filter(|x| !x.is_null());
                    a.completeness_established = v["completenessEstablished"].as_bool();
                    a.source_malformed_rows = v["sourceMalformedRows"].as_u64();
                }
                Some("oi_rank") => {
                    let rank = &v["earlyQualityRank"];
                    let row = (|| {
                        Some(OiRankRow {
                            session: v["session"].as_str()?.to_string(),
                            window_id: v["windowId"].as_str()?.to_string(),
                            opportunity_id: v["opportunityId"].as_str()?.to_string(),
                            early_quality_rank: if rank.is_null() { None } else { Some(rank.as_u64()?) },
                            computed_at: v["computedAt"].as_str()?.parse().ok()?,
                        })
                    })();
                    match row {
                        Some(r) => a.rows.push(r),
                        None => a.malformed_rows += 1,
                    }
                }
                // Anything else, including a second header, is malformed.
                _ => a.malformed_rows += 1,
            }
        }
        a
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OiJoinFailure {
    ArtifactIdentity { expected: String, found: String },
    /// The normalised artifact does not match its binding (source SHAs,
    /// session, implementation, contract or its own SHA).
    Binding(String),
    MissingHeader,
    /// The extractor could not establish the source capture complete.
    CompletenessNotEstablished,
    SourceMalformed(u64),
    SessionMismatch { expected: String, found: String },
    KnownLoss(u64),
    MalformedRows(u64),
    /// The extract's windows do not all belong to the stated session.
    ExtractNotInSession(String),
    /// A row names a window the session's extract does not contain.
    UnknownWindow(String),
    /// A rank computed after its window's anchor (future information).
    FutureInformation { window_id: String, opportunity_id: String },
    /// Two rows for one key disagree: fail closed, never choose.
    ConflictingRanks { window_id: String, opportunity_id: String },
}

/// What the preregistration expects of the session's OI evidence.
pub struct OiExpectation<'a> {
    /// The normalised artifact's SHA-256, as registered for the session.
    pub normalized_sha256: &'a str,
    /// This build's implementation SHA.
    pub implementation_sha: &'a str,
}

/// Authenticates the OI artifact against its binding and joins it on
/// `(windowId, opportunityId)`. Fails closed on any identity, binding,
/// completeness, loss, malformation, cross-session, future-information or
/// conflicting-duplicate defect. Exact duplicates collapse deterministically.
pub fn authenticate_oi(
    artifact: &OiArtifact,
    binding: &super::oi_extract::OiBinding,
    expected: &OiExpectation<'_>,
    extract: &SessionExtract,
    session: &str,
) -> Result<OiRanks, OiJoinFailure> {
    use super::oi_extract::{EXTRACTION_CONTRACT, OI_ARTIFACT_SCHEMA, OI_BINDING_SCHEMA};
    if artifact.sha256 != expected.normalized_sha256 {
        return Err(OiJoinFailure::ArtifactIdentity { expected: expected.normalized_sha256.into(), found: artifact.sha256.clone() });
    }
    let bind_err = |m: &str| Err(OiJoinFailure::Binding(m.to_string()));
    if binding.schema != OI_BINDING_SCHEMA || binding.extraction_contract != EXTRACTION_CONTRACT {
        return bind_err("binding schema or extraction contract");
    }
    if binding.normalized_sha256 != artifact.sha256 {
        return bind_err("binding names a different normalised artifact");
    }
    if binding.session != session {
        return bind_err("binding session");
    }
    if binding.implementation_sha != expected.implementation_sha {
        return bind_err("binding implementation SHA");
    }
    if binding.normalized_rows != artifact.rows.len() as u64 {
        return bind_err("binding row count");
    }
    let (Some(a_session), Some(loss), Some(complete), Some(src_bad)) =
        (&artifact.session, artifact.known_loss, artifact.completeness_established, artifact.source_malformed_rows)
    else {
        return Err(OiJoinFailure::MissingHeader);
    };
    if artifact.schema.as_deref() != Some(OI_ARTIFACT_SCHEMA)
        || artifact.extraction_contract.as_deref() != Some(binding.extraction_contract.as_str())
        || artifact.implementation_sha.as_deref() != Some(binding.implementation_sha.as_str())
        || artifact.sources.as_ref() != serde_json::to_value(&binding.sources).ok().as_ref()
    {
        return bind_err("artifact header does not match its binding");
    }
    if a_session != session {
        return Err(OiJoinFailure::SessionMismatch { expected: session.into(), found: a_session.clone() });
    }
    if loss != 0 {
        return Err(OiJoinFailure::KnownLoss(loss));
    }
    if src_bad != 0 {
        return Err(OiJoinFailure::SourceMalformed(src_bad));
    }
    if !complete {
        return Err(OiJoinFailure::CompletenessNotEstablished);
    }
    if artifact.malformed_rows != 0 {
        return Err(OiJoinFailure::MalformedRows(artifact.malformed_rows));
    }
    let mut anchors = HashMap::new();
    for w in &extract.windows {
        if market_data::trading_session::market_day(w.anchor_at).to_string() != session {
            return Err(OiJoinFailure::ExtractNotInSession(w.window_id.clone()));
        }
        anchors.insert(w.window_id.clone(), w.anchor_at);
    }
    let mut rows: HashMap<(String, String), Option<u64>> = HashMap::new();
    for r in &artifact.rows {
        if r.session != session {
            return Err(OiJoinFailure::SessionMismatch { expected: session.into(), found: r.session.clone() });
        }
        let Some(anchor) = anchors.get(&r.window_id) else {
            return Err(OiJoinFailure::UnknownWindow(r.window_id.clone()));
        };
        if r.computed_at > *anchor {
            return Err(OiJoinFailure::FutureInformation { window_id: r.window_id.clone(), opportunity_id: r.opportunity_id.clone() });
        }
        let key = (r.window_id.clone(), r.opportunity_id.clone());
        match rows.get(&key) {
            Some(existing) if *existing != r.early_quality_rank => {
                return Err(OiJoinFailure::ConflictingRanks { window_id: key.0, opportunity_id: key.1 });
            }
            Some(_) => {}
            None => {
                rows.insert(key, r.early_quality_rank);
            }
        }
    }
    Ok(OiRanks { zero_loss_established: true, rows })
}

// ===========================================================================
// Selection audit
// ===========================================================================

/// One auditable selection record per comparison window.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionAuditRecord {
    pub schema: &'static str,
    pub session: String,
    pub run_id: String,
    pub window_id: String,
    pub valid: bool,
    pub anchor_at: Option<DateTime<Utc>>,
    pub pool: Vec<String>,
    pub retired: Vec<String>,
    pub arm_a_order: Vec<String>,
    pub arm_a_selected: Vec<String>,
    pub arm_b_order: Vec<String>,
    pub arm_b_selected: Vec<String>,
    pub k: usize,
    pub discriminating: bool,
    pub exclusions: Vec<SelectionExclusion>,
    /// Session-level reason arm B cannot be compared at all, if any.
    pub comparison_indeterminate: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionExclusion {
    pub opportunity_id: String,
    pub reason: String,
}

pub const SELECTION_AUDIT_SCHEMA: &str = "selection-audit-v1";

/// Audit records in window order, including invalid windows (no pool).
pub fn selection_audit(selection: &Selection, session: &str, run_id: &str, extract: &SessionExtract) -> Vec<SelectionAuditRecord> {
    let invalid: HashSet<&String> = selection.invalid_window_ids.iter().collect();
    let by_id: HashMap<&String, &PoolWindow> = selection.windows.iter().map(|w| (&w.window_id, w)).collect();
    let mut out = Vec::new();
    for w in &extract.windows {
        let base = SelectionAuditRecord {
            schema: SELECTION_AUDIT_SCHEMA,
            session: session.into(),
            run_id: run_id.into(),
            window_id: w.window_id.clone(),
            valid: false,
            anchor_at: None,
            pool: vec![],
            retired: vec![],
            arm_a_order: vec![],
            arm_a_selected: vec![],
            arm_b_order: vec![],
            arm_b_selected: vec![],
            k: 0,
            discriminating: false,
            exclusions: vec![],
            comparison_indeterminate: selection.comparison_indeterminate.clone(),
        };
        if invalid.contains(&w.window_id) {
            out.push(SelectionAuditRecord {
                exclusions: vec![SelectionExclusion { opportunity_id: "*".into(), reason: "invalid-window".into() }],
                ..base
            });
            continue;
        }
        let Some(p) = by_id.get(&w.window_id) else { continue };
        let mut exclusions: Vec<SelectionExclusion> = p
            .exclusions
            .iter()
            .map(|(id, r)| SelectionExclusion { opportunity_id: id.clone(), reason: (*r).into() })
            .collect();
        exclusions.extend(p.oi_missing.iter().map(|id| SelectionExclusion { opportunity_id: id.clone(), reason: "oi-row-missing".into() }));
        out.push(SelectionAuditRecord {
            valid: true,
            anchor_at: Some(p.anchor_at),
            pool: p.pool.iter().map(|c| c.opportunity_id.clone()).collect(),
            retired: p.retired.clone(),
            arm_a_order: p.arm_a_order.clone(),
            arm_a_selected: p.arm_a.clone(),
            arm_b_order: p.arm_b_order.clone(),
            arm_b_selected: p.arm_b.clone(),
            k: p.k,
            discriminating: p.discriminating,
            exclusions,
            ..base
        });
    }
    out
}

// ===========================================================================
// Outcome evaluator (+2% / 300 s)
// ===========================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct Trade {
    pub exchange_at: DateTime<Utc>,
    pub price: Micros,
    /// Tape (`A`/`B` CTA, `C` UTP): conditions are interpreted per tape.
    pub tape: String,
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
    /// A HALT / PAUSE / NON_TRADABLE interruption overlaps the horizon and
    /// the target was not reached before it began.
    HaltOverlap,
    /// A status the policy does not classify overlaps the horizon and the
    /// target was not reached before it.
    StatusUnclassified,
    /// A print whose conditions are uncertain under the policy would have
    /// reached the target before any counted print did.
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
    /// The condition table is not the one the preregistration binds.
    PolicyIdentity { expected: String, found: String },
    /// The status policy is not the one the preregistration binds.
    StatusPolicyIdentity { expected: String, found: String },
    /// Outcome firewall: the session is outside the closed, authorized set.
    OutcomeFirewall { session: String },
}

/// Everything the evaluator binds, checked on every call.
pub struct EvaluationContext<'a> {
    pub access: &'a OutcomeAccess,
    pub session: &'a str,
    pub conditions: &'a ConditionPolicy,
    pub statuses: &'a StatusPolicy,
    pub expected_condition_sha: &'a str,
    pub expected_status_sha: &'a str,
}

/// Evaluates one candidate. Horizon `(t0, t0 + 300 s]` in exchange time;
/// success iff a counted print reaches `anchor * 1.02` (inclusive) before any
/// interruption of execution began. Missing, incomplete, partial or gapped
/// evidence, uncertified status knowledge, an overlapping interruption that
/// was not preceded by a reach, and an uncertain print that would have been
/// the first reach are all censored (unknown), never failure. A complete,
/// uninterrupted horizon with no qualifying print is a valid failure.
///
/// An uncertain print *below* the target is ignored: under any
/// classification it could not have been a success, so it cannot change the
/// outcome and censoring on it would only discard information.
pub fn evaluate(
    ctx: &EvaluationContext<'_>,
    t0: DateTime<Utc>,
    anchor_price: f64,
    symbol: &str,
    trades: Option<&TradeEvidence>,
    status: &StatusEvidence,
) -> Result<Outcome, EvaluationError> {
    if !ctx.access.allows(ctx.session) {
        return Err(EvaluationError::OutcomeFirewall { session: ctx.session.to_string() });
    }
    if ctx.conditions.sha256 != ctx.expected_condition_sha {
        return Err(EvaluationError::PolicyIdentity {
            expected: ctx.expected_condition_sha.to_string(),
            found: ctx.conditions.sha256.clone(),
        });
    }
    if ctx.statuses.sha256 != ctx.expected_status_sha {
        return Err(EvaluationError::StatusPolicyIdentity {
            expected: ctx.expected_status_sha.to_string(),
            found: ctx.statuses.sha256.clone(),
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
    let interruption = status.first_interruption(symbol, t0, end, ctx.statuses);
    let mut in_horizon: Vec<&Trade> = ev.trades.iter().filter(|t| t.exchange_at > t0 && t.exchange_at <= end).collect();
    in_horizon.sort_by_key(|t| t.exchange_at);
    for t in in_horizon {
        if interruption.is_some_and(|i| t.exchange_at >= i.start) {
            break;
        }
        if !reaches_target(t.price, anchor, PRIMARY_TARGET_BP) {
            continue;
        }
        match ctx.conditions.classify(&t.tape, &t.conditions) {
            PrintVerdict::Counts => return Ok(Outcome::Success { at: t.exchange_at }),
            PrintVerdict::Ignored => continue,
            PrintVerdict::Uncertain => return Ok(Outcome::Censored(CensorReason::UnknownCondition)),
        }
    }
    Ok(match interruption.map(|i| i.kind) {
        Some(InterruptionKind::Halted) => Outcome::Censored(CensorReason::HaltOverlap),
        Some(InterruptionKind::Unclassified) => Outcome::Censored(CensorReason::StatusUnclassified),
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
    /// `(hits_a - hits_b) / total_k`; `None` when no window was usable or
    /// the comparison is indeterminate.
    pub d: Option<f64>,
    /// Set when arm B cannot be compared at all (OI not authenticated): no
    /// outcome is consulted and the session has no statistic.
    pub indeterminate: Option<String>,
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
    if let Some(reason) = &selection.comparison_indeterminate {
        s.indeterminate = Some(reason.clone());
        return s;
    }
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
    if selection.comparison_indeterminate.is_some() {
        return rows;
    }
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
