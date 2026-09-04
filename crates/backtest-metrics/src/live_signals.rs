//! Live counterpart to `signals::extract_signals` — that module turns a
//! finished `replay_engine::ReplayResult` (the whole session's history,
//! already collected) into discrete signal moments. This one does the
//! same *edge-triggering* job, but incrementally, one real
//! `market_data::ScanEvent` at a time as they arrive off the live
//! broadcast (`ws-server`'s own `events` channel — see that binary's
//! `main.rs` for the subscriber wiring), since there's no finished
//! result to scan over live.
//!
//! Same methodology as `signals.rs`, deliberately kept in sync so a live
//! hit-rate number and a backtest hit-rate number are actually
//! comparable, not two different definitions of "a signal fired":
//! funnel/momentum are edge-triggered (only the flip into
//! passed/qualifying counts), ignition only counts a confirmed
//! follow-through, and consolidation-breakout/micropullback only count
//! the actual breakout entry — `SurgeDetected`/`ConsolidationConfirmed`
//! are diagnostic-only, same as `signals.rs`'s own reasoning.
//!
//! Real, structural difference from `signals.rs::SignalMoment`: a live
//! signal doesn't have an outcome yet (there's no future to look up —
//! that's the whole point of tracking it live). `PendingSignal` is the
//! not-yet-evaluated shape; the `live_efficiency` binary is what later
//! fills in a real `SignalOutcome` from real subsequent price action and
//! promotes it into a `LoggedSignal`, the exact same struct backtests
//! already produce — so `metrics::aggregate_by_strategy` works
//! identically over live and backtest data, no separate code path.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use market_data::events::{ConsolidationEventKind, ConsolidationStrategy, IgnitionEventKind, ScanEvent};
use serde::{Deserialize, Serialize};

use crate::signals::Strategy;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingSignal {
    pub symbol: String,
    pub strategy: Strategy,
    pub timestamp: DateTime<Utc>,
    pub signal_price: f64,
    /// When this signal was actually captured live — distinct from
    /// `timestamp` (the market event's own time) the same way
    /// `log::LoggedSignal::logged_at` is, though for a live signal the
    /// two are normally within a second of each other (unlike a
    /// backtest replay, which can log a signal from hours/days in the
    /// past all at once).
    pub captured_at: DateTime<Utc>,
}

/// Incremental, per-symbol edge-trigger state — the live equivalent of
/// `extract_signals`'s two local `bool`s, just keyed by symbol since a
/// live tracker watches every symbol at once instead of one replay
/// result for a single symbol at a time.
#[derive(Debug, Default)]
pub struct LiveSignalTracker {
    funnel_qualified: HashMap<String, bool>,
    momentum_qualified: HashMap<String, bool>,
    /// Needed because `ScanEvent::MomentumUpdate` (unlike FunnelSignal/
    /// IgnitionEvent/ConsolidationEvent) carries no price of its own —
    /// see that variant's fields in `market_data::events`. Tracked from
    /// the most recent `BarUpdate` for the same symbol, which is sent
    /// alongside every other event for a tracked symbol (see
    /// `ScanEvent::BarUpdate`'s own doc comment).
    last_price: HashMap<String, f64>,
}

