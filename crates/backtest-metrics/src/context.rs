//! Signal-time feature capture — Milestone A's single biggest analytical
//! blocker.
//!
//! `live_signals::PendingSignal` records five fields (symbol, strategy,
//! timestamp, price, captured_at). That is enough to compute a hit rate and
//! nothing else: it cannot answer "did ignition-plus-high-momentum outperform
//! ignition alone", cannot segment by relative volume or catalyst state, and
//! cannot calibrate a ranking — **not even retrospectively from a complete
//! production journal**, because the causing state was never written down.
//!
//! This module is the fix. It is **purely observational**: nothing here feeds
//! a detector, a gate, or a trade decision. It records what the system already
//! knew, at the moment it knew it.
//!
//! # The causality invariant
//!
//! Every field in a `SignalContext` must represent information Stockspotter
//! possessed **at or before** `detected_at`. A feature observed later must
//! never be back-attached. That invariant is what makes these records usable
//! for research at all, and it is enforced structurally by `FeatureCache`:
//! observations are folded in as events arrive, and a snapshot is taken from
//! whatever has accumulated *by* the signal moment. There is no lookup of
//! "current" state at analysis time, because there is no analysis-time state.
//!
//! # Why versioned, not optional-field accretion
//!
//! `schema_version` is written on every record. Readers branch on it rather
//! than inferring capability from which fields happen to be present, so a
//! future change that alters the *meaning* of a field (not just its presence)
//! remains detectable in already-written data.

use chrono::{DateTime, Utc};
use market_data::events::{
    ConsolidationEventKind, ConsolidationStrategy, HaltAlertLevel, IgnitionEventKind, ScanEvent,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::signals::Strategy;

/// Bumped when the *meaning* of an existing field changes, not merely when a
/// new optional field is added.
pub const SIGNAL_CONTEXT_SCHEMA_VERSION: u32 = 1;

/// How stale an observation may be and still be considered part of the state
/// "at" a signal. Beyond this a feature group is recorded as absent rather
/// than as a stale value pretending to be current -- an old momentum reading
/// attached to a fresh ignition would silently corrupt any interaction study.
pub const FEATURE_FRESHNESS_SECS: i64 = 120;

/// Everything Stockspotter knew about one symbol at one signal moment.
///
/// Every feature group is `Option` because detectors genuinely do not all
/// observe every symbol: a universe-tier ignition candidate may never have
/// been scored by the momentum scorer, and fabricating a zero there would be
/// indistinguishable from a real zero score. Absent means *unknown*.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalContext {
    pub schema_version: u32,
    pub symbol: String,
    /// UTC date of the session this signal belongs to. Used as part of the
    /// episode identity, so it is stored rather than derived at read time.
    pub session_date: String,
    pub strategy: Strategy,
    /// Market time of the event that triggered this signal.
    pub detected_at: DateTime<Utc>,
    /// Wall-clock time this record was assembled. Distinct from
    /// `detected_at` for the same reason `PendingSignal::captured_at` is:
    /// a replay can log an hours-old event instantly.
    pub captured_at: DateTime<Utc>,
    pub signal_price: f64,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market: Option<MarketFeatures>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub funnel: Option<FunnelFeatures>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignition: Option<IgnitionFeatures>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub momentum: Option<MomentumFeatures>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consolidation: Option<ConsolidationFeatures>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub halt: Option<HaltFeatures>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalyst: Option<CatalystFeatures>,
    /// Pre-detection price context -- how much of the move had already
    /// happened before we noticed. See `PreDetectionContext`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_detection: Option<PreDetectionContext>,
    /// Episode this signal belongs to (`episode::EpisodeId::as_key`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_id: Option<String>,
}

/// Core market state. `bid`/`ask`/`spread` are optional because the live
/// `ScanEvent` stream carries trades and bars, not quotes -- recording them as
/// unknown is honest; `quote_execution` is where quote-aware analysis lives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketFeatures {
    pub price: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bid: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spread: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spread_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_volume: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap_pct: Option<f64>,
    /// Session classification at `detected_at` (premarket / regular /
    /// after-hours / overnight), as `market_data::classify_session` names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Minutes since 00:00 UTC -- a cheap, timezone-free time-of-day bucket
    /// for segmentation that does not require re-parsing timestamps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minute_of_day_utc: Option<u32>,
}

