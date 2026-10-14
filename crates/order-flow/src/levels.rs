//! Absorption levels: prices where heavy aggressive volume kept printing
//! and price did not get through.
//!
//! This is the closest the available data gets to what a liquidity heatmap
//! is used for. A heatmap shows resting orders before they trade; there is
//! no depth feed, so that is not possible. What trades and the best
//! bid/ask DO show is the aftermath: buyers lifting again and again at the
//! top of the range for a minute or more while the high holds means someone
//! was selling into them there. That price is reported as a level, sized
//! by how much it absorbed.
//!
//! **It is evidence of liquidity that was hit, never of liquidity that is
//! waiting.** A level says "this much was absorbed here"; it does not say
//! orders are resting there now, and any UI must word it that way.
//!
//! A level is removed the moment price trades through it by more than the
//! band it was found in (it did not hold), and after 30 minutes without
//! being re-confirmed (it is old news).
//!
//! Every number here is an untuned default, like the rest of the crate.

use serde::{Deserialize, Serialize};

use crate::classify::TradeSide;
use crate::window::ClassifiedTrade;

/// Who is doing the absorbing. `SellSide` = aggressive buyers are being
/// absorbed by sellers (price will not rise); `BuySide` is the mirror.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbsorptionSide {
    BuySide,
    SellSide,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbsorptionLevel {
    pub price: f64,
    pub side: AbsorptionSide,
    /// Aggressive volume absorbed in the band at this level, in shares.
    pub volume: u64,
    pub first_secs: f64,
    pub last_secs: f64,
}

const WINDOW_SECS: f64 = 300.0;
/// Fewer prints than this in the window and nothing is concluded.
const MIN_TRADES: usize = 30;
/// The "top of the range" is this share of the window's high-low range.
const BAND_SHARE_OF_RANGE: f64 = 0.1;
/// Price must have been pressing on the level at least this long.
const MIN_PRESS_SECS: f64 = 60.0;
/// The band must hold at least this share of the side's volume...
const MIN_SHARE_OF_SIDE: f64 = 0.25;
/// ...and at least this multiple of a normal 30-second bucket's delta.
const MIN_MULTIPLE_OF_TYPICAL: f64 = 2.0;
const STALE_SECS: f64 = 1800.0;
const MAX_LEVELS: usize = 6;

#[derive(Debug, Clone, Copy)]
struct Tracked {
    level: AbsorptionLevel,
    /// The band the level was found in; trading through it by more than
    /// this means the level did not hold.
    tolerance: f64,
}

#[derive(Debug, Clone, Default)]
pub struct AbsorptionLevels {
    levels: Vec<Tracked>,
}

impl AbsorptionLevels {
    /// Call on every trade: drops any level price has now traded through.
    pub fn on_trade(&mut self, price: f64) {
        self.levels.retain(|t| match t.level.side {
            AbsorptionSide::SellSide => price <= t.level.price + t.tolerance,
            AbsorptionSide::BuySide => price >= t.level.price - t.tolerance,
        });
    }

