//! Opportunity-native forward outcome measurement.
//!
//! **Parallel to the episode outcome model, never a replacement.** `episode`,
//! `outcome` and `horizon` are untouched and still authoritative for existing
//! metrics. This is a second, independently versioned measurement whose only
//! reason to exist is that the first one cannot be used to judge a ranking
//! model.
//!
//! # The defect this answers
//!
//! Episode outcomes attach through episode membership, and membership is not
//! independent of the score. Measured on the frozen 2026-09-17/18 artifacts:
//! opportunities with `membershipStatus = no_episodes` (8,459 THU / 8,524 FRI,
//! ~55% of the population) carry a forward excursion **0.00% of the time in
//! every V2 decile on both days**, while the `no_episodes` share rises with
//! the score to 87.23% in Friday's top decile. That is not censoring -- there
//! is no partial follow-up to correct -- it is structural non-measurement, and
//! it makes every MFE/MAE statistic a score-selected subsample.
//!
//! # The invariant that fixes it
//!
//! **Every ranking anchor gets a measurement row.** Whether a row exists may
//! not depend on EarlyQuality, Continuation, V2, rank, detector provenance,
//! episode membership, or on what the price later did. If the forward path
//! cannot be completed the row is *censored*, never dropped. Coverage becomes
//! a property of the capture window instead of a property of the score.
//!
//! # Why an anchor is not an episode
//!
//! An anchor is `(opportunityId, windowId, scoreTimestamp)` -- one ranking
//! *decision*, not a live object. It is deliberately detached from the
//! `Opportunity` that produced it: once created it is fed by the symbol's
//! price stream and nothing else, so it keeps measuring after its opportunity
//! is closed by inactivity, by a session boundary, or by capacity eviction.
//! Closure is recorded as provenance, never as a censor.
//!
//! # Why accumulators and not stored paths
//!
//! The episode collector keeps a price path per pending episode. That does not
//! scale here: the derived capacity below is 297,000 outstanding anchors, and
//! at the episode collector's 2,048-point path that would be ~12 GB. Every
//! quantity this module reports is instead computed incrementally as prices
//! arrive -- running extrema with their timestamps, a horizon cursor, and
//! first-crossing latches -- so an anchor costs a few hundred bytes regardless
//! of how many observations it sees, and a price update is O(1) per anchor.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::horizon::{CensorReason, Observation};

/// Identifies this measurement contract in every row it writes. Separate from
/// `HORIZON_SCHEMA_VERSION` because this is a different contract over a
/// different population, not a revision of that one.
pub const OPPORTUNITY_OUTCOME_VERSION: &str = "opportunity-outcome-v1";

/// The horizon grid for opportunity-native measurement, in seconds.
///
/// Deliberately **not** `horizon::HORIZON_SECS`. That grid
/// (`30,60,180,300,600,900,1800`) belongs to the episode contract and is
/// authoritative there. This one is the grid the milestone specifies, and the
/// two are kept separate so neither can drift into the other by accident.
pub const OUTCOME_HORIZON_SECS: [i64; 6] = [30, 60, 120, 300, 600, 1200];

/// Target thresholds for first-crossing measurement, in percent.
pub const OUTCOME_TARGET_PCTS: [f64; 3] = [2.0, 5.0, 10.0];

/// Longest configured horizon, derived from the grid rather than restated.
pub const fn longest_outcome_horizon_secs() -> i64 {
    let mut longest = OUTCOME_HORIZON_SECS[0];
    let mut i = 1;
    while i < OUTCOME_HORIZON_SECS.len() {
        if OUTCOME_HORIZON_SECS[i] > longest {
            longest = OUTCOME_HORIZON_SECS[i];
        }
        i += 1;
    }
    longest
}

/// Observation continues this long past the longest horizon before settling.
///
/// The sampling rule takes the first observation at or **after** `anchor + h`,
/// so a deadline equal to `h` would leave the longest horizon observable only
/// by a race between the final price update and settlement -- the defect
/// `horizon`'s own margin exists to prevent (0.06% of episodes at 1800s in
/// Session 001). Same reasoning, same value.
pub const OUTCOME_MARGIN_SECS: i64 = 120;

/// When an outstanding anchor settles: a constant offset from `anchor_at`, so
/// key order is due order and settlement is a range query.
pub const OUTCOME_SETTLE_AFTER_SECS: i64 = longest_outcome_horizon_secs() + OUTCOME_MARGIN_SECS;

