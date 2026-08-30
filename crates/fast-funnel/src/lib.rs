//! Fast funnel — section 4.1 of the architecture doc.
//!
//! Pure, synchronous filtering logic only. No network, no async. This crate
//! doesn't know or care whether `TickerSnapshot`s came from a live Alpaca
//! stream (`market-data` crate) or a future replay engine — same functions,
//! same output, either way.

pub mod filters;
pub mod types;

pub use filters::{explain, run_fast_funnel, stage1_static_filter, stage2_dynamic_filter, FunnelExplanation};
pub use types::{FilterThresholds, TickerSnapshot};
