//! Threshold tuning sweep — answers the question the first `backtest`
//! run raised: is a 9.3% hit rate a bad detector, or a follow-through
//! confirmation that's too easy to trigger? Fetches one real historical
//! window *once*, then re-runs the pure (fast, in-memory) detection
//! pipeline against it under several `MonitorConfig` variants crossed
//! with several outcome-evaluation profiles, printing a comparison
//! table. This is what "tune against logged data" actually means —
//! systematic comparison, not guessing at a single new default.
//!
//! Run with: `cargo run -p backtest-metrics --bin tune`

use anyhow::Result;
use backtest_metrics::{
    aggregate, evaluate_outcome, extract_signals, extract_signals_with_momentum_threshold,
    following_prices, OutcomeThresholds, Strategy,
};
use ignition_detector::{FollowThroughThresholds, IgnitionThresholds, MonitorConfig};
use market_data::AlpacaConfig;
use replay_engine::{fetch_replay_data, run_replay, ReplayConfig};

const SYMBOL: &str = "SWVL";
const START: &str = "2026-08-28T13:30:00Z";
const END: &str = "2026-08-28T20:00:00Z";

#[tokio::main]
async fn main() -> Result<()> {
    // Deliberately quiet — this prints its own summary table, not a
    // stream of per-signal log lines like `backtest`/`replay` do.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("warn"))
        .init();
    dotenvy::dotenv().ok();

    let cfg = AlpacaConfig::from_env()?;

    println!("fetching real historical data once: {SYMBOL} {START}..{END}");
    let data = fetch_replay_data(&cfg, SYMBOL, START, END).await?;
    println!("fetched {} bars — sweeping in-memory from here\n", data.bars.len());

    let monitor_variants: Vec<(&str, MonitorConfig)> = vec![
        ("baseline (current defaults)", MonitorConfig::default()),
        (
            "confirm=20 trades (was 10)",
            MonitorConfig {
                confirmation_trade_count: 20,
                ..MonitorConfig::default()
            },
        ),
        (
            "confirm=40 trades",
            MonitorConfig {
                confirmation_trade_count: 40,
                ..MonitorConfig::default()
            },
        ),
        (
            "dip_recovery=1% (was 0.5%)",
            MonitorConfig {
                follow_through: FollowThroughThresholds {
                    dip_recovery_margin: 0.01,
                    ..FollowThroughThresholds::default()
                },
                ..MonitorConfig::default()
            },
        ),
        (
            "confirm=20 + dip_recovery=2%",
            MonitorConfig {
                confirmation_trade_count: 20,
                follow_through: FollowThroughThresholds {
                    dip_recovery_margin: 0.02,
                    ..FollowThroughThresholds::default()
                },
                ..MonitorConfig::default()
            },
        ),
        (
            "stricter spike (5x ratio, min 5 trades)",
            MonitorConfig {
                thresholds: IgnitionThresholds {
                    trade_frequency_spike_ratio: 5.0,
                    min_recent_trades_for_spike: 5,
                    ..IgnitionThresholds::default()
                },
                ..MonitorConfig::default()
            },
        ),
        (
            "confirm=20 + stricter spike",
            MonitorConfig {
                confirmation_trade_count: 20,
                thresholds: IgnitionThresholds {
                    trade_frequency_spike_ratio: 5.0,
                    min_recent_trades_for_spike: 5,
                    ..IgnitionThresholds::default()
                },
                ..MonitorConfig::default()
            },
        ),
    ];

    let outcome_variants: Vec<(&str, OutcomeThresholds)> = vec![
        (
            "target 5%/stop 3%/20bars (current)",
            OutcomeThresholds::default(),
        ),
        (
            "target 2%/stop 2%/10bars (scalp)",
            OutcomeThresholds {
                target_pct: 2.0,
                stop_pct: 2.0,
                lookforward_bars: 10,
            },
        ),
        (
            "target 3%/stop 2%/15bars",
            OutcomeThresholds {
                target_pct: 3.0,
                stop_pct: 2.0,
                lookforward_bars: 15,
            },
        ),
    ];

    // Funnel/momentum diagnostic first — unlike ignition (many discrete
    // tick-level events per session), Stage 1/2 and momentum
    // qualification are slow-moving, session-level signals. Edge-
    // triggered signal counting means a single session contributes at
    // most a handful of *transitions*, not enough to meaningfully
    // backtest a hit rate the way ignition's 300+ signals could. What
    // *can* be checked from one session: the actual score distribution,
    // to see whether a threshold is even in reach.
    {
        let result = run_replay(&data, &ReplayConfig::default());
        let mut momentum_scores: Vec<f64> = result.bar_events.iter().map(|e| e.momentum.overall).collect();
        momentum_scores.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = momentum_scores.len();
        let funnel_passed = result.bar_events.iter().filter(|e| e.funnel.passed()).count();

        println!("=== funnel/momentum diagnostic (single session — see note above) ===");
        println!(
            "funnel: {}/{} bars passed all 4 conditions ({:.0}% of the session)",
            funnel_passed,
            result.bar_events.len(),
            funnel_passed as f64 / result.bar_events.len() as f64 * 100.0
        );
        if n > 0 {
            println!(
                "momentum.overall distribution: min={:.3} p25={:.3} median={:.3} p75={:.3} max={:.3}",
                momentum_scores[0],
                momentum_scores[n / 4],
                momentum_scores[n / 2],
                momentum_scores[3 * n / 4],
                momentum_scores[n - 1]
            );
            for threshold in [0.90, 0.75, 0.60, 0.50, 0.40] {
                let would_qualify = momentum_scores.iter().filter(|&&s| s >= threshold).count();
                println!(
                    "  at threshold {threshold:.2}: {would_qualify}/{n} bars would qualify ({:.1}%)",
                    would_qualify as f64 / n as f64 * 100.0
                );
            }
        }
        println!();

        // Momentum threshold sweep: real edge-triggered signals (not just
        // "score is above X") at each candidate threshold, evaluated
        // against the swing outcome profile (funnel/momentum's default —
        // sustained qualification, not ignition's fast scalp).
        println!("=== momentum threshold sweep (edge-triggered signals, swing outcome profile) ===");
        println!("{:<12} {:>8} {:>6} {:>10} {:>11}", "threshold", "signals", "hits", "hit_rate%", "avg_move%");
        for threshold in [0.90, 0.75, 0.65, 0.60, 0.55, 0.50, 0.45] {
            let signals = extract_signals_with_momentum_threshold(&result, threshold);
            let momentum_signals: Vec<_> = signals.iter().filter(|s| s.strategy == Strategy::MomentumScorer).collect();
            let outcomes: Vec<_> = momentum_signals
                .iter()
                .map(|s| {
                    let prices = following_prices(&result, s);
                    evaluate_outcome(s.price, &prices, &OutcomeThresholds::default())
                })
                .collect();
            let metrics = aggregate(&outcomes);
            println!(
                "{:<12.2} {:>8} {:>6} {:>9.1}% {:>10.2}%",
                threshold, metrics.total_signals, metrics.hits, metrics.hit_rate_pct, metrics.avg_move_pct_on_winners
            );
        }
        println!();
    }

    println!(
        "{:<42} {:<36} {:>8} {:>6} {:>10} {:>11}",
        "monitor config", "outcome profile", "signals", "hits", "hit_rate%", "avg_move%"
    );
    println!("{}", "-".repeat(120));

    for (monitor_name, monitor_config) in &monitor_variants {
        let replay_config = ReplayConfig {
            monitor_config: *monitor_config,
            ..ReplayConfig::default()
        };
        let result = run_replay(&data, &replay_config);
        let signals = extract_signals(&result);
        let ignition_signals: Vec<_> = signals
            .iter()
            .filter(|s| s.strategy == Strategy::IgnitionDetector)
            .collect();

        for (outcome_name, outcome_thresholds) in &outcome_variants {
            let outcomes: Vec<_> = ignition_signals
                .iter()
                .map(|s| {
                    let prices = following_prices(&result, s);
                    evaluate_outcome(s.price, &prices, outcome_thresholds)
                })
                .collect();
            let metrics = aggregate(&outcomes);
            println!(
                "{:<42} {:<36} {:>8} {:>6} {:>9.1}% {:>10.2}%",
                monitor_name,
                outcome_name,
                metrics.total_signals,
                metrics.hits,
                metrics.hit_rate_pct,
                metrics.avg_move_pct_on_winners
            );
        }
    }

    Ok(())
}
