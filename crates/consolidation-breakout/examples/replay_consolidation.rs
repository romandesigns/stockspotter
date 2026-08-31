//! Verifies `consolidation-breakout` against real historical bar data —
//! dev-only (see Cargo.toml: replay-engine/market-data are
//! dev-dependencies used only here, not by the library itself).
//!
//! Run with:
//! `cargo run -p consolidation-breakout --example replay_consolidation -- SWVL 2026-08-28T13:30:00Z 2026-08-28T20:00:00Z`

use anyhow::{Context, Result};
use consolidation_breakout::{Candle, ConsolidationBreakoutConfig, ConsolidationBreakoutEvent, ConsolidationBreakoutMonitor};
use market_data::AlpacaConfig;
use replay_engine::fetch_replay_data;
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
                "usage: replay_consolidation <SYMBOL> <START RFC3339> <END RFC3339>\n  e.g. replay_consolidation SWVL 2026-08-28T13:30:00Z 2026-08-28T20:00:00Z"
            );
        }
    };

    let cfg = AlpacaConfig::from_env().context("loading Alpaca config")?;

    info!(symbol, start, end, "fetching real historical data");
    let data = fetch_replay_data(&cfg, &symbol, &start, &end).await?;
    info!(bars = data.bars.len(), "fetched, replaying through consolidation-breakout");

    let mut monitor = ConsolidationBreakoutMonitor::new(ConsolidationBreakoutConfig::default());

    let mut surges = 0u32;
    let mut confirmed = 0u32;
    let mut entries = 0u32;

    for bar in &data.bars {
        let candle = Candle {
            open: bar.open,
            high: bar.high,
            low: bar.low,
            close: bar.close,
            volume: bar.volume,
        };
        match monitor.on_candle(candle) {
            ConsolidationBreakoutEvent::None => {}
            ConsolidationBreakoutEvent::SurgeDetected { low, high } => {
                surges += 1;
                info!(timestamp = %bar.timestamp, low, high, "surge detected, tracking consolidation");
            }
            ConsolidationBreakoutEvent::ConsolidationConfirmed { consolidation_high, support } => {
                confirmed += 1;
                info!(
                    timestamp = %bar.timestamp,
                    price = bar.close,
                    consolidation_high,
                    support,
                    "consolidation confirmed"
                );
            }
            ConsolidationBreakoutEvent::EntryTriggered { price } => {
                entries += 1;
                info!(timestamp = %bar.timestamp, price, "ENTRY TRIGGERED (breakout)");
            }
        }
    }

    info!(
        total_bars = data.bars.len(),
        surges_detected = surges,
        consolidations_confirmed = confirmed,
        entries_triggered = entries,
        "consolidation-breakout replay summary"
    );

    Ok(())
}
