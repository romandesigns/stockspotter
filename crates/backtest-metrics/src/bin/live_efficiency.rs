//! Evaluates real live signals (captured by `ws-server`'s own
//! `live_signals::LiveSignalTracker`, see that binary's `main.rs`) into
//! real hit-rate/false-positive-rate metrics per strategy — the actual
//! "detection efficiency" benchmark Roman asked for 2026-09-03, as
//! opposed to `bin/backtest.rs`/`bin/tune_broad.rs`, which only ever
//! judge historical replay data.
//!
//! What this does NOT do (yet, deliberately out of v1's scope — see the
//! session's own memory log for the full reasoning): compute *coverage*
//! (real moves that never produced a signal at all) or *latency* (real
//! move start vs. first alert). Both need an independent, detector-
//! agnostic "a real move happened here" ground truth this v1 doesn't yet
//! have — building that without a careful separate pass would risk
//! measuring the wrong thing, same mistake this whole project has
//! avoided elsewhere (e.g. OutcomeThresholds::for_strategy's own history).
//!
//! Run with: `cargo run -p backtest-metrics --bin live_efficiency`
//! (repo root, needs `.env` for Alpaca REST access) — safe to run
//! repeatedly/on a schedule, each run only evaluates whatever pending
//! signals have aged past their own strategy's lookforward window since
//! the last run, and the aggregate report always reflects the full
//! evaluated history logged so far, not just this run's batch.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use backtest_metrics::{
    aggregate_by_strategy, decide_enabled_strategies, evaluate_outcome, read_pending, write_pending, AggregateMetrics, LoggedSignal, OutcomeThresholds,
    PendingSignal, Strategy, StrategyConfigFile,
};
use chrono::{DateTime, Duration, Utc};
use market_data::{fetch_recent_minute_bars, AlpacaConfig};
use tracing::{info, warn};

const PENDING_LOG_PATH: &str = "data/live_pending_signals.jsonl";
const EVALUATED_LOG_PATH: &str = "data/live_evaluated_signals.jsonl";
/// The auto-trader's own recurring "which signals do I trust" decision
/// (2026-09-05, see `backtest_metrics::strategy_config`'s own doc
/// comment for the full reasoning) -- same shared-mount convention as
/// every other file this benchmark and the auto-trader already read/
/// write (`data/auto_trader_journal.jsonl`, `data/push_tokens.json`).
const STRATEGY_CONFIG_PATH: &str = "data/auto_trader_strategy_config.json";

/// Extra buffer past a strategy's own `lookforward_bars` before treating
/// a signal as evaluable — real Alpaca REST bar data can lag the
/// realtime WS feed by a bar or two, so evaluating right at the exact
/// theoretical minute risks fetching a still-incomplete window and
/// under-counting `max_favorable_pct`. Cheap to be generous here since
/// nothing about the outcome definition itself depends on evaluating
/// the instant it's technically possible to.
const EVALUATION_BUFFER_MINUTES: i64 = 3;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .init();
    dotenvy::dotenv().ok();

    let cfg = AlpacaConfig::from_env().context("loading Alpaca config")?;
    let pending_path = Path::new(PENDING_LOG_PATH);
    let evaluated_path = Path::new(EVALUATED_LOG_PATH);

    let pending = read_pending(pending_path)?;
    info!(count = pending.len(), "pending signals captured so far");

    let now = Utc::now();
    let mut still_pending = Vec::new();
    let mut newly_evaluated = Vec::new();

    for signal in pending {
        let thresholds = OutcomeThresholds::for_strategy(signal.strategy);
        let evaluable_at = signal.timestamp + Duration::minutes(thresholds.lookforward_bars as i64 + EVALUATION_BUFFER_MINUTES);
        if now < evaluable_at {
            still_pending.push(signal);
            continue;
        }

        match evaluate_signal(&cfg, &signal, &thresholds, evaluable_at).await {
            Ok(Some(logged)) => {
                info!(
                    symbol = %logged.symbol,
                    strategy = ?logged.strategy,
                    hit = logged.outcome.hit,
                    max_favorable_pct = format!("{:.2}", logged.outcome.max_favorable_pct),
                    "live signal evaluated"
                );
                newly_evaluated.push(logged);
            }
            Ok(None) => {
                // Alpaca had no bars at all for this window -- a real,
                // if rare, gap (e.g. the symbol got halted for the whole
                // window, or a REST data lag exceeds even this buffer).
                // Retried next run rather than discarded, same fail-
                // open-but-log pattern used elsewhere in this codebase.
                warn!(symbol = %signal.symbol, strategy = ?signal.strategy, "no bars returned for this signal's evaluation window yet; will retry next run");
                still_pending.push(signal);
            }
            Err(e) => {
                warn!(symbol = %signal.symbol, strategy = ?signal.strategy, error = %e, "failed to evaluate live signal; will retry next run");
                still_pending.push(signal);
            }
        }
    }

    if !newly_evaluated.is_empty() {
        backtest_metrics::append(evaluated_path, &newly_evaluated)?;
        info!(count = newly_evaluated.len(), path = EVALUATED_LOG_PATH, "appended newly-evaluated live signals");
    }
    write_pending(pending_path, &still_pending)?;
    info!(still_pending = still_pending.len(), "signals still too young to evaluate, left pending");

    let all_history = backtest_metrics::read_all(evaluated_path)?;
    let entries: Vec<_> = all_history.iter().map(|l| (l.strategy, l.outcome)).collect();
    let by_strategy = aggregate_by_strategy(&entries);

    info!(total_evaluated_signals = all_history.len(), "real live detection-efficiency report (all evaluated history)");
    for (strategy, metrics) in &by_strategy {
        info!(
            strategy = ?strategy,
            total_signals = metrics.total_signals,
            hits = metrics.hits,
            hit_rate_pct = format!("{:.1}", metrics.hit_rate_pct),
            false_positive_rate_pct = format!("{:.1}", 100.0 - metrics.hit_rate_pct),
            avg_move_pct_on_winners = format!("{:.2}", metrics.avg_move_pct_on_winners),
            avg_bars_to_target_on_winners = format!("{:.1}", metrics.avg_bars_to_target_on_winners),
            "strategy metrics"
        );
    }

    update_strategy_config(&by_strategy)?;

    Ok(())
}

