//! Broad, repeatable backtest — the missing half of architecture doc
//! section 8's "prove real accuracy before risking capital".
//!
//! **Why this exists (2026-09-06).** `backtest.rs` evaluates exactly one
//! symbol over one date range, and in practice that is all that ever ran:
//! the entire logged evidence base in `data/backtest_log.jsonl` was 341
//! signals from a single symbol (SWVL) on a single session (2026-08-28).
//! Every tuned constant in this codebase — ignition's
//! `confirmation_trade_count`, `OutcomeThresholds::scalp`'s bracket, and
//! now the alert cooldown — traces back to that one day. Tuning a
//! detector on one session of one stock's microstructure is overfitting,
//! and the project's own stated gate ("the system isn't trusted with
//! live capital until its logged historical hit rate and move-size stats
//! are known") was therefore never actually met.
//!
//! `tune_broad` already screens and replays many real sessions, but it
//! sweeps *config variants* and prints comparisons — it never logs
//! `LoggedSignal`s, so it grows no evidence base. This binary is the
//! other shape: **one fixed config (the shipped defaults), many
//! sessions, everything logged**, so `aggregate_by_strategy` has a real
//! multi-symbol multi-session sample to report over.
//!
//! Run with: `cargo run --release -p backtest-metrics --bin backtest_broad`
//!
//! Optional args: `--symbols AAA,BBB` and `--lookback-days N` override
//! the defaults below. Slow by design (many real Alpaca fetches) — a
//! per-tuning-pass cost, not a per-commit one.
//!
//! Deliberately reuses `backtest.rs`'s exact evaluation path
//! (`extract_signals` -> `OutcomeThresholds::for_strategy` ->
//! `evaluate_outcome` -> `append`), so a signal logged here is
//! indistinguishable from one logged by a single-session run. Two
//! different logging definitions would defeat the point of having one
//! aggregate at all.

use anyhow::{Context, Result};
use backtest_metrics::{
    aggregate_by_strategy, append, compute_day_signals, evaluate_outcome, extract_signals, forward_path_pct,
    following_prices, pick_sessions, session_window_utc, LoggedSignal, OutcomeThresholds,
    SessionCategory,
};
use chrono::Utc;
use market_data::AlpacaConfig;
use replay_engine::{fetch_replay_data, run_replay, ReplayConfig};

const LOG_PATH: &str = "data/backtest_log.jsonl";

/// Real low-float small-caps this scanner has already touched live —
/// same starting universe `tune_broad` screens, kept identical on
/// purpose so the two binaries reason about the same population.
const DEFAULT_SYMBOLS: &[&str] = &["SWVL", "AEHL", "NCRA", "ORIO", "SIEB", "DAVEW", "QNRX", "AREN", "YDDL"];

const DEFAULT_LOOKBACK_DAYS: i64 = 75;

