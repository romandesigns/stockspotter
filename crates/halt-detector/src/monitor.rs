//! `HaltWarningMonitor` — the stateful entry point a live scan loop (or
//! replay engine) actually talks to, one instance per watched symbol.
//! Ties `reference.rs` (rolling reference price), `bands.rs` (LULD band
//! width + closing-window doubling), and `level.rs` (color escalation)
//! together into one `on_trade` call per tick, matching the same
//! pure-functions-plus-a-stateful-wrapper shape as
//! `ignition_detector::monitor`.
//!
//! Isolation, per the doc's explicit note: this only reads raw trade
//! ticks and (for relative volume) a caller-supplied avg-daily-volume
//! baseline — it never reads or is read by `fast_funnel`,
//! `momentum_scorer`, or `ignition_detector`'s state.

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::bands::{band_doubles, band_width_dollars, is_closing_window, luld_in_effect};
use crate::level::{classify, AlertLevel, AlertLevelThresholds};
use crate::reference::{ReferencePriceConfig, ReferencePriceTracker};
use crate::tick::Trade;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HaltWarningConfig {
    pub reference: ReferencePriceConfig,
    pub level: AlertLevelThresholds,
}

impl Default for HaltWarningConfig {
    fn default() -> Self {
        Self {
            reference: ReferencePriceConfig::default(),
            level: AlertLevelThresholds::default(),
        }
    }
}

/// One card's worth of data for the doc's UI concept — ticker/price come
/// from the caller (this crate doesn't know the symbol), everything else
/// here.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct HaltWarningReading {
    pub estimated_bands: bool,
    pub reference_price: f64,
    pub current_price: f64,
    pub band_width_dollars: f64,
    pub band_doubled: bool,
    /// Current move from the reference price, as a fraction of the
    /// applicable band width. 1.0 = exactly at the halt threshold, >1.0 =
    /// already past it.
    pub proximity_ratio: f64,
    /// `None` if no avg-daily-volume baseline was supplied at
    /// construction — relative volume simply isn't computable, not zero.
    pub relative_volume: Option<f64>,
    pub level: AlertLevel,
    /// False outside 9:30-16:00 ET on a weekday, when LULD bands aren't
    /// in force and no halt can be triggered. `level` is pinned to
    /// `Calm` whenever this is false (see `on_trade`), but the raw
    /// `proximity_ratio` is still computed and reported so the UI can
    /// show the real number with an honest "not in effect yet" caveat
    /// rather than a blank card or a fake-calm one.
    pub luld_in_effect: bool,
}

#[derive(Debug, Clone)]
pub struct HaltWarningMonitor {
    official_bands: Option<(chrono::NaiveDate, f64, f64)>,
    config: HaltWarningConfig,
    reference: ReferencePriceTracker,
    avg_daily_volume: u64,
    session_volume: u64,
    /// The level this symbol was classified as on its last reading — fed
    /// back into `classify` so it can apply hysteresis (see `level.rs`'s
    /// doc comment). Starts at `Calm`, the correct "nothing to be sticky
    /// about yet" state for a symbol's very first trade.
    last_level: AlertLevel,
}

impl HaltWarningMonitor {
    pub fn new(config: HaltWarningConfig, avg_daily_volume: u64) -> Self {
        Self {
            official_bands: None,
            reference: ReferencePriceTracker::new(config.reference),
            config,
            avg_daily_volume,
            session_volume: 0,
            last_level: AlertLevel::Calm,
        }
    }

    /// `now` is wall-clock time, used only for the closing-window
    /// doubling check — kept as an explicit parameter (not read from the
    /// trade itself) so replay can pass the trade's own historical
    /// timestamp and live can pass real wall-clock time, both through
    /// the identical code path.
    pub fn on_trade(&mut self, trade: Trade, now: DateTime<Utc>) -> HaltWarningReading {
        self.session_volume += trade.size;
        let estimated_reference = self.reference.on_trade(trade.timestamp_secs, trade.price);
        let date = now.with_timezone(&chrono_tz::America::New_York).date_naive();
        let official = self.official_bands.filter(|(d,_,_)| *d == date && luld_in_effect(now));
        let reference_price = official.map_or(estimated_reference, |(_,lower,upper)| (lower+upper)/2.0);

        let doubled = band_doubles(reference_price, is_closing_window(now));
        let band = official.map_or_else(|| band_width_dollars(reference_price, doubled), |(_,lower,upper)| (upper-lower)/2.0);
        let proximity_ratio = if band > 0.0 {
            (trade.price - reference_price).abs() / band
        } else {
            0.0
        };

        let relative_volume = if self.avg_daily_volume > 0 {
            Some(self.session_volume as f64 / self.avg_daily_volume as f64)
        } else {
            None
        };

        // Outside the regular session there is no LULD band to approach,
        // so no escalation — but the reference tracker above still gets
        // every tick, so the 5-minute rolling reference is already warm
        // and correct the moment 9:30 arrives, rather than starting cold
        // on the single most volatile minute of the day.
        let in_effect = luld_in_effect(now);
        let level = if in_effect {
            classify(proximity_ratio, relative_volume.unwrap_or(0.0), self.last_level, &self.config.level)
        } else {
            AlertLevel::Calm
        };
        self.last_level = level;

        HaltWarningReading {
            estimated_bands: official.is_none(),
            luld_in_effect: in_effect,
            reference_price,
            current_price: trade.price,
            band_width_dollars: band,
            band_doubled: doubled,
            proximity_ratio,
            relative_volume,
            level,
        }
    }

