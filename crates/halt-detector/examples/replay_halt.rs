//! Verifies `halt-detector` against real historical trade data — dev-only
//! (see Cargo.toml: replay-engine/market-data are dev-dependencies used
//! only here, not by the library itself).
//!
//! Run with:
//! `cargo run -p halt-detector --example replay_halt -- SWVL 2026-08-28T13:30:00Z 2026-08-28T20:00:00Z`

use anyhow::{Context, Result};
use chrono::{TimeZone, Utc};
use halt_detector::{AlertLevel, HaltWarningConfig, HaltWarningMonitor};
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
                "usage: replay_halt <SYMBOL> <START RFC3339> <END RFC3339>\n  e.g. replay_halt SWVL 2026-08-28T13:30:00Z 2026-08-28T20:00:00Z"
            );
        }
    };

    let cfg = AlpacaConfig::from_env().context("loading Alpaca config")?;

    info!(symbol, start, end, "fetching real historical data");
    let data = fetch_replay_data(&cfg, &symbol, &start, &end).await?;
    info!(trades = data.trades.len(), avg_daily_volume = data.avg_daily_volume, "fetched, replaying through halt-detector");

    let mut monitor = HaltWarningMonitor::new(HaltWarningConfig::default(), data.avg_daily_volume);

    let mut calm = 0u32;
    let mut amber = 0u32;
    let mut red = 0u32;
    let mut max_proximity = 0.0_f64;

    for t in &data.trades {
        let now: chrono::DateTime<Utc> = Utc.from_utc_datetime(&t.timestamp.naive_utc());
        let reading = monitor.on_trade(
            halt_detector::Trade {
                timestamp_secs: t.timestamp.timestamp() as f64
                    + t.timestamp.timestamp_subsec_nanos() as f64 / 1_000_000_000.0,
                price: t.price,
                size: t.size,
            },
            now,
        );

        max_proximity = max_proximity.max(reading.proximity_ratio);
        match reading.level {
            AlertLevel::Calm => calm += 1,
            AlertLevel::Amber => amber += 1,
            AlertLevel::Red => {
                red += 1;
                if red <= 5 {
                    info!(
                        timestamp = %t.timestamp,
                        price = reading.current_price,
                        reference = reading.reference_price,
                        band = reading.band_width_dollars,
                        proximity = format!("{:.2}", reading.proximity_ratio),
                        relative_volume = ?reading.relative_volume,
                        "RED reading"
                    );
                }
            }
        }
    }

    info!(
        total_trades = data.trades.len(),
        calm,
        amber,
        red,
        max_proximity = format!("{:.2}", max_proximity),
        "halt-detector replay summary"
    );

    Ok(())
}
