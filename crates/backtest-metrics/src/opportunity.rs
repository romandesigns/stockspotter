//! Opportunity Intelligence V1 — a **shadow research layer** that sits *after*
//! broad detection and never influences it.
//!
//! # Why this exists, and why it is not `EpisodeTracker`
//!
//! `episode::EpisodeTracker` is the measurement unit and stays exactly as it is.
//! It closes an episode on `IgnitionEventKind::FollowThroughRejected` (its
//! Rule 4), which is correct for *measurement*: the detector itself declared
//! that attempt over. But ignition emitted **2,648,711 rejections** in Analysis
//! Baseline 001, so a single developing move that goes
//! confirm → reject → confirm is recorded as several episodes with
//! `sequence + 1` each.
//!
//! That is exactly the inflation observed in the baseline: **129,655 regular-
//! session detector episodes collapse to ~67,032 distinct symbol/move
//! opportunities**. Ranking detector episodes therefore ranks the same move
//! repeatedly, and any per-episode precision figure counts one opportunity many
//! times.
//!
//! An `Opportunity` is the *rankable* unit. It differs from an episode in
//! exactly one rule: **invalidation does not end an opportunity.** Only
//! inactivity (`OiConfig::inactivity_secs`, defaulting to the project's
//! existing 300s) and a session boundary do. Everything else — identity,
//! sequencing, causality, the feature snapshot — deliberately mirrors
//! `EpisodeTracker` so the two remain comparable.
//!
//! # Hard boundaries
//!
//! * Nothing here is read by a detector, by client ordering, or by
//!   `auto_trader`. It is an independent consumer of the already-broadcast
//!   `ScanEvent` stream.
//! * No score, regime or rank uses information from after its own timestamp.
//!   `Opportunity::score_at` takes only the state accumulated so far.
//! * Unknown is `None`, never `0.0`. Every score carries the list of features
//!   that were missing when it was computed.
//! * Every output carries the versions and the configuration fingerprint that
//!   produced it, so a result is always attributable.
//!
//! # A caution the evidence demands
//!
//! The transparent V1 scores below were informed by descriptive findings from
//! Analysis Baseline 001. **September 14 is therefore development evidence, not
//! independent validation.** Scoring V1 against the session that motivated it
//! measures nothing about generalisation. See the milestone report's §26.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, Utc};
use market_data::events::{ConsolidationEventKind, IgnitionEventKind, ScanEvent};
use serde::{Deserialize, Serialize};

use crate::context::{FeatureCache, SignalContext};
use crate::signals::Strategy;

// ---------------------------------------------------------------------------
// Versions -- §17 demands every artifact be attributable to what produced it.
// ---------------------------------------------------------------------------

/// Bumped 1 -> 2 for the V2.1 correctness repair. Two changes force it, and
/// the first is decisive under this repo's own rule (bump when the *meaning*
/// of an existing field changes, not merely when an optional field is added):
///
///  1. `OpportunityId::sequence` changed meaning, from a per-process ordinal
///     to milliseconds-since-UTC-midnight of `opened_at`. Same type, same
///     position, different semantics -- exactly the case the rule names. A
///     reader that assumes "1 means the first opportunity of the day" is
///     wrong against a version-2 artifact and must be able to tell.
///  2. Six causal fields were added (`observedHigh`, `observedLow`,
///     `maxMovePct`, `minMovePct`, `openingPrice`, `openedAt`). These are
///     additive and optional, so on their own they would NOT justify a bump.
///
/// Version 2 therefore means: sequence is time-derived and ids are unique
/// across tracker lifetimes, and the risk/identity fields are present.
/// Version 1 artifacts remain fully parseable -- every new field is optional.
pub const OPPORTUNITY_SCHEMA_VERSION: u32 = 2;
/// 2 as of the Phase E review, not 1.
///
/// Version 1 emitted a `features` surface that was captured once, when the
/// opportunity opened, and never refreshed -- despite the field being named and
/// documented as the *latest* snapshot. On a real replay that left 25 of 31
/// ranking rows with no momentum inputs at all, so the early-quality score was
/// unavailable for 80% of the cohort: precisely the continuous information §3
/// exists to preserve, reduced to absence by a wiring defect.
///
/// Version 2 refreshes `features` on every observation and preserves the
/// detection-time surface separately as `detection_features`. Bumped rather
/// than silently corrected because the two versions mean different things by
/// the same field name (§24: "Version/add rather than silently reinterpret").
///
/// **3 as of 2026-09-25** (measurement-correctness contract, D3 + D7a). The
/// `features`/`detectionFeatures` surface is a `SignalContext`, whose schema
/// went 1 -> 2: every pre-detection baseline, the ignition/consolidation
/// counters and the `funnel`/`market` groups are now scoped to the market day
/// (04:00 ET) instead of the process lifetime. Same field names, different
/// meaning -- so the OI surface that carries them moves too, and a pinned
/// `expected_feature_schema` cannot silently accept a v2 session. Scores,
/// weights, thresholds, regimes and the ranking rule are untouched; what moves
/// is the value of the prior-move INPUT they read (`moveBeforeDetectionPct`),
/// which was measuring against an arbitrary earlier day.
///
/// `OPPORTUNITY_SCHEMA_VERSION` deliberately does NOT move: opportunity
/// identity and lifecycle are unchanged, and that bump is reserved for the
/// lifecycle-unit change (D5). `OiVersions::baseline_policy` makes every row
/// self-declare the baseline contract in addition to this number.
pub const OI_FEATURE_SCHEMA_VERSION: u32 = 3;
pub const REGIME_CLASSIFIER_VERSION: &str = "regime-v1";
pub const PRICE_REGIME_VERSION: &str = "price-regime-v1";
pub const EARLY_QUALITY_MODEL_VERSION: &str = "early-quality-v1-transparent";
pub const CONTINUATION_MODEL_VERSION: &str = "continuation-v1-transparent";
pub const RANKING_VERSION: &str = "opportunity-rank-v1";
/// Score *comparability* policy, versioned separately from the models.
///
/// The feature sets and weights of both V1 models are unchanged -- what changed
/// is how a partially-observed candidate's score is made comparable to a
/// fully-observed one. Bumping a model version would have implied the model
/// itself moved, which it did not; an analyst needs to distinguish those.
///
/// v1 (implicit) summed `transformed x weight` over present features only, so
/// an absent feature contributed 0. That is not neutral: 0 is at or below the
/// floor of every transform in both models, and strictly below the observable
/// floor of `continuation.priorMovePct` (0.10). Summing therefore imputed the
/// worst observable value for anything unmeasured, and capped a candidate's
/// attainable score at its coverage -- so feature availability partly
/// determined rank. Recording the missingness did not remove that.
pub const SCORE_POLICY_VERSION: &str = "score-policy-v2-core-gated-coverage-normalized";

// ---------------------------------------------------------------------------
// Research configuration (§22) -- deliberately separate from any production
// strategy threshold, so editing it cannot change detector behaviour.
// ---------------------------------------------------------------------------

/// Research-only configuration. **No field here is read by production.**
///
/// Boundary values are *configurable starting points for measurement*, not
/// production thresholds and not conclusions. They were seeded from the
/// baseline's descriptive buckets purely so the first comparative run has
/// somewhere to start; §26 forbids reading any V1 result on that session as
/// validation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OiConfig {
    /// Opportunity inactivity boundary. Defaults to the project's existing
    /// 300s (`INACTIVITY_TIMEOUT_SECS`, `UNIVERSE_MONITOR_IDLE_SECS`,
    /// `QUIET_TRANSITION_GRACE`) rather than inventing a fourth timeout.
    pub inactivity_secs: i64,
    /// Upper bound on `move_before_detection_pct` for `EarlyEmerging`.
    pub early_max_prior_move_pct: f64,
    /// Lower bound on prior move for `ContinuationAcceleration`.
    pub continuation_min_prior_move_pct: f64,
    /// Prior move at or below this (negative) enters `ReversalRecovery`
    /// territory, provided the opportunity is currently recovering.
    pub reversal_max_prior_move_pct: f64,
    /// Ascending price-band upper bounds, in dollars. A price above the last
    /// bound lands in the final open-ended band.
    pub price_regime_bounds: Vec<f64>,
    /// Ranking cadence. Defaults to the existing 30s research cadence.
    pub ranking_cadence_secs: i64,
    /// Supported opportunity-open rate, in hundredths per second. One of the
    /// two inputs to the flow bound. See `max_open_opportunities`.
    pub supported_open_rate_centi: u64,
    /// Supported mean opportunity *lifetime*, in seconds.
    ///
    /// **Not `inactivity_secs`, and that distinction is the whole repair.**
    /// An opportunity does not close 300s after it opens; it closes 300s after
    /// it last saw an event, and a symbol that keeps trading keeps refreshing
    /// it. The population is therefore `rate x lifetime` (Little's Law), not
    /// `rate x inactivity`. See `max_open_opportunities` for the measurement.
    #[serde(default = "default_supported_lifetime_secs")]
    pub supported_lifetime_secs: i64,
    /// Supported distinct-symbol universe, the engine's *structural* bound.
    ///
    /// `OpportunityIntelligence::open` is keyed by symbol, so it can hold at
    /// most one opportunity per symbol no matter how heavy the event stream
    /// gets. Capacity at or above the universe makes eviction impossible by
    /// construction rather than merely unlikely.
    #[serde(default = "default_supported_symbol_universe")]
    pub supported_symbol_universe: usize,
    /// Safety factor numerator/denominator applied to the derived bound.
    pub bound_safety_num: u64,
    pub bound_safety_den: u64,
    /// Bounded per-opportunity history (detector arrivals are a BTreeMap and
    /// naturally bounded by the strategy count; this bounds the price track).
    pub max_history_per_opportunity: usize,
    /// Bounded ranking cohort.
    pub max_rank_cohort: usize,
}

