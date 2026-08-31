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
/// the doc names for the support-level reference.
const MA_PERIOD: usize = 9;

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
        let cap = surge_window_needed.max(MA_PERIOD);
        self.recent.push_back(candle);
        while self.recent.len() > cap {
            self.recent.pop_front();
        }
        let ma9 = self.moving_average(MA_PERIOD);

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
                            State::TrackingConsolidation { surge, candles: Vec::new(), confirmed: false },
                            ConsolidationBreakoutEvent::SurgeDetected { low: surge.low, high: surge.high },
                        ),
                        None => (State::WatchingForSurge, ConsolidationBreakoutEvent::None),
                    }
                } else {
                    (State::WatchingForSurge, ConsolidationBreakoutEvent::None)
                }
            }
            State::TrackingConsolidation { surge, candles, confirmed } => {
                let support = support_level(surge.low, ma9.unwrap_or(surge.low));

                if confirmed {
                    let consolidation_high = highest_high(&candles);
                    if breakout_triggered(&candle, consolidation_high) {
                        (State::WatchingForSurge, ConsolidationBreakoutEvent::EntryTriggered { price: candle.close })
                    } else {
                        step_consolidation(candle, surge, candles, confirmed, support, &self.config.consolidation)
                    }
                } else {
                    step_consolidation(candle, surge, candles, confirmed, support, &self.config.consolidation)
                }
            }
        };
        self.state = next_state;
        event
    }

    fn moving_average(&self, period: usize) -> Option<f64> {
        if self.recent.len() < period {
            return None;
        }
        let sum: f64 = self.recent.iter().rev().take(period).map(|c| c.close).sum();
        Some(sum / period as f64)
    }
}

fn highest_high(candles: &[Candle]) -> f64 {
    candles.iter().map(|c| c.high).fold(f64::MIN, f64::max)
}

/// Shared by both the "still building toward confirmation" and "already
/// confirmed but this candle didn't break out" cases — checks the new
/// candle's own validity and either extends the consolidation, times it
/// out, or invalidates it back to watching.
fn step_consolidation(
    candle: Candle,
    surge: SurgeInfo,
    mut candles: Vec<Candle>,
    was_confirmed: bool,
    support: f64,
    thresholds: &ConsolidationThresholds,
) -> (State, ConsolidationBreakoutEvent) {
    if !is_valid_consolidation_candle(&candle, &surge, candles.last(), support, thresholds) {
        return (State::WatchingForSurge, ConsolidationBreakoutEvent::None);
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

    (State::TrackingConsolidation { surge, candles, confirmed: now_confirmed }, event)
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
    fn consolidation_invalidates_when_support_breaks_and_resets_to_watching() {
        let mut monitor = ConsolidationBreakoutMonitor::new(ConsolidationBreakoutConfig::default());
        feed(&mut monitor, &quiet(20));
        feed(&mut monitor, &surge_candles());

        // Support (surge low ~1.00) breaks — should invalidate, not confirm.
        let event = monitor.on_candle(candle(1.19, 1.19, 0.90, 1.05, 1000));
        assert_eq!(event, ConsolidationBreakoutEvent::None);

        // A subsequent close above the old surge high should NOT fire
        // EntryTriggered — there's no confirmed consolidation to break
        // out of anymore; the monitor is back to watching for a new surge.
        let next = monitor.on_candle(candle(1.05, 1.25, 1.04, 1.24, 900));
        assert_eq!(next, ConsolidationBreakoutEvent::None);
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
        // event — a timeout isn't itself a signal).
        let flat = candle(1.18, 1.185, 1.17, 1.18, 900);
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
