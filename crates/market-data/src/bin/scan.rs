//! Runnable proof that live Alpaca data flows through the *same*
//! `fast_funnel` functions the replay engine and WS server also use —
//! the architecture doc's "one code path" requirement.
//!
//! Watches a small fixed symbol list (the same real small-caps used by the
//! Super Chart prototype) via `market_data::run_live_scan`. Just a thin
//! CLI wrapper now — see `live.rs` for the actual loop, which `ws-server`
//! also calls, broadcasting the same events this only logs.
//!
//! Run with: `cargo run -p market-data --bin scan` (from the repo root, so
//! `.env` is found).

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use market_data::{run_live_scan, AlpacaConfig, TodayMovers};
use tokio::sync::{broadcast, RwLock};

const WATCH_SYMBOLS: &[&str] = &["SWVL", "WCT", "BCAB", "VISN", "WETO"];

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .init();
    dotenvy::dotenv().ok();

    let cfg = AlpacaConfig::from_env()?;
    let symbols: Vec<String> = WATCH_SYMBOLS.iter().map(|s| s.to_string()).collect();

    // This demo binary doesn't have any WS clients to broadcast to — keep
    // the receiver alive so `run_live_scan`'s sends don't just get
    // dropped for lack of a listener, but never actually read from it.
    let (tx, _rx) = broadcast::channel(256);
    // This demo binary has no REST layer to serve a catalysts backfill
    // from -- a throwaway map, written but never read.
    let catalysts = Arc::new(RwLock::new(HashMap::new()));
    // Real but never updated -- this demo binary doesn't run movers.rs's
    // own background scan, so the halt-watch tick this feeds just no-ops
    // every cycle (empty gainers/most_active). Intentional, not a gap:
    // this is a fixed-symbol funnel demo, not the real server.
    let movers = Arc::new(RwLock::new(TodayMovers::default()));
    run_live_scan(&cfg, &symbols, tx, catalysts, movers).await
}