/// A hole larger than this inside the measured window makes the extremes
/// across it unknown, so the row is marked `DataGap` rather than reporting
/// extrema it cannot support. Matches `horizon::MAX_GAP_SECS`.
pub const OUTCOME_MAX_GAP_SECS: i64 = 120;

/// Fewer forward observations than this and the row carries
/// `InsufficientForwardData`: one point cannot describe a path.
pub const OUTCOME_MIN_OBSERVATIONS: u32 = 2;

const _: () = {
    let mut i = 0;
    while i < OUTCOME_HORIZON_SECS.len() {
        assert!(
            OUTCOME_HORIZON_SECS[i] < OUTCOME_SETTLE_AFTER_SECS,
            "every horizon must be strictly shorter than the settlement deadline, \
             or it is observable only by a race"
        );
        i += 1;
    }
};

// ---------------------------------------------------------------------------
// Capacity, derived from measured session evidence (never picked)
// ---------------------------------------------------------------------------

/// Anchors created per second that this collector is **designed** to retain
/// fully, in hundredths, so the capacity below is exact const arithmetic.
///
/// **180.00/s, derived from the D2-corrected 2026-09-17/18 replays**, not
/// rounded up from a guess:
///
/// | quantity | THU 2026-09-17 | FRI 2026-09-18 |
/// |---|---|---|
/// | ranking cadence | 30s | 30s |
/// | rankable cohort, median | 3,247.5 | 3,120 |
/// | rankable cohort, p95 | 2,924.7 | 3,213.9 |
/// | rankable cohort, **max** | **4,005** | **4,308** |
/// | regular-session windows | 780 | 779 |
///
/// Every rankable opportunity in a window becomes one anchor, so the anchor
/// rate is `cohort / cadence`: a median of 108.3/s and an observed peak of
/// **143.6/s** (4,308 / 30). 180.00/s carries 25.4% headroom over that peak
/// and 1.66x the median.
///
/// The quantity that sizes the outstanding set is the sustained rate over one
/// settlement window, because that *is* the population -- not an instantaneous
/// burst, which at this cadence cannot exist: a ranking window emits its whole
/// cohort at once and then nothing for 30s.
pub const SUPPORTED_ANCHOR_RATE_CENTI: u64 = 18_000;

/// Headroom beyond the supported rate (5/4 = 1.25), matching the episode
/// collector's convention so the two are comparable.
pub const ANCHOR_SAFETY_NUM: u64 = 5;
pub const ANCHOR_SAFETY_DEN: u64 = 4;

/// Outstanding anchors retained, **derived** from the supported rate and the
/// settlement window rather than chosen.
///
/// `180.00/s x 1320s x 1.25 = 297,000`.
///
/// Stated plainly so the trade-off is visible: at the observed peak of
/// 143.6/s the steady-state population is ~189,500, so this bound is not
/// expected to bind in an ordinary session. It is still a hard bound, because
/// the process is realtime and must not grow without limit -- but it is a
/// bound derived from what the collector claims to support, which is the
/// lesson of defect 48-A (a flat 4,096 that silently force-settled episodes
/// and was mistaken for market behaviour).
pub const MAX_OUTSTANDING_ANCHORS: usize = ((SUPPORTED_ANCHOR_RATE_CENTI
    * OUTCOME_SETTLE_AFTER_SECS as u64
    * ANCHOR_SAFETY_NUM)
    / (100 * ANCHOR_SAFETY_DEN)) as usize;

/// Enforced at compile time: capacity must hold the supported rate for a
/// **full** settlement window before any safety margin counts. Lower the
/// capacity, raise the rate or extend the grid and the build fails rather than
/// silently reintroducing capacity-induced censoring.
const _: () = {
    assert!(
        (MAX_OUTSTANDING_ANCHORS as u64) * 100
            >= SUPPORTED_ANCHOR_RATE_CENTI * (OUTCOME_SETTLE_AFTER_SECS as u64),
        "outstanding capacity cannot hold the supported anchor rate for one \
         settlement window; anchors would be evicted before their horizons \
         mature (see defect 48-A)"
    );
};

// ---------------------------------------------------------------------------
// Record shapes
// ---------------------------------------------------------------------------