/// Supported opportunity-open rate, hundredths per second (10.00/s).
///
/// Unchanged by the capacity repair: at 11.7x the 0.853/s observed on
/// September 16 it was never the part that was wrong.
const DEFAULT_SUPPORTED_OPEN_RATE_CENTI: u64 = 1_000;

/// Supported mean opportunity lifetime, seconds.
///
/// Measured mean over the September-16 regular session was 3,752s; 4,800s
/// carries 1.28x on top of it. This is the quantity the previous derivation
/// got wrong by substituting `inactivity_secs` (300s) for it.
const DEFAULT_SUPPORTED_LIFETIME_SECS: i64 = 4_800;

/// Supported distinct-symbol universe.
///
/// The September-16 scanner reported a universe of 12,962-13,001 symbols on
/// every one of its 5,266 scans (`scan_started.universe`). 13,100 covers the
/// largest observed with a little room for new listings; the safety factor
/// supplies the rest.
const DEFAULT_SUPPORTED_SYMBOL_UNIVERSE: usize = 13_100;

/// Headroom beyond the binding requirement (5/4 = 1.25).
const DEFAULT_BOUND_SAFETY_NUM: u64 = 5;
const DEFAULT_BOUND_SAFETY_DEN: u64 = 4;

fn default_supported_lifetime_secs() -> i64 {
    DEFAULT_SUPPORTED_LIFETIME_SECS
}
fn default_supported_symbol_universe() -> usize {
    DEFAULT_SUPPORTED_SYMBOL_UNIVERSE
}

/// The binding requirement for the default configuration, const-evaluated so
/// the invariant below can be a build failure rather than a test.
const DEFAULT_REQUIRED_OPEN_CAPACITY: usize = {
    let flow =
        (DEFAULT_SUPPORTED_OPEN_RATE_CENTI * DEFAULT_SUPPORTED_LIFETIME_SECS as u64 / 100) as usize;
    if flow < DEFAULT_SUPPORTED_SYMBOL_UNIVERSE { flow } else { DEFAULT_SUPPORTED_SYMBOL_UNIVERSE }
};

/// Default open-opportunity capacity: **16,375**.
pub const DEFAULT_MAX_OPEN_OPPORTUNITIES: usize = DEFAULT_REQUIRED_OPEN_CAPACITY
    * DEFAULT_BOUND_SAFETY_NUM as usize
    / DEFAULT_BOUND_SAFETY_DEN as usize;

/// The section-3 invariant, enforced at compile time for the shipped config.
///
/// Mirrors `measurement.rs`'s `MAX_PENDING_OUTCOMES` assertion deliberately:
/// that one exists because a flat capacity below the real population censored
/// the thing being measured, which is exactly what happened here a second time
/// at a different layer. If someone later lowers the safety factor, raises the
/// supported rate or lifetime, or shrinks the declared universe, this fails
/// the build rather than silently reintroducing capacity-induced eviction.
const _: () = {
    assert!(
        DEFAULT_MAX_OPEN_OPPORTUNITIES >= DEFAULT_REQUIRED_OPEN_CAPACITY,
        "opportunity capacity is below the binding open-population requirement; \
         opportunities would be evicted before the engine reached the population \
         it claims to support (see the September-16 capacity truncation)"
    );
    assert!(
        DEFAULT_MAX_OPEN_OPPORTUNITIES >= DEFAULT_SUPPORTED_SYMBOL_UNIVERSE,
        "opportunity capacity is below the supported symbol universe; because the \
         open set is keyed by symbol, this would make eviction reachable by symbol \
         breadth alone, independent of event rate"
    );
};

impl Default for OiConfig {
    fn default() -> Self {
        Self {
            inactivity_secs: crate::episode::INACTIVITY_TIMEOUT_SECS,
            early_max_prior_move_pct: 2.0,
            continuation_min_prior_move_pct: 2.0,
            reversal_max_prior_move_pct: -5.0,
            price_regime_bounds: vec![0.50, 1.00, 5.00, 20.00],
            ranking_cadence_secs: 30,
            supported_open_rate_centi: DEFAULT_SUPPORTED_OPEN_RATE_CENTI,
            supported_lifetime_secs: DEFAULT_SUPPORTED_LIFETIME_SECS,
            supported_symbol_universe: DEFAULT_SUPPORTED_SYMBOL_UNIVERSE,
            bound_safety_num: DEFAULT_BOUND_SAFETY_NUM,
            bound_safety_den: DEFAULT_BOUND_SAFETY_DEN,
            max_history_per_opportunity: 512,
            max_rank_cohort: 4_096,
        }
    }
}

impl OiConfig {
    /// The open population the engine must hold without evicting, before any
    /// safety margin: whichever of the two independent bounds binds first.
    ///
    /// * **Flow** (Little's Law): `supported_open_rate x supported_lifetime`.
    /// * **Structural**: `supported_symbol_universe`, because `open` is keyed
    ///   by symbol and so holds at most one opportunity per symbol.
    ///
    /// Taking the minimum is the honest statement: a population cannot exceed
    /// either, and on any real session one of them is far looser than the
    /// other. On September 16 the structural bound was the binding one --
    /// flow allows 48,000, but only 13,001 symbols existed to fill it.
    pub fn required_open_capacity(&self) -> usize {
        let flow = self
            .supported_open_rate_centi
            .saturating_mul(self.supported_lifetime_secs.max(0) as u64)
            / 100;
        flow.min(self.supported_symbol_universe as u64) as usize
    }

    /// Derived bound on simultaneously-open opportunities:
    /// `required_open_capacity x safety`.
    ///
    /// Derived rather than picked, for the same reason the measurement
    /// collector's pending capacity had to be: a flat constant that happens to
    /// be below the real population silently truncates the thing being
    /// measured. See the `PendingCapacityReached` incident.
    ///
    /// # Why the previous derivation was wrong, and what it cost
    ///
    /// This used to be `supported_open_rate x inactivity_secs x safety`, which
    /// evaluated to `10.00/s x 300s x 1.25 = 3,750`. The rate was generous --
    /// 11.7x the 0.853/s actually observed -- but the *multiplier* was the
    /// wrong quantity. `inactivity_secs` is a silence timeout, not a lifetime:
    /// an opportunity survives as long as its symbol keeps trading. Measured
    /// over the September-16 regular session, by replaying the whole-market
    /// ignition stream the discovery capture preserved:
    ///
    /// | quantity | measured |
    /// |---|---|
    /// | sustained open rate | 0.853/s |
    /// | peak rolling-300s open rate | 6.457/s |
    /// | mean opportunity lifetime | **3,752s** (12.5x `inactivity_secs`) |
    /// | open population, p50 / p99 / max | 3,211 / 4,600 / **4,808** |
    /// | distinct symbols in the scanned universe | 12,962-13,001 |
    ///
    /// Little's Law closes on the measurement: `0.853/s x 3,752s = 3,201`,
    /// against a reconstructed p50 of 3,211. The old formula underestimated
    /// the multiplier by 12.5x, which is why a bound nominally carrying 11.7x
    /// headroom on rate still bound in 142 of 780 ranking windows (18.2%), and
    /// in *every* window of the final half hour.
    ///
    /// # The current derivation
    ///
    /// `min(10.00/s x 4,800s, 13,100 symbols) x 5/4 = 13,100 x 5/4 = 16,375`.
    ///
    /// That is 3.4x the 4,808 maximum the session actually required, and 1.26x
    /// the largest universe ever observed -- so eviction is now structurally
    /// impossible while the universe stays inside the supported envelope,
    /// rather than statistically unlikely. Measured cost is 5,471 bytes per
    /// open opportunity (`tests/opportunity_memory.rs`), so the bound is
    /// 85.4 MB of retained state at full occupancy.
    pub fn max_open_opportunities(&self) -> usize {
        (self.required_open_capacity() as u64).saturating_mul(self.bound_safety_num) as usize
            / self.bound_safety_den.max(1) as usize
    }

    /// The section-3 invariant, checkable at runtime for any configuration:
    /// capacity must cover the binding requirement.
    ///
    /// The default configuration is asserted at *compile* time below; this is
    /// for configurations built at runtime, which a `Vec` field makes
    /// impossible to const-evaluate.
    pub fn capacity_invariant(&self) -> Result<(), String> {
        let required = self.required_open_capacity();
        let capacity = self.max_open_opportunities();
        if capacity < required {
            return Err(format!(
                "opportunity capacity {capacity} is below the binding requirement {required} \
                 (supported rate {}/100 per s, supported lifetime {}s, supported universe {}, \
                 safety {}/{}); opportunities would be evicted before the engine reached the \
                 population it claims to support",
                self.supported_open_rate_centi,
                self.supported_lifetime_secs,
                self.supported_symbol_universe,
                self.bound_safety_num,
                self.bound_safety_den,
            ));
        }
        Ok(())
    }

    /// Stable identity for the effective configuration (§22). FNV-1a over the
    /// canonical JSON encoding -- dependency-free and stable across runs and
    /// platforms, unlike `DefaultHasher`.
    pub fn fingerprint(&self) -> String {
        let json = serde_json::to_string(self).unwrap_or_default();
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in json.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
        format!("oi-cfg-{hash:016x}")
    }