/// Fetches the real subsequent bars for one pending signal and evaluates
/// its outcome. `Ok(None)` means Alpaca genuinely returned no bars for
/// the window (not an error, but not evaluable either).
async fn evaluate_signal(
    cfg: &AlpacaConfig,
    signal: &PendingSignal,
    thresholds: &OutcomeThresholds,
    evaluable_at: DateTime<Utc>,
) -> Result<Option<LoggedSignal>> {
    let start = signal.timestamp.to_rfc3339();
    let end = evaluable_at.to_rfc3339();
    let bars = fetch_recent_minute_bars(cfg, &signal.symbol, &start, &end).await?;

    // Strictly after the signal's own timestamp -- following_prices must
    // never include the signal's own bar (same rule signals.rs's own
    // following_prices enforces for backtests), and Alpaca's [start, end)
    // range can include the exact `start` minute itself.
    let mut following: Vec<(DateTime<Utc>, f64)> = bars.into_iter().filter(|b| b.timestamp > signal.timestamp).map(|b| (b.timestamp, b.close)).collect();
    if following.is_empty() {
        return Ok(None);
    }
    following.sort_by_key(|(ts, _)| *ts);
    let prices: Vec<f64> = following.into_iter().map(|(_, price)| price).collect();

    let outcome = evaluate_outcome(signal.signal_price, &prices, thresholds);
    Ok(Some(LoggedSignal {
        symbol: signal.symbol.clone(),
        strategy: signal.strategy,
        timestamp: signal.timestamp,
        signal_price: signal.signal_price,
        outcome,
        logged_at: Utc::now(),
    }))
}

/// Reads whatever decision is already on disk (or seeds today's
/// hardcoded defaults on a genuinely first run -- see
/// `strategy_config::decide_enabled_strategies`'s own doc comment for why
/// that matters), re-derives it from this run's real `AggregateMetrics`,
/// and persists the fresh result. Always writes (so `sampleSize`/
/// `expectancyPct` stay current even when `enabled` didn't flip) but only
/// logs loudly on the first run or a real enabled/disabled transition --
/// this runs every time this binary does (the standing cron's own
/// cadence), most runs will have nothing new to report.
fn update_strategy_config(by_strategy: &HashMap<Strategy, AggregateMetrics>) -> Result<()> {
    let path = Path::new(STRATEGY_CONFIG_PATH);

    let previous: Option<StrategyConfigFile> = std::fs::read_to_string(path).ok().and_then(|content| serde_json::from_str(&content).ok());
    let is_first_run = previous.is_none();
    let current: HashMap<Strategy, bool> = previous.map(|f| f.enabled_map()).unwrap_or_default();

    let decisions = decide_enabled_strategies(&current, by_strategy);

    let transitions: Vec<String> = decisions
        .iter()
        .filter(|(strategy, decision)| current.get(strategy).is_some_and(|&was| was != decision.enabled))
        .map(|(strategy, decision)| format!("{strategy:?} -> {}", if decision.enabled { "enabled" } else { "disabled" }))
        .collect();

    if is_first_run {
        info!(path = STRATEGY_CONFIG_PATH, "seeded auto-trader strategy config for the first time (today's hardcoded defaults -- no live behavior change)");
    } else if !transitions.is_empty() {
        info!(transitions = ?transitions, "auto-trader strategy config: real evidence-driven change(s)");
    }

    let file = StrategyConfigFile::from_decisions(decisions, Utc::now());
    let json = serde_json::to_string_pretty(&file).context("serializing auto-trader strategy config")?;
    std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}