/// The funnel's own gate booleans plus the quantities behind them. Recorded
/// even on a rejection: a near-miss is exactly the sample a recall study
/// needs.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunnelFeatures {
    pub price: f64,
    pub gap_pct: f64,
    pub session_volume: u64,
    pub price_ok: bool,
    pub float_ok: bool,
    pub rel_vol_ok: bool,
    pub gap_ok: bool,
    pub passed: bool,
}

/// Ignition detector state. The live `IgnitionEvent` carries only its phase,
/// not the internal feature values (compression ratio, trade-frequency
/// acceleration, spread tightening, ask absorption) -- those live inside the
/// detector and are not on the wire. `phase` and the observed counts are what
/// is genuinely knowable from the event stream today; the richer fields are
/// deliberately absent rather than invented. See the report's measurement-gap
/// section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IgnitionFeatures {
    /// Most recent phase seen for this symbol in this episode.
    pub phase: IgnitionPhase,
    /// How many candidates opened for this symbol this session before this
    /// moment -- "is this the first attempt or the fourth" is a real feature.
    pub candidates_opened: u32,
    pub confirmations: u32,
    pub rejections: u32,
    pub price_at_phase: f64,
    pub phase_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IgnitionPhase {
    CandidateOpened,
    FollowThroughConfirmed,
    FollowThroughRejected,
}

impl From<IgnitionEventKind> for IgnitionPhase {
    fn from(kind: IgnitionEventKind) -> Self {
        match kind {
            IgnitionEventKind::CandidateOpened => Self::CandidateOpened,
            IgnitionEventKind::FollowThroughConfirmed => Self::FollowThroughConfirmed,
            IgnitionEventKind::FollowThroughRejected => Self::FollowThroughRejected,
        }
    }
}