/// Everything needed to join a measurement row back to the exact ranking model
/// that produced its anchor. Carried per row rather than per file so a merged
/// corpus stays attributable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnchorProvenance {
    pub measurement_version: String,
    pub opportunity_schema: u32,
    pub feature_schema: u32,
    pub early_quality_model: String,
    pub continuation_model: String,
    pub ranking: String,
    pub score_policy: String,
    pub config_fingerprint: String,
}

/// One forward return. `Censored` carries why, and never a number.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HorizonReturn {
    pub horizon_secs: i64,
    pub outcome: Observation<f64>,
}

/// First crossing of a target, or the censor that prevented observing one.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetCrossing {
    pub target_pct: f64,
    pub seconds_to: Observation<i64>,
}

/// Excursion over the measured window, as percent of `signal_price`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutcomeExcursion {
    pub mfe_pct: f64,
    pub mae_pct: f64,
    /// Lowest excursion reached between the anchor and the MFE instant. Zero
    /// when the MFE is the first observation.
    pub drawdown_before_mfe_pct: f64,
    pub seconds_to_mfe: i64,
    pub seconds_to_mae: i64,
}

/// Why the opportunity that produced this anchor is no longer open.
///
/// Recorded as provenance, **never** as a censor: the anchor measures the
/// symbol's forward price, which continues regardless of what happened to the
/// lifecycle object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpportunityDisposition {
    StillOpen,
    Closed,
    CapacityEvicted,
}

/// One settled measurement row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpportunityOutcomeRow {
    pub opportunity_id: String,
    pub window_id: String,
    pub symbol: String,
    pub session_date: String,
    pub anchor_at: DateTime<Utc>,
    pub signal_price: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened_at: Option<DateTime<Utc>>,
    pub provenance: AnchorProvenance,

    pub observation_count: u32,
    pub observed_span_secs: i64,
    pub fully_observed: bool,
    pub censor_reasons: Vec<CensorReason>,
    pub opportunity_disposition: OpportunityDisposition,

    pub returns: Vec<HorizonReturn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excursion: Option<OutcomeExcursion>,
    pub target_crossings: Vec<TargetCrossing>,
}

/// What a caller supplies to create an anchor. Deliberately carries no score
/// and no rank: nothing about admission may depend on them.
#[derive(Debug, Clone, PartialEq)]
pub struct AnchorRequest {
    pub opportunity_id: String,
    pub window_id: String,
    pub symbol: String,
    pub session_date: String,
    pub anchor_at: DateTime<Utc>,
    pub signal_price: f64,
    pub opened_at: Option<DateTime<Utc>>,
    /// End of the session this anchor belongs to. Horizons reaching past it
    /// are censored `SessionEnded` rather than silently shortened.
    pub session_end: DateTime<Utc>,
    /// Shared, because it is identical for every anchor a run produces.
    ///
    /// Storing it by value cost ~284 B per outstanding anchor -- 84 MB at
    /// capacity -- to repeat the same eight strings 297,000 times. The settled
    /// row still owns its copy, since only a few thousand rows exist at once
    /// and each is serialized immediately.
    pub provenance: Arc<AnchorProvenance>,
}

// ---------------------------------------------------------------------------
// Outstanding state
// ---------------------------------------------------------------------------

/// `(anchor_at, id)` -- ordered by settlement deadline, unique per anchor.
type AnchorKey = (DateTime<Utc>, u64);

struct Outstanding {
    req: AnchorRequest,
    /// Horizon slots, filled by the first observation at or after each
    /// horizon. `cursor` is the index of the lowest unfilled slot, so filling
    /// is amortised O(1) rather than a scan.
    filled: [Option<f64>; OUTCOME_HORIZON_SECS.len()],
    cursor: usize,
    crossed: [Option<i64>; OUTCOME_TARGET_PCTS.len()],
    observations: u32,
    first_at: Option<DateTime<Utc>>,
    last_at: Option<DateTime<Utc>>,
    max_gap_secs: i64,
    mfe_pct: f64,
    mae_pct: f64,
    secs_to_mfe: i64,
    secs_to_mae: i64,
    /// Running minimum excursion, and its value snapshotted whenever a new MFE
    /// is set -- which is exactly "the worst point reached before the best".
    running_min_pct: f64,
    drawdown_before_mfe_pct: f64,
    disposition: OpportunityDisposition,
}

