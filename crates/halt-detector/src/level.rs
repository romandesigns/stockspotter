//! Color escalation — architecture doc part-3's UI concept: "Color
//! escalation (calm -> amber -> red) based on both proximity to
//! threshold and volume strength together — a fast move on weak volume
//! should be flagged differently than the same move backed by heavy
//! volume, since volume-backed moves are more trustworthy."
//!
//! Implemented as: proximity alone sets a base tier, and a move sitting
//! at amber on proximity alone gets escalated to red if strong relative
//! volume backs it up. A red-by-proximity-alone reading stays red
//! regardless of volume — price is genuinely close to the real halt
//! threshold at that point regardless of how "trustworthy" the move
//! looks, so volume isn't used to *downgrade* it.
//!
//! **Hysteresis** (added 2026-08-31, found live on real market-open
//! data): a price sitting right at an exact boundary — AEHL wobbling
//! $6.51/$6.52 around proximity 0.50, CHGA around 0.80 — flapped the
//! bucket on every single tick with the original threshold-only version,
//! since two adjacent trades landing a cent apart could straddle the
//! exact cutoff. `classify` now takes the *previous* level and only lets
//! a reading de-escalate once it clears `hysteresis_margin` past the
//! threshold, not just past the threshold itself — escalating is still
//! immediate (no reason to delay flagging a real approach), only the
//! way back down is damped. A classic Schmitt trigger, applied to the
//! proximity tier only; the volume-escalation rule wasn't observed
//! flapping live (relative volume moves far more slowly tick-to-tick
//! than price does at a fixed dollar level) so it's left as a plain
//! threshold.
//!
//! **Margin widened 0.05 -> 0.10 (2026-09-03), found live via the
//! background accuracy watch**: the 0.05 margin de-escalates correctly
//! (verified against real logs — BIAF's Amber->Calm transitions landed
//! consistently at exactly proximity 0.43-0.45, precisely
//! threshold-minus-margin, not early), but 0.05 turned out to be too
//! narrow a band for how much a real, actively-priced-but-thin symbol's
//! proximity naturally wobbles tick-to-tick: BIAF (relative volume only
//! ~0.06-0.08x — essentially no real trading behind the move) still
//! flapped Calm<->Amber 7 times in under 9 seconds (09:41:47.26 ->
//! 09:41:56.00 UTC), because its real noise band (proximity 0.43-0.52,
//! amplitude ~0.07-0.09) exceeded the 0.05 margin meant to absorb it.
//! Doubling to 0.10 comfortably clears that observed noise band without
//! changing the escalation side at all (still immediate). Chosen the
//! same way the original 0.05 default was (reasoned directly from real
//! observed chatter, not a backtested value) — YQ's own chatter that
//! same session (114x relative volume, genuinely violent real price
//! action, level changes within single-digit milliseconds of each
//! other) is NOT what this change targets and won't fully resolve it: a
//! wider margin reduces but can't eliminate flapping when the underlying
//! real price swing itself exceeds the margin in one tick — and it
//! deliberately shouldn't try to, since suppressing a genuinely fast,
//! volume-backed move right at the halt boundary would hide exactly the
//! information this system exists to surface.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertLevel {
    Calm,
    Amber,
    Red,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AlertLevelThresholds {
    /// `proximity_ratio` (current move / band width) at or above this
    /// counts as amber — starting values, not yet backtested against
    /// real halt occurrences (same honestly-flagged status as the
    /// flat-base gate's thresholds).
    pub amber_proximity_ratio: f64,
    pub red_proximity_ratio: f64,
    /// Relative volume (session volume / avg daily volume) at or above
    /// this escalates an amber-by-proximity reading to red.
    pub volume_escalation_rel_volume: f64,
    /// How far *below* `amber_proximity_ratio`/`red_proximity_ratio` a
    /// reading must drop before de-escalating out of that level — see
    /// the module doc comment. Escalating is never delayed by this, only
    /// de-escalating. Widened 0.05 -> 0.10 on 2026-09-03 after real BIAF
    /// chatter (noise amplitude ~0.07-0.09) exceeded the original
    /// margin; still not backtested against real halt occurrences, same
    /// as before, just reasoned from a second, larger real sample of
    /// live chatter.
    pub hysteresis_margin: f64,
}

impl Default for AlertLevelThresholds {
    fn default() -> Self {
        Self {
            amber_proximity_ratio: 0.5,
            red_proximity_ratio: 0.8,
            volume_escalation_rel_volume: 3.0,
            hysteresis_margin: 0.10,
        }
    }
}

/// `previous` is the level this same symbol was classified as on its
/// last reading — `AlertLevel::Calm` is the correct choice for a
/// symbol's very first reading (nothing to be "sticky" about yet).
pub fn classify(
    proximity_ratio: f64,
    relative_volume: f64,
    previous: AlertLevel,
    thresholds: &AlertLevelThresholds,
) -> AlertLevel {
    let base = proximity_level_with_hysteresis(proximity_ratio, previous, thresholds);

    if base == AlertLevel::Amber && relative_volume >= thresholds.volume_escalation_rel_volume {
        AlertLevel::Red
    } else {
        base
    }
}

