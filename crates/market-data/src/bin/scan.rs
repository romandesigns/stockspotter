//! Runnable proof that live Alpaca data flows through the *same*
//! `fast_funnel` functions the (future) replay engine will use — the
//! architecture doc's "one code path, live or replay" requirement, made
//! real for the first time rather than just stated.
//!
//! Watches a small fixed symbol list (the same real small-caps used by the
//! Super Chart prototype), seeds each from Alpaca's daily-bars REST
//! endpoint, then streams realtime bars and logs each one's fast-funnel
//! verdict. Exits cleanly after an idle period with no bars — expected
//! outside market hours — rather than hanging forever.
//!
//! Run with: `cargo run -p market-data --bin scan` (from the repo root, so
//! `.env` is found).

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use fast_funnel::{explain, FilterThresholds};
use market_data::{fetch_daily_seeds, AlpacaConfig, AlpacaMessage, AlpacaStream, SessionTracker};
use tracing::{info, warn};

const WATCH_SYMBOLS: &[&str] = &["SWVL", "WCT", "BCAB", "VISN", "WETO"];
const IDLE_TIMEOUT: Duration = Duration::from_secs(20);
const DAILY_LOOKBACK: u32 = 20;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .init();
    dotenvy::dotenv().ok();

    let cfg = AlpacaConfig::from_env()?;
    let thresholds = FilterThresholds::default();
    let symbols: Vec<String> = WATCH_SYMBOLS.iter().map(|s| s.to_string()).collect();

    info!(?symbols, "seeding prior close / avg daily volume from alpaca rest");
    let seeds = fetch_daily_seeds(&cfg, &symbols, DAILY_LOOKBACK).await?;

    let mut trackers: HashMap<String, SessionTracker> = HashMap::new();
    for symbol in &symbols {
        match seeds.get(symbol) {
            Some(seed) => {
                info!(
                    symbol,
                    prior_close = seed.prior_close,
                    avg_daily_volume = seed.avg_daily_volume,
                    "seeded"
                );
                // float_shares: None — Alpaca has no float endpoint; see
                // SessionTracker's doc comment. Stage 1 fails closed on
                // this until a float source is wired in.
                trackers.insert(
                    symbol.clone(),
                    SessionTracker::new(symbol.clone(), seed.prior_close, seed.avg_daily_volume, None),
                );
            }
            None => warn!(symbol, "no seed data; bars for this symbol will be skipped"),
        }
    }

    info!(ws = %cfg.market_ws, "connecting to alpaca realtime stream");
    let mut stream = AlpacaStream::connect(&cfg, &symbols).await?;
    info!(idle_timeout = ?IDLE_TIMEOUT, "connected + subscribed, waiting for bars");

    let mut bars_seen = 0u32;
    loop {
        let batch = match tokio::time::timeout(IDLE_TIMEOUT, stream.next_batch()).await {
            Ok(Ok(Some(batch))) => batch,
            Ok(Ok(None)) => {
                info!("alpaca closed the stream");
                break;
            }
            Ok(Err(e)) => {
                warn!(error = %e, "stream error");
                break;
            }
            Err(_) => {
                info!(
                    bars_seen,
                    "idle timeout with no new bars — expected outside market hours, exiting cleanly"
                );
                break;
            }
        };

        for msg in batch {
            let AlpacaMessage::Bar(bar) = msg else { continue };
            bars_seen += 1;
            let Some(tracker) = trackers.get_mut(&bar.symbol) else {
                continue;
            };
            let snapshot = tracker.on_bar(&bar);
            let verdict = explain(&snapshot, &thresholds);
            info!(
                symbol = %bar.symbol,
                price = snapshot.price,
                gap_pct = format!("{:.2}", snapshot.gap_pct),
                session_volume = snapshot.session_volume,
                rel_vol_ok = verdict.rel_vol_ok,
                gap_ok = verdict.gap_ok,
                float_ok = verdict.float_ok,
                passed = verdict.passed(),
                "bar processed through fast funnel"
            );
        }
    }

    info!(bars_seen, "scan run finished");
    Ok(())
}