    /// The full version set, persisted with every research record.
    pub fn versions(&self) -> OiVersions {
        OiVersions {
            opportunity_schema: OPPORTUNITY_SCHEMA_VERSION,
            feature_schema: OI_FEATURE_SCHEMA_VERSION,
            regime_classifier: REGIME_CLASSIFIER_VERSION.to_string(),
            price_regime: PRICE_REGIME_VERSION.to_string(),
            early_quality_model: EARLY_QUALITY_MODEL_VERSION.to_string(),
            continuation_model: CONTINUATION_MODEL_VERSION.to_string(),
            ranking: RANKING_VERSION.to_string(),
            score_policy: SCORE_POLICY_VERSION.to_string(),
            config_fingerprint: self.fingerprint(),
            baseline_policy: Some(crate::context::BASELINE_POLICY.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OiVersions {
    pub opportunity_schema: u32,
    pub feature_schema: u32,
    pub regime_classifier: String,
    pub price_regime: String,
    pub early_quality_model: String,
    pub continuation_model: String,
    pub ranking: String,
    pub score_policy: String,
    pub config_fingerprint: String,
    /// The pre-detection baseline contract the row's features were measured
    /// under (`context::BASELINE_POLICY`). Absent on rows written before
    /// 2026-09-25, whose baselines ran from process start across days.
    /// Versioned here rather than as an `OiConfig` field so the config
    /// fingerprint -- and the qualification pin bound to it -- is unaffected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_policy: Option<String>,
}

// ---------------------------------------------------------------------------
// Identity (§2)
// ---------------------------------------------------------------------------

/// `symbol + session_date + sequence`, mirroring `EpisodeId` so the two units
/// can be joined without a translation table.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpportunityId {
    pub symbol: String,
    pub session_date: String,
    pub sequence: u32,
}

impl OpportunityId {
    pub fn as_key(&self) -> String {
        format!("{}:{}:{}", self.symbol, self.session_date, self.sequence)
    }

    /// The `sequence` component, derived **purely from the opening instant**:
    /// milliseconds elapsed since UTC midnight of `opened_at`.
    ///
    /// # Why this replaces an in-memory counter
    ///
    /// `sequence` used to come from a `HashMap` counter living only in the
    /// tracker. That made an `opportunityId` unique *within one process* and
    /// nowhere else: after the tracker was reconstructed the map re-seeded
    /// empty, numbering restarted at 1, and ids already issued earlier the same
    /// day were handed to brand-new opportunities with a fresh `opened_at`.
    /// The 2026-09-17 artifact shows 1,405 opportunities and 1,614 such
    /// re-issues, 324 of them at a single instant (13:30:24, the regular open).
    /// `DCX:2026-09-17:1` names two different opportunities in that one file.
    ///
    /// # Why milliseconds-since-midnight is sufficient, and minimal
    ///
    /// Uniqueness needs a value that (a) never repeats for one symbol within
    /// one session date and (b) does not depend on state that a restart can
    /// lose. A strictly increasing function of `opened_at` gives both, because
    /// the event stream's clock only moves forward: any opportunity opened
    /// after a restart necessarily has a later `opened_at`, hence a strictly
    /// larger sequence, than anything issued before it. No persistence, no run
    /// identity, and no map is required -- so the fix also **deletes** an
    /// unbounded `HashMap` that previously grew for the life of the process.
    ///
    /// Collision would require one symbol to open two opportunities in the
    /// same millisecond. `open_new` only runs when the symbol has no open
    /// opportunity, and every path that frees that slot separates the two
    /// opens by far more than a millisecond:
    ///   * `Inactivity` needs `inactivity_secs` (300s) of silence first;
    ///   * `SessionBoundary` only fires when the date changes, which changes
    ///     the `session_date` component anyway;
    ///   * `CapacityReached` evicts the least-recently-active symbol, never the
    ///     one being opened (it is not in `open` at that point);
    ///   * `finish()` closes without reopening.
    ///
    /// Resolution is milliseconds rather than seconds so the argument holds
    /// with margin rather than exactly; a day fits in `u32` either way
    /// (86,400,000 < 4,294,967,295).
    ///
    /// Deterministic, clock-free, and identical under replay.
    pub fn sequence_for(opened_at: DateTime<Utc>) -> u32 {
        use chrono::Timelike;
        let secs = opened_at.time().num_seconds_from_midnight();
        // `nanosecond()` reports >= 1e9 inside a leap second; clamp so the
        // value stays inside the day rather than wrapping.
        let millis = (opened_at.time().nanosecond() / 1_000_000).min(999);
        secs * 1_000 + millis
    }
}

// ---------------------------------------------------------------------------
// Regime (§4) and price regime (§5) -- research only, deterministic, versioned
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Regime {
    /// Little of the move has happened yet.
    EarlyEmerging,
    /// Material positive movement already underway.
    ContinuationAcceleration,
    /// Began materially negative and is recovering.
    ReversalRecovery,
    /// Deliberately not forced into a confident class.
    Unclassified,
}

/// Ascending price bands, indexed against `OiConfig::price_regime_bounds`.
/// Recorded for segmentation only -- a band **never** suppresses detection.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceRegime {
    /// Band index; `bounds.len()` means "above the last bound".
    pub band: usize,
    /// Inclusive lower / exclusive upper, `None` upper for the open band.
    pub lower: f64,
    pub upper: Option<f64>,
}

pub fn classify_price_regime(price: f64, config: &OiConfig) -> Option<PriceRegime> {
    if !price.is_finite() || price <= 0.0 {
        return None;
    }
    let mut lower = 0.0;
    for (band, upper) in config.price_regime_bounds.iter().enumerate() {
        if price < *upper {
            return Some(PriceRegime { band, lower, upper: Some(*upper) });
        }
        lower = *upper;
    }
    Some(PriceRegime { band: config.price_regime_bounds.len(), lower, upper: None })
}

/// Deterministic research regime from **contemporaneous state only**.
///
/// Returns `Unclassified` whenever the evidence cannot honestly support a
/// class -- §4 explicitly forbids forcing every opportunity into a confident
/// regime, and a missing prior-move measurement is exactly that case.
pub fn classify_regime(
    prior_move_pct: Option<f64>,
    move_from_start_pct: Option<f64>,
    config: &OiConfig,
) -> Regime {
    let Some(prior) = prior_move_pct else {
        return Regime::Unclassified;
    };
    if prior <= config.reversal_max_prior_move_pct {
        // Only a *recovering* opportunity is a reversal; still-falling is not.
        return match move_from_start_pct {
            Some(m) if m > 0.0 => Regime::ReversalRecovery,
            Some(_) => Regime::Unclassified,
            None => Regime::Unclassified,
        };
    }
    if prior >= config.continuation_min_prior_move_pct {
        return Regime::ContinuationAcceleration;
    }
    if prior <= config.early_max_prior_move_pct {
        return Regime::EarlyEmerging;
    }
    Regime::Unclassified
}

// ---------------------------------------------------------------------------
// Feature transformation layer (§6) -- explicit and testable so offline work
// can compare raw / binned / piecewise / presence variants *without* changing
// the engine interface.
// ---------------------------------------------------------------------------

/// How a continuous feature enters a transparent score.
///
/// The baseline showed several components behave **non-monotonically** --
/// `volumeConfirmation == 1.0` is not automatically better than an
/// intermediate value, and neither is `wickRejection == 1.0`. A linear term
/// cannot represent that, so the transform is a first-class, swappable object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Transform {
    /// Pass the value through unchanged.
    Raw,
    /// 1.0 when the value is strictly greater than `threshold`, else 0.0.
    /// Represents "positive MA slope is far more informative than zero".
    Presence { threshold: f64 },
    /// Ascending bin upper bounds mapped to explicit weights. The last weight
    /// applies above the final bound. Lengths must satisfy
    /// `weights.len() == bounds.len() + 1`.
    Bins { bounds: Vec<f64>, weights: Vec<f64> },
    /// Linear interpolation between explicit `(x, y)` knots, clamped at the
    /// ends. Represents a rise-then-flatten relationship.
    Piecewise { knots: Vec<(f64, f64)> },
}

impl Transform {
    pub fn apply(&self, value: f64) -> f64 {
        match self {
            Transform::Raw => value,
            Transform::Presence { threshold } => {
                if value > *threshold {
                    1.0
                } else {
                    0.0
                }
            }
            Transform::Bins { bounds, weights } => {
                let mut index = bounds.len();
                for (i, bound) in bounds.iter().enumerate() {
                    if value < *bound {
                        index = i;
                        break;
                    }
                }
                weights.get(index).copied().unwrap_or(0.0)
            }
            Transform::Piecewise { knots } => {
                if knots.is_empty() {
                    return 0.0;
                }
                if value <= knots[0].0 {
                    return knots[0].1;
                }
                for pair in knots.windows(2) {
                    let (x0, y0) = pair[0];
                    let (x1, y1) = pair[1];
                    if value <= x1 {
                        if (x1 - x0).abs() < f64::EPSILON {
                            return y1;
                        }
                        let t = (value - x0) / (x1 - x0);
                        return y0 + t * (y1 - y0);
                    }
                }
                knots[knots.len() - 1].1
            }
        }
    }
}

/// One named term in a transparent score, retained for explainability (§12's
/// "score reasons/components").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreComponent {
    pub feature: String,
    /// The causal input, or `None` when it was unavailable.
    pub raw: Option<f64>,
    /// Transform output. `None` when `raw` is `None` -- never a fabricated 0.
    pub transformed: Option<f64>,
    pub weight: f64,
    pub contribution: f64,
}

/// Why a candidate could not be scored comparably.
///
/// Explicit rather than a bare `None`, so "we lack the evidence" and "the
/// evidence says low quality" can never be confused downstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnrankableReason {
    /// A feature the model cannot be honestly evaluated without was absent.
    CoreFeatureMissing,
    /// Fewer than `MIN_PRESENT_INPUTS` inputs existed.
    TooFewInputs,
    /// Present weight was below `MIN_COMPARABLE_COVERAGE` of configured
    /// weight, so a normalized score would rest on too little of the model.
    InsufficientCoverage,
}

