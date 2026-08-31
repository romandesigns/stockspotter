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
}

impl Default for AlertLevelThresholds {
    fn default() -> Self {
        Self {
            amber_proximity_ratio: 0.5,
            red_proximity_ratio: 0.8,
            volume_escalation_rel_volume: 3.0,
        }
    }
}

pub fn classify(
    proximity_ratio: f64,
    relative_volume: f64,
    thresholds: &AlertLevelThresholds,
) -> AlertLevel {
    let base = if proximity_ratio >= thresholds.red_proximity_ratio {
        AlertLevel::Red
    } else if proximity_ratio >= thresholds.amber_proximity_ratio {
        AlertLevel::Amber
    } else {
        AlertLevel::Calm
    };

    if base == AlertLevel::Amber && relative_volume >= thresholds.volume_escalation_rel_volume {
        AlertLevel::Red
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thresholds() -> AlertLevelThresholds {
        AlertLevelThresholds::default()
    }

    #[test]
    fn low_proximity_is_calm_regardless_of_volume() {
        assert_eq!(classify(0.1, 10.0, &thresholds()), AlertLevel::Calm);
    }

    #[test]
    fn mid_proximity_with_weak_volume_stays_amber() {
        assert_eq!(classify(0.6, 1.0, &thresholds()), AlertLevel::Amber);
    }

    #[test]
    fn mid_proximity_with_strong_volume_escalates_to_red() {
        assert_eq!(classify(0.6, 5.0, &thresholds()), AlertLevel::Red);
    }

    #[test]
    fn high_proximity_is_red_even_with_weak_volume() {
        assert_eq!(classify(0.9, 0.5, &thresholds()), AlertLevel::Red);
    }
}
