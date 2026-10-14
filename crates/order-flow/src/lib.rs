//! Order flow / microstructure features — OBSERVATIONAL ONLY.
//!
//! Nothing in this crate gates an alert or feeds ranking. The order-flow
//! specification is explicit that these start as research features: earlier
//! work on this project showed that more detection sophistication does not
//! by itself improve precision, recall or expectancy, so each feature has to
//! be recorded and validated before anything consumes it.
//!
//! What the data allows (docs/order-flow-data-audit-2026-10-06.md): Alpaca
//! SIP supplies trades and top-of-book quotes, with no aggressor flag and no
//! depth. So the aggressor side is inferred here (`classify`), and nothing in
//! this crate pretends to see beyond the best bid/ask.
//!
//! Layout mirrors `ignition-detector`: provider-agnostic tick types
//! (`tick.rs`), pure functions where possible, and one stateful per-symbol
//! entry point (`OrderFlowEngine`) for the ingestion layer to drive.

pub mod book;
pub mod classify;
pub mod engine;
pub mod large;
pub mod levels;
pub mod profile;
pub mod tick;
pub mod validate;
pub mod window;

pub use book::{BookSideSummary, BookSummary, BookTopTracker};
pub use classify::{
    classify_against, ClassificationMethod, Classifier, ClassifierConfig, TradeClassification,
    TradeSide,
};
pub use engine::{
    Absorption, AbsorptionLevel, AbsorptionSide, DeltaBucket, Exhaustion, ExhaustionLevel, FlowRead, Interpretation,
    LargePrint, LargeStats, LargeTradeRead, Level, OrderFlowEngine, OrderFlowSummary, PressureSide,
    SweepEvent, SweepStats, TradeFlags,
};
pub use large::LargeTradeDetector;
pub use profile::{ProfileBin, ProfileSummary, VolumeProfile};
pub use tick::{Quote, Trade};
pub use window::{ClassifiedTrade, FlowSnapshot, FlowTracker, Session, WindowStats, WINDOWS_SECS};