/// A score plus everything needed to explain, reproduce **and re-derive** it.
///
/// The raw and normalized figures are both persisted, along with the weights
/// they were computed from, so offline work can reconstruct any alternative
/// comparability policy from the record alone -- without a code change and
/// without re-running the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShadowScore {
    pub model_version: String,
    /// The comparability policy applied. See `SCORE_POLICY_VERSION`.
    pub policy_version: String,
    /// **Comparable** score: the present-weighted mean of transformed inputs,
    /// i.e. `raw_weighted / present_weight`, in `[0, 1]`.
    ///
    /// `None` when the evidence could not support a comparable score -- see
    /// `unrankable_reason`. A missing score is **not** a low score; `Ranking`
    /// keeps it `unranked` rather than ranking it worst.
    ///
    /// Where coverage is 1.0 this equals `raw_weighted` exactly, because both
    /// models' weights sum to 1.0. Only partially-observed candidates differ
    /// from the v1 policy, which is precisely the bias being corrected.
    pub value: Option<f64>,
    /// Pre-normalization `sum(transformed x weight)` over present features --
    /// the figure the v1 policy used as the score. Retained so the change is
    /// auditable and reversible offline.
    pub raw_weighted: f64,
    /// Configured weight of the features that were actually present.
    pub present_weight: f64,
    /// Total configured weight of the model.
    pub total_weight: f64,
    /// `present_weight / total_weight`. Feature coverage, not a score.
    pub coverage: f64,
    pub components: Vec<ScoreComponent>,
    /// Features unavailable at scoring time (§6 missingness mask).
    pub missing: Vec<String>,
    /// The subset of `missing` that the model treats as core.
    pub core_missing: Vec<String>,
    /// How many of the model's inputs were actually present.
    pub present_inputs: usize,
    pub required_inputs: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unrankable_reason: Option<UnrankableReason>,
}

/// Minimum share of configured weight that must be present before a
/// normalized score is treated as comparable.
///
/// Normalization answers "how good is the evidence we have", and at low
/// coverage that question stops being about the same thing as a full
/// candidate's score. The figure is not tuned: it is set to exclude the
/// coverage classes that carry no evidence of the quantity the model names.
/// For `ContinuationConfidence` those are the 0.20 class (confluence + halt
/// only -- a continuation confidence with no measured move at all) and the
/// 0.30 class; both were rankable under the v1 policy.
pub const MIN_COMPARABLE_COVERAGE: f64 = 0.50;

/// Features `EarlyQualityScore` cannot be honestly evaluated without.
///
/// The momentum block, which is co-missing as a unit (every one of these reads
/// the same `Option<MomentumFeatures>`), and carries 0.85 of the model. Without
/// any momentum evidence there is no quality to assess, and earliness alone
/// (0.15) would rank a candidate on one weak input. `earliness.priorMovePct`
/// is deliberately **optional**: a symbol with no pre-detection history is
/// still assessable on its momentum structure.
const EARLY_QUALITY_CORE: [&str; 4] = [
    "momentum.maSlope.presence",
    "momentum.structure",
    "momentum.volumeConfirmation",
    "momentum.wickRejection",
];

/// Features `ContinuationConfidence` cannot be honestly evaluated without.
///
/// Just the opportunity's own measured move. The model's question presupposes
/// a move is underway; `confluence.detectorCount` is always present (it is
/// derived, never observed) so it cannot carry that requirement, and
/// `continuation.priorMovePct` stays optional because pre-detection history is
/// genuinely unavailable for some symbols.
const CONTINUATION_CORE: [&str; 1] = ["continuation.moveFromStartPct"];

// ---------------------------------------------------------------------------
// Detector confluence (§9) -- recorded as information, never a gate
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectorArrival {
    /// First time this detector was seen for this opportunity.
    pub first_seen_at: DateTime<Utc>,
    /// First *confirming* moment, when the detector has a confirm phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_confirmed_at: Option<DateTime<Utc>>,
    /// Arrival order, 1-based, in the order detectors first appeared.
    pub arrival_index: u32,
    pub events: u32,
}

// ---------------------------------------------------------------------------
// The Opportunity itself (§2)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpportunityCloseReason {
    Inactivity,
    SessionBoundary,
    CaptureEnded,
    /// The open-opportunity bound was reached and the least-recently-active
    /// opportunity was evicted. Explicit, never silent -- the measurement
    /// pending-capacity incident is the precedent.
    CapacityReached,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Opportunity {
    pub schema_version: u32,
    pub id: OpportunityId,
    pub symbol: String,
    pub session_date: String,
    /// First observation of any kind for this opportunity.
    pub first_seen_at: DateTime<Utc>,
    /// Qualifying moment that opened it (mirrors `Episode::opened_at`).
    pub opened_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub opening_price: f64,
    pub latest_price: f64,
    pub first_detector: Strategy,
    /// Ordered by `Strategy` so serialization is deterministic; arrival order
    /// is preserved in `DetectorArrival::arrival_index`.
    pub detectors_seen: BTreeMap<String, DetectorArrival>,
    /// Raw detector events folded in -- the redundancy measure (§15F).
    pub raw_event_count: u32,
    /// How many times the *active* detector changed.
    pub detector_transitions: u32,
    /// Underlying `EpisodeTracker` episodes this opportunity spans. Greater
    /// than 1 exactly when invalidation fragmented the move.
    pub episode_fragments: u32,
    /// Ignition rejections absorbed without ending the opportunity.
    pub invalidations_absorbed: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_before_detection_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_from_start_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_move_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_move_pct: Option<f64>,
    pub observed_high: f64,
    pub observed_low: f64,
    /// Latest full causal feature snapshot (§3), refreshed on every
    /// observation. Continuous values retained.
    ///
    /// Causal despite being refreshed: `FeatureCache::snapshot` filters every
    /// sub-struct to values observed at or before the instant asked for, so a
    /// refresh at time `t` can only contain information available at `t`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_context: Option<SignalContext>,
    /// The feature surface as it stood when this opportunity was first
    /// detected, frozen.
    ///
    /// Kept separately because earliness (§6) is a question about what was
    /// knowable *at detection*, and refreshing a single field would have
    /// destroyed the only record of it. `move_before_detection_pct` above is
    /// the scalar version of the same concern.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detection_context: Option<SignalContext>,
    /// Whether the detection surface has already been persisted for this
    /// opportunity, so it is written once rather than on every ranking window.
    ///
    /// A `bool` on the opportunity rather than a set of seen ids: the latter
    /// would grow without bound across a session, which §28 forbids outright.
    #[serde(default, skip_serializing)]
    pub detection_context_emitted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_reason: Option<OpportunityCloseReason>,
}

impl Opportunity {
    pub fn age_secs(&self, at: DateTime<Utc>) -> i64 {
        (at - self.opened_at).num_seconds().max(0)
    }

    /// Distinct detector families agreeing (§9).
    pub fn confluence_count(&self) -> usize {
        self.detectors_seen.len()
    }

    /// Seconds between the first and most recent detector confirmation, when
    /// at least two detectors have confirmed.
    pub fn confirmation_span_secs(&self) -> Option<i64> {
        let mut times: Vec<DateTime<Utc>> =
            self.detectors_seen.values().filter_map(|d| d.first_confirmed_at).collect();
        if times.len() < 2 {
            return None;
        }
        times.sort();
        Some((times[times.len() - 1] - times[0]).num_seconds())
    }
}

// ---------------------------------------------------------------------------
// Transparent V1 scores (§6, §7) -- SHADOW ONLY
// ---------------------------------------------------------------------------

fn momentum_of(ctx: Option<&SignalContext>) -> Option<&crate::context::MomentumFeatures> {
    ctx.and_then(|c| c.momentum.as_ref())
}

#[allow(clippy::too_many_arguments)]
fn push(
    components: &mut Vec<ScoreComponent>,
    missing: &mut Vec<String>,
    present: &mut usize,
    present_weight: &mut f64,
    total_weight: &mut f64,
    name: &str,
    raw: Option<f64>,
    transform: &Transform,
    weight: f64,
) -> f64 {
    *total_weight += weight;
    match raw {
        Some(value) => {
            let t = transform.apply(value);
            *present += 1;
            *present_weight += weight;
            components.push(ScoreComponent {
                feature: name.to_string(),
                raw: Some(value),
                transformed: Some(t),
                weight,
                contribution: t * weight,
            });
            t * weight
        }
        None => {
            missing.push(name.to_string());
            components.push(ScoreComponent {
                feature: name.to_string(),
                raw: None,
                transformed: None,
                weight,
                contribution: 0.0,
            });
            0.0
        }
    }
}

/// Minimum inputs before a score is emitted at all. Below this the score is
/// `None` -- honest absence, which `Ranking` keeps *unranked* rather than
/// ranking worst.
const MIN_PRESENT_INPUTS: usize = 2;

/// Applies the comparability policy (§ Correction 3) to an accumulated score.
///
/// Three gates, all causal and deterministic, evaluated in a fixed order so
/// the reported reason is stable:
///
/// 1. every core feature present,
/// 2. at least `MIN_PRESENT_INPUTS` inputs present,
/// 3. present weight at least `MIN_COMPARABLE_COVERAGE` of configured weight.
///
/// Passing all three, the comparable value is `raw_weighted / present_weight`
/// -- the present-weighted mean of the transformed inputs. This replaces the
/// implicit "absent contributes 0" of the v1 policy, which imputed the worst
/// observable value for anything unmeasured and capped attainable score at
/// coverage.
///
/// The assumption normalization does make is explicit: an absent optional
/// feature is treated as scoring at the mean of the present ones. That is a
/// weaker assumption than the v1 policy's, not an additional one -- v1 already
/// imputed, it just imputed the floor.
#[allow(clippy::too_many_arguments)]
fn finalize_score(
    model_version: &str,
    components: Vec<ScoreComponent>,
    missing: Vec<String>,
    core: &[&str],
    present: usize,
    present_weight: f64,
    total_weight: f64,
    raw_weighted: f64,
    required_inputs: usize,
) -> ShadowScore {
    let core_missing: Vec<String> =
        core.iter().filter(|c| missing.iter().any(|m| m == *c)).map(|c| c.to_string()).collect();
    let coverage = if total_weight > 0.0 { present_weight / total_weight } else { 0.0 };

    let unrankable_reason = if !core_missing.is_empty() {
        Some(UnrankableReason::CoreFeatureMissing)
    } else if present < MIN_PRESENT_INPUTS {
        Some(UnrankableReason::TooFewInputs)
    } else if coverage < MIN_COMPARABLE_COVERAGE || present_weight <= 0.0 {
        Some(UnrankableReason::InsufficientCoverage)
    } else {
        None
    };

    let value = if unrankable_reason.is_none() {
        Some(raw_weighted / present_weight)
    } else {
        None
    };

    ShadowScore {
        model_version: model_version.to_string(),
        policy_version: SCORE_POLICY_VERSION.to_string(),
        value,
        raw_weighted,
        present_weight,
        total_weight,
        coverage,
        components,
        missing,
        core_missing,
        present_inputs: present,
        required_inputs,
        unrankable_reason,
    }
}

