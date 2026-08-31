//! Broader replay-data gathering, per the 2026-08-31 follow-up to the
//! single-session tuning pass: `tune`'s own diagnostic flagged that one
//! SWVL session (n=1 discrete funnel/momentum signal) wasn't enough to
//! validate the fast funnel or momentum scorer, and that quiet
//! (non-qualifying) days were entirely missing from every prior tuning
//! run. This binary fixes both gaps — real historical daily-bar data
//! screens each candidate symbol for its own genuine gap/surge days
//! *and* a genuine ordinary day (see `session_finder`), then every picked
//! session gets a full intraday replay, and every metric below is
//! aggregated across all of them together, not one session at a time.
//!
//! Run with: `cargo run -p backtest-metrics --bin tune_broad`
//!
//! Slower than `tune` (fetches many real sessions from Alpaca/FMP
//! instead of one) — this is a deliberate one-time-per-tuning-pass cost,
//! not something meant to run on every commit.

use anyhow::Result;
use backtest_metrics::{
    aggregate, compute_day_signals, evaluate_outcome, extract_signals,
    extract_signals_with_momentum_threshold, following_prices, pick_sessions, session_window_utc,
    OutcomeThresholds, SessionCategory, Strategy,
};
use ignition_detector::{FollowThroughThresholds, IgnitionThresholds, MonitorConfig};
use market_data::AlpacaConfig;
use replay_engine::{fetch_replay_data, run_replay, ReplayConfig, ReplayData, ReplayResult};

/// The known real low-float small-caps this scanner has already touched
/// live (the funnel's own real Aug 30 shortlist — see
/// stockspotter-open-tasks memory) — a defensible, non-arbitrary starting
/// universe: these are confirmed real Stage-1/2 qualifiers, not guesses,
/// so their history is exactly the kind of data the funnel needs to be
/// judged against (does it *only* fire on their real gap days, not every
/// day?).
const CANDIDATE_SYMBOLS: &[&str] = &["SWVL", "AEHL", "NCRA", "ORIO", "SIEB", "DAVEW", "QNRX", "AREN", "YDDL"];

const LOOKBACK_DAYS: i64 = 75;
const MAX_HOT_PER_SYMBOL: usize = 2;
const MAX_QUIET_PER_SYMBOL: usize = 1;

struct Session {
    symbol: String,
    category: SessionCategory,
    date: chrono::NaiveDate,
    data: ReplayData,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("warn"))
        .init();
    dotenvy::dotenv().ok();

    let cfg = AlpacaConfig::from_env()?;

    // Anchor the daily-bar screen to yesterday's midnight UTC, not
    // `Utc::now()` directly — a still-forming, incomplete "today" bar
    // shouldn't be screened as if it were a finished session.
    let end = (chrono::Utc::now() - chrono::Duration::days(1))
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();
    let start = end - chrono::Duration::days(LOOKBACK_DAYS);

    println!("=== screening {} candidate symbols over the last {LOOKBACK_DAYS} days ===", CANDIDATE_SYMBOLS.len());

    let mut sessions: Vec<Session> = Vec::new();

    for symbol in CANDIDATE_SYMBOLS {
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

        let signals = compute_day_signals(&bars);
        let picks = pick_sessions(&signals, MAX_HOT_PER_SYMBOL, MAX_QUIET_PER_SYMBOL);
        println!(
            "  {symbol}: {} trading days screened -> {} hot, {} quiet picked",
            signals.len(),
            picks.iter().filter(|p| p.category == SessionCategory::Hot).count(),
            picks.iter().filter(|p| p.category == SessionCategory::Quiet).count()
        );

        for pick in picks {
            let (session_start, session_end) = match session_window_utc(pick.date) {
                Ok(w) => w,
                Err(e) => {
                    println!("    {} {}: session window failed ({e}); skipping", symbol, pick.date);
                    continue;
                }
            };
            print!(
                "    fetching {} {} ({:?}, gap={:.1}%, rel_vol={:.1}x)... ",
                symbol, pick.date, pick.category, pick.gap_pct, pick.rel_volume
            );
            use std::io::Write;
            std::io::stdout().flush().ok();

            match fetch_replay_data(&cfg, symbol, &session_start.to_rfc3339(), &session_end.to_rfc3339()).await {
                Ok(data) => {
                    println!(
                        "{} bars, {} trades, {} quotes, float={:?}",
                        data.bars.len(),
                        data.trades.len(),
                        data.quotes.len(),
                        data.float_shares
                    );
                    if data.bars.is_empty() {
                        println!("      (no bars — likely a halted/no-trade day; skipping from the sweep)");
                        continue;
                    }
                    sessions.push(Session {
                        symbol: symbol.to_string(),
                        category: pick.category,
                        date: pick.date,
                        data,
                    });
                }
                Err(e) => println!("failed ({e}); skipping"),
            }
        }
    }

    println!(
        "\n=== fetched {} real sessions ({} hot, {} quiet) across {} symbols — sweeping in-memory from here ===\n",
        sessions.len(),
        sessions.iter().filter(|s| s.category == SessionCategory::Hot).count(),
        sessions.iter().filter(|s| s.category == SessionCategory::Quiet).count(),
        sessions.iter().map(|s| s.symbol.as_str()).collect::<std::collections::HashSet<_>>().len()
    );

    if sessions.is_empty() {
        println!("no sessions fetched — nothing to sweep. Check Alpaca/FMP config and try again.");
        return Ok(());
    }

    println!("sessions actually used in the sweep below:");
    for s in &sessions {
        println!("  {} {} ({:?})", s.symbol, s.date, s.category);
    }
    println!();

    run_funnel_momentum_diagnostic(&sessions);
    run_momentum_threshold_sweep(&sessions);
    run_ignition_sweep(&sessions);

    Ok(())
}