/// The continuous momentum score and its four components.
///
/// This is the most valuable single group in the schema. Production computes
/// `overall` and then reduces it to a boolean at the 0.60 gate, discarding the
/// ordering information -- which is why Milestone A found ranking quality
/// unmeasurable. Persisting the continuous value costs nothing and does not
/// change the gate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentumFeatures {
    pub overall: f64,
    pub volume_confirmation: f64,
    pub structure: f64,
    pub ma_slope: f64,
    pub wick_rejection: f64,
    /// The existing 0.60 gate's verdict, exactly as production computed it.
    /// Stored rather than recomputed so a future threshold change cannot
    /// retroactively alter what old records claim production decided.
    pub qualifies: bool,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsolidationFeatures {
    pub strategy: ConsolidationVariant,
    pub phase: ConsolidationPhase,
    pub price_at_phase: f64,
    pub phase_at: DateTime<Utc>,
    /// Surge -> consolidation -> breakout progress counters for this episode.
    pub surges: u32,
    pub confirmations: u32,
    pub entries: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsolidationVariant {
    ConsolidationBreakout,
    Micropullback,
}

impl From<ConsolidationStrategy> for ConsolidationVariant {
    fn from(strategy: ConsolidationStrategy) -> Self {
        match strategy {
            ConsolidationStrategy::ConsolidationBreakout => Self::ConsolidationBreakout,
            ConsolidationStrategy::Micropullback => Self::Micropullback,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsolidationPhase {
    SurgeDetected,
    ConsolidationConfirmed,
    EntryTriggered,
}

impl From<ConsolidationEventKind> for ConsolidationPhase {
    fn from(kind: ConsolidationEventKind) -> Self {
        match kind {
            ConsolidationEventKind::SurgeDetected => Self::SurgeDetected,
            ConsolidationEventKind::ConsolidationConfirmed => Self::ConsolidationConfirmed,
            ConsolidationEventKind::EntryTriggered => Self::EntryTriggered,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HaltFeatures {
    pub level: HaltLevel,
    pub proximity_ratio: f64,
    pub reference_price: f64,
    pub current_price: f64,
    pub band_width_dollars: f64,
    pub band_doubled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_volume: Option<f64>,
    pub luld_in_effect: bool,
    /// True when the bands are estimated rather than official -- a materially
    /// different confidence level, and already on the wire.
    pub estimated_bands: bool,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HaltLevel {
    Calm,
    Amber,
    Red,
}

impl From<HaltAlertLevel> for HaltLevel {
    fn from(level: HaltAlertLevel) -> Self {
        match level {
            HaltAlertLevel::Calm => Self::Calm,
            HaltAlertLevel::Amber => Self::Amber,
            HaltAlertLevel::Red => Self::Red,
        }
    }
}

/// Catalyst state, with the timestamp distinction Milestone A demanded.
///
/// `observed_at` is when *Stockspotter* learned this (the qualify response
/// arriving), and it is the field that governs causality: a catalyst may only
/// be attached to a signal whose `detected_at` is at or after it.
///
/// `most_recent_published_at` is when the newest underlying headline was
/// published, straight from Alpaca's `created_at`. It is strictly less useful
/// for causality (we did not know it at publication time) and strictly more
/// useful for interpretation -- "was there fresh news, or is this a tag from a
/// three-week-old headline?".
///
/// That question matters: the upstream lookup requests the 10 most recent
/// items with **no time window**, so a catalyst tag carries no inherent
/// recency guarantee. `headline_age_secs` makes that visible instead of
/// leaving every analysis to assume freshness.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalystFeatures {
    pub tags: Vec<String>,
    pub headline_count: u32,
    /// When Stockspotter received this catalyst lookup. Causality is judged
    /// against this, never against publication time.
    pub observed_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub most_recent_published_at: Option<DateTime<Utc>>,
    /// `observed_at - most_recent_published_at`, when both are known. Large
    /// values mean the tag reflects stale news.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headline_age_secs: Option<i64>,
}

/// Bounded pre-detection price context, for earliness analysis.
///
/// Deliberately a handful of reference points rather than a tick history: the
/// question is "how much of the move preceded us", which a few anchors answer
/// at a fraction of the storage. Each is `Option` because a symbol that only
/// entered monitoring seconds ago genuinely has no five-minutes-ago price, and
/// interpolating one would invent data.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreDetectionContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_1m_before: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_3m_before: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_5m_before: Option<f64>,
    /// Lowest price observed for this symbol since monitoring began this
    /// session -- the practical baseline a "% of move already completed"
    /// calculation needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_low_observed: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_observed_price: Option<f64>,
    pub first_observed_at: DateTime<Utc>,
    /// Move from `first_observed_price` to the signal price, as a percentage.
    /// Compared against subsequent MFE, this is the earliness number:
    /// a +20% runner first detected after +17% is not the same opportunity as
    /// one detected after +3%.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_before_detection_pct: Option<f64>,
}

/// Rolling per-symbol observation state, from which snapshots are cut.
///
/// This is the structural guarantee behind the causality invariant: it only
/// ever moves forward, folding in events as they arrive. A snapshot reflects
/// what had accumulated by that instant, so a later observation cannot appear
/// in an earlier record.
#[derive(Debug, Default)]
pub struct FeatureCache {
    symbols: HashMap<String, SymbolState>,
}

#[derive(Debug, Clone, Default)]
struct SymbolState {
    market: Option<MarketFeatures>,
    funnel: Option<FunnelFeatures>,
    ignition: Option<IgnitionFeatures>,
    momentum: Option<MomentumFeatures>,
    consolidation: Option<ConsolidationFeatures>,
    halt: Option<HaltFeatures>,
    catalyst: Option<CatalystFeatures>,
    /// (observed_at, price), oldest first, bounded to `PRICE_TRAIL_CAP`.
    price_trail: Vec<(DateTime<Utc>, f64)>,
    first_observed: Option<(DateTime<Utc>, f64)>,
    session_low: Option<f64>,
    ignition_candidates: u32,
    ignition_confirmations: u32,
    ignition_rejections: u32,
    consolidation_surges: u32,
    consolidation_confirmations: u32,
    consolidation_entries: u32,
}

/// Enough anchors to answer 1/3/5-minutes-before without unbounded growth.
/// At the observed live cadence this is a few seconds of trail per symbol,
/// which is all the pre-detection questions require.
const PRICE_TRAIL_CAP: usize = 512;
/// Trail entries older than this are dropped -- the anchors of interest are
/// within five minutes, so retaining more is pure cost.
const PRICE_TRAIL_MAX_AGE_SECS: i64 = 400;

impl FeatureCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tracked_symbols(&self) -> usize {
        self.symbols.len()
    }

    /// Most recent finite price observed for `symbol`, if any. Events such as
    /// `MomentumUpdate` and `CatalystUpdate` carry no price of their own; this
    /// is how a consumer resolves one without inventing a value.
    pub fn last_price(&self, symbol: &str) -> Option<f64> {
        self.symbols
            .get(symbol)
            .and_then(|s| s.price_trail.last())
            .map(|(_, p)| *p)
    }

    /// Folds one event into per-symbol state. Purely additive; never emits,
    /// gates, or decides anything.
    pub fn observe(&mut self, event: &ScanEvent) {
        match event {
            ScanEvent::FunnelSignal {
                symbol, timestamp, price, gap_pct, session_volume,
                price_ok, float_ok, rel_vol_ok, gap_ok, passed,
            } => {
                let state = self.entry(symbol);
                state.note_price(*timestamp, *price);
                state.funnel = Some(FunnelFeatures {
                    price: *price,
                    gap_pct: *gap_pct,
                    session_volume: *session_volume,
                    price_ok: *price_ok,
                    float_ok: *float_ok,
                    rel_vol_ok: *rel_vol_ok,
                    gap_ok: *gap_ok,
                    passed: *passed,
                });
                state.market = Some(MarketFeatures {
                    price: *price,
                    bid: None,
                    ask: None,
                    spread: None,
                    spread_pct: None,
                    session_volume: Some(*session_volume),
                    gap_pct: Some(*gap_pct),
                    session: None,
                    minute_of_day_utc: Some(minute_of_day_utc(*timestamp)),
                });
            }
            ScanEvent::MomentumUpdate {
                symbol, timestamp, volume_confirmation, structure, ma_slope,
                wick_rejection, overall, qualifies,
            } => {
                let state = self.entry(symbol);
                state.momentum = Some(MomentumFeatures {
                    overall: *overall,
                    volume_confirmation: *volume_confirmation,
                    structure: *structure,
                    ma_slope: *ma_slope,
                    wick_rejection: *wick_rejection,
                    qualifies: *qualifies,
                    observed_at: *timestamp,
                });
            }
            ScanEvent::IgnitionEvent { symbol, timestamp, price, kind } => {
                let state = self.entry(symbol);
                state.note_price(*timestamp, *price);
                match kind {
                    IgnitionEventKind::CandidateOpened => state.ignition_candidates += 1,
                    IgnitionEventKind::FollowThroughConfirmed => state.ignition_confirmations += 1,
                    IgnitionEventKind::FollowThroughRejected => state.ignition_rejections += 1,
                }
                state.ignition = Some(IgnitionFeatures {
                    phase: (*kind).into(),
                    candidates_opened: state.ignition_candidates,
                    confirmations: state.ignition_confirmations,
                    rejections: state.ignition_rejections,
                    price_at_phase: *price,
                    phase_at: *timestamp,
                });
            }
            ScanEvent::ConsolidationEvent { symbol, timestamp, price, kind, strategy } => {
                let state = self.entry(symbol);
                state.note_price(*timestamp, *price);
                match kind {
                    ConsolidationEventKind::SurgeDetected => state.consolidation_surges += 1,
                    ConsolidationEventKind::ConsolidationConfirmed => state.consolidation_confirmations += 1,
                    ConsolidationEventKind::EntryTriggered => state.consolidation_entries += 1,
                }
                state.consolidation = Some(ConsolidationFeatures {
                    strategy: (*strategy).into(),
                    phase: (*kind).into(),
                    price_at_phase: *price,
                    phase_at: *timestamp,
                    surges: state.consolidation_surges,
                    confirmations: state.consolidation_confirmations,
                    entries: state.consolidation_entries,
                });
            }
            ScanEvent::HaltWarning {
                symbol, timestamp, reference_price, current_price, band_width_dollars,
                band_doubled, proximity_ratio, relative_volume, level, luld_in_effect,
                estimated_bands,
            } => {
                let state = self.entry(symbol);
                state.note_price(*timestamp, *current_price);
                state.halt = Some(HaltFeatures {
                    level: (*level).into(),
                    proximity_ratio: *proximity_ratio,
                    reference_price: *reference_price,
                    current_price: *current_price,
                    band_width_dollars: *band_width_dollars,
                    band_doubled: *band_doubled,
                    relative_volume: *relative_volume,
                    luld_in_effect: *luld_in_effect,
                    estimated_bands: *estimated_bands,
                    observed_at: *timestamp,
                });
            }
            ScanEvent::CatalystUpdate {
                symbol, timestamp, catalyst_tags, headline_count,
                most_recent_published_at, most_recent_headline: _,
            } => {
                let state = self.entry(symbol);
                // `timestamp` is the moment the qualify response was received
                // -- observation time, not publication time. That is exactly
                // the field causality must be judged against, so it is stored
                // as `observed_at`. Publication time is recorded alongside it
                // for recency interpretation, never for causality.
                let age = most_recent_published_at
                    .map(|published| (*timestamp - published).num_seconds());
                state.catalyst = Some(CatalystFeatures {
                    tags: catalyst_tags.clone(),
                    headline_count: *headline_count,
                    observed_at: *timestamp,
                    most_recent_published_at: *most_recent_published_at,
                    headline_age_secs: age,
                });
            }
            ScanEvent::BarUpdate { symbol, timestamp, close, .. } => {
                self.entry(symbol).note_price(*timestamp, *close);
            }
            ScanEvent::FunnelHealth { .. } => {}
        }
    }

    fn entry(&mut self, symbol: &str) -> &mut SymbolState {
        self.symbols.entry(symbol.to_string()).or_default()
    }

    /// Cuts a snapshot of everything known about `symbol` **by** `detected_at`.
    ///
    /// Feature groups whose own observation time is after `detected_at`, or
    /// older than `FEATURE_FRESHNESS_SECS`, are omitted rather than included
    /// stale. Omission means "unknown", which is the honest answer.
    pub fn snapshot(
        &self,
        symbol: &str,
        strategy: Strategy,
        detected_at: DateTime<Utc>,
        captured_at: DateTime<Utc>,
        signal_price: f64,
    ) -> SignalContext {
        let state = self.symbols.get(symbol);
        let fresh = |observed: DateTime<Utc>| -> bool {
            observed <= detected_at
                && (detected_at - observed).num_seconds() <= FEATURE_FRESHNESS_SECS
        };
        SignalContext {
            schema_version: SIGNAL_CONTEXT_SCHEMA_VERSION,
            symbol: symbol.to_string(),
            session_date: detected_at.date_naive().to_string(),
            strategy,
            detected_at,
            captured_at,
            signal_price,
            market: state.and_then(|s| s.market.clone()),
            funnel: state.and_then(|s| s.funnel),
            ignition: state
                .and_then(|s| s.ignition.clone())
                .filter(|f| fresh(f.phase_at)),
            momentum: state.and_then(|s| s.momentum).filter(|f| fresh(f.observed_at)),
            consolidation: state
                .and_then(|s| s.consolidation)
                .filter(|f| fresh(f.phase_at)),
            halt: state.and_then(|s| s.halt).filter(|f| fresh(f.observed_at)),
            catalyst: state
                .and_then(|s| s.catalyst.clone())
                // Strictly `<=`: a catalyst learned after the signal cannot
                // have informed it. This is the retroactive-attachment guard.
                .filter(|f| f.observed_at <= detected_at),
            pre_detection: state.map(|s| s.pre_detection(detected_at, signal_price)),
            episode_id: None,
        }
    }
}

impl SymbolState {
    fn note_price(&mut self, at: DateTime<Utc>, price: f64) {
        if !price.is_finite() || price <= 0.0 {
            return;
        }
        if self.first_observed.is_none() {
            self.first_observed = Some((at, price));
        }
        self.session_low = Some(match self.session_low {
            Some(low) if low <= price => low,
            _ => price,
        });
        self.price_trail.push((at, price));
        let cutoff = at - chrono::Duration::seconds(PRICE_TRAIL_MAX_AGE_SECS);
        self.price_trail.retain(|(t, _)| *t >= cutoff);
        if self.price_trail.len() > PRICE_TRAIL_CAP {
            let excess = self.price_trail.len() - PRICE_TRAIL_CAP;
            self.price_trail.drain(0..excess);
        }
    }

    /// Most recent observation at or before `at - back_secs`. Returns `None`
    /// rather than the nearest available price: a symbol observed for only
    /// ten seconds has no one-minute-ago price, and substituting the oldest
    /// value would silently understate how much move preceded detection.
    fn price_at_or_before(&self, at: DateTime<Utc>, back_secs: i64) -> Option<f64> {
        let target = at - chrono::Duration::seconds(back_secs);
        self.price_trail
            .iter()
            .rev()
            .find(|(t, _)| *t <= target)
            .map(|(_, p)| *p)
    }

    fn pre_detection(&self, detected_at: DateTime<Utc>, signal_price: f64) -> PreDetectionContext {
        let first = self.first_observed;
        let move_pct = first.and_then(|(_, first_price)| {
            (first_price > 0.0).then(|| (signal_price - first_price) / first_price * 100.0)
        });
        PreDetectionContext {
            price_1m_before: self.price_at_or_before(detected_at, 60),
            price_3m_before: self.price_at_or_before(detected_at, 180),
            price_5m_before: self.price_at_or_before(detected_at, 300),
            session_low_observed: self.session_low,
            first_observed_price: first.map(|(_, p)| p),
            first_observed_at: first.map(|(t, _)| t).unwrap_or(detected_at),
            move_before_detection_pct: move_pct,
        }
    }
}

fn minute_of_day_utc(at: DateTime<Utc>) -> u32 {
    use chrono::Timelike;
    at.hour() * 60 + at.minute()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_757_000_000 + secs, 0).unwrap()
    }

    fn momentum(symbol: &str, t: DateTime<Utc>, overall: f64, qualifies: bool) -> ScanEvent {
        ScanEvent::MomentumUpdate {
            symbol: symbol.into(),
            timestamp: t,
            volume_confirmation: 0.8,
            structure: 0.7,
            ma_slope: 0.6,
            wick_rejection: 0.5,
            overall,
            qualifies,
        }
    }

    fn ignition(symbol: &str, t: DateTime<Utc>, price: f64, kind: IgnitionEventKind) -> ScanEvent {
        ScanEvent::IgnitionEvent { symbol: symbol.into(), timestamp: t, price, kind }
    }

    #[test]
    fn momentum_score_and_every_factor_round_trip_exactly() {
        let mut cache = FeatureCache::new();
        cache.observe(&momentum("AAA", at(0), 0.6123456789, true));
        let snap = cache.snapshot("AAA", Strategy::IgnitionDetector, at(1), at(1), 10.0);
        let m = snap.momentum.expect("momentum should be captured");
        assert_eq!(m.overall, 0.6123456789);
        assert_eq!(m.volume_confirmation, 0.8);
        assert_eq!(m.structure, 0.7);
        assert_eq!(m.ma_slope, 0.6);
        assert_eq!(m.wick_rejection, 0.5);
        assert!(m.qualifies);

        let json = serde_json::to_string(&snap).unwrap();
        let back: SignalContext = serde_json::from_str(&json).unwrap();
        assert_eq!(back, snap, "context must survive a JSON round trip exactly");
    }

    #[test]
    fn a_snapshot_cannot_contain_information_observed_after_its_timestamp() {
        let mut cache = FeatureCache::new();
        cache.observe(&momentum("AAA", at(100), 0.9, true));
        // Signal at t=50; the momentum reading is from t=100 -- the future.
        let snap = cache.snapshot("AAA", Strategy::IgnitionDetector, at(50), at(50), 10.0);
        assert!(
            snap.momentum.is_none(),
            "a future observation must never appear in an earlier snapshot"
        );
    }

    #[test]
    fn a_later_catalyst_is_never_attached_retroactively() {
        let mut cache = FeatureCache::new();
        cache.observe(&ScanEvent::CatalystUpdate {
            symbol: "AAA".into(),
            timestamp: at(200),
            catalyst_tags: vec!["earnings".into()],
            headline_count: 3,
            most_recent_headline: Some("Q3 beat".into()),
            most_recent_published_at: None,
        });
        let before = cache.snapshot("AAA", Strategy::IgnitionDetector, at(100), at(100), 10.0);
        assert!(before.catalyst.is_none(), "catalyst learned at t=200 cannot inform a t=100 signal");

        let after = cache.snapshot("AAA", Strategy::IgnitionDetector, at(300), at(300), 10.0);
        let c = after.catalyst.expect("catalyst known by t=300 should attach");
        assert_eq!(c.observed_at, at(200));
        assert_eq!(c.tags, vec!["earnings".to_string()]);
    }

    #[test]
    fn catalyst_recency_is_recorded_separately_from_observation_time() {
        // The upstream lookup takes the 10 newest items with no time window,
        // so "has a catalyst" says nothing about freshness on its own.
        let mut cache = FeatureCache::new();
        let published = at(-86_400); // a day before we looked
        cache.observe(&ScanEvent::CatalystUpdate {
            symbol: "AAA".into(),
            timestamp: at(0),
            catalyst_tags: vec!["offering_dilution".into()],
            headline_count: 10,
            most_recent_headline: Some("Shelf offering".into()),
            most_recent_published_at: Some(published),
        });
        let snap = cache.snapshot("AAA", Strategy::IgnitionDetector, at(10), at(10), 3.0);
        let c = snap.catalyst.expect("catalyst known by t=10");
        assert_eq!(c.observed_at, at(0), "causality uses observation time");
        assert_eq!(c.most_recent_published_at, Some(published));
        assert_eq!(
            c.headline_age_secs,
            Some(86_400),
            "a day-old headline must be visibly day-old, not implicitly fresh"
        );
    }

    #[test]
    fn stale_features_are_recorded_as_unknown_rather_than_current() {
        let mut cache = FeatureCache::new();
        cache.observe(&momentum("AAA", at(0), 0.9, true));
        let snap = cache.snapshot(
            "AAA",
            Strategy::IgnitionDetector,
            at(FEATURE_FRESHNESS_SECS + 10),
            at(FEATURE_FRESHNESS_SECS + 10),
            10.0,
        );
        assert!(snap.momentum.is_none(), "a two-minute-old score is not current state");
    }

    #[test]
    fn pre_detection_anchors_answer_how_much_move_preceded_detection() {
        let mut cache = FeatureCache::new();
        // A move from 10.00 to 12.00 over five minutes.
        cache.observe(&ignition("AAA", at(0), 10.0, IgnitionEventKind::CandidateOpened));
        cache.observe(&ignition("AAA", at(120), 10.5, IgnitionEventKind::CandidateOpened));
        cache.observe(&ignition("AAA", at(240), 11.0, IgnitionEventKind::CandidateOpened));
        cache.observe(&ignition("AAA", at(300), 12.0, IgnitionEventKind::FollowThroughConfirmed));

        let snap = cache.snapshot("AAA", Strategy::IgnitionDetector, at(300), at(300), 12.0);
        let pre = snap.pre_detection.expect("pre-detection context");
        assert_eq!(pre.first_observed_price, Some(10.0));
        assert_eq!(pre.session_low_observed, Some(10.0));
        assert_eq!(pre.price_5m_before, Some(10.0));
        assert_eq!(pre.price_3m_before, Some(10.5));
        assert_eq!(pre.price_1m_before, Some(11.0));
        let moved = pre.move_before_detection_pct.unwrap();
        assert!((moved - 20.0).abs() < 1e-9, "20% preceded detection, got {moved}");
    }

    #[test]
    fn a_missing_anchor_is_absent_rather_than_substituted() {
        let mut cache = FeatureCache::new();
        cache.observe(&ignition("AAA", at(0), 10.0, IgnitionEventKind::CandidateOpened));
        let snap = cache.snapshot("AAA", Strategy::IgnitionDetector, at(10), at(10), 10.2);
        let pre = snap.pre_detection.unwrap();
        assert_eq!(pre.price_1m_before, None, "only 10s of history exists; do not invent a price");
        assert_eq!(pre.price_5m_before, None);
        assert_eq!(pre.first_observed_price, Some(10.0));
    }

    #[test]
    fn detector_counters_accumulate_within_a_symbol() {
        let mut cache = FeatureCache::new();
        cache.observe(&ignition("AAA", at(0), 10.0, IgnitionEventKind::CandidateOpened));
        cache.observe(&ignition("AAA", at(10), 10.1, IgnitionEventKind::FollowThroughRejected));
        cache.observe(&ignition("AAA", at(20), 10.4, IgnitionEventKind::CandidateOpened));
        cache.observe(&ignition("AAA", at(30), 10.9, IgnitionEventKind::FollowThroughConfirmed));
        let snap = cache.snapshot("AAA", Strategy::IgnitionDetector, at(30), at(30), 10.9);
        let ig = snap.ignition.unwrap();
        assert_eq!(ig.candidates_opened, 2);
        assert_eq!(ig.rejections, 1);
        assert_eq!(ig.confirmations, 1);
        assert_eq!(ig.phase, IgnitionPhase::FollowThroughConfirmed);
    }

    #[test]
    fn a_legacy_pending_signal_record_still_deserializes() {
        // Representative of what is already on the VPS: the five-field shape
        // that predates every field this milestone adds. It must keep loading.
        let legacy = r#"{"symbol":"AAA","strategy":"IgnitionDetector",
            "timestamp":"2026-09-03T14:30:00Z","signal_price":1.23,
            "captured_at":"2026-09-03T14:30:01Z"}"#;
        let parsed: crate::live_signals::PendingSignal =
            serde_json::from_str(legacy).expect("legacy record must still parse");
        assert_eq!(parsed.symbol, "AAA");
        assert_eq!(parsed.signal_price, 1.23);
    }

    #[test]
    fn a_context_missing_every_optional_group_deserializes() {
        // A minimal future/partial writer must not break readers.
        let minimal = r#"{"schemaVersion":1,"symbol":"AAA","sessionDate":"2026-09-03",
            "strategy":"FastFunnel","detectedAt":"2026-09-03T14:30:00Z",
            "capturedAt":"2026-09-03T14:30:00Z","signalPrice":1.5}"#;
        let parsed: SignalContext = serde_json::from_str(minimal).expect("must parse");
        assert!(parsed.momentum.is_none());
        assert!(parsed.catalyst.is_none());
        assert_eq!(parsed.schema_version, 1);
    }

    #[test]
    fn an_unobserved_symbol_yields_unknowns_not_zeros() {
        let cache = FeatureCache::new();
        let snap = cache.snapshot("ZZZ", Strategy::FastFunnel, at(0), at(0), 5.0);
        assert!(snap.momentum.is_none());
        assert!(snap.funnel.is_none());
        assert!(snap.ignition.is_none());
        assert!(snap.pre_detection.is_none());
        assert_eq!(snap.schema_version, SIGNAL_CONTEXT_SCHEMA_VERSION);
    }

    #[test]
    fn the_price_trail_stays_bounded_under_sustained_observation() {
        let mut cache = FeatureCache::new();
        for n in 0..5_000i64 {
            cache.observe(&ignition("AAA", at(n), 10.0 + n as f64 * 0.001, IgnitionEventKind::CandidateOpened));
        }
        let state = cache.symbols.get("AAA").unwrap();
        assert!(
            state.price_trail.len() <= PRICE_TRAIL_CAP,
            "trail must stay bounded, saw {}",
            state.price_trail.len()
        );
    }
}
