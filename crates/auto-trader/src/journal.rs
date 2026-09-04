//! Append-only audit log of every decision the engine makes — entries,
//! exits, AND skips (so this reads as a genuinely honest trade journal
//! that explains inaction, not just wins, matching this project's
//! established "fail-open but log honestly" ethos — the movers-scan
//! fail-open logging in `market_data::live` is the existing precedent).
//!
//! JSONL, mirrors `backtest_metrics::live_signals::append_pending`'s
//! exact pattern byte-for-byte: create-if-missing, one JSON object per
//! line, `std::fs` (not `tokio::fs`), errors propagated via `anyhow`
//! `Context` rather than silently swallowed — this is the house style,
//! not a new one.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use backtest_metrics::Strategy;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// `Deserialize` (added alongside crates/ws-server's new status endpoint)
// is new here for the same reason ScanEvent gained it earlier tonight --
// nothing before this had ever needed to read this type back out of its
// own wire JSON, only append it.
//
// Real bug fix, also new here: `rename_all = "snake_case"` on the enum
// only renames the variant TAGS ("entered"/"exited"/"skipped"), not the
// fields inside each struct variant -- the exact regression this project
// already hit once for ScanEvent (see events.rs's own
// funnel_signal_serializes_with_camel_case_fields comment). Without a
// per-variant `rename_all = "camelCase"`, every field here would have
// serialized as snake_case (entry_price, not entryPrice), inconsistent
// with every other wire response in this codebase. Caught and fixed
// before any frontend code was written against it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JournalEntry {
    #[serde(rename_all = "camelCase")]
    Entered {
        symbol: String,
        /// Which real trigger opened this position (v3, 2026-09-04,
        /// Roman's own ask to broaden past Micropullback-only) --
        /// `Strategy` already has its own Serialize impl (PascalCase,
        /// e.g. "IgnitionDetector"), reused as-is rather than a second,
        /// journal-local copy of the same five names.
        strategy: Strategy,
        entry_price: f64,
        qty: u64,
        position_size_usd: f64,
        target_price: f64,
        stop_price: f64,
        entered_at: DateTime<Utc>,
        momentum_overall: f64,
        momentum_volume_confirmation: f64,
        /// Real transparency on what backed the move, not a gate --
        /// empty if no catalyst is known for this symbol yet. Added
        /// 2026-09-04 alongside the halt-risk/momentum-deterioration
        /// context-awareness pass.
        catalyst_tags: Vec<String>,
    },
    #[serde(rename_all = "camelCase")]
    Exited {
        symbol: String,
        exit_price: f64,
        exit_reason: ExitReason,
        pnl_usd: f64,
        pnl_pct: f64,
        qty: u64,
        entered_at: DateTime<Utc>,
        exited_at: DateTime<Utc>,
    },
    #[serde(rename_all = "camelCase")]
    Skipped {
        symbol: String,
        reason: SkipReason,
        at: DateTime<Utc>,
        /// Free-form context for the reason (e.g. the actual momentum
        /// values, or "no data yet") — kept as a single string rather
        /// than a per-reason struct so this stays one flat, easy-to-grep
        /// journal shape instead of a five-way enum-of-structs.
        detail: String,
    },
    /// The trailing stop actually ratcheting up (2026-09-04, Roman's own
    /// ask: "stop losses should move up as price and bullish candle
    /// momentum continue developing"). Only emitted on a real increase,
    /// not every bar -- most bars for an open position don't make a new
    /// high, so this stays a meaningful "something happened" line, same
    /// edge-triggered spirit as the rest of this project's own logging
    /// discipline (e.g. live.rs's halt-level edge trigger). Exists so
    /// ws-server's /auto-trader/status stays honest about the CURRENT
    /// stop on an open position instead of showing the stale entry-time
    /// value forever.
    #[serde(rename_all = "camelCase")]
    StopAdjusted {
        symbol: String,
        previous_stop_price: f64,
        new_stop_price: f64,
        trigger_price: f64,
        at: DateTime<Utc>,
    },
    /// A strategy's trigger got enabled or disabled by real, evidence-
    /// driven review (2026-09-05, v4 -- Roman: "This sounds like the
    /// direction we want to go" after asking whether the auto-trader was
    /// "still learning"). `sample_size`/`expectancy_pct` are the actual
    /// numbers `backtest_metrics::decide_enabled_strategies` used to make
    /// the call -- this line IS the audit trail for a decision the
    /// engine now makes about itself, not just a passive log of a
    /// human's edit.
    #[serde(rename_all = "camelCase")]
    StrategyConfigChanged {
        strategy: Strategy,
        enabled: bool,
        sample_size: usize,
        expectancy_pct: Option<f64>,
        at: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    TargetHit,
    StopHit,
    Timeout,
    /// Real momentum, not just price, breaking down (overall < 0.4, the
    /// existing "critical" tier boundary MomentumScoreRow.tsx already
    /// uses on both frontends) -- cutting the trade before the trailing
    /// stop eventually catches up, real risk reduction per Roman's own
    /// "risk should be at a minimum" ask.
    MomentumDeteriorated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    MomentumGateFailed,
    OutsideRegularHours,
    MaxConcurrentPositions,
    AlreadyEnteredToday,
    ZeroQuantity,
    /// The symbol's latest known halt-proximity level is Amber or Red --
    /// don't open a fresh position on something already heating toward a
    /// halt band. Missing halt data (no HaltWarning seen yet for this
    /// symbol) does NOT trigger this -- fails open, matching this
    /// project's own established fail-open convention elsewhere.
    HaltRiskTooHigh,
    /// This strategy's trigger is currently turned off by real, evidence-
    /// driven review (2026-09-05, v4) -- see `JournalEntry::
    /// StrategyConfigChanged` for the decision that set this, and
    /// `backtest_metrics::decide_enabled_strategies` for the rule itself.
    StrategyDisabled,
}

