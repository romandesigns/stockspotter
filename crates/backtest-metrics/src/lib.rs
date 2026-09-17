//! Backtest logging & metrics — architecture doc section 8. Builds on
//! `replay_engine`: extracts discrete signal moments from a replay
//! result, evaluates each against a simple target/stop outcome model,
//! logs every one to an append-only file, and aggregates hit
//! rate/average winning move/timing accuracy per strategy.

pub mod completeness;
pub mod alpha;
pub mod attribution;
pub mod context;
pub mod episode;
pub mod evaluation;
pub mod horizon;
pub mod log_levels;
pub mod live_signals;
pub mod log;
pub mod membership;
pub mod metrics;
pub mod opportunity;
pub mod outcome;
pub mod quote_execution;
pub mod session_finder;
pub mod signals;
pub mod strategy_config;

pub use attribution::{
    attribute, attribute_all, coverage, AttributedSymbol, Attribution, AttributionCoverage, Stage,
    SymbolEvidence, UnknownReason, ATTRIBUTION_SCHEMA_VERSION,
};
pub use context::{
    CatalystFeatures, ConsolidationFeatures, FeatureCache, FunnelFeatures, HaltFeatures,
    IgnitionFeatures, MarketFeatures, MomentumFeatures, PreDetectionContext, SignalContext,
    SIGNAL_CONTEXT_SCHEMA_VERSION,
};
pub use episode::{
    EpisodeCloseReason, EpisodeId, EpisodeTracker, OpportunityEpisode, ResearchRank, TraderLinkage,
    EPISODE_SCHEMA_VERSION, INACTIVITY_TIMEOUT_SECS,
};
pub use evaluation::{
    build_evaluation, reconstruct_opportunities_from_snapshots, AnchorSelection, EvaluationCoverage, EvaluationCoverageReport,
    EvaluationSet, JoinGap, JoinedOutcome, OpportunityEvaluationRecord, ANCHOR_RULE_VERSION,
    EVALUATION_SCHEMA_VERSION,
};
pub use horizon::{
    evaluate_horizons, CensorReason, Excursion, HorizonOutcome, HorizonReturn, Observation,
    TargetTiming, HORIZON_SCHEMA_VERSION, HORIZON_SECS,
};
pub use live_signals::{append_pending, read_pending, write_pending, LiveSignalTracker, PendingSignal};
pub use log::{append, read_all, LoggedSignal, MAX_FORWARD_PATH_BARS};
pub use membership::{
    map_memberships, AmbiguityReason, AmbiguousEpisode, MembershipReport, MembershipStatus,
    OpportunityMembership, UnassignedEpisode, UnassignedReason, MEMBERSHIP_RULES_VERSION,
    MEMBERSHIP_SCHEMA_VERSION,
};
pub use metrics::{aggregate, aggregate_by_strategy, AggregateMetrics};
pub use opportunity::{
    classify_price_regime, classify_regime, continuation_confidence, early_quality_score,
    replay_events, OiConfig, OiHealth, OiVersions, Opportunity, OpportunityCloseReason, OpportunityId,
    OpportunityIntelligence, OpportunityScoreSnapshot, PriceRegime, Ranking, Regime,
    ReplayObservation, ScoreComponent, ShadowScore, ShadowState, Transform,
    CONTINUATION_MODEL_VERSION,
    EARLY_QUALITY_MODEL_VERSION, OPPORTUNITY_SCHEMA_VERSION, RANKING_VERSION,
    REGIME_CLASSIFIER_VERSION,
};
pub use outcome::{
    evaluate_outcome, evaluate_outcome_from_path, forward_path_pct, OutcomeKind, OutcomeThresholds, SignalOutcome,
};
pub use session_finder::{compute_day_signals, pick_sessions, session_window_utc, DaySignal, SessionCategory, SessionPick};
pub use strategy_config::{
    decide_enabled_strategies, default_enabled, round_trip_cost_pct, DecisionReason, StrategyConfigFile, StrategyDecision,
    DEFAULT_ROUND_TRIP_COST_PCT, EXPECTANCY_MARGIN_PCT, MIN_SAMPLE_FOR_DECISION,
};
pub use signals::{
    extract_signals, extract_signals_with_momentum_threshold, following_prices, SignalMoment,
    Strategy,
};
