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
use chrono::{DateTime, Utc};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JournalEntry {
    Entered {
        symbol: String,
        entry_price: f64,
        qty: u64,
        position_size_usd: f64,
        target_price: f64,
        stop_price: f64,
        entered_at: DateTime<Utc>,
        momentum_overall: f64,
        momentum_volume_confirmation: f64,
    },
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
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    TargetHit,
    StopHit,
    Timeout,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    MomentumGateFailed,
    OutsideRegularHours,
    MaxConcurrentPositions,
    AlreadyEnteredToday,
    ZeroQuantity,
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
                entry_price: 3.12,
                qty: 160,
                position_size_usd: 500.0,
                target_price: 3.1824,
                stop_price: 3.0576,
                entered_at: ts(),
                momentum_overall: 0.72,
                momentum_volume_confirmation: 0.68,
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
}