impl LiveSignalTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one real event through the tracker. Returns a `PendingSignal`
    /// only on the exact moment a strategy's own edge-trigger condition
    /// fires — most events (every bar, every trade) return `None`, same
    /// low signal-to-noise ratio `extract_signals` has for a replay.
    pub fn on_event(&mut self, event: &ScanEvent, now: DateTime<Utc>) -> Option<PendingSignal> {
        match event {
            ScanEvent::BarUpdate { symbol, close, .. } => {
                self.last_price.insert(symbol.clone(), *close);
                None
            }
            ScanEvent::FunnelSignal { symbol, timestamp, price, passed, .. } => {
                let was_passed = self.funnel_qualified.insert(symbol.clone(), *passed).unwrap_or(false);
                if *passed && !was_passed {
                    Some(PendingSignal {
                        symbol: symbol.clone(),
                        strategy: Strategy::FastFunnel,
                        timestamp: *timestamp,
                        signal_price: *price,
                        captured_at: now,
                    })
                } else {
                    None
                }
            }
            ScanEvent::MomentumUpdate { symbol, timestamp, qualifies, .. } => {
                let was_qualified = self.momentum_qualified.insert(symbol.clone(), *qualifies).unwrap_or(false);
                if *qualifies && !was_qualified {
                    // No price on this event itself -- fall back to the
                    // most recent BarUpdate close for this symbol. If
                    // none has arrived yet (the very first bar a symbol
                    // is tracked), the signal is skipped rather than
                    // logged with a fabricated price -- a real, minor,
                    // honestly-documented gap: a symbol that happens to
                    // qualify on momentum on its very first bar won't be
                    // captured until it re-qualifies later with a known
                    // price. Rare in practice (RollingWindow needs
                    // several candles before it can score at all, so a
                    // BarUpdate has almost always already arrived by
                    // then), not worth a fabricated price to close.
                    self.last_price.get(symbol).map(|&price| PendingSignal {
                        symbol: symbol.clone(),
                        strategy: Strategy::MomentumScorer,
                        timestamp: *timestamp,
                        signal_price: price,
                        captured_at: now,
                    })
                } else {
                    None
                }
            }
            ScanEvent::IgnitionEvent { symbol, timestamp, price, kind } => {
                if *kind == IgnitionEventKind::FollowThroughConfirmed {
                    Some(PendingSignal {
                        symbol: symbol.clone(),
                        strategy: Strategy::IgnitionDetector,
                        timestamp: *timestamp,
                        signal_price: *price,
                        captured_at: now,
                    })
                } else {
                    None
                }
            }
            ScanEvent::ConsolidationEvent { symbol, timestamp, price, kind, strategy } => {
                if *kind != ConsolidationEventKind::EntryTriggered {
                    return None;
                }
                let mapped = match strategy {
                    ConsolidationStrategy::ConsolidationBreakout => Strategy::ConsolidationBreakout,
                    ConsolidationStrategy::Micropullback => Strategy::Micropullback,
                };
                Some(PendingSignal {
                    symbol: symbol.clone(),
                    strategy: mapped,
                    timestamp: *timestamp,
                    signal_price: *price,
                    captured_at: now,
                })
            }
            // Halt warnings and catalyst tags aren't "entry" signals
            // with a target/stop outcome the same way the five
            // strategies above are -- see this module's own doc comment
            // on what "detection efficiency" was scoped to track first.
            ScanEvent::HaltWarning { .. } | ScanEvent::CatalystUpdate { .. } => None,
        }
    }
}

/// Appends newly-captured pending signals — same append-only JSONL
/// pattern as `log::append`, deliberately not sharing its code since the
/// two structs differ (`PendingSignal` has no `outcome` yet), but kept
/// byte-for-byte consistent in shape (create-if-missing, one JSON object
/// per line).
pub fn append_pending(path: &Path, entries: &[PendingSignal]) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating directory {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {} for append", path.display()))?;
    for entry in entries {
        let line = serde_json::to_string(entry).context("serializing pending signal")?;
        writeln!(file, "{line}").context("writing to live pending-signal log")?;
    }
    Ok(())
}

/// Missing file reads as empty, not an error — same convention as
/// `log::read_all` (nothing captured yet is a valid starting state, not
/// a broken one).
pub fn read_pending(path: &Path) -> Result<Vec<PendingSignal>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for (i, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("reading line {} of {}", i + 1, path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: PendingSignal = serde_json::from_str(&line).with_context(|| format!("parsing line {} of {}", i + 1, path.display()))?;
        out.push(entry);
    }
    Ok(out)
}

