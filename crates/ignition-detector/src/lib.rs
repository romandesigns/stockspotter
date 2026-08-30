//! Ignition detector — architecture doc section 4.3. Runs independently
//! of the fast funnel and momentum scorer, watching the whole eligible
//! universe rather than a pre-filtered shortlist, since explosive moves
//! can happen on any stock at any point in its price action.
//!
//! Two-stage design mirroring the doc's own framing:
//! 1. `detect()` — raw tick-level signals (trade frequency, spread,
//!    ask-side absorption) against a rolling trade/quote window.
//! 2. `follow_through::confirm()` — a separate confirmation pass over the
//!    price action *after* a candidate fires, filtering fake spikes and
//!    liquidity grabs before anything becomes a real alert.
//!
//! Halt-lift resumption (the doc's fourth signal) isn't implemented — see
//! the doc comment on `detect::IgnitionSignals` for why.

pub mod detect;
pub mod follow_through;
pub mod monitor;
pub mod tick;

pub use detect::{
    ask_absorbed, detect, spread_ratio, trade_frequency_ratio, IgnitionSignals,
    IgnitionThresholds,
};
pub use follow_through::{confirm, FollowThroughResult, FollowThroughThresholds};
pub use monitor::{IgnitionMonitor, MonitorConfig, MonitorEvent};
pub use tick::{Quote, Trade};