/// **EarlyQualityScore V1 — shadow only, never read by production.**
///
/// Answers: *does this deserve priority before acceleration is obvious?*
/// Deliberately does **not** reward prior movement — that is
/// `continuation_confidence`'s job, and mixing them is what makes the existing
/// research rank a confirmation signal rather than an early one.
///
/// Transform choices encode observed non-monotonicity rather than assuming
/// linearity, and are configurable objects so offline work can compare
/// variants (§6).
pub fn early_quality_score(
    opportunity: &Opportunity,
    at: DateTime<Utc>,
) -> ShadowScore {
    let ctx = opportunity.latest_context.as_ref();
    let mom = momentum_of(ctx);
    let mut components = Vec::new();
    let mut missing = Vec::new();
    let mut present = 0usize;
    let mut present_weight = 0.0;
    let mut total_weight = 0.0;
    let mut total = 0.0;

    // Positive MA slope was materially more informative than zero slope, while
    // increasing it further was not cleanly monotonic -> presence, not level.
    total += push(
        &mut components, &mut missing, &mut present, &mut present_weight, &mut total_weight,
        "momentum.maSlope.presence",
        mom.map(|m| m.ma_slope),
        &Transform::Presence { threshold: 0.0 },
        0.30,
    );
    // Structure enters raw: no evidence of non-monotonicity was observed.
    total += push(
        &mut components, &mut missing, &mut present, &mut present_weight, &mut total_weight,
        "momentum.structure",
        mom.map(|m| m.structure),
        &Transform::Raw,
        0.20,
    );
    // volumeConfirmation == 1.0 is NOT automatically superior -> bins that do
    // not peak at the extreme.
    total += push(
        &mut components, &mut missing, &mut present, &mut present_weight, &mut total_weight,
        "momentum.volumeConfirmation",
        mom.map(|m| m.volume_confirmation),
        &Transform::Bins {
            bounds: vec![0.40, 0.60, 0.80, 1.00],
            weights: vec![0.0, 0.35, 0.80, 1.00, 0.70],
        },
        0.20,
    );
    // wickRejection == 1.0 is NOT automatically superior -> same treatment.
    total += push(
        &mut components, &mut missing, &mut present, &mut present_weight, &mut total_weight,
        "momentum.wickRejection",
        mom.map(|m| m.wick_rejection),
        &Transform::Bins {
            bounds: vec![0.50, 0.75, 1.00],
            weights: vec![0.0, 0.50, 1.00, 0.75],
        },
        0.15,
    );
    // Earliness itself: *less* prior movement is better here, flattening once
    // the move is already obvious.
    total += push(
        &mut components, &mut missing, &mut present, &mut present_weight, &mut total_weight,
        "earliness.priorMovePct",
        opportunity.move_before_detection_pct,
        &Transform::Piecewise {
            knots: vec![(-5.0, 0.2), (0.0, 1.0), (2.0, 0.8), (5.0, 0.4), (10.0, 0.1), (20.0, 0.0)],
        },
        0.15,
    );

    let _ = at;
    finalize_score(
        EARLY_QUALITY_MODEL_VERSION,
        components,
        missing,
        &EARLY_QUALITY_CORE,
        present,
        present_weight,
        total_weight,
        total,
        5,
    )
}

/// **ContinuationConfidence V1 — shadow only, never read by production.**
///
/// Answers a *different* question: *a move is already underway — how likely is
/// further continuation?* It may therefore use move magnitude, which
/// `early_quality_score` deliberately may not.
///
/// Kept a separate quantity rather than folded into one global score, because
/// the baseline showed continuation and reversal populations behave differently
/// and a single score is not justified by the evidence.
pub fn continuation_confidence(
    opportunity: &Opportunity,
    at: DateTime<Utc>,
) -> ShadowScore {
    let ctx = opportunity.latest_context.as_ref();
    let mom = momentum_of(ctx);
    let mut components = Vec::new();
    let mut missing = Vec::new();
    let mut present = 0usize;
    let mut present_weight = 0.0;
    let mut total_weight = 0.0;
    let mut total = 0.0;

    // Precision rose with prior movement then flattened -- rise-then-flatten,
    // not linear, and explicitly not extrapolated past the observed range.
    total += push(
        &mut components, &mut missing, &mut present, &mut present_weight, &mut total_weight,
        "continuation.priorMovePct",
        opportunity.move_before_detection_pct,
        &Transform::Piecewise {
            knots: vec![(0.0, 0.10), (2.0, 0.25), (5.0, 0.50), (7.5, 0.65),
                        (10.0, 0.80), (15.0, 1.00), (20.0, 1.00)],
        },
        0.35,
    );
    total += push(
        &mut components, &mut missing, &mut present, &mut present_weight, &mut total_weight,
        "continuation.moveFromStartPct",
        opportunity.move_from_start_pct,
        &Transform::Piecewise {
            knots: vec![(-5.0, 0.0), (0.0, 0.2), (2.0, 0.6), (5.0, 0.9), (10.0, 1.0)],
        },
        0.20,
    );
    total += push(
        &mut components, &mut missing, &mut present, &mut present_weight, &mut total_weight,
        "momentum.overall",
        mom.map(|m| m.overall),
        &Transform::Raw,
        0.15,
    );
    total += push(
        &mut components, &mut missing, &mut present, &mut present_weight, &mut total_weight,
        "momentum.maSlope.presence",
        mom.map(|m| m.ma_slope),
        &Transform::Presence { threshold: 0.0 },
        0.10,
    );
    // Confluence as a *feature*, never a gate (§9).
    total += push(
        &mut components, &mut missing, &mut present, &mut present_weight, &mut total_weight,
        "confluence.detectorCount",
        Some(opportunity.confluence_count() as f64),
        &Transform::Bins {
            bounds: vec![2.0, 3.0],
            weights: vec![0.0, 0.5, 1.0],
        },
        0.10,
    );
    // Halt proximity is a risk signal, not an opportunity signal: closeness to
    // a band reduces confidence in further continuation.
    total += push(
        &mut components, &mut missing, &mut present, &mut present_weight, &mut total_weight,
        "halt.proximityRatio",
        ctx.and_then(|c| c.halt.as_ref()).map(|h| h.proximity_ratio),
        &Transform::Piecewise {
            knots: vec![(0.0, 1.0), (0.5, 0.6), (0.8, 0.2), (1.0, 0.0)],
        },
        0.10,
    );

    let _ = at;
    finalize_score(
        CONTINUATION_MODEL_VERSION,
        components,
        missing,
        &CONTINUATION_CORE,
        present,
        present_weight,
        total_weight,
        total,
        6,
    )
}

// ---------------------------------------------------------------------------
// Cross-sectional ranking (§8) -- ranks OPPORTUNITIES, not detector events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RankEntry {
    pub opportunity_id: String,
    pub symbol: String,
    pub rank: usize,
    pub score: f64,
}

/// One ranking window. Candidates without a score are **explicitly unranked**,
/// never ranked worst (§8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ranking {
    pub window_id: String,
    pub ranked_at: DateTime<Utc>,
    pub cohort_size: usize,
    pub entries: Vec<RankEntry>,
    /// Opportunity keys that could not be scored.
    pub unranked: Vec<String>,
    /// True when the cohort bound truncated the ranking -- explicit, not silent.
    pub cohort_truncated: bool,
    pub ranking_version: String,
}

