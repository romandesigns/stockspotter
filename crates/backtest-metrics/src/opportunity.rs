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
//! **D5 (2026-09-25): what "inactivity" means now depends on the lifecycle.**
//! Under the default [`Lifecycle::MoveV1`] only detector *evidence* keeps an
//! opportunity alive, opening is edge-triggered, and the session boundary is
//! the 04:00-ET market day; bars, trades, halt warnings and catalysts update
//! its state but never its life. The rule above -- any event refreshes it --
//! is [`Lifecycle::SymbolActivityV1`], kept selectable and byte-identical.
//! Invalidation is absorbed under both. See [`Lifecycle`].
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
use market_data::trading_session::{classify_session, market_day, TradingSession};
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
///
/// **3 as of 2026-09-25 (D5, `move-v1`).** An `opportunityId` now denotes one
/// causal move/setup, not a symbol's continuous event activity. Same key
/// format, same field names, different unit -- so `opportunityAgeSecs`,
/// `invalidationsAbsorbed`, `episodeFragments`, `moveBeforeDetectionPct`,
/// `detectionFeatures`, the extremes and `detectorsSeen` all change meaning.
/// See `docs/opportunity-lifecycle-move-v1-preregistration-2026-09-25.md`.
///
/// Only `move-v1` rows carry 3. A row produced under the retained
/// `symbol-activity-v1` lifecycle carries
/// [`SYMBOL_ACTIVITY_OPPORTUNITY_SCHEMA_VERSION`] (2), because that is what
/// its id denotes (preregistration amendment A1.1): schema 3 always means "a
/// move". Use [`OiConfig::opportunity_schema`] rather than this constant
/// wherever a row is stamped.
pub const OPPORTUNITY_SCHEMA_VERSION: u32 = 3;
/// The schema a `symbol-activity-v1` row carries: the pre-D5 unit, unchanged.
pub const SYMBOL_ACTIVITY_OPPORTUNITY_SCHEMA_VERSION: u32 = 2;
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
/// `OPPORTUNITY_SCHEMA_VERSION` deliberately did NOT move for that change:
/// opportunity identity and lifecycle were unchanged, and that bump was
/// reserved for the lifecycle-unit change (D5), which has since made it 3.
/// `OiVersions::baseline_policy` makes every row
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
/// `OiVersions::lifecycle` for a `move-v1` engine (D5).
pub const LIFECYCLE_MOVE_V1_VERSION: &str = "opportunity-lifecycle-move-v1";
/// `OiVersions::lifecycle` for the retained pre-D5 lifecycle. Also what a
/// row written before the field existed reads as.
pub const LIFECYCLE_SYMBOL_ACTIVITY_V1_VERSION: &str = "opportunity-lifecycle-symbol-activity-v1";

/// Which rule decides where one opportunity ends and the next begins.
///
/// # Why there are two, and why the default moved (D5, 2026-09-25)
///
/// The unit the project documents intended is **one causal move/setup**
/// (measurement-correctness contract, appendix D5.1). What V1 implemented was
/// "a symbol's continuous event activity": the 300s silence clock was
/// refreshed by *any* event, and a tracked symbol emits a bar, a funnel and a
/// momentum reading every minute and a halt warning on every trade -- so it
/// never went quiet, and an opportunity was operationally a symbol-day
/// (YMAT: open 46,240s, 197 invalidations absorbed, all five detectors).
///
/// * [`Lifecycle::MoveV1`], the default, implements the preregistered rule
///   (`docs/opportunity-lifecycle-move-v1-preregistration-2026-09-25.md`):
///   edge-triggered opening, only detector evidence extends life, market data
///   updates state only, the session boundary is the 04:00-ET market day.
/// * [`Lifecycle::SymbolActivityV1`] is the pre-D5 engine, kept byte-for-byte
///   so historical replay and the model-freeze proof keep their meaning.
///
/// The two are not comparable: an id denotes a different thing under each
/// (see `OPPORTUNITY_SCHEMA_VERSION`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Lifecycle {
    #[serde(rename = "symbol-activity-v1")]
    SymbolActivityV1,
    #[default]
    #[serde(rename = "move-v1")]
    MoveV1,
}

impl Lifecycle {
    /// The `OiVersions::lifecycle` string.
    pub fn version(self) -> &'static str {
        match self {
            Lifecycle::SymbolActivityV1 => LIFECYCLE_SYMBOL_ACTIVITY_V1_VERSION,
            Lifecycle::MoveV1 => LIFECYCLE_MOVE_V1_VERSION,
        }
    }
}

/// `T_move`, seconds. The existing 300s inactivity constant
/// (`episode::INACTIVITY_TIMEOUT_SECS`), not a new tuned number --
/// preregistration section 5.
pub const DEFAULT_MOVE_INACTIVITY_SECS: i64 = crate::episode::INACTIVITY_TIMEOUT_SECS;

