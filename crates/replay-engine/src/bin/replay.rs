//! Runnable proof that replay uses the exact same detection code as
//! `market-data`'s live `bin/scan.rs` — just fed historical data instead
//! of a live WebSocket. Prints what the funnel/momentum scorer/ignition
//! detector *would have* said at each historical moment, exactly as if
//! this had been a live session.
//!
//! Run with:
//! `cargo run -p replay-engine --bin replay -- SWVL 2026-08-28T13:30:00Z 2026-08-28T14:30:00Z`

use anyhow::{Context, Result};
use market_data::AlpacaConfig;
use replay_engine::replay_symbol;
use tracing::info;

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
                "usage: replay <SYMBOL> <START RFC3339> <END RFC3339>\n  e.g. replay SWVL 2026-08-28T13:30:00Z 2026-08-28T14:30:00Z"
            );
        }
    };

    let cfg = AlpacaConfig::from_env().context("loading Alpaca config")?;

    info!(symbol, start, end, "replaying historical data through the live detection pipeline");
    let result = replay_symbol(&cfg, &symbol, &start, &end).await?;

    let funnel_passed = result
        .bar_events
        .iter()
        .filter(|e| e.funnel.passed())
        .count();
    let momentum_qualified = result
        .bar_events
        .iter()
        .filter(|e| e.momentum.qualifies(momentum_scorer::DEFAULT_QUALIFY_THRESHOLD))
        .count();

    info!(
        bars = result.bar_events.len(),
        funnel_passed,
        momentum_qualified,
        ignition_events = result.ignition_events.len(),
        "replay summary"
    );

    for event in &result.bar_events {
        if event.funnel.passed() || event.momentum.qualifies(momentum_scorer::DEFAULT_QUALIFY_THRESHOLD) {
            info!(
                timestamp = %event.timestamp,
                price = event.price,
                gap_pct = format!("{:.2}", event.gap_pct),
                funnel_passed = event.funnel.passed(),
                momentum_overall = format!("{:.2}", event.momentum.overall),
                "bar of interest"
            );
        }
    }

    for event in &result.ignition_events {
        info!(
            timestamp = %event.timestamp,
            price = event.price,
            kind = ?event.kind,
            "ignition event"
        );
    }

    Ok(())
}
