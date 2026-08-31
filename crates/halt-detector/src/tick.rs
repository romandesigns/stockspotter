//! Minimal, provider-agnostic trade type — same reasoning as
//! `ignition_detector::tick::Trade` and `momentum_scorer::Candle`: this
//! crate doesn't know or care whether trades came from a live Alpaca
//! stream or a replay engine, and deliberately doesn't depend on
//! `market_data`'s wire types either, to keep this system's isolation
//! provable at the crate-dependency-graph level, not just asserted.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Trade {
    pub timestamp_secs: f64,
    pub price: f64,
    pub size: u64,
}