/// Deterministic ranking. Ties break on `(symbol, sequence)` so replay and live
/// produce byte-identical output.
fn rank_cohort(
    scored: Vec<(OpportunityId, f64)>,
    unranked: Vec<String>,
    window_id: String,
    ranked_at: DateTime<Utc>,
    max_cohort: usize,
) -> Ranking {
    let mut scored = scored;
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.symbol.cmp(&b.0.symbol))
            .then_with(|| a.0.sequence.cmp(&b.0.sequence))
    });
    let truncated = scored.len() > max_cohort;
    scored.truncate(max_cohort);
    let cohort_size = scored.len();
    let entries = scored
        .into_iter()
        .enumerate()
        .map(|(i, (id, score))| RankEntry {
            opportunity_id: id.as_key(),
            symbol: id.symbol,
            rank: i + 1,
            score,
        })
        .collect();
    Ranking {
        window_id,
        ranked_at,
        cohort_size,
        entries,
        unranked,
        cohort_truncated: truncated,
        ranking_version: RANKING_VERSION.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Shadow scoring snapshot (§12) -- what gets persisted
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpportunityScoreSnapshot {
    pub schema_version: u32,
    pub versions: OiVersions,
    pub timestamp: DateTime<Utc>,
    pub window_id: String,
    pub opportunity_id: String,
    pub symbol: String,
    pub session_date: String,
    pub regime: Regime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_regime: Option<PriceRegime>,
    pub opportunity_age_secs: i64,
    pub current_price: f64,

    // --- V2.1 risk / identity extension (schema 2) -------------------------
    // All six are state known AS OF `timestamp`, read from the live
    // `Opportunity`, never reconstructed offline. `Option` is carrying
    // "absent from this artifact version", not "unknown" and never "zero":
    // a version-2 writer always populates all six, so `None` on a row means
    // the row predates the extension.
    /// Highest price observed for this opportunity since `opened_at`, as of
    /// `timestamp`. Running extremum, never forward-looking. RiskQuality's
    /// preregistered core `risk.drawdownFromHigh` is unreplayable without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_high: Option<f64>,
    /// Lowest price observed since `opened_at`, as of `timestamp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_low: Option<f64>,
    /// Largest positive % move away from `opening_price` reached so far.
    /// Genuinely optional on the live struct -- absent until one is measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_move_pct: Option<f64>,
    /// Largest negative % move away from `opening_price` reached so far.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_move_pct: Option<f64>,
    /// Price at `opened_at`; the denominator `max_move_pct`/`min_move_pct`
    /// are measured against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opening_price: Option<f64>,
    /// The instant this opportunity opened. Redundant with
    /// `timestamp - opportunity_age_secs` while identity is sound, and that is
    /// precisely the point: persisting it makes any future id re-issue
    /// **visible in the artifact** instead of silently folded into an age that
    /// appears to run backwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_from_start_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_before_detection_pct: Option<f64>,
    pub raw_event_count: u32,
    pub episode_fragments: u32,
    pub invalidations_absorbed: u32,
    pub detectors_seen: Vec<String>,
    pub confluence_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation_span_secs: Option<i64>,
    /// Causal feature snapshot as of `timestamp`, continuous values intact
    /// (§3). Refreshed every window as of feature schema 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<SignalContext>,
    /// The same surface as it stood at first detection, carried on this
    /// opportunity's **first** snapshot only.
    ///
    /// Absent on later rows by design; forward-fill per `opportunityId`.
    /// Present on no row at all means the opportunity's first ranking window
    /// was lost (a counted writer drop) or it closed before ever being ranked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detection_features: Option<SignalContext>,
    pub early_quality: ShadowScore,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub early_quality_rank: Option<usize>,
    pub continuation: ShadowScore,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_rank: Option<usize>,
    pub early_cohort_size: usize,
    pub continuation_cohort_size: usize,
    /// Research-only alert concept (§10). Never surfaced to production users.
    pub shadow_state: ShadowState,
}

/// Semantic separation between "interesting early" and "already moving"
/// (§10). Research output only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShadowState {
    EarlyWatch,
    Accelerating,
    HighConfidenceContinuation,
    None,
}

// ---------------------------------------------------------------------------
// The engine (§13) -- one implementation, driven live or offline
// ---------------------------------------------------------------------------

/// Explicit saturation / loss accounting (§18). Absence and saturation must
/// never look alike.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OiHealth {
    pub open_opportunities: usize,
    pub peak_open_opportunities: usize,
    /// The bound `open_opportunities` is being held against.
    ///
    /// Reported alongside the peak because a peak without its capacity is not
    /// interpretable: 3,750 open is healthy at a capacity of 16,375 and is
    /// saturation at a capacity of 3,750, and the September-16 gate could not
    /// tell those apart from the log alone.
    pub opportunity_capacity: usize,
    pub capacity_evictions: u64,
    pub history_truncations: u64,
    pub cohort_truncations: u64,
    pub opportunities_opened: u64,
    pub opportunities_closed: u64,
    pub raw_events_observed: u64,
    pub scores_emitted: u64,
}

/// One capacity eviction, described exactly enough to be persisted as a
/// research marker (section 4).
///
/// Exists because the September-16 session could only establish that capacity
/// had bound by *inferring* it from cohort sizes pinned at 3,750 in the
/// surviving records. A subsystem that truncates evidence must say so in the
/// evidence, not leave it to be reconstructed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapacityEviction {
    pub at: DateTime<Utc>,
    pub opportunity_id: String,
    pub symbol: String,
    /// Always `opportunity_capacity_reached`. Carried in-band so a reader does
    /// not have to know which file it came from to know what it means.
    pub reason: String,
    pub capacity: usize,
    /// Open count at the instant of eviction, *before* the victim was removed.
    pub open_count: usize,
}

/// Pending eviction markers retained between drains.
///
/// The driver drains after every observation, so this holds single digits in
/// practice. It is bounded anyway: this is a realtime process, and an
/// unbounded diagnostic buffer is the failure mode the whole assignment is
/// about. Overflow is counted, never silent.
const MAX_PENDING_CAPACITY_EVICTIONS: usize = 4_096;

/// Opportunity Intelligence engine.
///
/// `observe` is the single entry point, used identically by the live subscriber
/// and by offline replay, which is what makes replay parity achievable rather
/// than aspirational (§13).
#[derive(Debug)]
pub struct OpportunityIntelligence {
    config: OiConfig,
    /// Open opportunities keyed by symbol -- an index, so a price event for an
    /// unrelated symbol is O(1) and never scans the open set (§18).
    open: HashMap<String, Opportunity>,
    /// `(last_seen_at, symbol)` for every open opportunity, so expiry is a
    /// range query over only what is actually due and capacity eviction is a
    /// first-key lookup, instead of a full scan on every event.
    ///
    /// Key order *is* due order: an opportunity expires at
    /// `last_seen_at + inactivity_secs`, a constant offset, so ordering by
    /// `last_seen_at` orders by deadline. The symbol is in the key only to make
    /// it unique when two opportunities share an instant, and it is the same
    /// tiebreak `enforce_capacity` already used.
    by_last_seen: std::collections::BTreeSet<(DateTime<Utc>, String)>,
    features: FeatureCache,
    last_ranked: Option<DateTime<Utc>>,
    ranking_windows: u64,
    health: OiHealth,
    /// Eviction markers awaiting persistence. Drained by the caller.
    pending_evictions: Vec<CapacityEviction>,
    /// Eviction markers that could not be buffered. Non-zero means the
    /// *markers* were lost, not the evictions -- `capacity_evictions` is still
    /// exact.
    eviction_markers_dropped: u64,
}

impl OpportunityIntelligence {
    pub fn new(config: OiConfig) -> Self {
        // Reported rather than enforced: a research engine must not refuse to
        // run because its own bound is mis-specified, but the mis-specification
        // must never be silent. The shipped configuration is asserted at
        // compile time; this catches one assembled at runtime.
        if let Err(error) = config.capacity_invariant() {
            tracing::error!(%error, "opportunity-intelligence capacity invariant violated");
        }
        let capacity = config.max_open_opportunities();
        Self {
            config,
            open: HashMap::new(),
            by_last_seen: std::collections::BTreeSet::new(),
            features: FeatureCache::default(),
            last_ranked: None,
            ranking_windows: 0,
            health: OiHealth { opportunity_capacity: capacity, ..OiHealth::default() },
            pending_evictions: Vec::new(),
            eviction_markers_dropped: 0,
        }
    }

    pub fn config(&self) -> &OiConfig {
        &self.config
    }

    pub fn health(&self) -> &OiHealth {
        &self.health
    }

    pub fn open_count(&self) -> usize {
        self.open.len()
    }

    /// Takes the eviction markers accumulated since the last call.
    ///
    /// Separate from `health()` because the two answer different questions:
    /// health says *how many*, exactly and at any moment; these say *which*,
    /// and are meant to be written into the capture so the artifact
    /// self-reports its own truncation.
    pub fn take_capacity_evictions(&mut self) -> Vec<CapacityEviction> {
        std::mem::take(&mut self.pending_evictions)
    }

    /// Eviction markers that overflowed the pending buffer. The evictions
    /// themselves are still counted exactly in `health().capacity_evictions`.
    pub fn eviction_markers_dropped(&self) -> u64 {
        self.eviction_markers_dropped
    }

    pub fn open_opportunities(&self) -> impl Iterator<Item = &Opportunity> {
        self.open.values()
    }

    /// Folds one already-broadcast event in. Returns opportunities closed by
    /// this event.
    ///
    /// **Never** mutates, reorders or suppresses the event. The caller owns the
    /// event; this is a read-only consumer.
    pub fn observe(
        &mut self,
        event: &ScanEvent,
        received_at: DateTime<Utc>,
    ) -> Vec<Opportunity> {
        self.features.observe(event);
        self.health.raw_events_observed += 1;

        let mut closed = self.expire_inactive(received_at);

        let Some((symbol, at, price)) = event_symbol_time_price(event) else {
            return closed;
        };

        // Session boundary closes a prior-date opportunity.
        if let Some(existing) = self.open.get(&symbol) {
            if existing.opened_at.date_naive() != at.date_naive() {
                if let Some(op) =
                    self.close(&symbol, at, OpportunityCloseReason::SessionBoundary)
                {
                    closed.push(op);
                }
            }
        }

        let invalidating = matches!(
            event,
            ScanEvent::IgnitionEvent { kind: IgnitionEventKind::FollowThroughRejected, .. }
        );

        if self.open.contains_key(&symbol) {
            // Continue. Invalidation is *absorbed*, not terminal -- the single
            // deliberate divergence from `EpisodeTracker`.
            self.record(&symbol, event, at, price, invalidating);
        } else if let Some(strategy) = qualifying_strategy(event) {
            let Some(price) = price.or_else(|| self.features.last_price(&symbol)) else {
                return closed;
            };
            self.enforce_capacity(at, &mut closed);
            self.open_new(&symbol, strategy, at, price, received_at);
        }
        closed
    }

