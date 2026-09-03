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

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use backtest_metrics::{append_pending, LiveSignalTracker};
use market_data::{run_live_scan, spawn_periodic_movers_scan, AlpacaConfig, TodayMovers};
use tokio::sync::{broadcast, RwLock};
use tracing::{error, info, warn};

// Bound to 0.0.0.0, not 127.0.0.1 -- this server has real non-localhost
// clients now (apps/mobile, over LAN or the tailnet per
// stockspotter-client-architecture's own "phone joins the tailnet
// directly" decision), and a loopback-only bind is unreachable from
// anywhere but this exact machine. Found live: the desktop web client
// (served from and run on the same machine) connected fine while the
// mobile app showed nothing at all -- not a data bug, a bind address
// that silently only ever worked for same-machine callers.
const DEFAULT_ADDR: &str = "0.0.0.0:8787";
/// Historical-bars backfill endpoint (http.rs) -- separate port since a
/// raw WS listener (tokio-tungstenite::accept_async) can't also serve
/// plain HTTP GET requests on the same socket.
const DEFAULT_HTTP_ADDR: &str = "0.0.0.0:8788";
/// How many events a lagging client can fall behind by before it starts
/// missing them (`broadcast::error::RecvError::Lagged`) — generous for
/// the expected symbol count/event rate.
const BROADCAST_CAPACITY: usize = 1024;
/// Live detection-efficiency benchmark (2026-09-03, Roman's own ask —
/// see `backtest_metrics::live_signals`' doc comment for the full
/// design). Relative to this process's CWD (`/app` in the container,
/// see the Dockerfile) — `docker-compose.yml`'s own `ws` service now
/// bind-mounts `/app/data` to a real host directory specifically so
/// this survives a redeploy; without that mount every pending signal
/// would be lost on the next `deploy.sh` run before most of them ever
/// reach their own evaluation window.
const LIVE_PENDING_SIGNALS_PATH: &str = "data/live_pending_signals.jsonl";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .init();
    dotenvy::dotenv().ok();

    let addr = std::env::var("WS_SERVER_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_string());
    let cfg = AlpacaConfig::from_env()?;

    let (tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);

    // Catalysts panel's backfill for a newly-connecting client -- see
    // market_data::live::run_live_scan's own doc comment on why the
    // live broadcast alone isn't enough for a one-shot-per-promotion
    // event type.
    let catalysts = Arc::new(RwLock::new(HashMap::new()));

    // Top Gainers / Highly Trading's live rankings -- created here (moved
    // up from after scan_handle's spawn below) because run_live_scan now
    // reads it too: halt-risk monitoring has its own independent trigger
    // off this same leaderboard now, not just Stage 1/2 qualification
    // (see market_data::live's HALT_WATCH_REFRESH_INTERVAL doc comment).
    // spawn_periodic_movers_scan is still the only writer; run_live_scan
    // is just a second reader of the same handle.
    let today_movers = Arc::new(RwLock::new(TodayMovers::default()));

    let scan_tx = tx.clone();
    let scan_cfg = cfg.clone();
    let scan_catalysts = catalysts.clone();
    let scan_movers = today_movers.clone();
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
            match run_live_scan(&scan_cfg, &[], scan_tx.clone(), scan_catalysts.clone(), scan_movers.clone()).await {
                Ok(()) => info!("live scan loop ended (idle timeout or stream closed), reconnecting"),
                Err(e) => error!(error = %e, "live scan loop exited with an error, reconnecting"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });

    // Top Gainers / Highly Trading's own background scan (movers.rs) --
    // kept as its own independent task/schedule from the funnel/
    // qualification loop above (see market_data::movers's doc comment).
    // `today_movers` itself is created earlier now, before scan_handle's
    // spawn, since run_live_scan reads it too.
    let movers_handle = spawn_periodic_movers_scan(cfg.clone(), today_movers.clone());

    // Live detection-efficiency collector (2026-09-03) -- a second,
    // independent subscriber on the same broadcast channel every WS
    // client also reads from (`broadcast::Sender::subscribe` supports
    // any number of independent receivers; this one is purely additive,
    // doesn't affect client fanout at all). Feeds every event through
    // `LiveSignalTracker`'s pure edge-trigger logic and appends whatever
    // real signal moments it finds -- `bin/live_efficiency` is the
    // separate process that later evaluates them against real
    // subsequent price action. See backtest_metrics::live_signals' own
    // doc comment for why this lives in ws-server rather than a
    // standalone WS client: it's the one process already holding the
    // live event stream in-process, no second connection needed.
    let mut signal_rx = tx.subscribe();
    let signal_handle = tokio::spawn(async move {
        let mut tracker = LiveSignalTracker::new();
        let path = std::path::Path::new(LIVE_PENDING_SIGNALS_PATH);
        loop {
            match signal_rx.recv().await {
                Ok(event) => {
                    if let Some(signal) = tracker.on_event(&event, chrono::Utc::now()) {
                        if let Err(e) = append_pending(path, &[signal]) {
                            warn!(error = %e, "failed to persist a live detection-efficiency signal");
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    // Same real tradeoff as a WS client falling behind
                    // (server.rs) -- a lagged edge-trigger read could
                    // theoretically miss a qualify/pass edge, undercounting
                    // signals slightly rather than double-counting. Not
                    // fatal to the benchmark's own validity (a missed
                    // signal just isn't logged, doesn't corrupt the ones
                    // that were), logged so a persistent lag pattern is
                    // visible rather than silently swallowed.
                    warn!(skipped, "live signal collector lagged behind the broadcast channel, some events missed");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    let http_addr = std::env::var("HTTP_SERVER_ADDR").unwrap_or_else(|_| DEFAULT_HTTP_ADDR.to_string());
    let http_cfg = cfg.clone();
    let http_addr_for_spawn = http_addr.clone();
    let http_movers = today_movers.clone();
    let http_catalysts = catalysts.clone();
    let http_handle = tokio::spawn(async move {
        if let Err(e) = http::run(&http_addr_for_spawn, http_cfg, http_movers, http_catalysts).await {
            error!(error = %e, "historical-bars http server exited with an error");
        }
    });

    info!(addr, http_addr, "starting ws server — watchlist is self-discovered via the universe scan, not fixed");
    server::run(&addr, tx).await?;

    http_handle.abort();
    movers_handle.abort();
    scan_handle.abort();
    signal_handle.abort();
    Ok(())
}
