//! The stateful per-symbol wrapper a live loop or replay actually talks
//! to — same "pure functions plus stateful monitor" split as
//! `ignition_detector::monitor` and `halt_detector::monitor`.
//!
//! State machine: `WatchingForSurge` -> (a real surge detected) ->
//! `TrackingConsolidation` -> either breaks out (`EntryTriggered`, resets
//! to watching) or gets invalidated/times out (silently resets to
//! watching, no event).

use std::collections::VecDeque;

use crate::candle::Candle;
use crate::consolidation::{breakout_triggered, is_valid_consolidation_candle, support_level, ConsolidationThresholds};
use crate::surge::{detect_surge, SurgeInfo, SurgeThresholds};

/// Matches `momentum_scorer`'s own MA_SHORT convention — same "9-period"
/// the doc names for the support-level reference. The *window size cap*
/// for the post-surge MA (see `post_surge_ma` below) — not a minimum
/// sample size, that's `MIN_CANDLES_FOR_POST_SURGE_MA`.
const MA_PERIOD: usize = 9;

/// Don't trust a post-surge "average" computed from only 1-2 candles —
/// with that few points it's barely different from just the most recent
/// close, which made support snap to whatever the last candle happened
/// to do rather than smoothing anything (found replaying real COOT data,
/// 2026-08-31: a 1-candle "average" made support jump to that exact
/// candle's close, invalidating the very next candle for any dip at
/// all). Below this many post-surge candles, `support_level` falls back
/// to `surge_low` alone.
const MIN_CANDLES_FOR_POST_SURGE_MA: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConsolidationBreakoutConfig {
    pub surge: SurgeThresholds,
    pub consolidation: ConsolidationThresholds,
}