    fn open_new(
        &mut self,
        symbol: &str,
        strategy: Strategy,
        at: DateTime<Utc>,
        price: f64,
        received_at: DateTime<Utc>,
    ) {
        let session_date = at.date_naive().to_string();
        // Derived from the opening instant, never from tracker state -- see
        // `OpportunityId::sequence_for` for why that is what makes the id
        // survive a restart.
        let id = OpportunityId {
            symbol: symbol.to_string(),
            session_date: session_date.clone(),
            sequence: OpportunityId::sequence_for(at),
        };
        let context = self.features.snapshot(symbol, strategy, at, received_at, price);
        let prior = context.pre_detection.as_ref().and_then(|p| p.move_before_detection_pct);
        let mut detectors = BTreeMap::new();
        detectors.insert(
            strategy_name(strategy),
            DetectorArrival {
                first_seen_at: at,
                first_confirmed_at: Some(at),
                arrival_index: 1,
                events: 1,
            },
        );
        self.open.insert(
            symbol.to_string(),
            Opportunity {
                schema_version: OPPORTUNITY_SCHEMA_VERSION,
                id,
                symbol: symbol.to_string(),
                session_date,
                first_seen_at: at,
                opened_at: at,
                last_seen_at: at,
                opening_price: price,
                latest_price: price,
                first_detector: strategy,
                detectors_seen: detectors,
                raw_event_count: 1,
                detector_transitions: 0,
                episode_fragments: 1,
                invalidations_absorbed: 0,
                move_before_detection_pct: prior,
                move_from_start_pct: Some(0.0),
                max_move_pct: Some(0.0),
                min_move_pct: Some(0.0),
                observed_high: price,
                observed_low: price,
                latest_context: Some(context.clone()),
                detection_context: Some(context),
                detection_context_emitted: false,
                closed_at: None,
                close_reason: None,
            },
        );
        self.by_last_seen.insert((at, symbol.to_string()));
        self.health.opportunities_opened += 1;
        self.health.open_opportunities = self.open.len();
        self.health.peak_open_opportunities =
            self.health.peak_open_opportunities.max(self.open.len());
        debug_assert_eq!(self.by_last_seen.len(), self.open.len());
    }

    fn record(
        &mut self,
        symbol: &str,
        event: &ScanEvent,
        at: DateTime<Utc>,
        price: Option<f64>,
        invalidating: bool,
    ) {
        let arrival_index = {
            let Some(op) = self.open.get(symbol) else { return };
            op.detectors_seen.len() as u32 + 1
        };
        // Refreshed here, before the borrow below, because `FeatureCache` and
        // `self.open` are both fields of `self`.
        //
        // Attributed to `first_detector`, not to whichever detector produced
        // this event: the context belongs to the *opportunity*, whose identity
        // is the detector that opened it. Using the current event's strategy
        // would make the snapshot's `strategy` field oscillate between windows
        // and turn a stable key into noise.
        let refreshed = {
            let Some(op) = self.open.get(symbol) else { return };
            let strategy = op.first_detector;
            let signal_price = price
                .filter(|p| p.is_finite() && *p > 0.0)
                .unwrap_or(op.latest_price);
            self.features.snapshot(symbol, strategy, at, at, signal_price)
        };
        let Some(op) = self.open.get_mut(symbol) else { return };
        op.latest_context = Some(refreshed);
        op.raw_event_count += 1;
        // Reindex before the field moves, or the old key is unreachable and the
        // set desynchronises from `open`.
        let previous = op.last_seen_at;
        op.last_seen_at = at;
        if previous != at {
            self.by_last_seen.remove(&(previous, symbol.to_string()));
            self.by_last_seen.insert((at, symbol.to_string()));
        }
        if invalidating {
            op.invalidations_absorbed += 1;
            // A confirm after a rejection is the *same* move resuming; count
            // the fragment so the episode/opportunity ratio stays measurable.
            op.episode_fragments += 1;
        }
        if let Some(price) = price.filter(|p| p.is_finite() && *p > 0.0) {
            op.latest_price = price;
            op.observed_high = op.observed_high.max(price);
            op.observed_low = op.observed_low.min(price);
            if op.opening_price > 0.0 {
                let move_pct = (price - op.opening_price) / op.opening_price * 100.0;
                op.move_from_start_pct = Some(move_pct);
                op.max_move_pct = Some(op.max_move_pct.unwrap_or(move_pct).max(move_pct));
                op.min_move_pct = Some(op.min_move_pct.unwrap_or(move_pct).min(move_pct));
            }
        }
        if let Some(strategy) = qualifying_strategy(event) {
            let confirming = matches!(
                event,
                ScanEvent::IgnitionEvent {
                    kind: IgnitionEventKind::FollowThroughConfirmed, ..
                } | ScanEvent::ConsolidationEvent {
                    kind: ConsolidationEventKind::EntryTriggered, ..
                } | ScanEvent::FunnelSignal { passed: true, .. }
                    | ScanEvent::MomentumUpdate { qualifies: true, .. }
            );
            match op.detectors_seen.get_mut(&strategy_name(strategy)) {
                Some(existing) => {
                    existing.events += 1;
                    if confirming && existing.first_confirmed_at.is_none() {
                        existing.first_confirmed_at = Some(at);
                    }
                }
                None => {
                    op.detector_transitions += 1;
                    op.detectors_seen.insert(
                        strategy_name(strategy),
                        DetectorArrival {
                            first_seen_at: at,
                            first_confirmed_at: if confirming { Some(at) } else { None },
                            arrival_index,
                            events: 1,
                        },
                    );
                }
            }
        }
    }

    fn expire_inactive(&mut self, now: DateTime<Utc>) -> Vec<Opportunity> {
        let boundary = self.config.inactivity_secs;
        // Only the entries actually due are touched. The predicate is
        // unchanged: `(now - last_seen).num_seconds() >= boundary` holds
        // exactly when `last_seen <= now - boundary`, and the index is ordered
        // by `last_seen`, so the due set is a prefix.
        //
        // Also now deterministic, which the `HashMap` scan it replaces was not:
        // expiries come out in deadline order rather than in whatever order
        // `RandomState` produced that run.
        let cutoff = now - Duration::seconds(boundary);
        let stale: Vec<String> = self
            .by_last_seen
            .range(..=(cutoff, String::from('\u{10FFFF}')))
            .map(|(_, symbol)| symbol.clone())
            .collect();
        stale
            .into_iter()
            .filter_map(|symbol| {
                let op = self.open.get(&symbol)?;
                // An overnight gap exceeds any inactivity timeout, so without
                // this the session boundary could never be the recorded reason
                // -- inactivity would always pre-empt it, and the two mean
                // different things for analysis.
                let (closed_at, reason) = if op.opened_at.date_naive() != now.date_naive() {
                    (op.last_seen_at, OpportunityCloseReason::SessionBoundary)
                } else {
                    (
                        op.last_seen_at + Duration::seconds(boundary),
                        OpportunityCloseReason::Inactivity,
                    )
                };
                self.close(&symbol, closed_at, reason)
            })
            .collect()
    }

    /// Evicts the least-recently-active opportunity when the derived bound is
    /// reached, marking it `CapacityReached` so saturation can never be mistaken
    /// for ordinary closure.
    fn enforce_capacity(&mut self, at: DateTime<Utc>, closed: &mut Vec<Opportunity>) {
        let bound = self.config.max_open_opportunities();
        if bound == 0 || self.open.len() < bound {
            return;
        }
        // The index is ordered by exactly the key this used to scan for, so the
        // least-recently-active opportunity is the first entry.
        let victim = self.by_last_seen.iter().next().map(|(_, symbol)| symbol.clone());
        if let Some(symbol) = victim {
            // Captured before `close` removes it, so the marker reports the
            // population that actually forced the eviction.
            let open_count = self.open.len();
            if let Some(op) = self.close(&symbol, at, OpportunityCloseReason::CapacityReached) {
                self.health.capacity_evictions += 1;
                if self.pending_evictions.len() < MAX_PENDING_CAPACITY_EVICTIONS {
                    self.pending_evictions.push(CapacityEviction {
                        at,
                        opportunity_id: op.id.as_key(),
                        symbol: op.symbol.clone(),
                        reason: "opportunity_capacity_reached".to_string(),
                        capacity: bound,
                        open_count,
                    });
                } else {
                    self.eviction_markers_dropped += 1;
                }
                closed.push(op);
            }
        }
    }

    fn close(
        &mut self,
        symbol: &str,
        at: DateTime<Utc>,
        reason: OpportunityCloseReason,
    ) -> Option<Opportunity> {
        let mut op = self.open.remove(symbol)?;
        self.by_last_seen.remove(&(op.last_seen_at, symbol.to_string()));
        debug_assert_eq!(self.by_last_seen.len(), self.open.len());
        // An opportunity can never close before it opened -- the same clamp the
        // episode tracker needed after Session 002's inverted records.
        op.closed_at = Some(at.max(op.opened_at));
        op.close_reason = Some(reason);
        self.health.opportunities_closed += 1;
        self.health.open_opportunities = self.open.len();
        Some(op)
    }

    /// Closes everything still open as `CaptureEnded` -- censored, not concluded.
    pub fn finish(&mut self, at: DateTime<Utc>) -> Vec<Opportunity> {
        let symbols: Vec<String> = self.open.keys().cloned().collect();
        symbols
            .into_iter()
            .filter_map(|s| self.close(&s, at, OpportunityCloseReason::CaptureEnded))
            .collect()
    }