impl Outstanding {
    fn new(req: AnchorRequest) -> Self {
        Self {
            req,
            filled: [None; OUTCOME_HORIZON_SECS.len()],
            cursor: 0,
            crossed: [None; OUTCOME_TARGET_PCTS.len()],
            observations: 0,
            first_at: None,
            last_at: None,
            max_gap_secs: 0,
            mfe_pct: 0.0,
            mae_pct: 0.0,
            secs_to_mfe: 0,
            secs_to_mae: 0,
            running_min_pct: 0.0,
            drawdown_before_mfe_pct: 0.0,
            disposition: OpportunityDisposition::StillOpen,
        }
    }

    /// Folds one forward price in. O(1) amortised.
    fn observe(&mut self, at: DateTime<Utc>, price: f64) {
        // Strictly forward: an observation at or before the anchor instant is
        // not forward information and must not enter the measurement.
        if at <= self.req.anchor_at || !price.is_finite() || price <= 0.0 {
            return;
        }
        // And strictly inside the measured window. Without this the window
        // would end whenever the caller next happened to call `settle_due`, so
        // MFE, MAE and target crossings would depend on the caller's cadence
        // rather than on the contract -- two runs over identical prices could
        // disagree. The equivalence gate against an independent reference
        // caught exactly that: 687 rows with one extra observation and 68 with
        // a shifted extremum. Every horizon is shorter than the deadline, so
        // this can never truncate one.
        if (at - self.req.anchor_at).num_seconds() > OUTCOME_SETTLE_AFTER_SECS {
            return;
        }
        if self.req.signal_price <= 0.0 || !self.req.signal_price.is_finite() {
            return;
        }
        let elapsed = (at - self.req.anchor_at).num_seconds();
        let pct = (price - self.req.signal_price) / self.req.signal_price * 100.0;

        if let Some(prev) = self.last_at {
            let gap = (at - prev).num_seconds();
            if gap > self.max_gap_secs {
                self.max_gap_secs = gap;
            }
        } else {
            self.first_at = Some(at);
            // The opening gap counts too: a first forward price arriving long
            // after the anchor leaves the early horizons unsupported.
            self.max_gap_secs = self.max_gap_secs.max(elapsed);
        }
        self.last_at = Some(at);
        self.observations = self.observations.saturating_add(1);

        // Horizon slots: first observation at or after `anchor + h`.
        while self.cursor < OUTCOME_HORIZON_SECS.len()
            && elapsed >= OUTCOME_HORIZON_SECS[self.cursor]
        {
            self.filled[self.cursor] = Some(pct);
            self.cursor += 1;
        }

        // Extrema, with their timing.
        if pct > self.mfe_pct {
            self.mfe_pct = pct;
            self.secs_to_mfe = elapsed;
            self.drawdown_before_mfe_pct = self.running_min_pct;
        }
        if pct < self.mae_pct {
            self.mae_pct = pct;
            self.secs_to_mae = elapsed;
        }
        if pct < self.running_min_pct {
            self.running_min_pct = pct;
        }

        // First crossing of each target, latched.
        for (i, target) in OUTCOME_TARGET_PCTS.iter().enumerate() {
            if self.crossed[i].is_none() && pct >= *target {
                self.crossed[i] = Some(elapsed);
            }
        }
    }