impl Default for ConsolidationBreakoutConfig {
    fn default() -> Self {
        Self {
            surge: SurgeThresholds::default(),
            consolidation: ConsolidationThresholds::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConsolidationBreakoutEvent {
    None,
    /// Fires once, the moment a surge is first detected and consolidation
    /// tracking begins — informational (lets a UI show "watching X" the
    /// instant this phase starts, before there's a consolidation range to
    /// show yet) and useful for diagnosing this strategy against real
    /// data: distinguishes "no surge ever recognized" from "a surge fired
    /// but consolidation never confirmed/broke out".
    SurgeDetected { low: f64, high: f64 },
    /// Fires once when the consolidation pattern first becomes confirmed
    /// (reached `min_consolidation_candles` valid candles in a row) —
    /// informational, so a UI panel can show "watching a consolidation"
    /// before the actual entry trigger fires.
    ConsolidationConfirmed { consolidation_high: f64, support: f64 },
    /// The actual entry signal: a confirmed consolidation's range just
    /// broke to the upside.
    EntryTriggered { price: f64 },
}

#[derive(Debug, Clone)]
enum State {
    WatchingForSurge,
    TrackingConsolidation {
        surge: SurgeInfo,
        candles: Vec<Candle>,
        confirmed: bool,
        /// How many *consecutive* candles have just failed validity —
        /// tolerated up to `ConsolidationThresholds::max_consecutive_invalid`
        /// before giving up on this attempt entirely. A valid candle
        /// resets this to 0.
        consecutive_invalid: usize,
    },
}

pub struct ConsolidationBreakoutMonitor {
    config: ConsolidationBreakoutConfig,
    recent: VecDeque<Candle>,
    state: State,
}

impl ConsolidationBreakoutMonitor {
    pub fn new(config: ConsolidationBreakoutConfig) -> Self {
        Self {
            config,
            recent: VecDeque::new(),
            state: State::WatchingForSurge,
        }
    }

    pub fn on_candle(&mut self, candle: Candle) -> ConsolidationBreakoutEvent {
        let surge_window_needed = self.config.surge.baseline_candles + self.config.surge.lookback_candles;
        self.recent.push_back(candle);
        while self.recent.len() > surge_window_needed {
            self.recent.pop_front();
        }

        // Owned extraction (`mem::replace`) rather than matching
        // `&mut self.state` directly — the arms below need to both read
        // fields out of the current state *and* reassign `self.state`,
        // which two live mutable borrows of the same field can't do.
        let state = std::mem::replace(&mut self.state, State::WatchingForSurge);
        let (next_state, event) = match state {
            State::WatchingForSurge => {
                if self.recent.len() >= surge_window_needed {
                    let window: Vec<Candle> = self.recent.iter().copied().collect();
                    match detect_surge(&window, &self.config.surge) {
                        Some(surge) => (
                            State::TrackingConsolidation { surge, candles: Vec::new(), confirmed: false, consecutive_invalid: 0 },
                            ConsolidationBreakoutEvent::SurgeDetected { low: surge.low, high: surge.high },
                        ),
                        None => (State::WatchingForSurge, ConsolidationBreakoutEvent::None),
                    }
                } else {
                    (State::WatchingForSurge, ConsolidationBreakoutEvent::None)
                }
            }
            State::TrackingConsolidation { surge, candles, confirmed, consecutive_invalid } => {
                if confirmed {
                    let consolidation_high = highest_high(&candles);
                    if breakout_triggered(&candle, consolidation_high) {
                        (State::WatchingForSurge, ConsolidationBreakoutEvent::EntryTriggered { price: candle.close })
                    } else {
                        step_consolidation(candle, surge, candles, confirmed, consecutive_invalid, &self.config.consolidation)
                    }
                } else {
                    step_consolidation(candle, surge, candles, confirmed, consecutive_invalid, &self.config.consolidation)
                }
            }
        };
        self.state = next_state;
        event
    }
}

fn highest_high(candles: &[Candle]) -> f64 {
    candles.iter().map(|c| c.high).fold(f64::MIN, f64::max)
}

/// The support-level reference computed purely from candles *after* the
/// surge ended — see `support_level`'s doc comment for why this can't be
/// a blanket MA spanning the surge itself. `None` below
/// `MIN_CANDLES_FOR_POST_SURGE_MA` candles (not enough of a sample to
/// trust as an average yet).
fn post_surge_ma(candles: &[Candle]) -> Option<f64> {
    if candles.len() < MIN_CANDLES_FOR_POST_SURGE_MA {
        return None;
    }
    let window = &candles[candles.len().saturating_sub(MA_PERIOD)..];
    Some(window.iter().map(|c| c.close).sum::<f64>() / window.len() as f64)
}

/// Shared by both the "still building toward confirmation" and "already
/// confirmed but this candle didn't break out" cases — checks the new
/// candle's own validity and either extends the consolidation, tolerates
/// a bad candle (up to `max_consecutive_invalid`), times out, or gives
/// up on this attempt back to watching for a fresh surge.
fn step_consolidation(
    candle: Candle,
    surge: SurgeInfo,
    mut candles: Vec<Candle>,
    was_confirmed: bool,
    consecutive_invalid: usize,
    thresholds: &ConsolidationThresholds,
) -> (State, ConsolidationBreakoutEvent) {
    let support = support_level(surge.low, post_surge_ma(&candles));

    if !is_valid_consolidation_candle(&candle, &surge, candles.last(), support, thresholds) {
        let strikes = consecutive_invalid + 1;
        if strikes > thresholds.max_consecutive_invalid {
            return (State::WatchingForSurge, ConsolidationBreakoutEvent::None);
        }
        // Tolerated — stay in TrackingConsolidation with the *same*
        // candles (this one doesn't count as part of the range), just
        // remember the strike.
        return (
            State::TrackingConsolidation { surge, candles, confirmed: was_confirmed, consecutive_invalid: strikes },
            ConsolidationBreakoutEvent::None,
        );
    }

    candles.push(candle);

    if candles.len() > thresholds.max_consolidation_candles {
        return (State::WatchingForSurge, ConsolidationBreakoutEvent::None);
    }

    let now_confirmed = was_confirmed || candles.len() >= thresholds.min_consolidation_candles;
    let event = if now_confirmed && !was_confirmed {
        ConsolidationBreakoutEvent::ConsolidationConfirmed { consolidation_high: highest_high(&candles), support }
    } else {
        ConsolidationBreakoutEvent::None
    };

    (State::TrackingConsolidation { surge, candles, confirmed: now_confirmed, consecutive_invalid: 0 }, event)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candle(open: f64, high: f64, low: f64, close: f64, volume: u64) -> Candle {
        Candle { open, high, low, close, volume }
    }

    fn quiet(n: usize) -> Vec<Candle> {
        (0..n).map(|_| candle(1.0, 1.01, 0.99, 1.0, 1000)).collect()
    }

    fn feed(monitor: &mut ConsolidationBreakoutMonitor, candles: &[Candle]) -> Vec<ConsolidationBreakoutEvent> {
        candles.iter().map(|c| monitor.on_candle(*c)).collect()
    }

    fn surge_candles() -> Vec<Candle> {
        vec![
            candle(1.00, 1.05, 1.00, 1.05, 5000),
            candle(1.05, 1.10, 1.04, 1.09, 6000),
            candle(1.09, 1.15, 1.08, 1.14, 7000),
            candle(1.14, 1.18, 1.13, 1.17, 5000),
            candle(1.17, 1.20, 1.16, 1.19, 4000),
        ]
    }

    #[test]
    fn full_pattern_fires_entry_triggered_on_a_real_breakout() {
        let mut monitor = ConsolidationBreakoutMonitor::new(ConsolidationBreakoutConfig::default());

        feed(&mut monitor, &quiet(20));
        let surge_events = feed(&mut monitor, &surge_candles());
        assert_eq!(surge_events[..4], [ConsolidationBreakoutEvent::None; 4]);
        assert!(matches!(surge_events[4], ConsolidationBreakoutEvent::SurgeDetected { .. }));

        // Tight, low-volume consolidation candles holding above support.
        let consolidation = vec![
            candle(1.19, 1.195, 1.17, 1.18, 1500),
            candle(1.18, 1.19, 1.16, 1.175, 1000),
        ];
        let consolidation_events = feed(&mut monitor, &consolidation);
        assert!(
            consolidation_events
                .iter()
                .any(|e| matches!(e, ConsolidationBreakoutEvent::ConsolidationConfirmed { .. })),
            "expected confirmation after {} valid candles, got {:?}",
            ConsolidationThresholds::default().min_consolidation_candles,
            consolidation_events
        );

        // Breaks back above the consolidation range's high (~1.195).
        let breakout_event = monitor.on_candle(candle(1.18, 1.22, 1.18, 1.21, 2000));
        assert_eq!(breakout_event, ConsolidationBreakoutEvent::EntryTriggered { price: 1.21 });
    }

    #[test]
    fn no_surge_means_no_consolidation_tracking_ever_starts() {
        let mut monitor = ConsolidationBreakoutMonitor::new(ConsolidationBreakoutConfig::default());
        let events = feed(&mut monitor, &quiet(60));
        assert!(events.iter().all(|e| *e == ConsolidationBreakoutEvent::None));
    }

    #[test]
    fn consolidation_gives_up_after_exceeding_the_consecutive_invalid_tolerance() {
        let mut monitor = ConsolidationBreakoutMonitor::new(ConsolidationBreakoutConfig::default());
        feed(&mut monitor, &quiet(20));
        feed(&mut monitor, &surge_candles());

        // Support (surge low ~1.00) broken 3 times in a row — exceeds
        // the default tolerance (max_consecutive_invalid=2), so the 3rd
        // strike finally gives up and resets to watching for a new surge.
        let bad = candle(1.19, 1.19, 0.90, 1.05, 1000);
        let events: Vec<_> = (0..3).map(|_| monitor.on_candle(bad)).collect();
        assert!(events.iter().all(|e| *e == ConsolidationBreakoutEvent::None));

        // A subsequent close above the old surge high should NOT fire
        // EntryTriggered — tracking was actually abandoned by the 3rd
        // strike; there's no confirmed consolidation to break out of.
        let next = monitor.on_candle(candle(1.05, 1.25, 1.04, 1.24, 900));
        assert_eq!(next, ConsolidationBreakoutEvent::None);
    }

    #[test]
    fn consolidation_tolerates_an_isolated_bad_candle_within_the_limit() {
        // Real premarket replay (COOT, 2026-08-31) found a single noisy
        // candle right after a surge shouldn't throw away an otherwise-
        // forming consolidation — this proves the tolerance actually
        // works, not just that it's configured.
        let mut monitor = ConsolidationBreakoutMonitor::new(ConsolidationBreakoutConfig::default());
        feed(&mut monitor, &quiet(20));
        feed(&mut monitor, &surge_candles());

        // One bad candle (support broken) — within the default
        // tolerance of 2, must NOT reset tracking.
        monitor.on_candle(candle(1.19, 1.19, 0.90, 1.05, 1000));

        // Genuinely valid candles right after should still be able to
        // build toward confirmation, proving the single bad tick didn't
        // wipe out the attempt.
        let first_good = monitor.on_candle(candle(1.19, 1.195, 1.17, 1.18, 1500));
        assert_eq!(first_good, ConsolidationBreakoutEvent::None); // valid, but only 1 so far

        let second_good = monitor.on_candle(candle(1.18, 1.19, 1.16, 1.175, 1000));
        assert!(matches!(second_good, ConsolidationBreakoutEvent::ConsolidationConfirmed { .. }));
    }

    #[test]
    fn consolidation_gives_up_after_max_candles_without_breaking_out() {
        let config = ConsolidationBreakoutConfig {
            consolidation: ConsolidationThresholds {
                min_consolidation_candles: 2,
                max_consolidation_candles: 3,
                ..ConsolidationThresholds::default()
            },
            ..ConsolidationBreakoutConfig::default()
        };
        let mut monitor = ConsolidationBreakoutMonitor::new(config);
        feed(&mut monitor, &quiet(20));
        feed(&mut monitor, &surge_candles());

        // 4 valid-but-never-breaking-out consolidation candles: the 2nd
        // reaches min_consolidation_candles=2 and confirms; the 4th
        // exceeds max_consolidation_candles=3 and resets silently (no
        // event — a timeout isn't itself a signal). `low == close` (no
        // lower wick) deliberately, so this stays valid even once the
        // post-surge MA activates at 3 candles — isolates the
        // max_consolidation_candles mechanism from support specifically,
        // which has its own dedicated tests.
        let flat = candle(1.18, 1.19, 1.18, 1.18, 900);
        let events: Vec<_> = (0..4).map(|_| monitor.on_candle(flat)).collect();
        assert_eq!(events[0], ConsolidationBreakoutEvent::None);
        assert!(matches!(events[1], ConsolidationBreakoutEvent::ConsolidationConfirmed { .. }));
        assert_eq!(events[2], ConsolidationBreakoutEvent::None);
        assert_eq!(events[3], ConsolidationBreakoutEvent::None);

        // Now back in WatchingForSurge — an old-surge-range breakout
        // shouldn't fire since consolidation tracking was abandoned.
        let after = monitor.on_candle(candle(1.18, 1.30, 1.18, 1.29, 900));
        assert_eq!(after, ConsolidationBreakoutEvent::None);
    }
}