    /// Looks at the last five minutes for a level being absorbed at either
    /// end of the range. `typical_delta` is a normal 30-second bucket's
    /// absolute delta for this symbol; without it there is no way to say
    /// what "heavy" means, so nothing is detected.
    pub fn evaluate<'a>(&mut self, now: f64, trades: impl Iterator<Item = &'a ClassifiedTrade>, typical_delta: Option<f64>) {
        self.levels.retain(|t| now - t.level.last_secs <= STALE_SECS);
        let Some(typical) = typical_delta.filter(|t| *t > 0.0) else {
            return;
        };
        let window: Vec<&ClassifiedTrade> = trades.filter(|t| (0.0..WINDOW_SECS).contains(&(now - t.timestamp_secs))).collect();
        if window.len() < MIN_TRADES {
            return;
        }
        let high = window.iter().map(|t| t.price).fold(f64::MIN, f64::max);
        let low = window.iter().map(|t| t.price).fold(f64::MAX, f64::min);
        let tick = if high >= 1.0 { 0.01 } else { 0.001 };
        let band = ((high - low) * BAND_SHARE_OF_RANGE).max(tick);

        for (side, aggressor, level_price) in [(AbsorptionSide::SellSide, TradeSide::Buy, high), (AbsorptionSide::BuySide, TradeSide::Sell, low)] {
            let in_band = |price: f64| match side {
                AbsorptionSide::SellSide => price >= high - band,
                AbsorptionSide::BuySide => price <= low + band,
            };
            let side_total: u64 = window.iter().filter(|t| t.side == aggressor).map(|t| t.size).sum();
            let absorbed: u64 = window.iter().filter(|t| t.side == aggressor && in_band(t.price)).map(|t| t.size).sum();
            let first_touch = window.iter().filter(|t| in_band(t.price)).map(|t| t.timestamp_secs).fold(f64::MAX, f64::min);
            let heavy = absorbed as f64 >= MIN_SHARE_OF_SIDE * side_total as f64 && absorbed as f64 >= MIN_MULTIPLE_OF_TYPICAL * typical;
            if side_total == 0 || !heavy || now - first_touch < MIN_PRESS_SECS {
                continue;
            }
            self.upsert(AbsorptionLevel { price: level_price, side, volume: absorbed, first_secs: first_touch, last_secs: now }, band);
        }
    }

    fn upsert(&mut self, found: AbsorptionLevel, band: f64) {
        if let Some(existing) = self.levels.iter_mut().find(|t| t.level.side == found.side && (t.level.price - found.price).abs() <= band) {
            // Same level seen again: keep when it was first seen and the
            // most it has absorbed, follow where the range's edge is now.
            existing.level.price = found.price;
            existing.level.volume = existing.level.volume.max(found.volume);
            existing.level.last_secs = found.last_secs;
            existing.tolerance = band;
            return;
        }
        self.levels.push(Tracked { level: found, tolerance: band });
        if self.levels.len() > MAX_LEVELS {
            // Keep the most recently confirmed.
            self.levels.sort_by(|a, b| b.level.last_secs.total_cmp(&a.level.last_secs));
            self.levels.truncate(MAX_LEVELS);
        }
    }

    /// Current levels, highest price first.
    pub fn current(&self) -> Vec<AbsorptionLevel> {
        let mut out: Vec<AbsorptionLevel> = self.levels.iter().map(|t| t.level).collect();
        out.sort_by(|a, b| b.price.total_cmp(&a.price));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(timestamp_secs: f64, price: f64, size: u64, side: TradeSide) -> ClassifiedTrade {
        ClassifiedTrade { timestamp_secs, price, size, side, large: false }
    }

    /// Five minutes of trading between 2.00 and 2.25 where buyers keep
    /// lifting at the 2.25 high for the last three minutes.
    fn pressing_on_the_high() -> Vec<ClassifiedTrade> {
        let mut trades = Vec::new();
        for i in 0..20 {
            trades.push(t(f64::from(i) * 5.0, 2.00 + f64::from(i) * 0.012, 500, if i % 2 == 0 { TradeSide::Buy } else { TradeSide::Sell }));
        }
        for i in 0..36 {
            trades.push(t(110.0 + f64::from(i) * 5.0, if i % 3 == 0 { 2.24 } else { 2.25 }, 4_000, TradeSide::Buy));
        }
        trades
    }

    #[test]
    fn heavy_buying_held_under_a_high_is_a_sell_side_level() {
        let trades = pressing_on_the_high();
        let mut levels = AbsorptionLevels::default();
        levels.evaluate(295.0, trades.iter(), Some(10_000.0));
        let found = levels.current();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].side, AbsorptionSide::SellSide);
        assert!((found[0].price - 2.25).abs() < 1e-9);
        assert_eq!(found[0].volume, 36 * 4_000);
        assert!(found[0].first_secs <= 110.0 + 1e-9);
    }

    #[test]
    fn heavy_selling_held_over_a_low_is_a_buy_side_level() {
        let mut trades = Vec::new();
        for i in 0..20 {
            trades.push(t(f64::from(i) * 5.0, 2.25 - f64::from(i) * 0.012, 500, if i % 2 == 0 { TradeSide::Buy } else { TradeSide::Sell }));
        }
        for i in 0..36 {
            trades.push(t(110.0 + f64::from(i) * 5.0, 2.00, 4_000, TradeSide::Sell));
        }
        let mut levels = AbsorptionLevels::default();
        levels.evaluate(295.0, trades.iter(), Some(10_000.0));
        let found = levels.current();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].side, AbsorptionSide::BuySide);
        assert!((found[0].price - 2.00).abs() < 1e-9);
    }

    #[test]
    fn nothing_without_a_baseline_enough_trades_or_enough_time() {
        let trades = pressing_on_the_high();
        let mut levels = AbsorptionLevels::default();
        levels.evaluate(295.0, trades.iter(), None);
        assert!(levels.current().is_empty());
        levels.evaluate(295.0, trades.iter().take(10), Some(10_000.0));
        assert!(levels.current().is_empty());
        // The first touch of the high was seconds ago: a spike, not a level.
        let mut spike: Vec<ClassifiedTrade> = (0..40).map(|i| t(f64::from(i) * 5.0, 2.00 + f64::from(i) * 0.005, 500, TradeSide::Sell)).collect();
        spike.push(t(290.0, 2.60, 900_000, TradeSide::Buy));
        levels.evaluate(295.0, spike.iter(), Some(10_000.0));
        assert!(levels.current().is_empty());
    }

    #[test]
    fn ordinary_volume_at_the_high_is_not_a_level() {
        let trades = pressing_on_the_high();
        let mut levels = AbsorptionLevels::default();
        // "Normal" here is far larger than what printed at the high.
        levels.evaluate(295.0, trades.iter(), Some(5_000_000.0));
        assert!(levels.current().is_empty());
    }

    #[test]
    fn a_level_is_dropped_when_price_trades_through_it_or_it_goes_stale() {
        let trades = pressing_on_the_high();
        let mut levels = AbsorptionLevels::default();
        levels.evaluate(295.0, trades.iter(), Some(10_000.0));
        // A print just above, inside the band, does not break it.
        levels.on_trade(2.26);
        assert_eq!(levels.current().len(), 1);
        levels.on_trade(2.40);
        assert!(levels.current().is_empty());

        levels.evaluate(295.0, trades.iter(), Some(10_000.0));
        assert_eq!(levels.current().len(), 1);
        levels.evaluate(295.0 + STALE_SECS + 1.0, std::iter::empty(), Some(10_000.0));
        assert!(levels.current().is_empty());
    }

    #[test]
    fn seeing_the_same_level_again_updates_it_instead_of_duplicating() {
        let trades = pressing_on_the_high();
        let mut levels = AbsorptionLevels::default();
        levels.evaluate(280.0, trades.iter(), Some(10_000.0));
        levels.evaluate(295.0, trades.iter(), Some(10_000.0));
        let found = levels.current();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].last_secs, 295.0);
    }
}