/// Reads and parses every line of the journal — used once at startup
/// (2026-09-04, a real gap found live: the engine's own closed-trade
/// history and today's-entries dedup silently reset on every process
/// restart, see `Engine::seed_from_history`'s own doc comment for why
/// that's a real problem, not a cosmetic one) to rebuild the parts of
/// engine state that should survive a restart. A missing file returns an
/// empty history (nothing to seed from yet, e.g. a genuinely fresh
/// deploy), not an error — same convention `append`'s own create-if-
/// missing behavior already establishes for this file. An individual
/// unparseable line is silently skipped rather than failing the whole
/// read — same resilience idiom `ws-server`'s own `read_journal` already
/// uses for this identical file, just without a `tracing` dependency
/// this otherwise-minimal-deps module doesn't otherwise need.
pub fn read_all(path: &Path) -> Result<Vec<JournalEntry>> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut entries = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<JournalEntry>(line) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

/// Appends `entry` as one line to `path`, creating the file (and its
/// parent directory) if it doesn't exist yet. Matches
/// `live_signals::append_pending` exactly: synchronous `std::fs`, not
/// `tokio::fs` — this only ever runs from the single-threaded decision
/// path, never a hot loop, so blocking I/O here is a deliberate,
/// consistent choice, not an oversight.
pub fn append(path: &Path, entry: &JournalEntry) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating directory {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {} for append", path.display()))?;
    let line = serde_json::to_string(entry).context("serializing journal entry")?;
    writeln!(file, "{line}").context("writing to auto-trader journal")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile_like_dir::temp_journal_path;

    // No `tempfile` crate dependency anywhere else in this workspace, so
    // this small local helper (rather than pulling in a new dep for one
    // test module) creates a throwaway path under the OS temp dir and
    // cleans it up on drop.
    mod tempfile_like_dir {
        use std::path::PathBuf;

        pub struct TempJournalPath(pub PathBuf);
        impl Drop for TempJournalPath {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }

        pub fn temp_journal_path(name: &str) -> TempJournalPath {
            let mut path = std::env::temp_dir();
            path.push(format!("auto_trader_journal_test_{name}_{}.jsonl", std::process::id()));
            TempJournalPath(path)
        }
    }

    fn ts() -> DateTime<Utc> {
        chrono::Utc::now()
    }

    #[test]
    fn appends_create_if_missing_one_json_object_per_line() {
        let temp = temp_journal_path("append_basic");
        append(
            &temp.0,
            &JournalEntry::Skipped {
                symbol: "SWVL".to_string(),
                reason: SkipReason::OutsideRegularHours,
                at: ts(),
                detail: "premarket".to_string(),
            },
        )
        .unwrap();
        append(
            &temp.0,
            &JournalEntry::Entered {
                symbol: "SWVL".to_string(),
                strategy: Strategy::Micropullback,
                entry_price: 3.12,
                qty: 160,
                position_size_usd: 500.0,
                target_price: 3.1824,
                stop_price: 3.0576,
                entered_at: ts(),
                momentum_overall: 0.72,
                momentum_volume_confirmation: 0.68,
                catalyst_tags: vec![],
            },
        )
        .unwrap();

        let content = std::fs::read_to_string(&temp.0).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains(r#""type":"skipped""#));
        assert!(lines[1].contains(r#""type":"entered""#));
        // Each line parses as its own standalone JSON object.
        for line in &lines {
            serde_json::from_str::<serde_json::Value>(line).unwrap();
        }
    }

    #[test]
    fn entered_fields_serialize_as_camel_case_not_snake_case() {
        // Regression: rename_all on the enum itself only renames variant
        // tags -- without the per-variant rename_all this project already
        // got bitten by once (events.rs), every field here would have
        // silently come out snake_case.
        let entry = JournalEntry::Entered {
            symbol: "SWVL".to_string(),
            strategy: Strategy::IgnitionDetector,
            entry_price: 3.12,
            qty: 160,
            position_size_usd: 500.0,
            target_price: 3.1824,
            stop_price: 3.0576,
            entered_at: ts(),
            momentum_overall: 0.72,
            momentum_volume_confirmation: 0.68,
            catalyst_tags: vec!["earnings".to_string()],
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains(r#""strategy":"IgnitionDetector""#));
        assert!(json.contains(r#""entryPrice":3.12"#));
        assert!(json.contains(r#""positionSizeUsd":500.0"#));
        assert!(json.contains(r#""targetPrice":3.1824"#));
        assert!(json.contains(r#""momentumVolumeConfirmation":0.68"#));
        assert!(json.contains(r#""catalystTags":["earnings"]"#));
        assert!(!json.contains("entry_price"));
        assert!(!json.contains("position_size_usd"));
    }

    #[test]
    fn stop_adjusted_fields_serialize_as_camel_case_and_round_trip() {
        let entry = JournalEntry::StopAdjusted {
            symbol: "SWVL".to_string(),
            previous_stop_price: 2.94,
            new_stop_price: 3.00,
            trigger_price: 3.06,
            at: ts(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains(r#""type":"stop_adjusted""#));
        assert!(json.contains(r#""previousStopPrice":2.94"#));
        assert!(json.contains(r#""newStopPrice":3.0"#));
        assert!(json.contains(r#""triggerPrice":3.06"#));
        assert!(!json.contains("previous_stop_price"));

        let parsed: JournalEntry = serde_json::from_str(&json).unwrap();
        match parsed {
            JournalEntry::StopAdjusted { new_stop_price, .. } => assert_eq!(new_stop_price, 3.00),
            other => panic!("expected StopAdjusted, got {other:?}"),
        }
    }

    #[test]
    fn journal_entry_round_trips_through_deserialize() {
        let original = JournalEntry::Exited {
            symbol: "SWVL".to_string(),
            exit_price: 3.18,
            exit_reason: ExitReason::TargetHit,
            pnl_usd: 9.60,
            pnl_pct: 2.0,
            qty: 160,
            entered_at: ts(),
            exited_at: ts(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: JournalEntry = serde_json::from_str(&json).unwrap();
        match parsed {
            JournalEntry::Exited { symbol, exit_reason, pnl_usd, .. } => {
                assert_eq!(symbol, "SWVL");
                assert_eq!(exit_reason, ExitReason::TargetHit);
                assert_eq!(pnl_usd, 9.60);
            }
            other => panic!("expected Exited, got {other:?}"),
        }
    }
}
