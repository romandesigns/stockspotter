//! Ignition detector — architecture doc section 4.3. Runs independently
//! of the fast funnel and momentum scorer, watching the whole eligible
//! universe rather than a pre-filtered shortlist, since explosive moves
//! can happen on any stock at any point in its price action.
//!
//! Two-stage design mirroring the doc's own framing:
//! 1. Raw tick-level signals — three (trade frequency, spread, ask-side
//!    absorption) computed fresh each call by the pure `detect()`
//!    function; the fourth (halt-lift resumption) tracked as a state
//!    transition by `IgnitionMonitor` in `monitor.rs`, since "a halt was
//!    just lifted" isn't something a single trade/quote window can see.
//! 2. `follow_through::confirm()` — a separate confirmation pass over the
//!    price action *after* a candidate fires, filtering fake spikes and
//!    liquidity grabs before anything becomes a real alert.
//!
//! `IgnitionMonitor` (in `monitor.rs`) is the stateful entry point that
//! actually ties both stages together per symbol — that's what a live
//! scan loop or replay engine talks to, not the bare functions directly.
//!
//! `flat_base.rs` adds one refinement on top
//! (docs/trading-scanner-architecture-part-3.md): an additive, opt-in
//! gate for low-float stocks that only fires ignition alerts if a tight,
//! quiet range held immediately before the surge. Off by default
//! (`MonitorConfig::flat_base: None`) — see that module's doc comment for
//! the isolation guarantee.

pub mod detect;
pub mod flat_base;
pub mod follow_through;
pub mod monitor;
pub mod tick;

pub use detect::{
    ask_absorbed, detect, spread_ratio, trade_frequency_ratio, IgnitionSignals,
    IgnitionThresholds,
};
pub use flat_base::{in_gated_price_band, is_flat_base, FlatBaseThresholds};
pub use follow_through::{confirm, FollowThroughResult, FollowThroughThresholds};
pub use monitor::{IgnitionMonitor, MonitorConfig, MonitorEvent, StatusTransition};
pub use tick::{Quote, Trade};
