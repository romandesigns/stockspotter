//! Latest NBBO quote per subscribed symbol (2026-10-15, the shortlist's
//! tradability read).
//!
//! Spread was the cost that sank every executable test in the research
//! (premise audit 2026-09-06: brackets that looked promising lost 1.4-1.5%
//! a trade once fills were priced at bid/ask), yet no spread ever reached a
//! client: quotes were consumed only inside the ignition monitor and the
//! default-off order-flow store. This keeps the one number a trader needs
//! before entering -- the current bid and ask -- for every symbol the live
//! scanner already subscribes quotes for (funnel, movers and quiet-watch
//! symbols; universe mode subscribes trades only, so it adds nothing here).
//!
//! Global, like `order_flow_store`, so the live loop feeds it without a new
//! parameter threaded through `run_live_scan`. Bounded: at most
//! `MAX_SYMBOLS` entries, the least recently quoted evicted first.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Utc};

use crate::bar::Quote;

/// Far above the live scanner's per-symbol subscription count; a bound,
/// not a working size.
const MAX_SYMBOLS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LatestQuote {
    pub bid: f64,
    pub ask: f64,
    pub bid_size: u64,
    pub ask_size: u64,
    /// The quote's own SIP timestamp, not when it was received.
    pub at: DateTime<Utc>,
}

impl LatestQuote {
    pub fn mid(&self) -> f64 {
        (self.bid + self.ask) / 2.0
    }

    /// Ask minus bid as a percentage of the midpoint -- the round-trip cost
    /// of crossing the spread once in, once out, before any slippage.
    pub fn spread_pct(&self) -> f64 {
        (self.ask - self.bid) / self.mid() * 100.0
    }
}

/// Only a quote that can be priced: both sides positive and finite, and not
/// crossed. A one-sided or crossed quote (common around halts and in thin
/// premarket books) would publish a spread that is not a cost anyone can
/// trade at, so it is ignored rather than stored.
fn accept(q: &Quote) -> Option<LatestQuote> {
    let sane = q.bid_price.is_finite() && q.ask_price.is_finite() && q.bid_price > 0.0 && q.ask_price > 0.0 && q.ask_price >= q.bid_price;
    sane.then_some(LatestQuote { bid: q.bid_price, ask: q.ask_price, bid_size: q.bid_size, ask_size: q.ask_size, at: q.timestamp })
}

#[derive(Debug, Default)]
pub struct Book {
    quotes: HashMap<String, LatestQuote>,
}

impl Book {
    /// Keeps the newest quote per symbol; an out-of-order older quote never
    /// replaces a newer one.
    pub fn record(&mut self, q: &Quote) {
        let Some(latest) = accept(q) else { return };
        match self.quotes.get(&q.symbol) {
            Some(existing) if existing.at > latest.at => return,
            _ => {}
        }
        self.quotes.insert(q.symbol.clone(), latest);
        if self.quotes.len() > MAX_SYMBOLS {
            if let Some(oldest) = self.quotes.iter().min_by_key(|(_, v)| v.at).map(|(k, _)| k.clone()) {
                self.quotes.remove(&oldest);
            }
        }
    }

    pub fn latest(&self, symbol: &str) -> Option<LatestQuote> {
        self.quotes.get(symbol).copied()
    }
}

static BOOK: OnceLock<Mutex<Book>> = OnceLock::new();

fn book() -> &'static Mutex<Book> {
    BOOK.get_or_init(|| Mutex::new(Book::default()))
}

/// Call for every quote the live loop receives.
pub fn on_quote(q: &Quote) {
    book().lock().unwrap_or_else(|e| e.into_inner()).record(q);
}

pub fn latest(symbol: &str) -> Option<LatestQuote> {
    book().lock().unwrap_or_else(|e| e.into_inner()).latest(symbol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn quote(symbol: &str, bid: f64, ask: f64, secs: i64) -> Quote {
        Quote { symbol: symbol.into(), bid_price: bid, bid_size: 100, ask_price: ask, ask_size: 200, timestamp: Utc.timestamp_opt(1_790_000_000 + secs, 0).unwrap() }
    }

    #[test]
    fn spread_is_a_percentage_of_the_midpoint() {
        let mut b = Book::default();
        b.record(&quote("ABC", 1.98, 2.02, 0));
        let q = b.latest("ABC").unwrap();
        assert!((q.mid() - 2.0).abs() < 1e-12);
        assert!((q.spread_pct() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn unpriceable_quotes_are_ignored() {
        let mut b = Book::default();
        for (bid, ask) in [(0.0, 2.0), (2.0, 0.0), (2.1, 2.0), (f64::NAN, 2.0), (2.0, f64::INFINITY)] {
            b.record(&quote("ABC", bid, ask, 0));
        }
        assert_eq!(b.latest("ABC"), None);
    }

    #[test]
    fn an_older_quote_never_replaces_a_newer_one() {
        let mut b = Book::default();
        b.record(&quote("ABC", 2.00, 2.01, 10));
        b.record(&quote("ABC", 1.50, 2.50, 5));
        assert_eq!(b.latest("ABC").unwrap().bid, 2.00);
        b.record(&quote("ABC", 2.02, 2.03, 11));
        assert_eq!(b.latest("ABC").unwrap().bid, 2.02);
    }

    #[test]
    fn the_book_is_bounded_and_evicts_the_least_recently_quoted() {
        let mut b = Book::default();
        for i in 0..=MAX_SYMBOLS as i64 {
            b.record(&quote(&format!("S{i}"), 1.0, 1.01, i));
        }
        assert_eq!(b.quotes.len(), MAX_SYMBOLS);
        assert_eq!(b.latest("S0"), None);
        assert!(b.latest(&format!("S{MAX_SYMBOLS}")).is_some());
    }
}