/// Funnel pass-rate and momentum score distribution, split Hot vs Quiet —
/// this split is the whole point of gathering quiet-day data at all. A
/// funnel/momentum that passes about as often on quiet days as hot ones
/// isn't discriminating; before this binary existed there was no data to
/// even check that.
fn run_funnel_momentum_diagnostic(sessions: &[Session]) {
    println!("=== funnel/momentum diagnostic, Hot vs Quiet sessions ===");

    for category in [SessionCategory::Hot, SessionCategory::Quiet] {
        let results: Vec<ReplayResult> = sessions
            .iter()
            .filter(|s| s.category == category)
            .map(|s| run_replay(&s.data, &ReplayConfig::default()))
            .collect();

        let total_bars: usize = results.iter().map(|r| r.bar_events.len()).sum();
        let funnel_passed: usize = results
            .iter()
            .flat_map(|r| &r.bar_events)
            .filter(|e| e.funnel.passed())
            .count();

        let mut momentum_scores: Vec<f64> = results
            .iter()
            .flat_map(|r| &r.bar_events)
            .map(|e| e.momentum.overall)
            .collect();
        momentum_scores.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = momentum_scores.len();

        println!("--- {category:?} ({} sessions, {total_bars} total bars) ---", results.len());
        if total_bars == 0 {
            println!("  no bars; skipping");
            continue;
        }
        println!(
            "  funnel: {funnel_passed}/{total_bars} bars passed all 4 conditions ({:.1}%)",
            funnel_passed as f64 / total_bars as f64 * 100.0
        );
        if n > 0 {
            println!(
                "  momentum.overall distribution: min={:.3} p25={:.3} median={:.3} p75={:.3} max={:.3}",
                momentum_scores[0],
                momentum_scores[n / 4],
                momentum_scores[n / 2],
                momentum_scores[3 * n / 4],
                momentum_scores[n - 1]
            );
        }
    }
    println!();
}

/// Momentum threshold sweep — like `tune`'s single-session version, but
/// with edge-triggered signals combined across every fetched session
/// (both categories), so both the qualifying and non-qualifying days
/// contribute to the hit rate at each candidate threshold.
fn run_momentum_threshold_sweep(sessions: &[Session]) {
    println!("=== momentum threshold sweep (edge-triggered signals across all sessions, swing outcome profile) ===");
    println!("{:<12} {:>8} {:>6} {:>10} {:>11}", "threshold", "signals", "hits", "hit_rate%", "avg_move%");

    let results: Vec<ReplayResult> = sessions.iter().map(|s| run_replay(&s.data, &ReplayConfig::default())).collect();

    for threshold in [0.90, 0.75, 0.65, 0.60, 0.55, 0.50, 0.45] {
        let mut all_outcomes = Vec::new();
        for result in &results {
            let signals = extract_signals_with_momentum_threshold(result, threshold);
            for signal in signals.iter().filter(|s| s.strategy == Strategy::MomentumScorer) {
                let prices = following_prices(result, signal);
                all_outcomes.push(evaluate_outcome(signal.price, &prices, &OutcomeThresholds::default()));
            }
        }
        let metrics = aggregate(&all_outcomes);
        println!(
            "{:<12.2} {:>8} {:>6} {:>9.1}% {:>10.2}%",
            threshold, metrics.total_signals, metrics.hits, metrics.hit_rate_pct, metrics.avg_move_pct_on_winners
        );
    }
    println!();
}

/// Same monitor-config x outcome-profile sweep as `tune`, but with
/// ignition signals combined across every fetched session instead of
/// one.
fn run_ignition_sweep(sessions: &[Session]) {
    let monitor_variants: Vec<(&str, MonitorConfig)> = vec![
        ("baseline (current defaults)", MonitorConfig::default()),
        (
            "confirm=20 trades (current default)",
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
    ];

    let outcome_variants: Vec<(&str, OutcomeThresholds)> = vec![
        ("scalp 2%/2%/10bars (current default)", OutcomeThresholds::scalp()),
        ("target 5%/stop 3%/20bars (swing)", OutcomeThresholds::default()),
        (
            "target 3%/stop 2%/15bars",
            OutcomeThresholds {
                target_pct: 3.0,
                stop_pct: 2.0,
                lookforward_bars: 15,
            },
        ),
    ];

    println!("=== ignition sweep, signals combined across all {} sessions ===", sessions.len());
    println!(
        "{:<42} {:<38} {:>8} {:>6} {:>10} {:>11}",
        "monitor config", "outcome profile", "signals", "hits", "hit_rate%", "avg_move%"
    );
    println!("{}", "-".repeat(122));

    for (monitor_name, monitor_config) in &monitor_variants {
        let results: Vec<ReplayResult> = sessions
            .iter()
            .map(|s| {
                let replay_config = ReplayConfig {
                    monitor_config: *monitor_config,
                    ..ReplayConfig::default()
                };
                run_replay(&s.data, &replay_config)
            })
            .collect();

        for (outcome_name, outcome_thresholds) in &outcome_variants {
            let mut all_outcomes = Vec::new();
            for result in &results {
                let signals = extract_signals(result);
                for signal in signals.iter().filter(|s| s.strategy == Strategy::IgnitionDetector) {
                    let prices = following_prices(result, signal);
                    all_outcomes.push(evaluate_outcome(signal.price, &prices, outcome_thresholds));
                }
            }
            let metrics = aggregate(&all_outcomes);
            println!(
                "{:<42} {:<38} {:>8} {:>6} {:>9.1}% {:>10.2}%",
                monitor_name, outcome_name, metrics.total_signals, metrics.hits, metrics.hit_rate_pct, metrics.avg_move_pct_on_winners
            );
        }
    }
}