fn proximity_level_with_hysteresis(
    proximity_ratio: f64,
    previous: AlertLevel,
    thresholds: &AlertLevelThresholds,
) -> AlertLevel {
    let red_deescalate = thresholds.red_proximity_ratio - thresholds.hysteresis_margin;
    let amber_deescalate = thresholds.amber_proximity_ratio - thresholds.hysteresis_margin;

    if proximity_ratio >= thresholds.red_proximity_ratio {
        return AlertLevel::Red;
    }
    if previous == AlertLevel::Red && proximity_ratio >= red_deescalate {
        return AlertLevel::Red;
    }
    if proximity_ratio >= thresholds.amber_proximity_ratio {
        return AlertLevel::Amber;
    }
    if matches!(previous, AlertLevel::Red | AlertLevel::Amber) && proximity_ratio >= amber_deescalate {
        return AlertLevel::Amber;
    }
    AlertLevel::Calm
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thresholds() -> AlertLevelThresholds {
        AlertLevelThresholds::default()
    }

    #[test]
    fn low_proximity_is_calm_regardless_of_volume() {
        assert_eq!(classify(0.1, 10.0, AlertLevel::Calm, &thresholds()), AlertLevel::Calm);
    }

    #[test]
    fn mid_proximity_with_weak_volume_stays_amber() {
        assert_eq!(classify(0.6, 1.0, AlertLevel::Calm, &thresholds()), AlertLevel::Amber);
    }

    #[test]
    fn mid_proximity_with_strong_volume_escalates_to_red() {
        assert_eq!(classify(0.6, 5.0, AlertLevel::Calm, &thresholds()), AlertLevel::Red);
    }

    #[test]
    fn high_proximity_is_red_even_with_weak_volume() {
        assert_eq!(classify(0.9, 0.5, AlertLevel::Calm, &thresholds()), AlertLevel::Red);
    }

    // --- Hysteresis: the actual bug this was built to fix ---

    #[test]
    fn escalation_from_calm_to_amber_is_immediate_no_delay() {
        // Crossing 0.5 for the first time — no reason to delay flagging
        // a genuine new approach just because hysteresis exists.
        assert_eq!(classify(0.50, 0.0, AlertLevel::Calm, &thresholds()), AlertLevel::Amber);
    }

    #[test]
    fn amber_stays_amber_within_the_hysteresis_margin_below_threshold() {
        // 0.47 is below the 0.5 amber threshold but within the default
        // 0.05 margin — real AEHL data wobbled in exactly this range.
        assert_eq!(classify(0.47, 0.0, AlertLevel::Amber, &thresholds()), AlertLevel::Amber);
    }

    #[test]
    fn amber_deescalates_to_calm_once_clearly_below_the_margin() {
        // 0.38 is comfortably below the 0.10 margin (de-escalate point
        // 0.40) -- was 0.44 pre-widening; that value now sits INSIDE the
        // wider margin (0.44 >= 0.40) and would incorrectly stay Amber,
        // which is exactly the point of the widening.
        assert_eq!(classify(0.38, 0.0, AlertLevel::Amber, &thresholds()), AlertLevel::Calm);
    }

    #[test]
    fn red_stays_red_within_the_hysteresis_margin_below_threshold() {
        // 0.72 is below the 0.8 red threshold but within the 0.10
        // margin (de-escalate point 0.70) -- real CHGA data wobbled in
        // this general range.
        assert_eq!(classify(0.72, 0.0, AlertLevel::Red, &thresholds()), AlertLevel::Red);
    }

    #[test]
    fn red_deescalates_to_amber_once_clearly_below_the_margin_but_still_above_amber() {
        // 0.65 is comfortably below the 0.10 margin (de-escalate point
        // 0.70) -- was 0.70 pre-widening, which now sits exactly AT the
        // new de-escalate boundary (inclusive >=) and would incorrectly
        // stay Red.
        assert_eq!(classify(0.65, 0.0, AlertLevel::Red, &thresholds()), AlertLevel::Amber);
    }

    // --- Real BIAF chatter (2026-09-03), the reason the margin widened ---

    #[test]
    fn biaf_real_chatter_sequence_is_fully_absorbed_by_the_wider_margin() {
        // The exact proximity sequence BIAF produced live, 09:41:47-
        // 09:41:56 UTC (see the module doc comment) -- flapped Calm<->
        // Amber 7 times under the old 0.05 margin. Replayed here bar by
        // bar against the real classify() state machine: with 0.10 it
        // should settle into Amber and never drop back to Calm, since
        // every "low" reading in the real sequence (0.43-0.45) sits
        // above the new de-escalate point (0.40).
        let seq = [0.43, 0.50, 0.45, 0.50, 0.45, 0.50, 0.40];
        let mut level = AlertLevel::Calm;
        for p in seq {
            level = classify(p, 0.0, level, &thresholds());
        }
        assert_eq!(level, AlertLevel::Amber);
    }

    #[test]
    fn red_can_drop_all_the_way_to_calm_on_a_real_reversal() {
        assert_eq!(classify(0.1, 0.0, AlertLevel::Red, &thresholds()), AlertLevel::Calm);
    }

    #[test]
    fn a_symbols_very_first_reading_has_nothing_to_be_sticky_about() {
        // Calm as the "previous" for a brand-new symbol shouldn't behave
        // any differently than the pre-hysteresis version did.
        assert_eq!(classify(0.47, 0.0, AlertLevel::Calm, &thresholds()), AlertLevel::Calm);
    }
}
