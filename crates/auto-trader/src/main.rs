//! auto-trader — dry-run paper-trading journal. Micropullback-only at
//! first; broadened (v3, 2026-09-04) to also act on IgnitionDetector and
//! ConsolidationBreakout triggers — see `engine.rs`'s own doc comment for
//! which strategies and why.
//!
//! Defaults to a local simulation journal. AUTO_TRADER_EXECUTION_MODE=paper
//! selects the separate Alpaca paper runner and its broker-confirmed ledger.
//! Neither mode submits real-money orders.

use std::path::{Path, PathBuf};
use std::time::{Duration as StdDuration, Instant};

use backtest_metrics::StrategyConfigFile;
use tracing::{error, info, warn};

use auto_trader::client::AutoTraderClient;
use auto_trader::config::Config;
use auto_trader::engine::Engine;
use auto_trader::journal::{self, JournalEntry};

/// Same shared-mount convention as the journal itself -- written by
/// `bin/live_efficiency` (see `backtest_metrics::strategy_config`'s own
/// doc comment for the full "evidence-driven strategy selection"
/// reasoning), read here.
const STRATEGY_CONFIG_PATH: &str = "data/auto_trader_strategy_config.json";
/// How often to actually hit the filesystem for a fresh copy -- cheap
/// either way, but there's no reason to stat/read this file on every
/// single incoming WS event when `bin/live_efficiency` itself only ever
/// updates it once per its own run (the standing cron's cadence, at most
/// every few hours).
const STRATEGY_CONFIG_RECHECK_INTERVAL: StdDuration = StdDuration::from_secs(5 * 60);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::new("info")).init();
    dotenvy::dotenv().ok();

    match std::env::var("AUTO_TRADER_EXECUTION_MODE").as_deref() {
        Ok("paper") => return auto_trader::paper_runtime::run(false).await,
        Ok("journal") | Err(_) => {},
        Ok(_) => anyhow::bail!("AUTO_TRADER_EXECUTION_MODE must be journal or paper"),
    }

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
        Err(e) => return Err(e.context("cannot safely restore trader journal")),
    }

    // Persists across reconnects (same reasoning as engine state itself)
    // -- `None` forces an immediate first check right after startup,
    // rather than waiting a full interval before ever reading the file.
    let mut last_config_check: Option<Instant> = None;

    loop {
        match run_once(&config.ws_url, &mut engine, &journal_path, &mut last_config_check).await {
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

async fn run_once(ws_url: &str, engine: &mut Engine, journal_path: &Path, last_config_check: &mut Option<Instant>) -> anyhow::Result<()> {
    let mut client = AutoTraderClient::connect(ws_url).await?;
    // Restore durable state on every reconnect, then replay missing completed bars.
    engine.seed_from_history(&journal::read_all(journal_path)?);
    let requests = engine.recovery_requests();
    if !requests.is_empty() {
        let cfg = market_data::AlpacaConfig::from_env()?;
        let now = chrono::Utc::now();
        let mut bars = Vec::new();
        for (symbol,entered_at) in requests {
            let fetched = market_data::fetch_recent_minute_bars(&cfg,&symbol,
                &(entered_at - chrono::Duration::minutes(1)).to_rfc3339(),&now.to_rfc3339()).await?;
            bars.extend(fetched.into_iter().filter(|b| b.timestamp + chrono::Duration::minutes(1) > entered_at
                && b.timestamp + chrono::Duration::minutes(1) <= now));
        }
        bars.sort_by_key(|b| b.timestamp);
        for b in bars {
            let event = market_data::ScanEvent::BarUpdate {symbol:b.symbol,timestamp:b.timestamp,
                open:b.open,high:b.high,low:b.low,close:b.close,volume:b.volume,interval_secs:60,is_final:true};
            for entry in engine.on_event(&event) { journal::append(journal_path,&entry)?; }
        }
    }
    loop {
        maybe_reload_strategy_config(engine, journal_path, last_config_check).await;

        let Some(event) = client.next_event().await? else {
            return Ok(());
        };
        // Never enter from delayed alerts buffered during reconnect reconciliation.
        let stale_entry = match &event {
            market_data::ScanEvent::IgnitionEvent { timestamp,.. } | market_data::ScanEvent::ConsolidationEvent { timestamp,.. } =>
                chrono::Utc::now() - *timestamp > chrono::Duration::minutes(2),
            _ => false,
        };
        if stale_entry { continue; }
        for entry in engine.on_event(&event) {
            journal::append(journal_path, &entry)?;
            log_entry(&entry, engine);
        }
    }
}

/// The actual mechanism behind "genuinely improve its own decision-
/// making over time" (2026-09-05, v4) -- checked once per incoming WS
/// event (cheap: gated by `last_config_check` so the file is only
/// actually read every `STRATEGY_CONFIG_RECHECK_INTERVAL`, not on every
/// tick). A missing/unparseable file is a normal, expected state (e.g.
/// `bin/live_efficiency` hasn't run yet since this feature shipped) --
/// silently retried next interval, not an error.
async fn maybe_reload_strategy_config(engine: &mut Engine, journal_path: &Path, last_config_check: &mut Option<Instant>) {
    let due = last_config_check.map(|t| t.elapsed() >= STRATEGY_CONFIG_RECHECK_INTERVAL).unwrap_or(true);
    if !due {
        return;
    }
    *last_config_check = Some(Instant::now());

    let content = match tokio::fs::read_to_string(STRATEGY_CONFIG_PATH).await {
        Ok(content) => content,
        Err(_) => return,
    };
    let file: StrategyConfigFile = match serde_json::from_str(&content) {
        Ok(file) => file,
        Err(e) => {
            warn!(error = %e, "auto-trader: failed to parse strategy config, keeping current state");
            return;
        }
    };

    let changes = engine.set_enabled_strategies(&file.decisions(), chrono::Utc::now());
    for entry in changes {
        if let Err(e) = journal::append(journal_path, &entry) {
            error!(error = ?e, "auto-trader: failed to write journal entry -- decision was still made, only the audit log write failed");
        }
        log_entry(&entry, engine);
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
        JournalEntry::StrategyConfigChanged { strategy, enabled, sample_size, expectancy_pct, .. } => {
            info!(?strategy, enabled, sample_size, expectancy_pct, "auto-trader: strategy config changed by real evidence-driven review");
        }
    }
}
