//! Top-of-book liquidity changes (specification sections 12 and 13),
//! limited to what the best bid/ask can show.
//!
//! There is no depth feed (docs/order-flow-data-audit-2026-10-06.md), so
//! this sees exactly one level per side. Two consequences are built into
//! the names here and must survive into any UI:
//!
//! - **"Removed" is not "consumed".** Displayed size at the touch falls
//!   when it trades AND when it is cancelled, and the quote alone cannot
//!   tell which. Read it next to aggressive volume, never on its own.
//! - **Replenishment is only ever "possible persistent liquidity".** Size
//!   returning at a held price is consistent with a refilling order and
//!   with several unrelated ones. Nothing here can confirm an iceberg.
//!
//! Sizes are whatever unit the feed quotes in (round lots on Alpaca SIP),
//! passed through unconverted, hence `_size` rather than `_shares`.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::tick::Quote;

const WINDOW_SECS: f64 = 30.0;
/// Refills at one held price before it is called possibly persistent.
const PERSISTENT_MIN_REFILLS: u32 = 3;

#[derive(Debug, Clone, Copy)]
struct BookEvent {
    timestamp_secs: f64,
    ask: bool,
    removed: u64,
    added: u64,
    level_cleared: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookSideSummary {
    /// Displayed size that left the touch in the window (trades + cancels).
    pub removed_size: u64,
    /// Displayed size that appeared at the touch without the price moving.
    pub added_size: u64,
    /// Times the whole level went and the touch moved away.
    pub levels_cleared: u32,
    /// Refills at the CURRENT touch price since it became the touch.
    pub refills_at_touch: u32,
    pub possible_persistent_liquidity: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct BookSummary {
    pub ask: BookSideSummary,
    pub bid: BookSideSummary,
}

#[derive(Debug, Clone, Default)]
pub struct BookTopTracker {
    previous: Option<Quote>,
    events: VecDeque<BookEvent>,
    ask_refills: u32,
    bid_refills: u32,
}

impl BookTopTracker {
    pub fn on_quote(&mut self, quote: Quote) {
        if !quote.is_two_sided() {
            // A one-sided or crossed book says nothing comparable; drop
            // the reference so the next good quote starts clean.
            self.previous = None;
            return;
        }
        if let Some(prev) = self.previous {
            // Ask: a higher ask means the old level is gone.
            let ask = side_event(quote.timestamp_secs, true, prev.ask_price, prev.ask_size, quote.ask_price, quote.ask_size, quote.ask_price > prev.ask_price);
            self.apply(ask, quote.ask_price == prev.ask_price);
            // Bid: mirrored, a lower bid means the old level is gone.
            let bid = side_event(quote.timestamp_secs, false, prev.bid_price, prev.bid_size, quote.bid_price, quote.bid_size, quote.bid_price < prev.bid_price);
            self.apply(bid, quote.bid_price == prev.bid_price);
        }
        self.previous = Some(quote);
        let cutoff = quote.timestamp_secs - WINDOW_SECS;
        while self.events.front().is_some_and(|e| e.timestamp_secs <= cutoff) {
            self.events.pop_front();
        }
    }

    fn apply(&mut self, event: BookEvent, same_price: bool) {
        let refills = if event.ask { &mut self.ask_refills } else { &mut self.bid_refills };
        if !same_price {
            *refills = 0;
        } else if event.added > 0 {
            *refills += 1;
        }
        if event.removed > 0 || event.added > 0 || event.level_cleared {
            self.events.push_back(event);
        }
    }

    pub fn summary(&self, now_secs: f64) -> BookSummary {
        let mut out = BookSummary::default();
        for e in self.events.iter().filter(|e| now_secs - e.timestamp_secs < WINDOW_SECS) {
            let side = if e.ask { &mut out.ask } else { &mut out.bid };
            side.removed_size += e.removed;
            side.added_size += e.added;
            side.levels_cleared += u32::from(e.level_cleared);
        }
        out.ask.refills_at_touch = self.ask_refills;
        out.bid.refills_at_touch = self.bid_refills;
        for side in [&mut out.ask, &mut out.bid] {
            // Refills alone are not enough: size has to keep leaving and
            // coming back, or it is just a quote being resized.
            side.possible_persistent_liquidity =
                side.refills_at_touch >= PERSISTENT_MIN_REFILLS && side.removed_size > 0 && side.added_size * 2 >= side.removed_size;
        }
        out
    }
}

fn side_event(timestamp_secs: f64, ask: bool, prev_price: f64, prev_size: u64, price: f64, size: u64, level_cleared: bool) -> BookEvent {
    let mut event = BookEvent { timestamp_secs, ask, removed: 0, added: 0, level_cleared };
    if price == prev_price {
        event.removed = prev_size.saturating_sub(size);
        event.added = size.saturating_sub(prev_size);
    } else if level_cleared {
        event.removed = prev_size;
    }
    event
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(t: f64, bid: f64, bid_size: u64, ask: f64, ask_size: u64) -> Quote {
        Quote { timestamp_secs: t, bid_price: bid, bid_size, ask_price: ask, ask_size }
    }

    #[test]
    fn shrinking_ask_then_a_higher_ask_is_removal_and_a_cleared_level() {
        let mut book = BookTopTracker::default();
        for (t, size) in [(1.0, 120), (2.0, 90), (3.0, 40)] {
            book.on_quote(q(t, 2.24, 50, 2.25, size));
        }
        book.on_quote(q(4.0, 2.25, 50, 2.26, 70));
        let s = book.summary(4.0);
        // 30 + 50 while the price held, then the last 40 when it cleared.
        assert_eq!(s.ask.removed_size, 120);
        assert_eq!(s.ask.levels_cleared, 1);
        assert_eq!(s.ask.added_size, 0);
        assert!(!s.ask.possible_persistent_liquidity);
    }

    #[test]
    fn ask_that_keeps_refilling_at_one_price_is_possibly_persistent() {
        // The specification's example: 20K -> hit -> 18K -> hit -> 22K.
        let mut book = BookTopTracker::default();
        let sizes = [200, 20, 180, 30, 220, 40, 190];
        for (i, size) in sizes.iter().enumerate() {
            book.on_quote(q(i as f64, 2.24, 50, 2.25, *size));
        }
        let s = book.summary(sizes.len() as f64);
        assert_eq!(s.ask.refills_at_touch, 3);
        assert!(s.ask.possible_persistent_liquidity);
        assert!(!s.bid.possible_persistent_liquidity);
    }

    #[test]
    fn refill_count_resets_when_the_touch_moves() {
        let mut book = BookTopTracker::default();
        for (t, size) in [(0.0, 100), (1.0, 20), (2.0, 100), (3.0, 20), (4.0, 100)] {
            book.on_quote(q(t, 2.24, 50, 2.25, size));
        }
        book.on_quote(q(5.0, 2.25, 50, 2.26, 100));
        assert_eq!(book.summary(5.0).ask.refills_at_touch, 0);
    }

    #[test]
    fn bid_side_mirrors_the_ask() {
        let mut book = BookTopTracker::default();
        book.on_quote(q(1.0, 2.24, 100, 2.25, 50));
        book.on_quote(q(2.0, 2.24, 30, 2.25, 50));
        book.on_quote(q(3.0, 2.23, 80, 2.25, 50));
        let s = book.summary(3.0);
        assert_eq!(s.bid.removed_size, 100);
        assert_eq!(s.bid.levels_cleared, 1);
    }

    #[test]
    fn events_age_out_and_bad_quotes_break_the_comparison() {
        let mut book = BookTopTracker::default();
        book.on_quote(q(1.0, 2.24, 50, 2.25, 100));
        book.on_quote(q(2.0, 2.24, 50, 2.25, 40));
        assert_eq!(book.summary(40.0).ask.removed_size, 0);
        // A crossed quote in between must not produce a phantom change.
        book.on_quote(q(41.0, 2.26, 50, 2.25, 10));
        book.on_quote(q(42.0, 2.24, 50, 2.25, 500));
        assert_eq!(book.summary(42.0).ask.added_size, 0);
    }
}