    /// Scores and ranks every open opportunity if the cadence has elapsed.
    ///
    /// Produces **two independent rankings** (§8): early quality and
    /// continuation. One opportunity occupies at most one slot in each.
    pub fn rank(&mut self, now: DateTime<Utc>) -> Option<Vec<OpportunityScoreSnapshot>> {
        let due = self
            .last_ranked
            .is_none_or(|last| (now - last).num_seconds() >= self.config.ranking_cadence_secs);
        if !due || self.open.is_empty() {
            return None;
        }
        self.last_ranked = Some(now);
        self.ranking_windows += 1;
        let window_id = format!("oiw-{}", self.ranking_windows);

        let mut early_scored = Vec::new();
        let mut early_unranked = Vec::new();
        let mut cont_scored = Vec::new();
        let mut cont_unranked = Vec::new();
        let mut scored: Vec<(OpportunityId, ShadowScore, ShadowScore)> = Vec::new();

        let mut symbols: Vec<&String> = self.open.keys().collect();
        symbols.sort();
        let ordered: Vec<&Opportunity> =
            symbols.into_iter().filter_map(|s| self.open.get(s)).collect();
        for op in ordered {
            let eq = early_quality_score(op, now);
            let cc = continuation_confidence(op, now);
            match eq.value {
                Some(v) => early_scored.push((op.id.clone(), v)),
                None => early_unranked.push(op.id.as_key()),
            }
            match cc.value {
                Some(v) => cont_scored.push((op.id.clone(), v)),
                None => cont_unranked.push(op.id.as_key()),
            }
            scored.push((op.id.clone(), eq, cc));
        }

        let max_cohort = self.config.max_rank_cohort;
        let early = rank_cohort(early_scored, early_unranked, window_id.clone(), now, max_cohort);
        let cont = rank_cohort(cont_scored, cont_unranked, window_id.clone(), now, max_cohort);
        if early.cohort_truncated || cont.cohort_truncated {
            self.health.cohort_truncations += 1;
        }

        let early_rank: HashMap<&str, usize> =
            early.entries.iter().map(|e| (e.opportunity_id.as_str(), e.rank)).collect();
        let cont_rank: HashMap<&str, usize> =
            cont.entries.iter().map(|e| (e.opportunity_id.as_str(), e.rank)).collect();

        let versions = self.config.versions();
        let mut out = Vec::with_capacity(scored.len());
        let mut emit_detection_for: Vec<String> = Vec::new();
        for (id, eq, cc) in scored {
            let key = id.as_key();
            let op = match self.open.get(&id.symbol) {
                Some(op) => op,
                None => continue,
            };
            // Written once per opportunity, on its first ranking window.
            //
            // Absence on a later row therefore means "carried forward from
            // this opportunity's first row", NOT "unobserved" -- the same
            // change-based convention the discovery reduction uses, and stated
            // explicitly because §3 requires absence and unknown to stay
            // distinguishable. Emitting it on every window would roughly
            // double the log for information that cannot change.
            let detection_features =
                if op.detection_context_emitted { None } else { op.detection_context.clone() };
            if detection_features.is_some() {
                emit_detection_for.push(op.symbol.clone());
            }
            let regime = classify_regime(
                op.move_before_detection_pct,
                op.move_from_start_pct,
                &self.config,
            );
            let er = early_rank.get(key.as_str()).copied();
            let cr = cont_rank.get(key.as_str()).copied();
            let shadow_state = shadow_state_for(
                regime,
                er,
                cr,
                early.cohort_size,
                cont.cohort_size,
            );
            out.push(OpportunityScoreSnapshot {
                schema_version: OPPORTUNITY_SCHEMA_VERSION,
                versions: versions.clone(),
                timestamp: now,
                window_id: window_id.clone(),
                opportunity_id: key,
                symbol: op.symbol.clone(),
                session_date: op.session_date.clone(),
                regime,
                price_regime: classify_price_regime(op.latest_price, &self.config),
                opportunity_age_secs: op.age_secs(now),
                current_price: op.latest_price,
                observed_high: Some(op.observed_high),
                observed_low: Some(op.observed_low),
                max_move_pct: op.max_move_pct,
                min_move_pct: op.min_move_pct,
                opening_price: Some(op.opening_price),
                opened_at: Some(op.opened_at),
                move_from_start_pct: op.move_from_start_pct,
                move_before_detection_pct: op.move_before_detection_pct,
                raw_event_count: op.raw_event_count,
                episode_fragments: op.episode_fragments,
                invalidations_absorbed: op.invalidations_absorbed,
                detectors_seen: op.detectors_seen.keys().cloned().collect(),
                confluence_count: op.confluence_count(),
                confirmation_span_secs: op.confirmation_span_secs(),
                features: op.latest_context.clone(),
                detection_features: detection_features.clone(),
                early_quality: eq,
                early_quality_rank: er,
                continuation: cc,
                continuation_rank: cr,
                early_cohort_size: early.cohort_size,
                continuation_cohort_size: cont.cohort_size,
                shadow_state,
            });
        }
        for symbol in emit_detection_for {
            if let Some(op) = self.open.get_mut(&symbol) {
                op.detection_context_emitted = true;
            }
        }
        out.sort_by(|a, b| a.opportunity_id.cmp(&b.opportunity_id));
        self.health.scores_emitted += out.len() as u64;
        Some(out)
    }
}

/// Research-only alert semantics (§10). Top-decile within the contemporaneous
/// cohort, split by whether the move is still early or already underway.
fn shadow_state_for(
    regime: Regime,
    early_rank: Option<usize>,
    cont_rank: Option<usize>,
    early_cohort: usize,
    cont_cohort: usize,
) -> ShadowState {
    let decile = |rank: Option<usize>, cohort: usize| -> bool {
        match rank {
            Some(r) if cohort > 0 => (r as f64) <= (cohort as f64 * 0.10).max(1.0),
            _ => false,
        }
    };
    let early_top = decile(early_rank, early_cohort);
    let cont_top = decile(cont_rank, cont_cohort);
    match regime {
        Regime::EarlyEmerging if early_top => ShadowState::EarlyWatch,
        Regime::ContinuationAcceleration if cont_top && early_top => {
            ShadowState::HighConfidenceContinuation
        }
        Regime::ContinuationAcceleration if cont_top => ShadowState::Accelerating,
        Regime::ReversalRecovery if cont_top => ShadowState::Accelerating,
        _ => ShadowState::None,
    }
}

// ---------------------------------------------------------------------------
// Shared helpers -- intentionally mirroring `episode.rs` so the opportunity
// and episode units stay comparable rather than drifting apart.
// ---------------------------------------------------------------------------

/// Stable textual name for a strategy, used as the detector-confluence map key.
/// A name rather than the enum because `Strategy` is shared with
/// `strategy_config` and this module will not add trait derives to it.
fn strategy_name(strategy: Strategy) -> String {
    format!("{strategy:?}")
}

/// Which strategy, if any, this event constitutes an edge-triggered signal for.
/// Identical semantics to `episode::qualifying_strategy`.
fn qualifying_strategy(event: &ScanEvent) -> Option<Strategy> {
    match event {
        ScanEvent::FunnelSignal { passed: true, .. } => Some(Strategy::FastFunnel),
        ScanEvent::MomentumUpdate { qualifies: true, .. } => Some(Strategy::MomentumScorer),
        ScanEvent::IgnitionEvent { kind: IgnitionEventKind::FollowThroughConfirmed, .. } => {
            Some(Strategy::IgnitionDetector)
        }
        ScanEvent::ConsolidationEvent {
            kind: ConsolidationEventKind::EntryTriggered,
            strategy,
            ..
        } => Some(match strategy {
            market_data::events::ConsolidationStrategy::ConsolidationBreakout => {
                Strategy::ConsolidationBreakout
            }
            market_data::events::ConsolidationStrategy::Micropullback => Strategy::Micropullback,
        }),
        _ => None,
    }
}

/// Symbol, market time and the event's own price where it carries one.
fn event_symbol_time_price(event: &ScanEvent) -> Option<(String, DateTime<Utc>, Option<f64>)> {
    match event {
        ScanEvent::FunnelSignal { symbol, timestamp, price, .. } => {
            Some((symbol.clone(), *timestamp, Some(*price)))
        }
        ScanEvent::MomentumUpdate { symbol, timestamp, .. } => {
            Some((symbol.clone(), *timestamp, None))
        }
        ScanEvent::IgnitionEvent { symbol, timestamp, price, .. } => {
            Some((symbol.clone(), *timestamp, Some(*price)))
        }
        ScanEvent::ConsolidationEvent { symbol, timestamp, price, .. } => {
            Some((symbol.clone(), *timestamp, Some(*price)))
        }
        ScanEvent::HaltWarning { symbol, timestamp, current_price, .. } => {
            Some((symbol.clone(), *timestamp, Some(*current_price)))
        }
        ScanEvent::BarUpdate { symbol, timestamp, close, is_final, interval_secs, .. } => {
            // Same causal correction the measurement collector applies: a
            // finalised bar's close is only knowable one interval after the
            // bar's opening timestamp.
            let at = if *is_final {
                *timestamp + Duration::seconds(i64::from(*interval_secs))
            } else {
                *timestamp
            };
            Some((symbol.clone(), at, Some(*close)))
        }
        ScanEvent::CatalystUpdate { symbol, timestamp, .. } => {
            Some((symbol.clone(), *timestamp, None))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Offline driver (§13, §23 Phase E)
// ---------------------------------------------------------------------------

/// One observation in an ordered offline sequence: the event, plus the instant
/// the recorder received it.
///
/// `received_at` is carried separately and is *not* derivable from the event's
/// own timestamp -- that separation is what makes the R2 bar correction
/// meaningful, and collapsing the two would quietly make replay acausal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayObservation {
    pub received_at: DateTime<Utc>,
    pub event: ScanEvent,
}

/// Drives the engine over an ordered sequence and returns every snapshot it
/// produced, in order.
///
/// This is the **same** `OpportunityIntelligence` the live subscriber drives.
/// Replay parity is therefore a property of there being one implementation,
/// not of two implementations being kept in step -- which is the only version
/// of that claim worth making.
pub fn replay_events(
    config: OiConfig,
    events: &[ReplayObservation],
) -> Vec<OpportunityScoreSnapshot> {
    let mut engine = OpportunityIntelligence::new(config);
    let mut out = Vec::new();
    for observation in events {
        let _closed = engine.observe(&observation.event, observation.received_at);
        if let Some(snapshots) = engine.rank(observation.received_at) {
            out.extend(snapshots);
        }
    }
    if let Some(last) = events.last() {
        let _ = engine.finish(last.received_at);
    }
    out
}

#[cfg(test)]
#[path = "opportunity_identity_tests.rs"]
mod identity_tests;

#[cfg(test)]
#[path = "opportunity_tests.rs"]
mod tests;