/// Full overwrite (not append) — what `live_efficiency` uses after
/// evaluating a batch of pending signals, to remove exactly the ones it
/// just promoted into the evaluated log without disturbing ones still
/// too young to evaluate. Acceptable at this scale (real signal counts
/// per session are dozens-to-low-hundreds, not the per-trade volume
/// halt_warning produces — same reasoning `log.rs`'s own doc comment
/// already gives for flat-file JSONL over a real database here).
pub fn write_pending(path: &Path, entries: &[PendingSignal]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating directory {}", parent.display()))?;
    }
    let mut file = std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    for entry in entries {
        let line = serde_json::to_string(entry).context("serializing pending signal")?;
        writeln!(file, "{line}").context("writing to live pending-signal log")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 + secs, 0).unwrap()
    }

    fn funnel(symbol: &str, timestamp: DateTime<Utc>, price: f64, passed: bool) -> ScanEvent {
        ScanEvent::FunnelSignal {
            symbol: symbol.to_string(),
            timestamp,
            price,
            gap_pct: 10.0,
            session_volume: 100_000,
            price_ok: passed,
            float_ok: passed,
            rel_vol_ok: passed,
            gap_ok: passed,
            passed,
        }
    }

    fn momentum(symbol: &str, timestamp: DateTime<Utc>, qualifies: bool) -> ScanEvent {
        ScanEvent::MomentumUpdate {
            symbol: symbol.to_string(),
            timestamp,
            volume_confirmation: 0.9,
            structure: 0.9,
            ma_slope: 0.9,
            wick_rejection: 0.9,
            overall: if qualifies { 0.9 } else { 0.1 },
            qualifies,
        }
    }

    fn bar_update(symbol: &str, timestamp: DateTime<Utc>, close: f64) -> ScanEvent {
        ScanEvent::BarUpdate { symbol: symbol.to_string(), timestamp, open: close, high: close, low: close, close, volume: 1000, interval_secs: 60 }
    }

    #[test]
    fn funnel_signal_is_edge_triggered_across_events_not_repeated() {
        let mut tracker = LiveSignalTracker::new();
        let now = ts(1000);
        assert!(tracker.on_event(&funnel("SWVL", ts(0), 3.0, true), now).is_some());
        // Still passed on the next bar -- must NOT re-signal.
        assert!(tracker.on_event(&funnel("SWVL", ts(60), 3.1, true), now).is_none());
        assert!(tracker.on_event(&funnel("SWVL", ts(120), 3.2, true), now).is_none());
    }

    #[test]
    fn funnel_signal_fires_again_after_dropping_and_requalifying() {
        let mut tracker = LiveSignalTracker::new();
        let now = ts(1000);
        assert!(tracker.on_event(&funnel("SWVL", ts(0), 3.0, true), now).is_some());
        assert!(tracker.on_event(&funnel("SWVL", ts(60), 3.0, false), now).is_none());
        assert!(tracker.on_event(&funnel("SWVL", ts(120), 3.0, true), now).is_some());
    }

    #[test]
    fn unrelated_symbols_track_independent_funnel_edges() {
        let mut tracker = LiveSignalTracker::new();
        let now = ts(1000);
        assert!(tracker.on_event(&funnel("SWVL", ts(0), 3.0, true), now).is_some());
        // AEHL's first-ever reading is also a real edge (false -> true),
        // must not be suppressed by SWVL's already-passed state.
        assert!(tracker.on_event(&funnel("AEHL", ts(0), 6.0, true), now).is_some());
    }

    #[test]
    fn momentum_signal_uses_the_most_recent_bar_update_price() {
        let mut tracker = LiveSignalTracker::new();
        let now = ts(1000);
        tracker.on_event(&bar_update("SWVL", ts(0), 3.25), now);
        let signal = tracker.on_event(&momentum("SWVL", ts(60), true), now).expect("should emit a signal");
        assert_eq!(signal.strategy, Strategy::MomentumScorer);
        assert_eq!(signal.signal_price, 3.25);
    }

    #[test]
    fn momentum_signal_is_skipped_without_a_known_price_yet() {
        // Real, documented gap (see on_event's own comment) -- no
        // BarUpdate has arrived for this symbol yet, so there's no price
        // to log a signal against.
        let mut tracker = LiveSignalTracker::new();
        let now = ts(1000);
        assert!(tracker.on_event(&momentum("SWVL", ts(0), true), now).is_none());
    }

    #[test]
    fn momentum_signal_is_edge_triggered_same_as_funnel() {
        let mut tracker = LiveSignalTracker::new();
        let now = ts(1000);
        tracker.on_event(&bar_update("SWVL", ts(0), 3.0), now);
        assert!(tracker.on_event(&momentum("SWVL", ts(60), true), now).is_some());
        assert!(tracker.on_event(&momentum("SWVL", ts(120), true), now).is_none());
    }

    #[test]
    fn ignition_candidate_opened_does_not_signal_only_confirmed_follow_through_does() {
        let mut tracker = LiveSignalTracker::new();
        let now = ts(1000);
        let opened = ScanEvent::IgnitionEvent { symbol: "SWVL".to_string(), timestamp: ts(0), price: 3.0, kind: IgnitionEventKind::CandidateOpened };
        assert!(tracker.on_event(&opened, now).is_none());
        let confirmed = ScanEvent::IgnitionEvent { symbol: "SWVL".to_string(), timestamp: ts(30), price: 3.05, kind: IgnitionEventKind::FollowThroughConfirmed };
        let signal = tracker.on_event(&confirmed, now).expect("confirmed follow-through should signal");
        assert_eq!(signal.strategy, Strategy::IgnitionDetector);
    }

    #[test]
    fn consolidation_events_only_signal_on_entry_triggered_not_surge_or_confirmed() {
        let mut tracker = LiveSignalTracker::new();
        let now = ts(1000);
        let surge = ScanEvent::ConsolidationEvent {
            symbol: "SWVL".to_string(), timestamp: ts(0), price: 3.0,
            kind: ConsolidationEventKind::SurgeDetected, strategy: ConsolidationStrategy::ConsolidationBreakout,
        };
        assert!(tracker.on_event(&surge, now).is_none());
        let entry = ScanEvent::ConsolidationEvent {
            symbol: "SWVL".to_string(), timestamp: ts(60), price: 3.1,
            kind: ConsolidationEventKind::EntryTriggered, strategy: ConsolidationStrategy::ConsolidationBreakout,
        };
        let signal = tracker.on_event(&entry, now).expect("entry_triggered should signal");
        assert_eq!(signal.strategy, Strategy::ConsolidationBreakout);
    }

    #[test]
    fn micropullback_entries_are_tagged_as_their_own_distinct_strategy() {
        let mut tracker = LiveSignalTracker::new();
        let now = ts(1000);
        let entry = ScanEvent::ConsolidationEvent {
            symbol: "SWVL".to_string(), timestamp: ts(60), price: 3.1,
            kind: ConsolidationEventKind::EntryTriggered, strategy: ConsolidationStrategy::Micropullback,
        };
        let signal = tracker.on_event(&entry, now).expect("entry_triggered should signal");
        assert_eq!(signal.strategy, Strategy::Micropullback);
    }

    #[test]
    fn halt_warning_and_catalyst_update_never_signal() {
        let mut tracker = LiveSignalTracker::new();
        let now = ts(1000);
        let halt = ScanEvent::HaltWarning {
            symbol: "SWVL".to_string(), timestamp: ts(0), reference_price: 3.0, current_price: 3.5,
            band_width_dollars: 0.5, band_doubled: false, proximity_ratio: 1.0, relative_volume: Some(5.0),
            level: market_data::events::HaltAlertLevel::Red,
        };
        assert!(tracker.on_event(&halt, now).is_none());
        let catalyst = ScanEvent::CatalystUpdate {
            symbol: "SWVL".to_string(), timestamp: ts(0), catalyst_tags: vec!["earnings".to_string()],
            headline_count: 1, most_recent_headline: None,
        };
        assert!(tracker.on_event(&catalyst, now).is_none());
    }

    fn sample_pending(symbol: &str, price: f64) -> PendingSignal {
        PendingSignal { symbol: symbol.to_string(), strategy: Strategy::FastFunnel, timestamp: ts(0), signal_price: price, captured_at: ts(1) }
    }

    #[test]
    fn missing_pending_file_reads_as_empty_not_an_error() {
        let path = std::env::temp_dir().join(format!("stockspotter-test-pending-missing-{}.jsonl", std::process::id()));
        assert!(read_pending(&path).unwrap().is_empty());
    }

    #[test]
    fn append_pending_then_read_all_round_trips() {
        let path = std::env::temp_dir().join(format!("stockspotter-test-pending-roundtrip-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        append_pending(&path, &[sample_pending("SWVL", 1.0), sample_pending("AEHL", 2.0)]).unwrap();
        append_pending(&path, &[sample_pending("NCRA", 3.0)]).unwrap(); // append doesn't clobber
        let all = read_pending(&path).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[2].symbol, "NCRA");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn write_pending_overwrites_rather_than_appends() {
        // The real behavior live_efficiency relies on: after evaluating
        // a batch, it rewrites the file with only the still-pending
        // (not-yet-evaluable) entries -- must actually replace the file
        // contents, not add to them.
        let path = std::env::temp_dir().join(format!("stockspotter-test-pending-overwrite-{}.jsonl", std::process::id()));
        append_pending(&path, &[sample_pending("SWVL", 1.0), sample_pending("AEHL", 2.0)]).unwrap();
        write_pending(&path, &[sample_pending("ONLY_THIS_ONE", 5.0)]).unwrap();
        let all = read_pending(&path).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].symbol, "ONLY_THIS_ONE");
        std::fs::remove_file(&path).unwrap();
    }
}
