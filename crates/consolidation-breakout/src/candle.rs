//! Deliberately minimal and provider-agnostic — same reasoning as
//! `momentum_scorer::Candle` (a near-identical shape exists there too;
//! redefined here rather than imported, since this crate depends on
//! nothing else in the workspace — see the isolation note in `lib.rs`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Candle {
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: u64,
}

impl Candle {
    pub fn range(&self) -> f64 {
        self.high - self.low
    }
}
