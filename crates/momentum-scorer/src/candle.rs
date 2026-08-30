//! Deliberately minimal and provider-agnostic, same reasoning as
//! `fast_funnel::TickerSnapshot`: this crate doesn't know or care whether
//! candles came from a live Alpaca bar stream or a replay engine feeding
//! historical bars — same struct in, same scoring functions run either way.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Candle {
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: u64,
}

/// A fixed-capacity rolling window of the most recent candles for one
/// ticker — what the scorer actually reads from. Not a ring buffer
/// internally (a plain `Vec` with front-eviction) since the expected
/// capacity is small (tens of bars); simplicity over micro-optimization.
#[derive(Debug, Clone)]
pub struct RollingWindow {
    capacity: usize,
    candles: Vec<Candle>,
}

impl RollingWindow {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            candles: Vec::with_capacity(capacity),
        }
    }

    pub fn push(&mut self, candle: Candle) {
        self.candles.push(candle);
        if self.candles.len() > self.capacity {
            let excess = self.candles.len() - self.capacity;
            self.candles.drain(0..excess);
        }
    }

    pub fn as_slice(&self) -> &[Candle] {
        &self.candles
    }

    pub fn len(&self) -> usize {
        self.candles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.candles.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(close: f64) -> Candle {
        Candle {
            open: close,
            high: close,
            low: close,
            close,
            volume: 1,
        }
    }

    #[test]
    fn rolling_window_evicts_oldest_past_capacity() {
        let mut w = RollingWindow::new(3);
        w.push(c(1.0));
        w.push(c(2.0));
        w.push(c(3.0));
        w.push(c(4.0));

        assert_eq!(w.len(), 3);
        let closes: Vec<f64> = w.as_slice().iter().map(|c| c.close).collect();
        assert_eq!(closes, vec![2.0, 3.0, 4.0]);
    }

    #[test]
    fn rolling_window_starts_empty() {
        let w = RollingWindow::new(5);
        assert!(w.is_empty());
        assert!(w.as_slice().is_empty());
    }
}