/// Both hot and quiet days get replayed, and the quiet ones matter as
/// much: they're the negative control. A detector that fires on real
/// gap days AND on ordinary nothing-happening days isn't selective, it's
/// just noisy — and a hit-rate computed only over hot days would never
/// reveal that.
const MAX_HOT_PER_SYMBOL: usize = 3;
const MAX_QUIET_PER_SYMBOL: usize = 2;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("warn"))
        .init();
    dotenvy::dotenv().ok();

    let args: Vec<String> = std::env::args().collect();
    let symbols: Vec<String> = match arg_value(&args, "--symbols") {
        Some(v) => v.split(',').map(|s| s.trim().to_uppercase()).filter(|s| !s.is_empty()).collect(),
        None => DEFAULT_SYMBOLS.iter().map(|s| s.to_string()).collect(),
    };
    let lookback_days: i64 = arg_value(&args, "--lookback-days")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_LOOKBACK_DAYS);

    let cfg = AlpacaConfig::from_env().context("loading Alpaca config")?;

    // Anchor to yesterday's midnight UTC, not `Utc::now()` — a
    // still-forming, incomplete "today" bar shouldn't be screened as if
    // it were a finished session. Same reasoning as `tune_broad`.
    let end = (Utc::now() - chrono::Duration::days(1)).date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc();
    let start = end - chrono::Duration::days(lookback_days);

    println!("=== screening {} symbols over the last {lookback_days} days ===", symbols.len());

    let mut all_logged: Vec<LoggedSignal> = Vec::new();
    let mut sessions_replayed = 0usize;
    let mut sessions_skipped = 0usize;
    let mut hot_sessions = 0usize;
    let mut quiet_sessions = 0usize;

    for symbol in &symbols {
        let bars = match market_data::fetch_daily_bar_series(&cfg, symbol, &start.to_rfc3339(), &end.to_rfc3339()).await {
            Ok(b) => b,
            Err(e) => {
                println!("  {symbol}: daily-bar fetch failed ({e}); skipping");
                continue;
            }
        };
        if bars.len() < 2 {
            println!("  {symbol}: not enough daily history ({} bars); skipping", bars.len());
            continue;
        }

        let day_signals = compute_day_signals(&bars);
        let picks = pick_sessions(&day_signals, MAX_HOT_PER_SYMBOL, MAX_QUIET_PER_SYMBOL);
        println!("  {symbol}: {} days screened -> {} sessions picked", day_signals.len(), picks.len());

        for pick in picks {
            let Ok((session_start, session_end)) = session_window_utc(pick.date) else {
                sessions_skipped += 1;
                continue;
            };

            let data = match fetch_replay_data(&cfg, symbol, &session_start.to_rfc3339(), &session_end.to_rfc3339()).await {
                Ok(d) => d,
                Err(e) => {
                    println!("    {symbol} {}: replay fetch failed ({e}); skipping", pick.date);
                    sessions_skipped += 1;
                    continue;
                }
            };
            if data.bars.is_empty() {
                println!("    {symbol} {}: no bars (halted/no-trade day); skipping", pick.date);
                sessions_skipped += 1;
                continue;
            }

            // The shipped defaults, not a swept variant — this binary
            // measures what actually runs live, which is the whole
            // difference between it and `tune_broad`.
            let result = run_replay(&data, &ReplayConfig::default());
            let signals = extract_signals(&result);
            let logged_at = Utc::now();

            let mut logged = Vec::with_capacity(signals.len());
            for signal in &signals {
                // Per-strategy bracket, identical to `backtest.rs` — a
                // tick-level ignition signal and a sustained multi-minute
                // funnel qualification are not comparable against one
                // blanket target/stop.
                let thresholds = OutcomeThresholds::for_strategy(signal.strategy);
                let prices = following_prices(&result, signal);
                let outcome = evaluate_outcome(signal.price, &prices, &thresholds);
                logged.push(LoggedSignal {
                    symbol: symbol.clone(),
                    strategy: signal.strategy,
                    timestamp: signal.timestamp,
                    signal_price: signal.price,
                    outcome,
                    logged_at,
                    // Raw evidence for offline bracket sweeps -- see
                    // LoggedSignal::forward_path_pct's own doc comment.
                    forward_path_pct: forward_path_pct(signal.price, &prices),
                });
            }

            println!(
                "    {symbol} {} ({:?}, gap={:.1}%, rel_vol={:.1}x): {} bars, {} trades -> {} signals",
                pick.date,
                pick.category,
                pick.gap_pct,
                pick.rel_volume,
                data.bars.len(),
                data.trades.len(),
                logged.len()
            );

            all_logged.extend(logged);
            sessions_replayed += 1;
            match pick.category {
                SessionCategory::Hot => hot_sessions += 1,
                SessionCategory::Quiet => quiet_sessions += 1,
            }
        }
    }

    if sessions_replayed == 0 {
        anyhow::bail!("no sessions could be replayed — check Alpaca credentials and the symbol list");
    }

    append(std::path::Path::new(LOG_PATH), &all_logged).context("appending broad backtest results")?;

    println!();
    println!("=== replayed {sessions_replayed} sessions ({sessions_skipped} skipped) ===");
    println!("  hot: {hot_sessions} sessions, quiet (negative control): {quiet_sessions} sessions");
    println!("  logged {} new signals to {LOG_PATH}", all_logged.len());
    println!();

    // Reported over THIS run's sessions specifically, so the numbers can
    // be attributed to a known sample rather than to whatever historical
    // mix happens to sit in the log file.
    println!("=== metrics for this run's sample only ===");
    let pairs: Vec<_> = all_logged.iter().map(|l| (l.strategy, l.outcome)).collect();
    let by_strategy = aggregate_by_strategy(&pairs);
    let mut rows: Vec<_> = by_strategy.iter().collect();
    rows.sort_by_key(|(s, _)| format!("{s:?}"));
    for (strategy, m) in rows {
        let thresholds = OutcomeThresholds::for_strategy(*strategy);
        let expectancy = m.real_expectancy_pct.unwrap_or(0.0) - backtest_metrics::round_trip_cost_pct();
        println!(
            "  {strategy:?}: n={} hit_rate={:.1}% avg_win={:.2}% bracket=+{:.1}/-{:.1} net_expectancy={:+.2}pp",
            m.total_signals, m.hit_rate_pct, m.avg_move_pct_on_winners, thresholds.target_pct, thresholds.stop_pct, expectancy
        );
    }

    Ok(())
}

/// Minimal `--flag value` reader — this binary has two options and no
/// other need for a CLI-parsing dependency.
fn arg_value(args: &[String], flag: &str) -> Option<String> {
    let i = args.iter().position(|a| a == flag)?;
    args.get(i + 1).cloned()
}