    pub fn on_luld(&mut self, lower: f64, upper: f64, now: DateTime<Utc>) {
        self.official_bands = if lower.is_finite() && upper.is_finite() && lower > 0.0 && upper > lower {
            Some((now.with_timezone(&chrono_tz::America::New_York).date_naive(), lower, upper))
        } else { None };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn official_sip_bands_override_the_estimate_and_invalid_bands_clear_them() {
        let mut monitor = HaltWarningMonitor::new(HaltWarningConfig::default(),1000);
        monitor.on_luld(9.5,10.5,midday());
        let reading = monitor.on_trade(trade(0.0,10.3),midday());
        assert!(!reading.estimated_bands);
        assert!((reading.proximity_ratio-0.6).abs()<1e-9);
        monitor.on_luld(0.0,0.0,midday());
        assert!(monitor.on_trade(trade(1.0,10.3),midday()).estimated_bands);
    }

    fn trade(t: f64, price: f64) -> Trade {
        Trade {
            timestamp_secs: t,
            price,
            size: 1000,
        }
    }

    fn midday() -> DateTime<Utc> {
        // Not the closing window, any ordinary trading-day timestamp.
        Utc.with_ymd_and_hms(2026, 8, 28, 15, 0, 0).unwrap()
    }

    #[test]
    fn calm_when_price_sits_near_the_reference() {
        let mut monitor = HaltWarningMonitor::new(HaltWarningConfig::default(), 1_000_000);
        monitor.on_trade(trade(0.0, 5.00), midday());
        let reading = monitor.on_trade(trade(1.0, 5.01), midday());

        assert_eq!(reading.level, AlertLevel::Calm);
        assert!(reading.proximity_ratio < 0.1);
    }

    #[test]
    fn proximity_ratio_exceeds_1_once_price_outruns_the_lagging_reference() {
        let mut monitor = HaltWarningMonitor::new(HaltWarningConfig::default(), 1_000_000);
        // Seed a real 5-min-window's worth of trades at $5.00 first — the
        // reference is a rolling average, so it only ever "catches up" to
        // a fast move as fast as the window's trade count lets it. With
        // only one or two trades total, the average (and so the
        // reference) would swing almost as fast as price itself and this
        // scenario couldn't actually happen — this is what a realistic
        // multi-trade session looks like instead.
        for i in 0..10 {
            monitor.on_trade(trade(i as f64, 5.00), midday());
        }
        // One fast trade well outside the $5 band ($0.50 at the 10%
        // tier) — the reference nudges up from the average, but nowhere
        // near enough to absorb this move.
        let reading = monitor.on_trade(trade(10.0, 6.00), midday());

        assert!(reading.proximity_ratio > 1.0, "expected proximity past 1.0, got {}", reading.proximity_ratio);
        assert_eq!(reading.level, AlertLevel::Red);
    }

    /// 8:00 AM EDT on the same weekday as `midday()` — premarket, when
    /// LULD bands are not in force.
    fn premarket() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 28, 12, 0, 0).unwrap()
    }

    #[test]
    fn premarket_never_escalates_even_on_a_move_that_would_be_red_intraday() {
        let mut monitor = HaltWarningMonitor::new(HaltWarningConfig::default(), 1_000_000);
        for i in 0..10 {
            monitor.on_trade(trade(i as f64, 5.00), premarket());
        }
        // The exact move that
        // `proximity_ratio_exceeds_1_once_price_outruns_the_lagging_reference`
        // classifies as Red during the session.
        let reading = monitor.on_trade(trade(10.0, 6.00), premarket());

        assert!(!reading.luld_in_effect);
        assert_eq!(
            reading.level,
            AlertLevel::Calm,
            "no LULD band is in force premarket, so nothing to escalate toward"
        );
        // The underlying number is still reported honestly — only the
        // escalation is suppressed, so the UI can show the real move.
        assert!(reading.proximity_ratio > 1.0);
    }

    #[test]
    fn the_reference_price_is_already_warm_when_the_session_opens() {
        // Why premarket ticks still feed the reference tracker: if they
        // didn't, the 5-minute rolling reference would start cold at
        // 9:30 — the single most volatile minute of the day — and the
        // first intraday readings would be garbage.
        let mut monitor = HaltWarningMonitor::new(HaltWarningConfig::default(), 1_000_000);
        for i in 0..10 {
            monitor.on_trade(trade(i as f64, 5.00), premarket());
        }
        let open = Utc.with_ymd_and_hms(2026, 8, 28, 13, 31, 0).unwrap(); // 9:31 AM EDT
        let reading = monitor.on_trade(trade(10.0, 5.01), open);

        assert!(reading.luld_in_effect);
        assert!(
            (reading.reference_price - 5.00).abs() < 0.05,
            "reference should already be anchored near $5 from premarket, got {}",
            reading.reference_price
        );
    }

    #[test]
    fn relative_volume_is_none_without_an_avg_daily_volume_baseline() {
        let mut monitor = HaltWarningMonitor::new(HaltWarningConfig::default(), 0);
        let reading = monitor.on_trade(trade(0.0, 5.00), midday());
        assert_eq!(reading.relative_volume, None);
    }

    #[test]
    fn relative_volume_accumulates_across_trades() {
        let mut monitor = HaltWarningMonitor::new(HaltWarningConfig::default(), 1000);
        monitor.on_trade(trade(0.0, 5.00), midday());
        let reading = monitor.on_trade(trade(1.0, 5.00), midday());
        // Two 1000-share trades = 2000 session volume / 1000 avg = 2.0x.
        assert_eq!(reading.relative_volume, Some(2.0));
    }

    #[test]
    fn band_doubles_during_the_closing_window_for_a_sub_3_dollar_stock() {
        let mut monitor = HaltWarningMonitor::new(HaltWarningConfig::default(), 1_000_000);
        monitor.on_trade(trade(0.0, 2.00), midday());
        let closing = Utc.with_ymd_and_hms(2026, 8, 28, 19, 40, 0).unwrap(); // 3:40 PM EDT
        let reading = monitor.on_trade(trade(1.0, 2.00), closing);

        assert!(reading.band_doubled);
        // $2 is in the 20% tier -> normally $0.40, doubled -> $0.80.
        assert!((reading.band_width_dollars - 0.80).abs() < 1e-9);
    }

    #[test]
    fn hysteresis_stops_the_level_flapping_on_a_wobbling_price_near_the_boundary() {
        // Real bug found live 2026-08-31 (AEHL wobbling $6.51/$6.52 around
        // proximity 0.50): without hysteresis this exact trade sequence
        // flaps Amber->Calm->Amber every tick. Reproduces the shape of
        // that real sequence, not the exact prices.
        let mut monitor = HaltWarningMonitor::new(HaltWarningConfig::default(), 0);

        // A large, quiet seed window at $5.00 so a few later boundary
        // trades barely move the rolling average — keeps the reference
        // pinned at exactly $5.00 (well under reference.rs's 1% hysteresis
        // threshold) so the proximity math below is exact and predictable.
        for i in 0..50 {
            monitor.on_trade(trade(i as f64, 5.00), midday());
        }

        // A real move to proximity 0.60 (>$3 tier: 10% band = $0.50;
        // move $0.30 -> 0.30/0.50 = 0.60) — clearly escalates to Amber.
        let escalate = monitor.on_trade(trade(50.0, 5.30), midday());
        assert_eq!(escalate.level, AlertLevel::Amber);

        // Price pulls back to proximity ~0.47 — below the 0.5 amber
        // threshold, but within the default 0.05 hysteresis margin. The
        // pre-hysteresis version of this code would drop this to Calm;
        // the real bug this test guards against.
        let wobble = monitor.on_trade(trade(51.0, 5.235), midday());
        assert_eq!(wobble.level, AlertLevel::Amber, "should stay Amber within the hysteresis margin, not flap to Calm");

        // A genuine reversal, clearly below the margin — de-escalation
        // still has to actually work, hysteresis only damps noise.
        let reversal = monitor.on_trade(trade(52.0, 5.10), midday());
        assert_eq!(reversal.level, AlertLevel::Calm, "a real move back down should still de-escalate");
    }
}
