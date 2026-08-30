//! Runs a replay for one symbol/date-range, evaluates every signal it
//! produced against a simple target/stop outcome model, logs each one,
//! and prints aggregate hit-rate/move-size/timing metrics per strategy —
//! the whole point of architecture doc section 8: proving accuracy
//! before trusting any of this with real capital.
//!
//! Run with:
//! `cargo run -p backtest-metrics --bin backtest -- SWVL 2026-08-28T13:30:00Z 2026-08-28T20:00:00Z`
//!
//! Results accumulate in `data/backtest_log.jsonl` (gitignored, local
//! run output) across every run, so the aggregate metrics printed here
//! reflect *all* logged history, not just this one run.

use anyhow::{Context, Result};
use backtest_metrics::{
    aggregate_by_strategy, evaluate_outcome, extract_signals, following_prices, LoggedSignal,
    OutcomeThresholds,
};
use chrono::Utc;
use market_data::AlpacaConfig;
use replay_engine::replay_symbol;
use tracing::info;

const LOG_PATH: &str = "data/backtest_log.jsonl";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .init();
    dotenvy::dotenv().ok();

    let args: Vec<String> = std::env::args().collect();
    let (symbol, start, end) = match args.as_slice() {
        [_, symbol, start, end] => (symbol.clone(), start.clone(), end.clone()),
        _ => {
            anyhow::bail!(
                "usage: backtest <SYMBOL> <START RFC3339> <END RFC3339>\n  e.g. backtest SWVL 2026-08-28T13:30:00Z 2026-08-28T20:00:00Z"
            );
        }
    };

    let cfg = AlpacaConfig::from_env().context("loading Alpaca config")?;
    let thresholds = OutcomeThresholds::default();

    info!(symbol, start, end, "running replay for backtest evaluation");
    let result = replay_symbol(&cfg, &symbol, &start, &end).await?;

    let signals = extract_signals(&result);
    info!(count = signals.len(), "signals extracted from this replay");

    let mut logged = Vec::with_capacity(signals.len());
    for signal in &signals {
        let prices = following_prices(&result, signal);
        let outcome = evaluate_outcome(signal.price, &prices, &thresholds);
        info!(
            strategy = ?signal.strategy,
            timestamp = %signal.timestamp,
            price = signal.price,
            hit = outcome.hit,
            max_favorable_pct = format!("{:.2}", outcome.max_favorable_pct),
            bars_to_target = ?outcome.bars_to_target,
            "signal outcome"
        );
        logged.push(LoggedSignal {
            symbol: symbol.clone(),
            strategy: signal.strategy,
            timestamp: signal.timestamp,
            signal_price: signal.price,
            outcome,
            logged_at: Utc::now(),
        });
    }

    let log_path = std::path::Path::new(LOG_PATH);
    backtest_metrics::append(log_path, &logged)?;
    info!(path = LOG_PATH, count = logged.len(), "logged this run's signals");

    // Aggregate across *all* history logged so far, not just this run —
    // that's the whole point of persisting rather than only printing.
    let all_history = backtest_metrics::read_all(log_path)?;
    let entries: Vec<_> = all_history
        .iter()
        .map(|l| (l.strategy, l.outcome))
        .collect();
    let by_strategy = aggregate_by_strategy(&entries);

    info!(total_logged_signals = all_history.len(), "aggregate metrics across all logged history");
    for (strategy, metrics) in &by_strategy {
        info!(
            strategy = ?strategy,
            total_signals = metrics.total_signals,
            hits = metrics.hits,
            hit_rate_pct = format!("{:.1}", metrics.hit_rate_pct),
            avg_move_pct_on_winners = format!("{:.2}", metrics.avg_move_pct_on_winners),
            avg_bars_to_target_on_winners = format!("{:.1}", metrics.avg_bars_to_target_on_winners),
            "strategy metrics"
        );
    }

    Ok(())
}
