//! Backtest logging & metrics — architecture doc section 8. Builds on
//! `replay_engine`: extracts discrete signal moments from a replay
//! result, evaluates each against a simple target/stop outcome model,
//! logs every one to an append-only file, and aggregates hit
//! rate/average winning move/timing accuracy per strategy.

pub mod log;
pub mod metrics;
pub mod outcome;
pub mod session_finder;
pub mod signals;

pub use log::{append, read_all, LoggedSignal};
pub use metrics::{aggregate, aggregate_by_strategy, AggregateMetrics};
pub use outcome::{evaluate_outcome, OutcomeThresholds, SignalOutcome};
pub use session_finder::{compute_day_signals, pick_sessions, session_window_utc, DaySignal, SessionCategory, SessionPick};
pub use signals::{
    extract_signals, extract_signals_with_momentum_threshold, following_prices, SignalMoment,
    Strategy,
};
