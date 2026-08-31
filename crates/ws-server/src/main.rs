//! stockspotter's realtime WS backend skeleton — the piece
//! `stockspotter-open-tasks` (and the architecture doc's UI/panel layer)
//! has been waiting on: a server every client (web, desktop, mobile)
//! connects to and gets fed the *exact same* detection events, live.
//!
//! Runs `market_data::run_live_scan` once, in the background, broadcasting
//! every `ScanEvent` it produces to all connected WS clients via
//! `server.rs` — see that module's doc comment for the actual "same
//! notifications to everyone" guarantee.
//!
//! Run with: `cargo run -p ws-server` (from the repo root, so `.env` is
//! found). Listens on `WS_SERVER_ADDR` (default `127.0.0.1:8787`).

mod protocol;
mod server;

use anyhow::Result;
use market_data::{run_live_scan, AlpacaConfig};
use tokio::sync::broadcast;
use tracing::{error, info};

// The fast funnel's own real Stage 1/2 shortlist, from running
// `market-data --bin scan_universe` against the live full universe
// (13,378 symbols) fresh this morning — not a frozen list. Gap% and
// relative volume are per-day conditions; yesterday's (Aug 30) shortlist
// was reused briefly last night, but stayed valid dead code past that
// day's own session — a shortlist this stale is standing in for the
// funnel, not actually running it. Re-run scan_universe each trading
// day and refresh this list before market open; all 9 of Aug 30's
// symbols still qualified as of this scan (2026-08-31 ~01:33 ET,
// pre-market), plus 7 new ones.
const WATCH_SYMBOLS: &[&str] = &[
    "NCRA", "SIEB", "CHAI", "WCT", "YDDL", "QNRX", "ORIO", "SWVL", "DUO", "AREN", "DAVEW", "AEHL", "CLGN", "COOT",
    "YDESW", "IDACW",
];
const DEFAULT_ADDR: &str = "127.0.0.1:8787";
/// How many events a lagging client can fall behind by before it starts
/// missing them (`broadcast::error::RecvError::Lagged`) — generous for
/// this symbol count/event rate.
const BROADCAST_CAPACITY: usize = 1024;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .init();
    dotenvy::dotenv().ok();

    let addr = std::env::var("WS_SERVER_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_string());
    let cfg = AlpacaConfig::from_env()?;
    let symbols: Vec<String> = WATCH_SYMBOLS.iter().map(|s| s.to_string()).collect();

    let (tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);

    let scan_tx = tx.clone();
    let scan_symbols = symbols.clone();
    let scan_cfg = cfg.clone();
    let scan_handle = tokio::spawn(async move {
        // `run_live_scan` exits on its own IDLE_TIMEOUT (20s of no new
        // messages) or on a stream error — by design for the CLI demo
        // binary it also powers, where a finite run is correct. A
        // persistent server can't just let the feed go stale after one
        // quiet moment (confirmed live: clients could still connect and
        // "successfully" handshake for a data feed that had already
        // died) — so this wraps it in an unconditional reconnect loop
        // instead. The WS server itself (already-connected clients,
        // accept loop) is unaffected by a reconnect; only the upstream
        // Alpaca connection restarts.
        loop {
            match run_live_scan(&scan_cfg, &scan_symbols, scan_tx.clone()).await {
                Ok(()) => info!("live scan loop ended (idle timeout or stream closed), reconnecting"),
                Err(e) => error!(error = %e, "live scan loop exited with an error, reconnecting"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });

    info!(addr, symbols = ?symbols, "starting ws server");
    server::run(&addr, tx).await?;

    scan_handle.abort();
    Ok(())
}
