//! stockspotter's realtime WS backend — the piece
//! `stockspotter-open-tasks` (and the architecture doc's UI/panel layer)
//! has been waiting on: a server every client (web, desktop, mobile)
//! connects to and gets fed the *exact same* detection events, live.
//!
//! Runs `market_data::run_live_scan` in the background, broadcasting
//! every `ScanEvent` it produces to all connected WS clients via
//! `server.rs` — see that module's doc comment for the actual "same
//! notifications to everyone" guarantee.
//!
//! **No hardcoded watchlist.** Earlier versions of this file passed
//! `run_live_scan` a fixed symbol list (first 5 arbitrary test symbols,
//! later the funnel's real Aug 30 output frozen in place) — both had the
//! same real problem: gap%/relative-volume are per-day conditions, so a
//! watchlist that doesn't refresh itself isn't actually running the
//! funnel, it's standing in for it with a stale answer. `run_live_scan`
//! now runs the full Stage 1/2 universe scan on its own schedule
//! internally and dynamically subscribes/unsubscribes symbols as they
//! start/stop qualifying — see `market_data::live`'s doc comment for the
//! full "two loops" design. This binary just starts it with an empty
//! seed and lets it discover the real watchlist itself.
//!
//! Run with: `cargo run -p ws-server` (from the repo root, so `.env` is
//! found). Listens on `WS_SERVER_ADDR` (default `127.0.0.1:8787`).

mod http;
mod protocol;
mod server;

use std::sync::Arc;

use anyhow::Result;
use market_data::{run_live_scan, spawn_periodic_movers_scan, AlpacaConfig, TodayMovers};
use tokio::sync::{broadcast, RwLock};
use tracing::{error, info};

const DEFAULT_ADDR: &str = "127.0.0.1:8787";
/// Historical-bars backfill endpoint (http.rs) -- separate port since a
/// raw WS listener (tokio-tungstenite::accept_async) can't also serve
/// plain HTTP GET requests on the same socket.
const DEFAULT_HTTP_ADDR: &str = "127.0.0.1:8788";
/// How many events a lagging client can fall behind by before it starts
/// missing them (`broadcast::error::RecvError::Lagged`) — generous for
/// the expected symbol count/event rate.
const BROADCAST_CAPACITY: usize = 1024;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .init();
    dotenvy::dotenv().ok();

    let addr = std::env::var("WS_SERVER_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_string());
    let cfg = AlpacaConfig::from_env()?;

    let (tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);

    let scan_tx = tx.clone();
    let scan_cfg = cfg.clone();
    let scan_handle = tokio::spawn(async move {
        // `run_live_scan` exits on its own IDLE_TIMEOUT (a real dead-
        // connection safety net now, see market_data::live's doc
        // comment) or on a stream error — by design for the CLI demo
        // binary it also powers, where a finite run is correct. A
        // persistent server can't just let the feed go stale after one
        // quiet moment (confirmed live: clients could still connect and
        // "successfully" handshake for a data feed that had already
        // died) — so this wraps it in an unconditional reconnect loop
        // instead. The WS server itself (already-connected clients,
        // accept loop) is unaffected by a reconnect; only the upstream
        // Alpaca connection restarts, and a fresh universe scan runs
        // again as soon as it reconnects.
        loop {
            match run_live_scan(&scan_cfg, &[], scan_tx.clone()).await {
                Ok(()) => info!("live scan loop ended (idle timeout or stream closed), reconnecting"),
                Err(e) => error!(error = %e, "live scan loop exited with an error, reconnecting"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });

    // Top Gainers / Highly Trading's live rankings -- kept as its own
    // independent background task (movers.rs) and shared state, entirely
    // decoupled from the funnel/qualification loop above (see
    // market_data::movers's doc comment).
    let today_movers = Arc::new(RwLock::new(TodayMovers::default()));
    let movers_handle = spawn_periodic_movers_scan(cfg.clone(), today_movers.clone());

    let http_addr = std::env::var("HTTP_SERVER_ADDR").unwrap_or_else(|_| DEFAULT_HTTP_ADDR.to_string());
    let http_cfg = cfg.clone();
    let http_addr_for_spawn = http_addr.clone();
    let http_movers = today_movers.clone();
    let http_handle = tokio::spawn(async move {
        if let Err(e) = http::run(&http_addr_for_spawn, http_cfg, http_movers).await {
            error!(error = %e, "historical-bars http server exited with an error");
        }
    });

    info!(addr, http_addr, "starting ws server — watchlist is self-discovered via the universe scan, not fixed");
    server::run(&addr, tx).await?;

    http_handle.abort();
    movers_handle.abort();
    scan_handle.abort();
    Ok(())
}
