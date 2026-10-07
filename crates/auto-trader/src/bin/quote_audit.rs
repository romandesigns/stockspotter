//! Frozen candidate execution research over cached SIP data. No network or orders.
use anyhow::{Context, Result};
use backtest_metrics::{
    extract_signals,
    quote_execution::{evaluate, Bracket, ExecutionModel},
    Strategy,
};
use chrono::{DateTime, Duration, Utc};
use replay_engine::{run_replay, ReplayConfig, ReplayData};
use std::{
    fs::File,
    io::{BufReader, BufWriter, Write},
    path::Path,
};

fn main() -> Result<()> {
    let output = Path::new("data/quote-execution");
    std::fs::create_dir_all(output)?;
    let dates = [
        "2026-08-20",
        "2026-08-21",
        "2026-08-24",
        "2026-08-26",
        "2026-08-27",
        "2026-08-28",
        "2026-08-31",
        "2026-09-01",
        "2026-09-02",
        "2026-09-03",
        "2026-09-04",
    ];
    let symbols = [
        "SWVL", "QNRX", "CDTG", "AEHL", "SIEB", "AREN", "YDDL", "NCRA", "AAPL",
    ];
    let models: Vec<_> = [
        ("zero_delay_no_extra_slip", 0, 0.0),
        ("250ms_no_extra_slip", 250, 0.0),
        ("baseline_250ms_5bps", 250, 5.0),
        ("slow_1000ms_5bps", 1000, 5.0),
        ("stress_250ms_10bps", 250, 10.0),
    ]
    .into_iter()
    .map(|(name, delay_ms, slippage_bps_per_side)| {
        (
            name,
            ExecutionModel {
                delay_ms,
                slippage_bps_per_side,
                fee_bps_per_side: 1.0,
                max_quote_age_ms: 1000,
                entry_wait_ms: 2000,
                exit_wait_ms: 23_400_000,
            },
        )
    })
    .collect();
    let variants = [
        (
            "ignition_frozen",
            Strategy::IgnitionDetector,
            Bracket {
                target_pct: 1.5,
                stop_pct: 5.0,
                hold_minutes: 20,
            },
        ),
        (
            "ignition_current",
            Strategy::IgnitionDetector,
            Bracket {
                target_pct: 2.0,
                stop_pct: 2.0,
                hold_minutes: 10,
            },
        ),
        (
            "momentum_frozen",
            Strategy::MomentumScorer,
            Bracket {
                target_pct: 2.0,
                stop_pct: 5.0,
                hold_minutes: 30,
            },
        ),
        (
            "momentum_current",
            Strategy::MomentumScorer,
            Bracket {
                target_pct: 5.0,
                stop_pct: 3.0,
                hold_minutes: 20,
            },
        ),
    ];
    std::fs::write(
        output.join("frozen-model.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "dates":dates,"symbols":symbols,"models":models,"variants":variants,
            "entry_window_et":"09:30 inclusive to 15:30 exclusive","forced_exit_decision_et":"15:59",
            "quantity_shares":1,"max_positions_per_symbol_per_variant":1
        }))?,
    )?;
    let mut rows = BufWriter::new(File::create(output.join("executions.partial"))?);
    let mut coverage = Vec::new();
    for date in dates {
        let cohort = if date >= "2026-09-03" {
            "later_dates"
        } else if date <= "2026-08-24" {
            "supplementary_earlier"
        } else {
            "reference"
        };
        let open: DateTime<Utc> = format!("{date}T13:30:00Z").parse()?;
        let latest_entry: DateTime<Utc> = format!("{date}T19:30:00Z").parse()?;
        let close: DateTime<Utc> = format!("{date}T20:00:00Z").parse()?;
        for symbol in symbols {
            let cache = format!("data/audit-validation/{symbol}-{date}-sip-v1.json");
            let mut data: ReplayData =
                serde_json::from_reader(BufReader::new(File::open(&cache).with_context(
                    || format!("missing cache {cache}; finish --execution-dates first"),
                )?))?;
            data.quotes.sort_by_key(|q| q.timestamp);
            let replay = run_replay(&data, &ReplayConfig::default());
            let signals = extract_signals(&replay);
            coverage.push(serde_json::json!({"symbol":symbol,"date":date,"cohort":cohort,"bars":data.bars.len(),"trades":data.trades.len(),"quotes":data.quotes.len(),"signals":signals.len()}));
            for (variant, strategy, bracket) in &variants {
                for (scenario, model) in &models {
                    let mut blocked_until = open - Duration::seconds(1);
                    for signal in signals.iter().filter(|s| s.strategy == *strategy) {
                        let mut row = serde_json::json!({"symbol":symbol,"date":date,"cohort":cohort,"variant":variant,"scenario":scenario,"signal_at":signal.timestamp,"signal_price":signal.price});
                        if signal.timestamp < open || signal.timestamp >= latest_entry {
                            row["execution"] = serde_json::json!({"status":"outside_entry_window"});
                        } else if signal.timestamp <= blocked_until {
                            row["execution"] = serde_json::json!({"status":"overlap_skipped"});
                        } else {
                            let result =
                                evaluate(&data.quotes, signal.timestamp, close, *bracket, *model);
                            blocked_until = match result.status {
                                "filled" => result.exit_at.unwrap(),
                                "unresolved_exit" => close,
                                _ => {
                                    signal.timestamp
                                        + Duration::milliseconds(
                                            model.delay_ms + model.entry_wait_ms,
                                        )
                                }
                            };
                            row["execution"] = serde_json::to_value(result)?;
                        }
                        serde_json::to_writer(&mut rows, &row)?;
                        rows.write_all(b"\n")?;
                    }
                }
            }
            rows.flush()?;
            println!(
                "{cohort} {symbol} {date}: {} quotes, {} signals",
                data.quotes.len(),
                signals.len()
            );
        }
    }
    rows.flush()?;
    rows.get_ref().sync_all()?;
    drop(rows);
    std::fs::rename(
        output.join("executions.partial"),
        output.join("executions.jsonl"),
    )?;
    std::fs::write(
        "docs/quote-execution-coverage-2026-09-06.json",
        serde_json::to_vec_pretty(&coverage)?,
    )?;
    println!("Completed {} symbol/date combinations", coverage.len());
    Ok(())
}