/// Serde default for a serialized `OiConfig` that predates the field: such a
/// configuration ran the symbol-activity lifecycle (amendment A1.9).
fn legacy_lifecycle() -> Lifecycle {
    Lifecycle::SymbolActivityV1
}
fn default_move_inactivity_secs() -> i64 {
    DEFAULT_MOVE_INACTIVITY_SECS
}
fn legacy_lifecycle_version() -> String {
    LIFECYCLE_SYMBOL_ACTIVITY_V1_VERSION.to_string()
}

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
    /// Bound on each surface's ranked cohort, per ranking window.
    ///
    /// **Derived, not chosen: it equals the open-opportunity capacity**
    /// (`DEFAULT_MAX_RANK_COHORT`, 16,375), which makes truncation
    /// structurally unreachable. A ranked cohort is a subset of the open set
    /// (`rank` iterates `self.open` and nothing else), and the open set is
    /// held at or below `max_open_opportunities()` before every insert, so a
    /// bound at least that large can never cut a cohort.
    ///
    /// # Why it is no longer 4,096 (defect D6, 2026-09-25)
    ///
    /// 4,096 was never derived. It arrived with the V1 engine (79c21e1) when
    /// open capacity was 3,750, so it could not bind. The September-16
    /// capacity repair (af986b8) raised open capacity to 16,375 against a
    /// measured open peak of 4,808 and kept this at 4,096 as "out of scope",
    /// which made it reachable by its own measurements. It then bound in 69
    /// production windows (peak open ~4,678): scored opportunities were
    /// emitted with `*Rank = None` -- indistinguishable at that field from
    /// unscorable ones -- and `*CohortSize` was pinned at 4,096, so every
    /// cohort-normalised quantity (`shadowState`, alpha TopPercent) was wrong
    /// in those windows and the session was INVALID under completeness.
    ///
    /// The cap never bought anything: every open opportunity is scored,
    /// sorted and emitted before the cut, so removing it costs +0-1 ms per
    /// 30s window at the production peak and <= ~17 ms at full capacity, with
    /// transient memory within 2% (measured, see
    /// `docs/measurement-correctness-contract-2026-09-25.md` D6).
    ///
    /// Kept as a field rather than deleted: that keeps old config JSON
    /// deserialisable and makes the change visible in the fingerprint, which
    /// is the attribution handle for "cap removed". A configuration assembled
    /// at runtime below the open capacity is reported by
    /// `capacity_invariant` and every cut it causes is still counted and
    /// marked, never silent.
    pub max_rank_cohort: usize,
    /// Opportunity lifecycle rule (D5). Last in the struct so it is last in
    /// the canonical JSON the fingerprint hashes; included in it by design,
    /// because a lifecycle change is a change in what every row means.
    ///
    /// A serialized configuration without this field predates D5 and ran
    /// `symbol-activity-v1`, so that is what it reads as.
    #[serde(default = "legacy_lifecycle")]
    pub lifecycle: Lifecycle,
    /// `T_move` for `move-v1`: evidence silence after which a move ends.
    /// `inactivity_secs` remains the `symbol-activity-v1` clock and is not
    /// read by `move-v1`. Also in the fingerprint.
    #[serde(default = "default_move_inactivity_secs")]
    pub move_inactivity_secs: i64,
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

/// Default ranked-cohort bound: exactly the default open capacity, **16,375**.
///
/// Bound to the open capacity rather than to an observed peak, for the same
/// reason the open capacity is bound to the symbol universe: a flat number
/// between the observed peak and the reachable population is precisely the
/// defect (D6) this replaces.
pub const DEFAULT_MAX_RANK_COHORT: usize = DEFAULT_MAX_OPEN_OPPORTUNITIES;

