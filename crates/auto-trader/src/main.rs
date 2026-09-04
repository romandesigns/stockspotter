//! auto-trader — dry-run paper-trading journal. Micropullback-only at
//! first; broadened (v3, 2026-09-04) to also act on IgnitionDetector and
//! ConsolidationBreakout triggers — see `engine.rs`'s own doc comment for
//! which strategies and why.
//!
//! **Places no real orders.** There is no HTTP client to Alpaca's
//! trading API anywhere in this crate — that's a deliberate safety
//! property, not a disabled flag (see `config.rs`'s own doc comment).
//! This process only ever: connects to `ws-server` as a real WS client
//! (`client.rs`), runs a purely in-memory decision engine (`engine.rs`),
//! and appends a readable JSONL audit trail of what it would have done
//! (`journal.rs`).

use std::path::{Path, PathBuf};
use std::time::Duration as StdDuration;

use tracing::{error, info, warn};

use auto_trader::client::AutoTraderClient;
use auto_trader::config::Config;
use auto_trader::engine::Engine;
use auto_trader::journal::{self, JournalEntry};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::new("info")).init();
    dotenvy::dotenv().ok();

    let config = Config::from_env();
    let journal_path = PathBuf::from(&config.journal_path);
    info!(
        ws_url = %config.ws_url,
        position_size_usd = config.position_size_usd,
        max_concurrent_positions = config.max_concurrent_positions,
        journal_path = %journal_path.display(),
        "auto-trader starting -- DRY RUN, no real orders will ever be placed by this process"
    );

    let mut engine = Engine::new(config.clone());

    // Real gap found live (2026-09-04 standing cycle): this VPS
    // redeploys multiple times a day, recreating this container each
    // time -- without this, closed-trade history and today's-entries
    // dedup silently reset every single restart. See
    // Engine::seed_from_history's own doc comment for why that's a real
    // problem (the self-adapting position size, and the one-per-day risk
    // gate), not a cosmetic one.
    match journal::read_all(&journal_path) {
        Ok(history) => {
            let closed_trades_replayed = history.iter().filter(|e| matches!(e, JournalEntry::Exited { .. })).count();
            let entries_today_replayed = history.iter().filter(|e| matches!(e, JournalEntry::Entered { .. })).count();
            engine.seed_from_history(&history);
            info!(closed_trades_replayed, entries_today_replayed, "auto-trader: seeded engine state from the existing journal");
        }
        Err(e) => warn!(error = ?e, "auto-trader: failed to read existing journal for seeding -- starting with empty history"),
    }

    loop {
        match run_once(&config.ws_url, &mut engine, &journal_path).await {
            Ok(()) => warn!("auto-trader: connection to ws-server closed cleanly, reconnecting in 5s"),
            Err(e) => error!(error = ?e, "auto-trader: connection error, reconnecting in 5s"),
        }
        // Engine state (open positions, momentum cache, today's entries)
        // deliberately persists in memory across a reconnect -- a
        // transient network blip to ws-server shouldn't forget an
        // already-open simulated position.
        tokio::time::sleep(StdDuration::from_secs(5)).await;
    }
}

async fn run_once(ws_url: &str, engine: &mut Engine, journal_path: &Path) -> anyhow::Result<()> {
    let mut client = AutoTraderClient::connect(ws_url).await?;
    loop {
        let Some(event) = client.next_event().await? else {
            return Ok(());
        };
        for entry in engine.on_event(&event) {
            if let Err(e) = journal::append(journal_path, &entry) {
                error!(error = ?e, "auto-trader: failed to write journal entry -- decision was still made, only the audit log write failed");
            }
            log_entry(&entry, engine);
        }
    }
}

fn log_entry(entry: &JournalEntry, engine: &Engine) {
    match entry {
        JournalEntry::Entered { symbol, strategy, entry_price, qty, target_price, stop_price, .. } => {
            info!(symbol, ?strategy, entry_price, qty, target_price, stop_price, "auto-trader: ENTERED (simulated)");
        }
        JournalEntry::Exited { symbol, exit_price, exit_reason, pnl_usd, pnl_pct, .. } => {
            info!(
                symbol,
                exit_price,
                ?exit_reason,
                pnl_usd,
                pnl_pct,
                trades = engine.stats.trades,
                wins = engine.stats.wins,
                losses = engine.stats.losses,
                cumulative_pnl_usd = engine.stats.cumulative_pnl_usd,
                "auto-trader: EXITED (simulated) -- running totals"
            );
        }
        JournalEntry::Skipped { symbol, reason, detail, .. } => {
            info!(symbol, ?reason, detail, "auto-trader: skipped");
        }
        JournalEntry::StopAdjusted { symbol, previous_stop_price, new_stop_price, trigger_price, .. } => {
            info!(symbol, previous_stop_price, new_stop_price, trigger_price, "auto-trader: trailing stop raised (simulated)");
        }
    }
}