    /// Turns accumulated state into a row. `forced` carries the censor when
    /// settlement is not the ordinary deadline.
    fn finish(self, forced: Option<CensorReason>) -> OpportunityOutcomeRow {
        let mut censors: Vec<CensorReason> = Vec::new();
        if let Some(reason) = forced {
            censors.push(reason);
        }

        let enough = self.observations >= OUTCOME_MIN_OBSERVATIONS;
        if !enough {
            censors.push(CensorReason::InsufficientForwardData);
        }
        let gapped = self.max_gap_secs > OUTCOME_MAX_GAP_SECS;
        if gapped {
            censors.push(CensorReason::DataGap);
        }

        // A horizon reaching past the session close was never observable, and
        // says nothing about the symbol.
        let session_limit = (self.req.session_end - self.req.anchor_at).num_seconds();

        let mut returns = Vec::with_capacity(OUTCOME_HORIZON_SECS.len());
        for (i, h) in OUTCOME_HORIZON_SECS.iter().enumerate() {
            let outcome = match self.filled[i] {
                Some(pct) if !gapped => Observation::Observed(pct),
                Some(_) => Observation::Censored(CensorReason::DataGap),
                None if *h > session_limit => Observation::Censored(CensorReason::SessionEnded),
                None => Observation::Censored(
                    forced.unwrap_or(CensorReason::InsufficientForwardData),
                ),
            };
            if let Observation::Censored(r) = outcome {
                if !censors.contains(&r) {
                    censors.push(r);
                }
            }
            returns.push(HorizonReturn { horizon_secs: *h, outcome });
        }

        let mut target_crossings = Vec::with_capacity(OUTCOME_TARGET_PCTS.len());
        for (i, target) in OUTCOME_TARGET_PCTS.iter().enumerate() {
            // A crossing that DID happen is a fact even on a gapped or
            // force-settled path -- we saw it. Only the absence of one is
            // uncertain, and that inherits the row's censor.
            let outcome = match self.crossed[i] {
                Some(secs) => Observation::Observed(secs),
                None if !censors.is_empty() => Observation::Censored(
                    forced
                        .or(if gapped { Some(CensorReason::DataGap) } else { None })
                        .unwrap_or(CensorReason::InsufficientForwardData),
                ),
                None => Observation::Observed(-1),
            };
            target_crossings.push(TargetCrossing { target_pct: *target, seconds_to: outcome });
        }

        let excursion = if enough && !gapped {
            Some(OutcomeExcursion {
                mfe_pct: self.mfe_pct,
                mae_pct: self.mae_pct,
                drawdown_before_mfe_pct: self.drawdown_before_mfe_pct,
                seconds_to_mfe: self.secs_to_mfe,
                seconds_to_mae: self.secs_to_mae,
            })
        } else {
            None
        };

        let observed_span_secs = match (self.first_at, self.last_at) {
            (Some(_), Some(last)) => (last - self.req.anchor_at).num_seconds(),
            _ => 0,
        };

        OpportunityOutcomeRow {
            opportunity_id: self.req.opportunity_id,
            window_id: self.req.window_id,
            symbol: self.req.symbol,
            session_date: self.req.session_date,
            anchor_at: self.req.anchor_at,
            signal_price: self.req.signal_price,
            opened_at: self.req.opened_at,
            provenance: (*self.req.provenance).clone(),
            observation_count: self.observations,
            observed_span_secs,
            fully_observed: censors.is_empty(),
            censor_reasons: censors,
            opportunity_disposition: self.disposition,
            returns,
            excursion,
            target_crossings,
        }
    }
}

// ---------------------------------------------------------------------------
// Collector
// ---------------------------------------------------------------------------

/// Live counters, so capacity pressure is visible while it happens rather than
/// inferred from span distributions afterwards -- the lesson of defect 48-A.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutcomeHealth {
    pub outstanding: usize,
    pub peak_outstanding: usize,
    pub capacity: usize,
    pub anchors_created: u64,
    pub anchors_settled: u64,
    pub capacity_evictions: u64,
    pub symbols_tracked: usize,
}

/// Accepts ranking anchors and symbol price observations; emits settled rows.
///
/// Deliberately free of IO and of any dependency on `OpportunityIntelligence`,
/// so the score-independence invariant can be tested directly rather than
/// through a live driver.
pub struct OpportunityOutcomeCollector {
    outstanding: BTreeMap<AnchorKey, Outstanding>,
    /// Symbol -> its outstanding anchors, so a price event touches only the
    /// anchors it can affect. Without this every price would be linear in the
    /// whole outstanding set, which at a 297,000 capacity is not survivable on
    /// a realtime path.
    by_symbol: HashMap<String, Vec<AnchorKey>>,
    next_id: u64,
    health: OutcomeHealth,
}

impl Default for OpportunityOutcomeCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl OpportunityOutcomeCollector {
    pub fn new() -> Self {
        Self {
            outstanding: BTreeMap::new(),
            by_symbol: HashMap::new(),
            next_id: 0,
            health: OutcomeHealth { capacity: MAX_OUTSTANDING_ANCHORS, ..Default::default() },
        }
    }

    pub fn health(&self) -> OutcomeHealth {
        let mut h = self.health;
        h.outstanding = self.outstanding.len();
        h.symbols_tracked = self.by_symbol.len();
        h
    }