/// D6, enforced at compile time for the shipped configuration: the ranked
/// cohort can never be smaller than the set it ranks. Lower this below the
/// open capacity and the build fails rather than reintroducing a silent
/// ranking truncation.
const _: () = {
    assert!(
        DEFAULT_MAX_RANK_COHORT >= DEFAULT_MAX_OPEN_OPPORTUNITIES,
        "ranked-cohort bound is below the open-opportunity capacity; scored opportunities \
         would be emitted unranked with a misreported cohort size (defect D6)"
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
            // D6: was 4,096. See the field doc for why it is now derived.
            max_rank_cohort: DEFAULT_MAX_RANK_COHORT,
            lifecycle: Lifecycle::MoveV1,
            move_inactivity_secs: DEFAULT_MOVE_INACTIVITY_SECS,
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
    /// capacity must cover the binding requirement, and (D6) the ranked-cohort
    /// bound must cover capacity.
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
        // D6: ranked cohort is a subset of the open set, so a bound at or
        // above open capacity makes truncation unreachable. Below it, a busy
        // window emits scored opportunities unranked and pins every
        // `*CohortSize` at the bound.
        if self.max_rank_cohort < capacity {
            return Err(format!(
                "ranked-cohort bound {} is below the open-opportunity capacity {capacity}; \
                 ranking windows with more scored opportunities than the bound would be \
                 truncated (defect D6)",
                self.max_rank_cohort
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

    /// The pre-D5 configuration: every field as it is today except the
    /// lifecycle. What historical replay and the model-freeze proof run.
    pub fn symbol_activity_v1() -> Self {
        Self { lifecycle: Lifecycle::SymbolActivityV1, ..Self::default() }
    }

    /// The opportunity schema a row produced under this configuration
    /// carries: 3 for `move-v1`, 2 for `symbol-activity-v1` (amendment A1.1).
    pub fn opportunity_schema(&self) -> u32 {
        match self.lifecycle {
            Lifecycle::MoveV1 => OPPORTUNITY_SCHEMA_VERSION,
            Lifecycle::SymbolActivityV1 => SYMBOL_ACTIVITY_OPPORTUNITY_SCHEMA_VERSION,
        }
    }

    /// The full version set, persisted with every research record.
    pub fn versions(&self) -> OiVersions {
        OiVersions {
            opportunity_schema: self.opportunity_schema(),
            feature_schema: OI_FEATURE_SCHEMA_VERSION,
            regime_classifier: REGIME_CLASSIFIER_VERSION.to_string(),
            price_regime: PRICE_REGIME_VERSION.to_string(),
            early_quality_model: EARLY_QUALITY_MODEL_VERSION.to_string(),
            continuation_model: CONTINUATION_MODEL_VERSION.to_string(),
            ranking: RANKING_VERSION.to_string(),
            score_policy: SCORE_POLICY_VERSION.to_string(),
            config_fingerprint: self.fingerprint(),
            baseline_policy: Some(crate::context::BASELINE_POLICY.to_string()),
            lifecycle: self.lifecycle.version().to_string(),
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
    /// The opportunity lifecycle rule (D5): `LIFECYCLE_MOVE_V1_VERSION` or
    /// `LIFECYCLE_SYMBOL_ACTIVITY_V1_VERSION`. A row written before the field
    /// existed ran the symbol-activity lifecycle and reads as it.
    #[serde(default = "legacy_lifecycle_version")]
    pub lifecycle: String,
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
    /// Under `move-v1` the same argument holds with `SetupInactivity` and
    /// `Invalidated` (each needs `T_move` of evidence silence) and a
    /// market-day `SessionBoundary`. The one residual -- an out-of-order event
    /// exactly at an earlier `opened_at` -- is refused and counted
    /// (`OiHealth::duplicate_identity_refused`) rather than minted twice.
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
    /// `symbol-activity-v1` only: `inactivity_secs` without *any* event for
    /// the symbol. Kept so schema <= 2 artifacts stay readable; `move-v1`
    /// never writes it.
    Inactivity,
    /// `move-v1`: `T_move` without relevant evidence, the last evidence being
    /// positive. `closed_at = last_relevant_at + T_move`.
    SetupInactivity,
    /// `move-v1`: the same clock, when the last evidence was an ignition
    /// `FollowThroughRejected` with no positive evidence after it. A distinct
    /// label on the same causal clock, never an immediate close -- a
    /// confirm -> reject -> confirm within `T_move` is still one move.
    Invalidated,
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
    /// `move-v1`: event time of the latest *positive* detector evidence --
    /// the clock `T_move` runs against. Market data never moves it, and an
    /// invalidation never moves it (preregistration section 4). Monotone
    /// (amendment A1.5). `None` under `symbol-activity-v1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_relevant_at: Option<DateTime<Utc>>,
    /// `move-v1`: whether the most recent evidence was positive or an
    /// invalidation; decides `SetupInactivity` versus `Invalidated`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_evidence_kind: Option<EvidenceKind>,
    /// `move-v1`: `market_data::classify_session(opened_at)`, so analysis can
    /// filter by phase without the lifecycle splitting at the bell
    /// (preregistration section 9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened_phase: Option<TradingSession>,
}

/// What a detector event says about a `move-v1` opportunity's life
/// (preregistration section 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// The setup is continuing: refreshes `last_relevant_at`.
    Positive,
    /// An ignition `FollowThroughRejected`: absorbed and counted, no refresh.
    Invalidation,
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
///
/// Scores compare with `f64::total_cmp` (D6), not `partial_cmp(..)
/// .unwrap_or(Equal)`. The latter is not a total order once a NaN is present
/// -- NaN would compare "equal" to everything while finite scores do not, so
/// the sort's result would depend on input order, and recent `std` sorts may
/// panic on an inconsistent comparator. `total_cmp` is total for every bit
/// pattern, so the order stays `(score desc, symbol, sequence)` and is
/// reproducible. Its only differences from the old comparator on non-NaN
/// input: `-0.0` now orders just below `0.0` (previously tied and broken by
/// symbol). For ordinary finite, non-zero-signed scores the order -- and
/// therefore every rank -- is identical, which the top-rank invariance test
/// pins. A NaN score (reachable in principle through a `Raw` transform) sorts
/// by its sign bit: positive NaN ahead of every finite score, negative NaN
/// behind. Deterministic, stated, and a finding if it ever appears; making
/// non-finite values unrankable would be a score-policy change, not a ranking
/// one, and is deliberately not done here.
fn rank_cohort(
    scored: Vec<(OpportunityId, f64)>,
    unranked: Vec<String>,
    window_id: String,
    ranked_at: DateTime<Utc>,
    max_cohort: usize,
) -> Ranking {
    let mut scored = scored;
    scored.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
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

/// The share of the same surface's ranked cohort ordered strictly ahead of
/// this opportunity: `(rank - 1) / cohort_size`, in `[0, 1)`. Lower is better,
/// the same direction as `rank`. `None` exactly when there is no rank.
///
/// # Why this definition (D6)
///
/// Three normalisations already disagree in this codebase --
/// `shadow_state_for` uses `rank <= max(0.10 N, 1)`, V2's `current_percentile`
/// uses `100 rank / N`, alpha `TopPercent` uses `rank <= ceil(N p)`. This one
/// is chosen because it matches the **evaluation** contract exactly: for an
/// integer rank, `rank <= ceil(N p)` iff `rank - 1 < N p` iff
/// `fraction < p`, so filtering `fraction < 0.10` selects precisely alpha's
/// top-10% cohort (pinned by a test over p in {0.05, 0.10, 0.25}). `rank / N`
/// disagrees at the boundary (N = 10, p = 0.25: rank 3 is inside `ceil`, yet
/// 3/10 > 0.25). It is defined at N = 1 (0.0, "best"), where
/// `(N - rank)/(N - 1)` divides by zero and `rank / N` calls a lone candidate
/// the worst.
///
/// The denominator is the per-surface *ranked* cohort -- scored rows only,
/// same window -- so it is contemporaneous and cannot be paired with the other
/// surface's size, which is the mistake `alpha::dataset` made with
/// `max(early, continuation)`. Computed from integers at emission, so it is
/// exactly reproducible. Ties inherit the ordinal rank's deterministic
/// `(symbol, sequence)` break rather than a mid-rank, so it never disagrees
/// with `rank`.
pub fn rank_fraction(rank: Option<usize>, cohort_size: usize) -> Option<f64> {
    match rank {
        Some(r) if r >= 1 && r <= cohort_size => Some((r - 1) as f64 / cohort_size as f64),
        _ => None,
    }
}

/// Which ranking surface a cohort belongs to. Used only to label the D6
/// truncation counters and markers, so a non-zero value says *which* surface
/// was cut rather than only that one was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RankSurface {
    EarlyQuality,
    Continuation,
}

/// One ranking window in which a surface's scored set exceeded
/// `max_rank_cohort`, persisted as a `ranking_cohort_truncated` capture marker.
///
/// Under the D6 invariant (`max_rank_cohort >= max_open_opportunities()`) this
/// cannot happen, so a marker means a runtime configuration violated the
/// invariant or the engine held more open opportunities than its bound -- a
/// safety-bound event, not market behaviour. Kept reachable on purpose: a
/// counter no configuration can drive is indistinguishable from one that does
/// not work (the `start_inner` lesson in `ws-server::opportunity_shadow`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CohortTruncation {
    pub window_id: String,
    pub ranked_at: DateTime<Utc>,
    pub surface: RankSurface,
    /// Opportunities with a score on this surface in this window.
    pub scored: usize,
    /// The bound that cut it, so `scored - cap` rows were emitted unranked.
    pub cap: usize,
    /// Always `ranking_cohort_truncated`, carried in-band like
    /// `CapacityEviction::reason`.
    pub reason: String,
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
    /// `(earlyQualityRank - 1) / earlyCohortSize`; see [`rank_fraction`].
    /// Additive and optional (D6): absent exactly when the rank is absent,
    /// and absent on every row written before the field existed. Does not
    /// replace the absolute rank, which stays authoritative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub early_quality_rank_fraction: Option<f64>,
    /// `(continuationRank - 1) / continuationCohortSize`; same contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_rank_fraction: Option<f64>,
    /// `move-v1`: the trading session the opportunity opened in
    /// (`premarket | regular | after_hours | overnight`). Absent under
    /// `symbol-activity-v1` and on every row written before D5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened_phase: Option<TradingSession>,
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
    /// Ranking windows in which **either** surface was cut by
    /// `max_rank_cohort`. Kept as the OR for backward compatibility; the two
    /// per-surface counters below say which. Under the D6 invariant every one
    /// of these is structurally zero, so non-zero means a mis-specified bound
    /// or an engine defect -- a safety-bound event, never market behaviour.
    pub cohort_truncations: u64,
    /// D6: windows in which the EarlyQuality surface was cut.
    #[serde(default)]
    pub early_cohort_truncations: u64,
    /// D6: windows in which the Continuation surface was cut.
    #[serde(default)]
    pub continuation_cohort_truncations: u64,
    /// The bound the ranked cohorts are held against, reported beside them
    /// for the same reason `opportunity_capacity` sits beside the open peak.
    #[serde(default)]
    pub rank_cohort_capacity: usize,
    /// Scored (= ranked, when untruncated) cohort size per surface in the most
    /// recent ranking window, and the largest seen. The true N, not the
    /// post-truncation length.
    #[serde(default)]
    pub early_cohort_last: usize,
    #[serde(default)]
    pub continuation_cohort_last: usize,
    #[serde(default)]
    pub early_cohort_peak: usize,
    #[serde(default)]
    pub continuation_cohort_peak: usize,
    /// Ranking windows produced.
    #[serde(default)]
    pub ranking_windows: u64,
    /// `ranking_cohort_truncated` markers that overflowed their buffer. The
    /// truncations themselves are still counted exactly above.
    #[serde(default)]
    pub truncation_markers_dropped: u64,
    pub opportunities_opened: u64,
    pub opportunities_closed: u64,
    /// `opportunities_closed`, split by the engine's close reason. A fixed
    /// enum, so bounded. `capacity_reached` duplicates `capacity_evictions`
    /// as a cross-check.
    #[serde(default)]
    pub closed_by_reason: ClosedByReason,
    pub raw_events_observed: u64,
    pub scores_emitted: u64,
    /// `move-v1`: opening edges refused because the id they would mint had
    /// already been issued for the symbol (preregistration section 7). Only
    /// an out-of-order event exactly at an earlier `opened_at` can cause
    /// one. A qualification gate: non-zero means the stream was not causal.
    #[serde(default)]
    pub duplicate_identity_refused: u64,
}

/// Closes by `OpportunityCloseReason`. One field per variant; adding a close
/// reason without a field here fails `ClosedByReason::count`'s exhaustive
/// match. `default` on the struct so an older health document lacking a
/// reason still parses; the gates read presence from the raw JSON instead.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ClosedByReason {
    pub inactivity: u64,
    pub session_boundary: u64,
    pub capacity_reached: u64,
    pub capture_ended: u64,
    /// D5 `move-v1` reasons. Default 0 so a pre-D5 report still parses.
    #[serde(default)]
    pub setup_inactivity: u64,
    #[serde(default)]
    pub invalidated: u64,
}

impl ClosedByReason {
    pub fn count(&mut self, reason: OpportunityCloseReason) {
        let slot = match reason {
            OpportunityCloseReason::Inactivity => &mut self.inactivity,
            OpportunityCloseReason::SetupInactivity => &mut self.setup_inactivity,
            OpportunityCloseReason::Invalidated => &mut self.invalidated,
            OpportunityCloseReason::SessionBoundary => &mut self.session_boundary,
            OpportunityCloseReason::CapacityReached => &mut self.capacity_reached,
            OpportunityCloseReason::CaptureEnded => &mut self.capture_ended,
        };
        *slot += 1;
    }

    pub fn total(&self) -> u64 {
        self.inactivity
            + self.setup_inactivity
            + self.invalidated
            + self.session_boundary
            + self.capacity_reached
            + self.capture_ended
    }
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

/// Pending `ranking_cohort_truncated` markers retained between drains.
///
/// At most two per ranking window (one per surface), drained after every
/// observation, and structurally zero under the D6 invariant -- so this only
/// ever holds anything under a mis-specified bound. Bounded regardless, and
/// overflow is counted in `OiHealth::truncation_markers_dropped`.
const MAX_PENDING_COHORT_TRUNCATIONS: usize = 256;

/// `move-v1` per-symbol state that outlives individual opportunities
/// (preregistration section 2, amendments A1.3 and A1.7).
#[derive(Debug, Default)]
struct EdgeState {
    /// Last `FunnelSignal::passed`; `false` when never seen.
    funnel_passing: bool,
    /// Last `MomentumUpdate::qualifies`; `false` when never seen.
    momentum_qualifying: bool,
    /// Market day `funnel_passing`/`momentum_qualifying` belong to (amendment
    /// A2). A reading from a later market day starts both from `false` --
    /// what a freshly started process would see -- so a day's opening edges
    /// do not depend on whether the process ran through the previous one.
    edge_market_day: Option<chrono::NaiveDate>,
    /// Latest market day any event for this symbol carried (amendment A3).
    /// An event from an earlier market day is a late correction and is
    /// ignored by the `move-v1` lifecycle entirely.
    latest_market_day: Option<chrono::NaiveDate>,
    /// Market day `issued` belongs to.
    issued_market_day: Option<chrono::NaiveDate>,
    /// `(UTC date, sequence)` of every opportunity issued for this symbol on
    /// `issued_market_day`: the duplicate-identity guard's memory. One entry
    /// per open, and consecutive opens need `T_move` of silence or a market
    /// day change, so this holds at most a few hundred entries and is
    /// cleared every market day.
    issued: Vec<(chrono::NaiveDate, u32)>,
}

/// How `record` folds one event into an open opportunity, per lifecycle.
#[derive(Debug, Clone, Copy)]
enum Step {
    /// The pre-D5 rule: every event refreshes life; a rejection is absorbed.
    SymbolActivity { invalidating: bool },
    /// `move-v1`: `edge` joins as confluence, `evidence` alone decides life.
    Move { edge: Option<Strategy>, evidence: Option<EvidenceKind> },
}

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
    ///
    /// **Under `move-v1` the key is `last_relevant_at`, not `last_seen_at`**
    /// (see `clock_key`): the deadline is `last_relevant_at + T_move`, so the
    /// same "key order is due order" argument holds on that clock, and the
    /// capacity victim becomes the least recently *relevant* opportunity
    /// (amendment A1.10). The field keeps its name so the symbol-activity
    /// path reads exactly as it did.
    by_last_seen: std::collections::BTreeSet<(DateTime<Utc>, String)>,
    /// `move-v1` per-symbol edge state (preregistration section 2). Outlives
    /// every opportunity and is never reset by the engine (amendment A1.3);
    /// keyed by symbol, so bounded by the universe. Empty under
    /// `symbol-activity-v1`.
    edges: HashMap<String, EdgeState>,
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
    /// D6 truncation markers awaiting persistence. Drained by the caller.
    pending_truncations: Vec<CohortTruncation>,
    /// UTC date of the most recent `open_new` -- the `sessionDate` the engine
    /// is currently assigning to identities. Observability only (reported on
    /// the health route so the UTC rollover is visible while it happens).
    current_session_date: Option<chrono::NaiveDate>,
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
        let rank_cohort_capacity = config.max_rank_cohort;
        Self {
            config,
            open: HashMap::new(),
            by_last_seen: std::collections::BTreeSet::new(),
            edges: HashMap::new(),
            features: FeatureCache::default(),
            last_ranked: None,
            ranking_windows: 0,
            health: OiHealth {
                opportunity_capacity: capacity,
                rank_cohort_capacity: rank_cohort_capacity,
                ..OiHealth::default()
            },
            pending_evictions: Vec::new(),
            eviction_markers_dropped: 0,
            pending_truncations: Vec::new(),
            current_session_date: None,
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

    /// The UTC `sessionDate` the engine most recently assigned, if any
    /// opportunity has opened. Not a market day: see the D3/D7 contract.
    pub fn current_session_date(&self) -> Option<chrono::NaiveDate> {
        self.current_session_date
    }

    /// Takes the D6 `ranking_cohort_truncated` markers accumulated since the
    /// last call. Empty under the shipped configuration by construction.
    pub fn take_cohort_truncations(&mut self) -> Vec<CohortTruncation> {
        std::mem::take(&mut self.pending_truncations)
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

        if self.config.lifecycle == Lifecycle::MoveV1 {
            self.observe_move(event, &symbol, at, price, received_at, &mut closed);
            return closed;
        }

        // ---- symbol-activity-v1: the pre-D5 engine, unchanged -------------
        //
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
            self.record(&symbol, event, at, price, Step::SymbolActivity { invalidating });
        } else if let Some(strategy) = qualifying_strategy(event) {
            let Some(price) = price.or_else(|| self.features.last_price(&symbol)) else {
                return closed;
            };
            self.enforce_capacity(at, &mut closed);
            self.open_new(&symbol, strategy, at, price, received_at);
        }
        closed
    }

    /// `move-v1` (preregistration sections 2-7): one opportunity per causal
    /// move/setup.
    ///
    /// Order matters and is the whole causal argument:
    ///
    /// 1. expiry has already run on the receipt clock (`observe`);
    /// 2. the per-symbol edge state is updated from **this** event alone,
    ///    whether or not an opportunity is open -- it outlives opportunities;
    /// 3. a market-day change closes the open opportunity (`SessionBoundary`
    ///    at `at`) before this event is folded into anything;
    /// 4. an open opportunity absorbs the event -- state always, life only if
    ///    the event is evidence; an edge joins it as confluence;
    /// 5. otherwise only an opening edge opens, and only if the id it would
    ///    mint has not been issued before.
    ///
    /// Nothing here reads an event that has not yet arrived, so truncating
    /// the stream at any point leaves every earlier decision unchanged.
    fn observe_move(
        &mut self,
        event: &ScanEvent,
        symbol: &str,
        at: DateTime<Utc>,
        price: Option<f64>,
        received_at: DateTime<Utc>,
        closed: &mut Vec<Opportunity>,
    ) {
        // Amendment A3: an event from an earlier market day than this symbol
        // has already reached is a late correction. It cannot close, open,
        // extend or update anything, and it cannot touch edge state.
        let day = market_day(at);
        {
            let state = self.edges.entry(symbol.to_string()).or_default();
            match state.latest_market_day {
                Some(latest) if day < latest => return,
                _ => state.latest_market_day = Some(day),
            }
        }

        let (edge, evidence) = self.classify_move_evidence(symbol, event);

        // Section 5.3 as amended by A3: only a LATER 04:00-ET market day
        // closes the move -- not the UTC date, which split after-hours at
        // 19:00 ET all winter, and not an earlier day (handled above).
        if let Some(existing) = self.open.get(symbol) {
            if day > market_day(existing.opened_at) {
                if let Some(op) = self.close(symbol, at, OpportunityCloseReason::SessionBoundary) {
                    closed.push(op);
                }
            }
        }

        if self.open.contains_key(symbol) {
            self.record(symbol, event, at, price, Step::Move { edge, evidence });
            return;
        }
        let Some(strategy) = edge else { return };
        // Amendment A1.4: no known price, no open; the edge is spent.
        let Some(price) = price.or_else(|| self.features.last_price(symbol)) else {
            return;
        };
        // Section 7: refuse, never collide.
        let identity = (at.date_naive(), OpportunityId::sequence_for(at));
        let day = market_day(at);
        let state = self.edges.entry(symbol.to_string()).or_default();
        if state.issued_market_day != Some(day) {
            state.issued_market_day = Some(day);
            state.issued.clear();
        }
        if state.issued.contains(&identity) {
            self.health.duplicate_identity_refused += 1;
            tracing::warn!(
                symbol,
                opened_at = %at,
                "opportunity open refused: its id was already issued (out-of-order event)"
            );
            return;
        }
        state.issued.push(identity);
        self.enforce_capacity(at, closed);
        self.open_new(symbol, strategy, at, price, received_at);
    }

    /// `symbol`'s edge state for the market day of an edge-bearing reading
    /// timestamped `at` (amendment A2). The first reading of a later market
    /// day resets both levels to `false`, exactly as a process started that
    /// morning would hold them. A reading from an earlier market day than the
    /// state's -- a late correction -- returns `None` and changes nothing.
    fn day_scoped_edges(&mut self, symbol: &str, at: DateTime<Utc>) -> Option<&mut EdgeState> {
        let day = market_day(at);
        let state = self.edges.entry(symbol.to_string()).or_default();
        match state.edge_market_day {
            Some(current) if day < current => return None,
            Some(current) if day == current => {}
            _ => {
                state.funnel_passing = false;
                state.momentum_qualifying = false;
                state.edge_market_day = Some(day);
            }
        }
        Some(state)
    }

    /// Updates `symbol`'s edge state from `event` and says what the event is
    /// under `move-v1`: an opening edge (and for which strategy), and what it
    /// does to an open opportunity's life. Preregistration sections 3 and 4.
    ///
    /// The previous funnel/momentum value is `false` for a symbol never seen,
    /// as in `LiveSignalTracker`, so the first `true` reading a process sees
    /// is an edge (section 2). Only funnel and momentum readings create an
    /// entry; the map is keyed by symbol, so it is bounded by the universe.
    fn classify_move_evidence(
        &mut self,
        symbol: &str,
        event: &ScanEvent,
    ) -> (Option<Strategy>, Option<EvidenceKind>) {
        use EvidenceKind::{Invalidation, Positive};
        match event {
            ScanEvent::FunnelSignal { passed, timestamp, .. } => {
                let Some(state) = self.day_scoped_edges(symbol, *timestamp) else {
                    return (None, None);
                };
                let was = std::mem::replace(&mut state.funnel_passing, *passed);
                if *passed && !was {
                    (Some(Strategy::FastFunnel), Some(Positive))
                } else {
                    // The level `passed: true` is the gap/universe filter
                    // holding all day: state only, never life.
                    (None, None)
                }
            }
            ScanEvent::MomentumUpdate { qualifies, timestamp, .. } => {
                let Some(state) = self.day_scoped_edges(symbol, *timestamp) else {
                    return (None, None);
                };
                let was = std::mem::replace(&mut state.momentum_qualifying, *qualifies);
                match (*qualifies, was) {
                    (true, false) => (Some(Strategy::MomentumScorer), Some(Positive)),
                    // The detector re-asserts the setup on every bar.
                    (true, true) => (None, Some(Positive)),
                    (false, _) => (None, None),
                }
            }
            ScanEvent::IgnitionEvent { kind, .. } => match kind {
                IgnitionEventKind::FollowThroughConfirmed => {
                    (Some(Strategy::IgnitionDetector), Some(Positive))
                }
                IgnitionEventKind::CandidateOpened => (None, Some(Positive)),
                IgnitionEventKind::FollowThroughRejected => (None, Some(Invalidation)),
            },
            ScanEvent::ConsolidationEvent { kind, strategy, .. } => match kind {
                ConsolidationEventKind::EntryTriggered => (
                    Some(match strategy {
                        market_data::events::ConsolidationStrategy::ConsolidationBreakout => {
                            Strategy::ConsolidationBreakout
                        }
                        market_data::events::ConsolidationStrategy::Micropullback => {
                            Strategy::Micropullback
                        }
                    }),
                    Some(Positive),
                ),
                ConsolidationEventKind::SurgeDetected
                | ConsolidationEventKind::ConsolidationConfirmed => (None, Some(Positive)),
            },
            // Bars, halt proximity, catalysts, funnel health: state only.
            _ => (None, None),
        }
    }

    /// The instant `by_last_seen` is keyed on for `op` under this engine's
    /// lifecycle: `last_seen_at` (symbol-activity) or `last_relevant_at`
    /// (move). Every insert and remove goes through this, so the index can
    /// never be keyed on one clock and searched on the other.
    fn clock_key(&self, op: &Opportunity) -> DateTime<Utc> {
        match self.config.lifecycle {
            Lifecycle::SymbolActivityV1 => op.last_seen_at,
            Lifecycle::MoveV1 => op.last_relevant_at.unwrap_or(op.opened_at),
        }
    }

    fn open_new(
        &mut self,
        symbol: &str,
        strategy: Strategy,
        at: DateTime<Utc>,
        price: f64,
        received_at: DateTime<Utc>,
    ) {
        self.current_session_date = Some(at.date_naive());
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
        // `move-v1` only; absent under symbol-activity so that lifecycle's
        // records stay byte-identical to the pre-D5 engine (amendment A1.8).
        let moving = self.config.lifecycle == Lifecycle::MoveV1;
        self.open.insert(
            symbol.to_string(),
            Opportunity {
                schema_version: self.config.opportunity_schema(),
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
                // The opening edge is itself positive evidence.
                last_relevant_at: moving.then_some(at),
                last_evidence_kind: moving.then_some(EvidenceKind::Positive),
                opened_phase: moving.then(|| classify_session(at)),
            },
        );
        // Both clocks start at `at`, so this key is right for either lifecycle.
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
        step: Step,
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
        let invalidating = match step {
            Step::SymbolActivity { invalidating } => {
                // Reindex before the field moves, or the old key is unreachable
                // and the set desynchronises from `open`.
                let previous = op.last_seen_at;
                op.last_seen_at = at;
                if previous != at {
                    self.by_last_seen.remove(&(previous, symbol.to_string()));
                    self.by_last_seen.insert((at, symbol.to_string()));
                }
                invalidating
            }
            Step::Move { evidence, .. } => {
                // `last_seen_at` stays "last event of any kind" -- it is state,
                // and membership reads it for a still-open window -- but it is
                // no longer the clock, so it is not indexed.
                op.last_seen_at = at;
                match evidence {
                    Some(EvidenceKind::Positive) => {
                        let previous = op.last_relevant_at.unwrap_or(op.opened_at);
                        // Monotone (amendment A1.5): an out-of-order older
                        // confirmation cannot shorten a live move.
                        let refreshed_at = previous.max(at);
                        op.last_relevant_at = Some(refreshed_at);
                        op.last_evidence_kind = Some(EvidenceKind::Positive);
                        if refreshed_at != previous {
                            self.by_last_seen.remove(&(previous, symbol.to_string()));
                            self.by_last_seen.insert((refreshed_at, symbol.to_string()));
                        }
                        false
                    }
                    Some(EvidenceKind::Invalidation) => {
                        // Absorbed and counted, NO refresh: the rejection
                        // labels how the move will end if nothing positive
                        // follows, and never extends it.
                        op.last_evidence_kind = Some(EvidenceKind::Invalidation);
                        true
                    }
                    None => false,
                }
            }
        };
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
        // Confluence. Under `move-v1` only an opening edge joins as a detector
        // (amendment A1.2) and every edge is a confirming moment; under
        // symbol-activity the V1 level rule stands.
        let confluence = match step {
            Step::SymbolActivity { .. } => qualifying_strategy(event).map(|s| {
                let confirming = matches!(
                    event,
                    ScanEvent::IgnitionEvent {
                        kind: IgnitionEventKind::FollowThroughConfirmed, ..
                    } | ScanEvent::ConsolidationEvent {
                        kind: ConsolidationEventKind::EntryTriggered, ..
                    } | ScanEvent::FunnelSignal { passed: true, .. }
                        | ScanEvent::MomentumUpdate { qualifies: true, .. }
                );
                (s, confirming)
            }),
            Step::Move { edge, .. } => edge.map(|s| (s, true)),
        };
        if let Some((strategy, confirming)) = confluence {
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
        if self.config.lifecycle == Lifecycle::MoveV1 {
            return self.expire_moves(now);
        }
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
                //
                // `last_seen_at` here is backdated (it mirrors the episode
                // tracker); `close` floors it at the last ranking instant so it
                // can never precede an anchor issued while the opportunity was
                // open (D4-5).
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

    /// `move-v1` terminal conditions 1-3 (preregistration section 5), on the
    /// same range query: the index is keyed by `last_relevant_at` here, so
    /// the due set is still a prefix.
    ///
    /// * `SetupInactivity` / `Invalidated`: `now - last_relevant_at >= T_move`,
    ///   labelled by `last_evidence_kind`, `closed_at = last_relevant_at +
    ///   T_move`. Market data cannot defer this: it never touches the key.
    /// * `SessionBoundary` when the due opportunity opened on an earlier
    ///   market day than `now`'s, dated `last_relevant_at` and then floored by
    ///   `close` at the last ranking instant (P2 D4; amendment A1.6).
    fn expire_moves(&mut self, now: DateTime<Utc>) -> Vec<Opportunity> {
        let boundary = self.config.move_inactivity_secs;
        let cutoff = now - Duration::seconds(boundary);
        let stale: Vec<String> = self
            .by_last_seen
            .range(..=(cutoff, String::from('\u{10FFFF}')))
            .map(|(_, symbol)| symbol.clone())
            .collect();
        let today = market_day(now);
        stale
            .into_iter()
            .filter_map(|symbol| {
                let op = self.open.get(&symbol)?;
                let last_relevant = op.last_relevant_at.unwrap_or(op.opened_at);
                let (closed_at, reason) = if market_day(op.opened_at) != today {
                    (last_relevant, OpportunityCloseReason::SessionBoundary)
                } else {
                    let reason = match op.last_evidence_kind {
                        Some(EvidenceKind::Invalidation) => OpportunityCloseReason::Invalidated,
                        _ => OpportunityCloseReason::SetupInactivity,
                    };
                    (last_relevant + Duration::seconds(boundary), reason)
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
        let key = self.clock_key(&op);
        self.by_last_seen.remove(&(key, symbol.to_string()));
        debug_assert_eq!(self.by_last_seen.len(), self.open.len());
        // An opportunity can never close before it opened -- the same clamp the
        // episode tracker needed after Session 002's inverted records.
        //
        // Nor before an instant at which it was demonstrably open and ranked
        // (D4-5, 2026-09-25). A session boundary found by expiry is dated
        // `last_seen_at`, which is backdated: the opportunity stayed open --
        // and was ranked, so outcome anchors were issued for it -- for up to
        // `inactivity_secs` after that. Without this floor those anchors carry
        // `anchor_at > closedAt`, i.e. a row measured from an opportunity that
        // had supposedly already closed. The same inversion is possible on the
        // event-clock paths (event-path session boundary, capacity eviction),
        // because `at` is event time while ranking runs on the receipt clock.
        //
        // `last_ranked` is the most recent ranking instant, and it is a valid
        // floor for *this* opportunity exactly when the opportunity was ranked
        // at least once: `rank` scores every open opportunity, so one that has
        // been ranked and is still open was in every window since, including
        // the latest. `detection_context_emitted` is that flag -- it is set in
        // `rank` on the opportunity's first ranked window and on no other path
        // (`open_new` always supplies a detection context). An opportunity
        // never ranked issued no anchor, so it needs no floor.
        //
        // The floor only ever raises `closed_at`, and only in the inverted
        // cases; inactivity closes (`last_seen_at + inactivity_secs`) already
        // exceed every anchor issued while open. `closeObservedAt`, not this,
        // is what outcome disposition is decided on -- see
        // `opportunity_outcome::ClosureNotice`.
        let ranked_floor = if op.detection_context_emitted { self.last_ranked } else { None };
        let mut closed_at = at.max(op.opened_at);
        if let Some(floor) = ranked_floor {
            closed_at = closed_at.max(floor);
        }
        op.closed_at = Some(closed_at);
        op.close_reason = Some(reason);
        self.health.opportunities_closed += 1;
        self.health.closed_by_reason.count(reason);
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
        let (early_n, cont_n) = (early_scored.len(), cont_scored.len());
        let early = rank_cohort(early_scored, early_unranked, window_id.clone(), now, max_cohort);
        let cont = rank_cohort(cont_scored, cont_unranked, window_id.clone(), now, max_cohort);

        // D6 accounting. The true scored N per surface -- not the ranked
        // length, which a cut would shorten -- so the health surface reports
        // the population rather than the bound.
        let h = &mut self.health;
        h.ranking_windows += 1;
        h.early_cohort_last = early_n;
        h.continuation_cohort_last = cont_n;
        h.early_cohort_peak = h.early_cohort_peak.max(early_n);
        h.continuation_cohort_peak = h.continuation_cohort_peak.max(cont_n);
        if early.cohort_truncated || cont.cohort_truncated {
            // The OR, unchanged in meaning, so existing readers keep working.
            h.cohort_truncations += 1;
        }
        for (ranking, surface, scored) in [
            (&early, RankSurface::EarlyQuality, early_n),
            (&cont, RankSurface::Continuation, cont_n),
        ] {
            if !ranking.cohort_truncated {
                continue;
            }
            match surface {
                RankSurface::EarlyQuality => self.health.early_cohort_truncations += 1,
                RankSurface::Continuation => self.health.continuation_cohort_truncations += 1,
            }
            // Unreachable under the D6 invariant, so loud when it happens: the
            // artifact must self-report the cut rather than leave it to be
            // inferred from cohort sizes pinned at the bound -- which is how
            // the 69 production truncations had to be found.
            tracing::error!(
                window_id = %window_id,
                surface = ?surface,
                scored,
                cap = max_cohort,
                "opportunity ranking cohort truncated; the rank bound is below the open set"
            );
            if self.pending_truncations.len() < MAX_PENDING_COHORT_TRUNCATIONS {
                self.pending_truncations.push(CohortTruncation {
                    window_id: window_id.clone(),
                    ranked_at: now,
                    surface,
                    scored,
                    cap: max_cohort,
                    reason: "ranking_cohort_truncated".to_string(),
                });
            } else {
                self.health.truncation_markers_dropped += 1;
            }
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
                schema_version: versions.opportunity_schema,
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
                early_quality_rank_fraction: rank_fraction(er, early.cohort_size),
                continuation_rank_fraction: rank_fraction(cr, cont.cohort_size),
                opened_phase: op.opened_phase,
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

#[cfg(test)]
#[path = "opportunity_lifecycle_tests.rs"]
mod lifecycle_tests;
