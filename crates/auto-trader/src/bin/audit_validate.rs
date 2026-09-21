//! Isolated Alpaca validation. Never writes to the live journal or strategy configuration.
use anyhow::{Context, Result};
use backtest_metrics::{
    aggregate_by_strategy, evaluate_outcome, extract_signals, following_prices, forward_path_pct,
    LoggedSignal, OutcomeThresholds,
};
use chrono::{Duration, Utc};
use market_data::ScanEvent;
use replay_engine::{fetch_replay_data, run_replay, ReplayConfig, ReplayData};
use std::path::Path;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let cfg = market_data::AlpacaConfig::from_env()?;
    let expanded = std::env::args().any(|a| a == "--expanded");
    let execution_dates = std::env::args().any(|a| a == "--execution-dates");
    let root = Path::new(if execution_dates {
        "data/execution-holdout"
    } else if expanded {
        "data/historical-expanded"
    } else {
        "data/audit-validation"
    });
    let cache_root = Path::new("data/audit-validation");
    std::fs::create_dir_all(root)?;
    std::fs::create_dir_all(cache_root)?;
    let mut cases = if execution_dates {
        let symbols = [
            "SWVL", "QNRX", "CDTG", "AEHL", "SIEB", "AREN", "YDDL", "NCRA", "AAPL",
        ];
        [
            "2026-08-20",
            "2026-08-21",
            "2026-08-24",
            "2026-09-03",
            "2026-09-04",
        ]
        .into_iter()
        .flat_map(|date| symbols.into_iter().map(move |symbol| (symbol, date)))
        .collect::<Vec<_>>()
    } else if expanded {
        let symbols = [
            "SWVL", "QNRX", "CDTG", "AEHL", "SIEB", "AREN", "YDDL", "NCRA", "AAPL",
        ];
        [
            "2026-08-26",
            "2026-08-27",
            "2026-08-28",
            "2026-08-31",
            "2026-09-01",
            "2026-09-02",
        ]
        .into_iter()
        .flat_map(|date| symbols.into_iter().map(move |symbol| (symbol, date)))
        .collect::<Vec<_>>()
    } else {
        vec![
            ("SWVL", "2026-08-28"),
            ("QNRX", "2026-08-28"),
            ("CDTG", "2026-08-18"),
            ("AAPL", "2026-09-04"),
        ]
    };
    cases.sort_by_key(|(_, date)| *date);
    std::fs::write(root.join("cases.json"), serde_json::to_vec_pretty(&cases)?)?;
    let trader_config = auto_trader::config::Config::from_env();
    let mut engine = auto_trader::engine::Engine::new(trader_config.clone());
    let mut journal = Vec::new();
    let mut previous_date = None;
    let mut all_signals = Vec::new();
    let mut timeline = Vec::new();
    let mut sessions = Vec::new();
    for (symbol, date) in cases {
        if previous_date.is_some_and(|previous| previous != date) {
            process_timeline(&mut timeline, &mut engine, &mut journal);
        }
        previous_date = Some(date);
        let cache = cache_root.join(format!("{symbol}-{date}-{}-v1.json", cfg.feed));
        let start = format!("{date}T08:00:00Z"); // 04:00 EDT, includes premarket warmup
        let end = format!("{date}T20:00:00Z");
        println!("loading {symbol} {date} ({})", cfg.feed);
        let data: ReplayData = if cache.exists() {
            serde_json::from_reader(std::io::BufReader::new(std::fs::File::open(&cache)?))?
        } else {
            let data = tokio::time::timeout(
                std::time::Duration::from_secs(600),
                fetch_replay_data(&cfg, symbol, &start, &end),
            )
            .await
            .context("historical session fetch exceeded 10 minutes")?
            .context(format!("fetching {symbol} {date}"))?;
            let tmp = cache.with_extension("partial");
            serde_json::to_writer(std::io::BufWriter::new(std::fs::File::create(&tmp)?), &data)?;
            std::fs::rename(tmp, &cache)?;
            data
        };
        if data.bars.is_empty() {
            println!(
                "no bars returned for {symbol} {date}; retaining this session in coverage counts"
            );
        }
        let started = std::time::Instant::now();
        let replay = run_replay(&data, &ReplayConfig::default());
        let elapsed = started.elapsed().as_secs_f64();
        for signal in extract_signals(&replay) {
            let prices = following_prices(&replay, &signal);
            all_signals.push(LoggedSignal {
                symbol: symbol.into(),
                strategy: signal.strategy,
                timestamp: signal.timestamp,
                signal_price: signal.price,
                outcome: evaluate_outcome(
                    signal.price,
                    &prices,
                    &OutcomeThresholds::for_strategy(signal.strategy),
                ),
                logged_at: Utc::now(),
                forward_path_pct: forward_path_pct(signal.price, &prices),
            });
        }
        for b in &data.bars {
            timeline.push((
                b.timestamp + Duration::minutes(1),
                0,
                ScanEvent::BarUpdate { coverage: market_data::events::Coverage::Unknown,
                    symbol: symbol.into(),
                    timestamp: b.timestamp,
                    open: b.open,
                    high: b.high,
                    low: b.low,
                    close: b.close,
                    volume: b.volume,
                    interval_secs: 60,
                    is_final: true,
                },
            ));
        }
        for b in &replay.bar_events {
            timeline.push((
                b.timestamp,
                1,
                ScanEvent::MomentumUpdate {
                    symbol: symbol.into(),
                    timestamp: b.timestamp,
                    overall: b.momentum.overall,
                    volume_confirmation: b.momentum.volume_confirmation,
                    structure: b.momentum.structure,
                    ma_slope: b.momentum.ma_slope,
                    wick_rejection: b.momentum.wick_rejection,
                    qualifies: b.momentum.qualifies(momentum_threshold()),
                },
            ));
        }
        for e in &replay.ignition_events {
            let kind = match e.kind {
                replay_engine::IgnitionEventKind::CandidateOpened => {
                    market_data::IgnitionEventKind::CandidateOpened
                }
                replay_engine::IgnitionEventKind::FollowThroughConfirmed => {
                    market_data::IgnitionEventKind::FollowThroughConfirmed
                }
                replay_engine::IgnitionEventKind::FollowThroughRejected => {
                    market_data::IgnitionEventKind::FollowThroughRejected
                }
            };
            timeline.push((
                e.timestamp,
                2,
                ScanEvent::IgnitionEvent {
                    symbol: symbol.into(),
                    timestamp: e.timestamp,
                    price: e.price,
                    kind,
                },
            ));
        }
        for e in &replay.consolidation_events {
            let kind = match e.kind {
                replay_engine::ConsolidationEventKind::SurgeDetected => {
                    market_data::ConsolidationEventKind::SurgeDetected
                }
                replay_engine::ConsolidationEventKind::ConsolidationConfirmed => {
                    market_data::ConsolidationEventKind::ConsolidationConfirmed
                }
                replay_engine::ConsolidationEventKind::EntryTriggered => {
                    market_data::ConsolidationEventKind::EntryTriggered
                }
            };
            timeline.push((
                e.timestamp,
                2,
                ScanEvent::ConsolidationEvent {
                    symbol: symbol.into(),
                    timestamp: e.timestamp,
                    price: e.price,
                    kind,
                    strategy: e.strategy,
                },
            ));
        }
        for e in replay.halt_events {
            if let ScanEvent::HaltWarning { timestamp, .. } = &e {
                timeline.push((*timestamp, 0, e));
            }
        }
        sessions.push(serde_json::json!({"symbol":symbol,"date":date,"bars":data.bars.len(),"trades":data.trades.len(),"quotes":data.quotes.len(),
            "prior_close":data.prior_close,"avg_daily_volume":data.avg_daily_volume,"historical_float":data.float_shares,
            "replay_seconds":elapsed}));
        println!(
            "replayed {symbol}: {} bars, {} trades, {:.3}s",
            data.bars.len(),
            data.trades.len(),
            elapsed
        );
    }
    process_timeline(&mut timeline, &mut engine, &mut journal);
    let pairs: Vec<_> = all_signals
        .iter()
        .filter(|s| !s.forward_path_pct.is_empty())
        .map(|s| (s.strategy, s.outcome))
        .collect();
    let cost = backtest_metrics::round_trip_cost_pct();
    let metrics: Vec<_> = aggregate_by_strategy(&pairs).into_iter().map(|(s,m)|serde_json::json!({
        "strategy":format!("{s:?}"),"signals":m.total_signals,"hit_rate_pct":m.hit_rate_pct,
        "gross_mean_pct":m.real_expectancy_pct,"net_mean_pct":m.real_expectancy_pct.map(|v|v-cost)
    })).collect();
    let report = serde_json::json!({"generated_at":Utc::now(),"feed":cfg.feed,"round_trip_cost_pct":cost,"sessions":sessions,"strategies":metrics,
        "signals_detected":all_signals.len(),"signals_evaluated":pairs.len(),"signals_without_forward_prices":all_signals.len()-pairs.len(),
        "simulated_position_size_usd":trader_config.position_size_usd,"simulated_max_concurrent_positions":trader_config.max_concurrent_positions,
        "simulated_closed_trades":engine.stats.trades,"simulated_net_pnl_usd":engine.stats.cumulative_pnl_usd,
        "limitations":["Selected-symbol regression sample, not a profitability study","Minute-close outcomes and simulated fills",
            "Historical float unavailable; funnel fails closed","Fixed symbol coverage does not validate historical discovery or subscription lifecycle",
            "Halt bands are estimates, not official SIP LULD bands"]});
    std::fs::write(
        if execution_dates {
            "docs/execution-holdout-data-2026-09-06.json"
        } else if expanded {
            "docs/historical-expanded-2026-09-06.json"
        } else {
            "docs/audit-validation-2026-09-06.json"
        },
        serde_json::to_vec_pretty(&report)?,
    )?;
    // One independent output file per validation run; original research evidence stays untouched.
    let log = all_signals
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .join("\n");
    std::fs::write(root.join("signals.jsonl"), format!("{log}\n"))?;
    std::fs::write(
        root.join("trader-journal.json"),
        serde_json::to_vec_pretty(&journal)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
fn momentum_threshold() -> f64 {
    0.6
}

fn process_timeline(
    timeline: &mut Vec<(chrono::DateTime<Utc>, i32, ScanEvent)>,
    engine: &mut auto_trader::engine::Engine,
    journal: &mut Vec<auto_trader::journal::JournalEntry>,
) {
    timeline.sort_by_key(|e| (e.0, e.1));
    for (_, _, event) in timeline.drain(..) {
        journal.extend(engine.on_event(&event));
    }
}
