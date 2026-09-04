//! Computes the auto-trader monitoring view (running stats + open
//! positions + recent activity) by reading the shared JSONL journal
//! `crates/auto-trader` writes to — this `ws` container already has the
//! same `../../data:/app/data` volume mounted (same one
//! `backtest_metrics::live_signals` uses for its own JSONL logs, see
//! `main.rs`'s `LIVE_PENDING_SIGNALS_PATH`), so no new mount or network
//! hop is needed. `auto-trader` itself deliberately has no HTTP server of
//! its own — this is a "read shared state" precedent, the same shape
//! `/movers/today`/`/catalysts/today` already use, just substituting a
//! shared file for a shared in-process `Arc<RwLock<..>>`.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;
use auto_trader::journal::JournalEntry;
use chrono::{DateTime, Utc};
use serde::Serialize;
use tracing::warn;

/// Relative to this process's CWD (`/app` in the container) — the exact
/// same relative path `auto-trader`'s own `AUTO_TRADER_JOURNAL_PATH`
/// default resolves to inside its own container, since both share the
/// same host directory bind-mounted at the same container path. Not an
/// env-configurable override (matches `LIVE_PENDING_SIGNALS_PATH`'s own
/// plain-const precedent in this same file's sibling).
pub const AUTO_TRADER_JOURNAL_PATH: &str = "data/auto_trader_journal.jsonl";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoTraderStatusOut {
    pub trades: u32,
    pub wins: u32,
    pub losses: u32,
    pub cumulative_pnl_usd: f64,
    pub open_positions: Vec<OpenPositionOut>,
    /// Newest first — a monitoring feed reads top-to-bottom as "what just
    /// happened", not chronologically forward.
    pub recent_entries: Vec<JournalEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenPositionOut {
    pub symbol: String,
    pub entry_price: f64,
    pub qty: u64,
    pub entered_at: DateTime<Utc>,
    pub target_price: f64,
    pub stop_price: f64,
}

/// Reads and parses every line, skipping (and warning on) any line that
/// fails to parse rather than failing the whole request — same
/// resilience idiom `auto_trader::client`'s own WS-message parsing
/// already uses ("ignore what we don't understand, don't take the whole
/// thing down over it"). A missing file is a valid starting state
/// (nothing captured yet, e.g. outside trading hours before the first
/// entry), not an error — same convention
/// `backtest_metrics::live_signals::read_pending` already established.
pub async fn read_journal(path: &Path) -> anyhow::Result<Vec<JournalEntry>> {
    let content = match tokio::fs::read_to_string(path).await {
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
        match serde_json::from_str::<JournalEntry>(line) {
            Ok(entry) => entries.push(entry),
            Err(e) => warn!(error = %e, "auto-trader status: skipping an unparseable journal line"),
        }
    }
    Ok(entries)
}

/// Pure, testable: a single pass over the journal in file order.
/// `Entered` opens a position; a later `Exited` for the same symbol
/// closes it and folds into the running totals — same semantics
/// `auto_trader::Engine` already keeps in memory, just recomputed here
/// from its own audit trail instead of live process state.
pub fn compute_status(entries: &[JournalEntry], recent_limit: usize) -> AutoTraderStatusOut {
    let mut open: HashMap<String, OpenPositionOut> = HashMap::new();
    let mut trades = 0u32;
    let mut wins = 0u32;
    let mut losses = 0u32;
    let mut cumulative_pnl_usd = 0.0;

    for entry in entries {
        match entry {
            JournalEntry::Entered { symbol, entry_price, qty, entered_at, target_price, stop_price, .. } => {
                open.insert(
                    symbol.clone(),
                    OpenPositionOut {
                        symbol: symbol.clone(),
                        entry_price: *entry_price,
                        qty: *qty,
                        entered_at: *entered_at,
                        target_price: *target_price,
                        stop_price: *stop_price,
                    },
                );
            }
            JournalEntry::Exited { symbol, pnl_usd, .. } => {
                open.remove(symbol);
                trades += 1;
                if *pnl_usd > 0.0 {
                    wins += 1;
                } else {
                    losses += 1;
                }
                cumulative_pnl_usd += pnl_usd;
            }
            JournalEntry::Skipped { .. } => {}
        }
    }

    let mut open_positions: Vec<OpenPositionOut> = open.into_values().collect();
    open_positions.sort_by(|a, b| a.symbol.cmp(&b.symbol));

    let recent_entries: Vec<JournalEntry> = entries.iter().rev().take(recent_limit).cloned().collect();

    AutoTraderStatusOut { trades, wins, losses, cumulative_pnl_usd, open_positions, recent_entries }
}

#[cfg(test)]
mod tests {
    use super::*;
    use auto_trader::journal::{ExitReason, SkipReason};
    use chrono::TimeZone;

    fn ts(min: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 4, 14, 0, 0).unwrap() + chrono::Duration::minutes(min)
    }

    #[test]
    fn a_position_with_no_matching_exit_is_still_open() {
        let entries = vec![JournalEntry::Entered {
            symbol: "SWVL".to_string(),
            entry_price: 3.00,
            qty: 166,
            position_size_usd: 500.0,
            target_price: 3.06,
            stop_price: 2.94,
            entered_at: ts(0),
            momentum_overall: 0.72,
            momentum_volume_confirmation: 0.68,
        }];
        let status = compute_status(&entries, 50);
        assert_eq!(status.open_positions.len(), 1);
        assert_eq!(status.open_positions[0].symbol, "SWVL");
        assert_eq!(status.trades, 0);
    }

    #[test]
    fn a_win_and_a_loss_fold_into_running_totals_and_close_the_position() {
        let entries = vec![
            JournalEntry::Entered {
                symbol: "AAA".to_string(),
                entry_price: 3.00,
                qty: 166,
                position_size_usd: 500.0,
                target_price: 3.06,
                stop_price: 2.94,
                entered_at: ts(0),
                momentum_overall: 0.7,
                momentum_volume_confirmation: 0.7,
            },
            JournalEntry::Exited {
                symbol: "AAA".to_string(),
                exit_price: 3.06,
                exit_reason: ExitReason::TargetHit,
                pnl_usd: 9.96,
                pnl_pct: 2.0,
                qty: 166,
                entered_at: ts(0),
                exited_at: ts(2),
            },
            JournalEntry::Entered {
                symbol: "BBB".to_string(),
                entry_price: 5.00,
                qty: 100,
                position_size_usd: 500.0,
                target_price: 5.10,
                stop_price: 4.90,
                entered_at: ts(0),
                momentum_overall: 0.7,
                momentum_volume_confirmation: 0.7,
            },
            JournalEntry::Exited {
                symbol: "BBB".to_string(),
                exit_price: 4.90,
                exit_reason: ExitReason::StopHit,
                pnl_usd: -10.0,
                pnl_pct: -2.0,
                qty: 100,
                entered_at: ts(0),
                exited_at: ts(2),
            },
        ];
        let status = compute_status(&entries, 50);
        assert!(status.open_positions.is_empty());
        assert_eq!(status.trades, 2);
        assert_eq!(status.wins, 1);
        assert_eq!(status.losses, 1);
        assert!((status.cumulative_pnl_usd - (-0.04)).abs() < 1e-9);
    }

    #[test]
    fn skips_are_excluded_from_pnl_but_still_returned_in_recent_entries() {
        let entries = vec![JournalEntry::Skipped {
            symbol: "CCC".to_string(),
            reason: SkipReason::MomentumGateFailed,
            at: ts(0),
            detail: "overall=0.40 volumeConfirmation=0.55, need >= 0.6".to_string(),
        }];
        let status = compute_status(&entries, 50);
        assert_eq!(status.trades, 0);
        assert_eq!(status.recent_entries.len(), 1);
    }

    #[test]
    fn recent_entries_are_newest_first_and_capped_at_the_limit() {
        let entries: Vec<JournalEntry> = (0..5)
            .map(|i| JournalEntry::Skipped {
                symbol: format!("SYM{i}"),
                reason: SkipReason::OutsideRegularHours,
                at: ts(i),
                detail: "premarket".to_string(),
            })
            .collect();
        let status = compute_status(&entries, 2);
        assert_eq!(status.recent_entries.len(), 2);
        match &status.recent_entries[0] {
            JournalEntry::Skipped { symbol, .. } => assert_eq!(symbol, "SYM4"),
            other => panic!("expected Skipped, got {other:?}"),
        }
    }
}
