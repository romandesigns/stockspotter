//! Halt Early-Warning detection — docs/trading-scanner-architecture-
//! part-3.md's "Halt Early-Warning Detection (new, independent system)".
//! Identifies stocks a few seconds *before* they potentially get halted,
//! based on proximity to the exchange's real LULD (Limit Up-Limit Down)
//! halt threshold — not a reaction after a halt already happened.
//!
//! Fully isolated per the doc's own note: this crate reads only raw
//! price/volume ticks and the LULD threshold table (`bands.rs`). It has
//! no dependency on `fast_funnel`, `momentum_scorer`, or
//! `ignition_detector`, and nothing in this codebase depends on it —
//! matching exactly how those three already don't depend on each other.
//!
//! - `reference.rs`: the rolling 5-minute reference price with
//!   1%-hysteresis stepping (the doc's own described mechanism).
//! - `bands.rs`: the price-tier band-width lookup table plus the
//!   3:35-4:00 PM ET closing-window doubling rule — real exchange rules,
//!   not something that needs backtesting to calibrate.
//! - `level.rs`: proximity + volume -> calm/amber/red color escalation
//!   for the doc's UI concept.
//! - `monitor.rs`: `HaltWarningMonitor`, the stateful per-symbol entry
//!   point tying the three together, same shape as
//!   `ignition_detector::monitor`.

pub mod bands;
pub mod level;
pub mod monitor;
pub mod reference;
pub mod tick;

pub use bands::{band_doubles, band_width_dollars, is_closing_window};
pub use level::{classify, AlertLevel, AlertLevelThresholds};
pub use monitor::{HaltWarningConfig, HaltWarningMonitor, HaltWarningReading};
pub use reference::{ReferencePriceConfig, ReferencePriceTracker};
pub use tick::Trade;
