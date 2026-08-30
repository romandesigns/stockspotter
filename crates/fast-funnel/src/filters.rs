//! Stage 1 (static) and Stage 2 (dynamic) filtering — section 4.1 of the
//! architecture doc.
//!
//! These are plain, synchronous, side-effect-free functions on purpose:
//! no network, no async, nothing that differs between live and replay. The
//! same functions run whether `TickerSnapshot`s come from a live Alpaca
//! stream or from historical bars fed by the future replay engine.

use crate::types::{FilterThresholds, TickerSnapshot};

/// Per-condition breakdown of why a ticker did or didn't pass the funnel.
/// Exists so callers (a live scan loop, a future "why didn't this qualify"
/// debug view in the scanner UI) can explain a result instead of only
/// getting a boolean.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FunnelExplanation {
    pub price_ok: bool,
    pub float_ok: bool,
    pub rel_vol_ok: bool,
    pub gap_ok: bool,
}

impl FunnelExplanation {
    pub fn stage1_passed(&self) -> bool {
        self.price_ok && self.float_ok
    }

    pub fn passed(&self) -> bool {
        self.stage1_passed() && self.rel_vol_ok && self.gap_ok
    }
}

/// Explains a single ticker against both stages without needing it to
/// already be part of a filtered pool — useful for logging/diagnostics on
/// the full live universe.
pub fn explain(t: &TickerSnapshot, thresholds: &FilterThresholds) -> FunnelExplanation {
    FunnelExplanation {
        price_ok: t.price >= thresholds.min_price && t.price <= thresholds.max_price,
        float_ok: t
            .float_shares
            .is_some_and(|f| f <= thresholds.max_float_shares),
        rel_vol_ok: t
            .relative_volume()
            .is_some_and(|rv| rv >= thresholds.min_relative_volume),
        gap_ok: t.gap_pct >= thresholds.min_gap_pct,
    }
}

/// Stage 1 — cheap static filtering. No live/session data required beyond
/// price and float, so this runs first against the full universe.
pub fn stage1_static_filter<'a>(
    universe: &'a [TickerSnapshot],
    thresholds: &FilterThresholds,
) -> Vec<&'a TickerSnapshot> {
    universe
        .iter()
        .filter(|t| {
            let price_ok = t.price >= thresholds.min_price && t.price <= thresholds.max_price;
            let float_ok = t
                .float_shares
                .is_some_and(|f| f <= thresholds.max_float_shares);
            price_ok && float_ok
        })
        .collect()
}

/// Stage 2 — dynamic filtering. Only ever called on the Stage 1 output, not
/// the full universe, per the architecture doc.
pub fn stage2_dynamic_filter<'a>(
    stage1_pool: &[&'a TickerSnapshot],
    thresholds: &FilterThresholds,
) -> Vec<&'a TickerSnapshot> {
    stage1_pool
        .iter()
        .copied()
        .filter(|t| {
            let rel_vol_ok = t
                .relative_volume()
                .is_some_and(|rv| rv >= thresholds.min_relative_volume);
            let gap_ok = t.gap_pct >= thresholds.min_gap_pct;
            rel_vol_ok && gap_ok
        })
        .collect()
}

