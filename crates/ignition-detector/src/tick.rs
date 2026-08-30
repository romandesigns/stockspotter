//! Tick-level types — provider-agnostic, same reasoning as
//! `fast_funnel::TickerSnapshot` and `momentum_scorer::Candle`. Timestamps
//! are plain seconds-since-an-arbitrary-epoch `f64` rather than a
//! wall-clock type: the detector only ever cares about elapsed time
//! between ticks, and plain floats keep the pure functions trivial to
//! unit test without dragging a clock/timezone type through every test.
//! The ingestion layer (market-data) is responsible for converting real
//! Alpaca timestamps into this before calling in.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Trade {
    pub timestamp_secs: f64,
    pub price: f64,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Quote {
    pub timestamp_secs: f64,
    pub bid_price: f64,
    pub bid_size: u64,
    pub ask_price: f64,
    pub ask_size: u64,
}

impl Quote {
    pub fn spread(&self) -> f64 {
        self.ask_price - self.bid_price
    }
}
