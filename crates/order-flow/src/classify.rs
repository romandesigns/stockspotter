//! Aggressor-side inference (specification section 5).
//!
//! The feed does not say who was the aggressor, so it is inferred:
//!
//! 1. **Quote rule.** A trade at or above the prevailing ask is an
//!    aggressive buy; at or below the prevailing bid, an aggressive sell.
//! 2. **Inside the spread.** The side is taken from which half of the
//!    spread the price sits in, with confidence growing toward the touch.
//!    Close to the midpoint there is no honest answer from the quote.
//! 3. **Tick rule.** With no usable quote, an uptick is a buy and a
//!    downtick a sell; a zero tick inherits the last direction.
//!
//! Every result carries a confidence and the method that produced it, and
//! anything below `min_confidence` is returned as `Unknown` rather than
//! forced onto a side. Unknown volume is tracked on its own downstream.
//!
//! **The confidence numbers are untuned defaults, not measurements.** They
//! encode an ordering (touch > inside spread > tick rule; fresh quote >
//! stale quote) and nothing more. Calibrating them against real tape is
//! part of the validation phase and must happen before any consumer treats
//! a confidence as a probability.

use serde::{Deserialize, Serialize};

use crate::tick::{Quote, Trade};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeSide {
    Buy,
    Sell,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationMethod {
    AtOrAboveAsk,
    AtOrBelowBid,
    InsideSpread,
    TickRule,
    /// Nothing to go on: no usable quote and no earlier price.
    Unclassified,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TradeClassification {
    pub side: TradeSide,
    /// 0.0..=1.0. See the module comment: an ordering, not a probability.
    pub confidence: f64,
    pub method: ClassificationMethod,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClassifierConfig {
    /// A quote this recent counts at full confidence.
    pub fresh_quote_secs: f64,
    /// Older than this, the quote is ignored and the tick rule is used.
    pub max_quote_age_secs: f64,
    /// Multiplier applied to a quote exactly `max_quote_age_secs` old;
    /// interpolated linearly from 1.0 at `fresh_quote_secs`.
    pub stale_quote_factor: f64,
    pub at_touch_confidence: f64,
    /// Confidence just outside the midpoint dead zone, rising linearly to
    /// `inside_spread_max_confidence` at the touch.
    pub inside_spread_min_confidence: f64,
    pub inside_spread_max_confidence: f64,
    /// Fraction of the half-spread around the midpoint treated as "no
    /// side". Odd lots print inside the round-lot NBBO constantly on
    /// low-priced names, so this zone is not a corner case.
    pub midpoint_dead_zone: f64,
    pub tick_rule_confidence: f64,
    /// Below this the side is reported as `Unknown`.
    pub min_confidence: f64,
}

impl Default for ClassifierConfig {
    fn default() -> Self {
        Self {
            fresh_quote_secs: 1.0,
            max_quote_age_secs: 5.0,
            stale_quote_factor: 0.6,
            at_touch_confidence: 0.9,
            inside_spread_min_confidence: 0.3,
            inside_spread_max_confidence: 0.6,
            midpoint_dead_zone: 0.1,
            tick_rule_confidence: 0.35,
            min_confidence: 0.3,
        }
    }
}

/// Pure classification of one trade.
///
/// `tick` is the direction the tick rule would give (see
/// `Classifier::classify` for how it is derived), or `None` when there is
/// no earlier price to compare against.
pub fn classify_against(
    trade: &Trade,
    quote: Option<&Quote>,
    tick: Option<TradeSide>,
    cfg: &ClassifierConfig,
) -> TradeClassification {
    if let Some(result) = quote.and_then(|q| classify_with_quote(trade, q, cfg)) {
        return finalize(result, cfg);
    }
    match tick {
        Some(side) if side != TradeSide::Unknown => finalize(
            TradeClassification { side, confidence: cfg.tick_rule_confidence, method: ClassificationMethod::TickRule },
            cfg,
        ),
        _ => TradeClassification { side: TradeSide::Unknown, confidence: 0.0, method: ClassificationMethod::Unclassified },
    }
}

/// `None` means "this quote cannot answer", which sends the caller to the
/// tick rule: the quote is not two-sided, is too old, postdates the trade,
/// or the trade sits in the midpoint dead zone.
fn classify_with_quote(trade: &Trade, quote: &Quote, cfg: &ClassifierConfig) -> Option<TradeClassification> {
    if !quote.is_two_sided() {
        return None;
    }
    // Trades and quotes arrive in separate batches with no ordering
    // between them, so a quote stamped after the trade can already be
    // "latest" by the time the trade is processed. It was not prevailing
    // when the trade happened and must not be used to judge it.
    let age = trade.timestamp_secs - quote.timestamp_secs;
    if age < 0.0 || age > cfg.max_quote_age_secs {
        return None;
    }
    let freshness = if age <= cfg.fresh_quote_secs {
        1.0
    } else {
        let t = (age - cfg.fresh_quote_secs) / (cfg.max_quote_age_secs - cfg.fresh_quote_secs);
        1.0 - t * (1.0 - cfg.stale_quote_factor)
    };

    if trade.price >= quote.ask_price {
        return Some(TradeClassification {
            side: TradeSide::Buy,
            confidence: cfg.at_touch_confidence * freshness,
            method: ClassificationMethod::AtOrAboveAsk,
        });
    }
    if trade.price <= quote.bid_price {
        return Some(TradeClassification {
            side: TradeSide::Sell,
            confidence: cfg.at_touch_confidence * freshness,
            method: ClassificationMethod::AtOrBelowBid,
        });
    }

    // Strictly inside the spread. `offset` is -1.0 at the bid, 0.0 at the
    // midpoint and +1.0 at the ask.
    let half_spread = (quote.ask_price - quote.bid_price) / 2.0;
    let offset = (trade.price - quote.mid()) / half_spread;
    let distance = offset.abs();
    if distance < cfg.midpoint_dead_zone {
        return None;
    }
    let span = cfg.inside_spread_max_confidence - cfg.inside_spread_min_confidence;
    let ramp = (distance - cfg.midpoint_dead_zone) / (1.0 - cfg.midpoint_dead_zone);
    Some(TradeClassification {
        side: if offset > 0.0 { TradeSide::Buy } else { TradeSide::Sell },
        confidence: (cfg.inside_spread_min_confidence + span * ramp) * freshness,
        method: ClassificationMethod::InsideSpread,
    })
}

/// Uncertain trades are reported as `Unknown`, never forced onto a side.
/// The method and confidence are kept so the reason is still visible.
fn finalize(mut c: TradeClassification, cfg: &ClassifierConfig) -> TradeClassification {
    if c.confidence < cfg.min_confidence {
        c.side = TradeSide::Unknown;
    }
    c
}

/// Per-symbol classifier state: the latest quote and the tick-rule memory.
#[derive(Debug, Clone)]
pub struct Classifier {
    cfg: ClassifierConfig,
    quote: Option<Quote>,
    last_price: Option<f64>,
    last_direction: Option<TradeSide>,
}

impl Classifier {
    pub fn new(cfg: ClassifierConfig) -> Self {
        Self { cfg, quote: None, last_price: None, last_direction: None }
    }

    pub fn on_quote(&mut self, quote: Quote) {
        self.quote = Some(quote);
    }

    pub fn classify(&mut self, trade: &Trade) -> TradeClassification {
        let tick = match self.last_price {
            Some(last) if trade.price > last => Some(TradeSide::Buy),
            Some(last) if trade.price < last => Some(TradeSide::Sell),
            // Zero tick: inherit the direction of the last price change.
            Some(_) => self.last_direction,
            None => None,
        };
        let result = classify_against(trade, self.quote.as_ref(), tick, &self.cfg);
        // The tick memory follows prices only, independent of whether the
        // quote rule answered, so it is correct the moment quotes go stale.
        if tick.is_some() {
            self.last_direction = tick;
        }
        self.last_price = Some(trade.price);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(t: f64, price: f64) -> Trade {
        Trade { timestamp_secs: t, price, size: 100 }
    }

    fn quote(t: f64, bid: f64, ask: f64) -> Quote {
        Quote { timestamp_secs: t, bid_price: bid, bid_size: 500, ask_price: ask, ask_size: 500 }
    }

    fn cfg() -> ClassifierConfig {
        ClassifierConfig::default()
    }

    #[test]
    fn at_or_above_ask_is_a_buy() {
        let q = quote(10.0, 2.00, 2.02);
        for price in [2.02, 2.03] {
            let c = classify_against(&trade(10.5, price), Some(&q), None, &cfg());
            assert_eq!(c.side, TradeSide::Buy);
            assert_eq!(c.method, ClassificationMethod::AtOrAboveAsk);
            assert!((c.confidence - 0.9).abs() < 1e-9);
        }
    }

    #[test]
    fn at_or_below_bid_is_a_sell() {
        let q = quote(10.0, 2.00, 2.02);
        for price in [2.00, 1.99] {
            let c = classify_against(&trade(10.5, price), Some(&q), None, &cfg());
            assert_eq!(c.side, TradeSide::Sell);
            assert_eq!(c.method, ClassificationMethod::AtOrBelowBid);
        }
    }

    #[test]
    fn inside_spread_leans_by_half_with_lower_confidence_than_the_touch() {
        let q = quote(10.0, 2.00, 2.10);
        let near_ask = classify_against(&trade(10.1, 2.09), Some(&q), None, &cfg());
        assert_eq!(near_ask.side, TradeSide::Buy);
        assert_eq!(near_ask.method, ClassificationMethod::InsideSpread);
        assert!(near_ask.confidence < 0.9 && near_ask.confidence > 0.3);

        let near_bid = classify_against(&trade(10.1, 2.01), Some(&q), None, &cfg());
        assert_eq!(near_bid.side, TradeSide::Sell);
        assert!((near_bid.confidence - near_ask.confidence).abs() < 1e-9);
    }

    #[test]
    fn midpoint_trade_is_not_forced_onto_a_side() {
        let q = quote(10.0, 2.00, 2.10);
        // No tick history: nothing to go on at all.
        let c = classify_against(&trade(10.1, 2.05), Some(&q), None, &cfg());
        assert_eq!(c.side, TradeSide::Unknown);
        assert_eq!(c.method, ClassificationMethod::Unclassified);
        // With tick history the tick rule answers, at its own confidence.
        let c = classify_against(&trade(10.1, 2.05), Some(&q), Some(TradeSide::Buy), &cfg());
        assert_eq!(c.side, TradeSide::Buy);
        assert_eq!(c.method, ClassificationMethod::TickRule);
    }

    #[test]
    fn stale_quote_lowers_confidence_then_is_ignored() {
        let q = quote(10.0, 2.00, 2.02);
        let fresh = classify_against(&trade(10.5, 2.02), Some(&q), None, &cfg());
        let aging = classify_against(&trade(13.0, 2.02), Some(&q), None, &cfg());
        assert_eq!(aging.side, TradeSide::Buy);
        assert!(aging.confidence < fresh.confidence);

        let too_old = classify_against(&trade(16.0, 2.02), Some(&q), Some(TradeSide::Sell), &cfg());
        assert_eq!(too_old.method, ClassificationMethod::TickRule);
        assert_eq!(too_old.side, TradeSide::Sell);
    }

    #[test]
    fn quote_stamped_after_the_trade_is_not_used() {
        let q = quote(10.5, 2.00, 2.02);
        let c = classify_against(&trade(10.0, 2.02), Some(&q), None, &cfg());
        assert_eq!(c.side, TradeSide::Unknown);
        assert_eq!(c.method, ClassificationMethod::Unclassified);
    }

    #[test]
    fn locked_crossed_and_one_sided_quotes_fall_back_to_the_tick_rule() {
        for q in [quote(10.0, 2.02, 2.02), quote(10.0, 2.03, 2.02), quote(10.0, 0.0, 2.02)] {
            let c = classify_against(&trade(10.1, 2.02), Some(&q), Some(TradeSide::Buy), &cfg());
            assert_eq!(c.method, ClassificationMethod::TickRule);
        }
    }

    #[test]
    fn low_confidence_result_is_unknown_but_keeps_its_method() {
        let strict = ClassifierConfig { min_confidence: 0.5, ..cfg() };
        let c = classify_against(&trade(10.0, 2.02), None, Some(TradeSide::Buy), &strict);
        assert_eq!(c.side, TradeSide::Unknown);
        assert_eq!(c.method, ClassificationMethod::TickRule);
        assert!((c.confidence - 0.35).abs() < 1e-9);
    }

    #[test]
    fn classifier_tick_rule_handles_first_trade_upticks_and_zero_ticks() {
        let mut c = Classifier::new(cfg());
        assert_eq!(c.classify(&trade(1.0, 2.00)).side, TradeSide::Unknown);
        assert_eq!(c.classify(&trade(2.0, 2.01)).side, TradeSide::Buy);
        // Zero tick inherits the last direction.
        assert_eq!(c.classify(&trade(3.0, 2.01)).side, TradeSide::Buy);
        assert_eq!(c.classify(&trade(4.0, 2.00)).side, TradeSide::Sell);
        assert_eq!(c.classify(&trade(5.0, 2.00)).side, TradeSide::Sell);
    }

    #[test]
    fn classifier_prefers_the_quote_and_keeps_tick_memory_current() {
        let mut c = Classifier::new(cfg());
        c.classify(&trade(1.0, 2.00));
        c.on_quote(quote(1.5, 2.00, 2.02));
        // A downtick that prints at the ask is a buy by the quote rule.
        c.classify(&trade(1.6, 2.03));
        let at_ask = c.classify(&trade(1.7, 2.02));
        assert_eq!(at_ask.side, TradeSide::Buy);
        assert_eq!(at_ask.method, ClassificationMethod::AtOrAboveAsk);
        // Once the quote is stale, the tick rule uses that same downtick.
        let later = c.classify(&trade(30.0, 2.02));
        assert_eq!(later.method, ClassificationMethod::TickRule);
        assert_eq!(later.side, TradeSide::Sell);
    }
}