/// Runs both stages in sequence, returning the final shortlist.
pub fn run_fast_funnel<'a>(
    universe: &'a [TickerSnapshot],
    thresholds: &FilterThresholds,
) -> Vec<&'a TickerSnapshot> {
    let stage1 = stage1_static_filter(universe, thresholds);
    stage2_dynamic_filter(&stage1, thresholds)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticker(symbol: &str, price: f64, float_shares: Option<u64>) -> TickerSnapshot {
        TickerSnapshot {
            symbol: symbol.to_string(),
            price,
            float_shares,
            avg_daily_volume: 1_000_000,
            session_volume: 1_000_000,
            gap_pct: 0.0,
        }
    }

    #[test]
    fn stage1_rejects_price_outside_range() {
        let thresholds = FilterThresholds::default();
        let universe = vec![
            ticker("TOO_CHEAP", 0.10, Some(5_000_000)),
            ticker("TOO_EXPENSIVE", 25.0, Some(5_000_000)),
            ticker("JUST_RIGHT", 5.0, Some(5_000_000)),
        ];

        let result = stage1_static_filter(&universe, &thresholds);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].symbol, "JUST_RIGHT");
    }

    #[test]
    fn stage1_rejects_large_float_and_unknown_float() {
        let thresholds = FilterThresholds::default();
        let universe = vec![
            ticker("HUGE_FLOAT", 5.0, Some(50_000_000)),
            ticker("UNKNOWN_FLOAT", 5.0, None),
            ticker("LOW_FLOAT", 5.0, Some(10_000_000)),
        ];

        let result = stage1_static_filter(&universe, &thresholds);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].symbol, "LOW_FLOAT");
    }

    #[test]
    fn stage2_requires_both_rel_volume_and_gap() {
        let thresholds = FilterThresholds::default();

        let mut low_rel_vol = ticker("LOW_RVOL", 5.0, Some(5_000_000));
        low_rel_vol.avg_daily_volume = 1_000_000;
        low_rel_vol.session_volume = 2_000_000; // 2x, below 5x threshold
        low_rel_vol.gap_pct = 15.0;

        let mut low_gap = ticker("LOW_GAP", 5.0, Some(5_000_000));
        low_gap.avg_daily_volume = 1_000_000;
        low_gap.session_volume = 10_000_000; // 10x, passes
        low_gap.gap_pct = 3.0; // below 10% threshold

        let mut qualifies = ticker("QUALIFIES", 5.0, Some(5_000_000));
        qualifies.avg_daily_volume = 1_000_000;
        qualifies.session_volume = 10_000_000; // 10x
        qualifies.gap_pct = 20.0;

        let stage1_pool: Vec<&TickerSnapshot> = vec![&low_rel_vol, &low_gap, &qualifies];
        let result = stage2_dynamic_filter(&stage1_pool, &thresholds);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].symbol, "QUALIFIES");
    }

    #[test]
    fn stage2_treats_zero_avg_volume_as_unqualified_not_a_crash() {
        let thresholds = FilterThresholds::default();
        let mut zero_avg = ticker("ZERO_AVG", 5.0, Some(5_000_000));
        zero_avg.avg_daily_volume = 0;
        zero_avg.session_volume = 500_000;
        zero_avg.gap_pct = 50.0;

        let stage1_pool: Vec<&TickerSnapshot> = vec![&zero_avg];
        let result = stage2_dynamic_filter(&stage1_pool, &thresholds);

        assert!(result.is_empty());
    }

    #[test]
    fn explain_reports_per_condition_breakdown() {
        let thresholds = FilterThresholds::default();
        let mut t = ticker("PARTIAL", 5.0, Some(5_000_000));
        t.avg_daily_volume = 1_000_000;
        t.session_volume = 10_000_000; // 10x, passes
        t.gap_pct = 3.0; // below 10% threshold, fails

        let e = explain(&t, &thresholds);

        assert!(e.price_ok);
        assert!(e.float_ok);
        assert!(e.stage1_passed());
        assert!(e.rel_vol_ok);
        assert!(!e.gap_ok);
        assert!(!e.passed());
    }

    #[test]
    fn full_funnel_reduces_universe_to_qualified_shortlist() {
        let thresholds = FilterThresholds::default();
        let mut winner = ticker("WINNER", 8.0, Some(8_000_000));
        winner.avg_daily_volume = 500_000;
        winner.session_volume = 6_000_000; // 12x
        winner.gap_pct = 30.0;

        let universe = vec![
            ticker("PENNY_TOO_CHEAP", 0.05, Some(1_000_000)),
            ticker("LARGE_CAP", 150.0, Some(300_000_000)),
            winner.clone(),
        ];

        let result = run_fast_funnel(&universe, &thresholds);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].symbol, "WINNER");
    }
}