    pub fn outstanding(&self) -> usize {
        self.outstanding.len()
    }

    /// Admits one ranking anchor.
    ///
    /// **Admission depends only on capacity.** No score, rank, provenance or
    /// membership is consulted, and there is no path by which one could be:
    /// `AnchorRequest` does not carry them.
    ///
    /// Returns any row force-settled to make room, so a caller never loses a
    /// measurement silently.
    pub fn anchor(&mut self, req: AnchorRequest) -> Vec<OpportunityOutcomeRow> {
        let mut evicted = Vec::new();
        if self.outstanding.len() >= MAX_OUTSTANDING_ANCHORS {
            // Evict the oldest, which is the closest to settling and therefore
            // the one losing the least. Its matured horizons are preserved;
            // only the unresolved ones carry the capacity censor.
            if let Some(key) = self.outstanding.keys().next().copied() {
                if let Some(entry) = self.take(&key) {
                    self.health.capacity_evictions += 1;
                    self.health.anchors_settled += 1;
                    evicted.push(entry.finish(Some(CensorReason::PendingCapacityReached)));
                }
            }
        }

        let key = (req.anchor_at, self.next_id);
        self.next_id += 1;
        self.by_symbol.entry(req.symbol.clone()).or_default().push(key);
        self.outstanding.insert(key, Outstanding::new(req));
        self.health.anchors_created += 1;
        if self.outstanding.len() > self.health.peak_outstanding {
            self.health.peak_outstanding = self.outstanding.len();
        }
        evicted
    }

    /// Folds one forward price into every outstanding anchor for that symbol.
    ///
    /// O(k) in the anchors for this symbol, never in the whole outstanding set.
    pub fn observe_price(&mut self, symbol: &str, at: DateTime<Utc>, price: f64) {
        let Some(keys) = self.by_symbol.get(symbol) else { return };
        for key in keys {
            if let Some(entry) = self.outstanding.get_mut(key) {
                entry.observe(at, price);
            }
        }
    }

    /// Records what became of the opportunity behind an anchor.
    ///
    /// Provenance only. It never censors and never stops measurement -- the
    /// anchor keeps taking prices for its symbol, which is the entire point of
    /// detaching it from the lifecycle object.
    pub fn note_disposition(&mut self, opportunity_id: &str, disposition: OpportunityDisposition) {
        for entry in self.outstanding.values_mut() {
            if entry.req.opportunity_id == opportunity_id {
                entry.disposition = disposition;
            }
        }
    }

    /// Settles every anchor whose observation window has elapsed.
    ///
    /// Key order is due order, so this touches only the entries being settled.
    pub fn settle_due(&mut self, now: DateTime<Utc>) -> Vec<OpportunityOutcomeRow> {
        let cutoff = now - Duration::seconds(OUTCOME_SETTLE_AFTER_SECS);
        let due: Vec<AnchorKey> =
            self.outstanding.range(..=(cutoff, u64::MAX)).map(|(k, _)| *k).collect();
        let mut out = Vec::with_capacity(due.len());
        for key in due {
            if let Some(entry) = self.take(&key) {
                self.health.anchors_settled += 1;
                out.push(entry.finish(None));
            }
        }
        out
    }

    /// Settles everything still outstanding as `CaptureEnded` -- censored, not
    /// concluded, and never dropped.
    pub fn finish(&mut self, _now: DateTime<Utc>) -> Vec<OpportunityOutcomeRow> {
        let keys: Vec<AnchorKey> = self.outstanding.keys().copied().collect();
        let mut out = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some(entry) = self.take(&key) {
                self.health.anchors_settled += 1;
                out.push(entry.finish(Some(CensorReason::CaptureEnded)));
            }
        }
        out
    }

    /// Removes an anchor from both indexes, so they can never disagree.
    fn take(&mut self, key: &AnchorKey) -> Option<Outstanding> {
        let entry = self.outstanding.remove(key)?;
        if let Some(keys) = self.by_symbol.get_mut(&entry.req.symbol) {
            keys.retain(|k| k != key);
            if keys.is_empty() {
                self.by_symbol.remove(&entry.req.symbol);
            }
        }
        Some(entry)
    }
}

#[cfg(test)]
#[path = "opportunity_outcome_tests.rs"]
mod tests;
