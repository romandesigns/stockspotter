//! Tick-level types — provider-agnostic, same reasoning as
//! `ignition_detector::tick`: plain seconds as `f64`, so the pure functions
//! stay trivial to unit test without a clock type.
//!
//! One difference that matters here: these seconds must keep their
//! sub-second part. Classification depends on which quote was prevailing at
//! the instant of a trade, and the 1-second window is a required resolution,
//! so the ingestion layer must pass fractional epoch seconds and not the
//! whole-second values some other consumers use. An `f64` holds current
//! epoch seconds to well under a microsecond, which is finer than the
//! ordering guarantees the feed gives us anyway.
//!
//! Trades that should not be classified at all (auction prints, average
//! price, out-of-sequence and similar) are the ingestion layer's job to
//! drop before they get here: condition codes are tape-specific, and this
//! crate deliberately knows nothing about tapes.

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
    /// False for a one-sided, locked or crossed market. Those carry no
    /// usable "at the bid / at the ask" information, so classification
    /// falls back to the tick rule instead of trusting them.
    pub fn is_two_sided(&self) -> bool {
        self.bid_price > 0.0 && self.ask_price > self.bid_price
    }

    pub fn mid(&self) -> f64 {
        (self.bid_price + self.ask_price) / 2.0
    }
}
