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

mod access;
mod auto_trader_status;
mod http;
mod measurement;
mod protocol;
mod push;
mod server;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use backtest_metrics::{append_pending, LiveSignalTracker};
use market_data::{run_live_scan, spawn_periodic_movers_scan, AlpacaConfig, IgnitionEventKind, ScanEvent, TodayMovers};
use tokio::sync::{broadcast, RwLock};
use tracing::{error, info, warn};

// Local development defaults to loopback. Network listeners require an
// explicit address and a private access key; compose sets both addresses.
const DEFAULT_ADDR: &str = "127.0.0.1:8787";
/// Historical-bars backfill endpoint (http.rs) -- separate port since a
/// raw WS listener (tokio-tungstenite::accept_async) can't also serve
/// plain HTTP GET requests on the same socket.
const DEFAULT_HTTP_ADDR: &str = "127.0.0.1:8788";
/// How far a subscriber can fall behind before it starts missing events
/// (`broadcast::error::RecvError::Lagged`).
///
/// Sized against a real measurement rather than a guess: production was
/// observed sustaining ~650 events/second during market hours (ignition,
/// momentum, halt-warning and bar traffic combined). The previous 1024 was
/// therefore only ~1.5 seconds of headroom — less than a single GC pause or
/// a brief network stall on any of the subscribers below. 16384 gives ~25
/// seconds at that rate, and costs only the queued `ScanEvent`s themselves,
/// which is negligible against this process's normal footprint.
///
/// This channel feeds the in-process subscribers (history collector, live
/// detection-efficiency tracker, push notifier). Client sockets read from a
/// second channel created in `server::run`, sized by the same constant.
const BROADCAST_CAPACITY: usize = 16_384;
/// Live detection-efficiency benchmark (2026-09-03, Roman's own ask —
/// see `backtest_metrics::live_signals`' doc comment for the full
/// design). Relative to this process's CWD (`/app` in the container,
/// see the Dockerfile) — `docker-compose.yml`'s own `ws` service now
/// bind-mounts `/app/data` to a real host directory specifically so
/// this survives a redeploy; without that mount every pending signal
/// would be lost on the next `deploy.sh` run before most of them ever
/// reach their own evaluation window.
const LIVE_PENDING_SIGNALS_PATH: &str = "data/live_pending_signals.jsonl";
/// Same shared, deploy-surviving mount as the constant above -- see
/// push.rs's own doc comment for why registered devices need to survive
/// a restart, not just this process's lifetime.
const PUSH_TOKENS_PATH: &str = "data/push_tokens.json";
/// Alpha measurement artifacts (opportunity episodes with their signal-time
/// context). Under the same `data/` mount every other durable capture uses, so
/// it survives a redeploy for the same reason those do.
const MEASUREMENT_DIR: &str = "data/research";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .init();
    dotenvy::dotenv().ok();

    let addr = std::env::var("WS_SERVER_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_string());
    let http_addr = std::env::var("HTTP_SERVER_ADDR").unwrap_or_else(|_| DEFAULT_HTTP_ADDR.to_string());
    if [&addr, &http_addr].iter().any(|a| a.parse::<std::net::SocketAddr>().map_or(true, |a| !a.ip().is_loopback())) {
        anyhow::ensure!(access::configured_token().is_some_and(|t| t.len() >= 32),
            "non-loopback listeners require STOCKSPOTTER_API_TOKEN (at least 32 characters)");
    }
    // One limiter shared by the HTTP and WebSocket listeners: a guesser must
    // not get a fresh allowance simply by switching protocol.
    let auth_limiter = Arc::new(access::AuthLimiter::new());

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

    // Real server-side push for a confirmed ignition (2026-09-04,
    // Roman: "I want to be notified on my phone even if my phone is
    // locked and I'm not looking at the screen"). Same "second
    // independent broadcast subscriber" shape as signal_handle above --
    // this is purely additive, doesn't touch client fanout at all. See
    // push.rs's own doc comment for the full reasoning on why this has
    // to be server-side (a client-only alert can't reach a suspended/
    // backgrounded app), and IGNITION_PUSH_COOLDOWN's for why a real
    // per-symbol quiet window is needed (ignition's raw signal is far
    // too frequent to push on every confirmation).
    let push_tokens = push::PushTokenStore::load(PUSH_TOKENS_PATH).await;
    let push_tokens_for_task = push_tokens.clone();
    let mut push_rx = tx.subscribe();
    let push_handle = tokio::spawn(async move {
        let http_client = reqwest::Client::new();
        let mut last_pushed: HashMap<String, chrono::DateTime<chrono::Utc>> = HashMap::new();
        loop {
            match push_rx.recv().await {
                Ok(ScanEvent::IgnitionEvent { symbol, price, kind: IgnitionEventKind::FollowThroughConfirmed, .. }) => {
                    let now = chrono::Utc::now();
                    let cooling_down = last_pushed.get(&symbol).is_some_and(|last| now - *last < push::IGNITION_PUSH_COOLDOWN);
                    if cooling_down {
                        continue;
                    }
                    last_pushed.insert(symbol.clone(), now);
                    let tokens = push_tokens_for_task.snapshot().await;
                    push::send_ignition_push(&http_client, &tokens, &symbol, price).await;
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    // Same real tradeoff as the live-efficiency collector
                    // above -- a lagged read could miss a real
                    // confirmation and undercount pushes, never
                    // double-push. Logged so a persistent lag pattern is
                    // visible, not silently swallowed.
                    warn!(skipped, "push notifier lagged behind the broadcast channel, some events missed");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Alpha measurement capture (Milestone B) -- a third independent
    // subscriber on the same broadcast, alongside the detection-efficiency
    // collector and the push notifier above. Purely observational: it reads
    // events that have already been broadcast, emits nothing, and gates
    // nothing. Writes go through a bounded queue to a dedicated thread, so a
    // slow or failing disk drops and counts research records rather than
    // touching the realtime path (see measurement.rs).
    let measurement_handle = measurement::MeasurementRecorder::start(MEASUREMENT_DIR.into())
        .map(|recorder| {
            let mut measurement_rx = tx.subscribe();
            tokio::spawn(async move {
                let mut collector = measurement::MeasurementCollector::new();
                loop {
                    match measurement_rx.recv().await {
                        Ok(event) => {
                            for episode in collector.observe(&event, chrono::Utc::now()) {
                                recorder.record_episode(episode);
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            // Same tradeoff the other collectors accept: a
                            // lagged read can miss observations, undercounting
                            // an episode rather than corrupting it. Logged so
                            // a persistent pattern stays visible.
                            warn!(skipped, "measurement collector lagged; some observations missed");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            // Shutdown: close what is still open as censored,
                            // never as concluded, then drain with a bound so
                            // research bookkeeping cannot hang the process.
                            for episode in collector.finish(chrono::Utc::now()) {
                                recorder.record_episode(episode);
                            }
                            recorder.flush(std::time::Duration::from_secs(5));
                            let health = recorder.health();
                            if health.is_degraded() {
                                warn!(
                                    dropped = health.dropped.load(std::sync::atomic::Ordering::Relaxed),
                                    write_errors = health.write_errors.load(std::sync::atomic::Ordering::Relaxed),
                                    "measurement capture finished with gaps; completeness claims are invalid"
                                );
                            }
                            // Always reported, pass or fail: an operator must be
                            // able to establish whether capacity ever bound
                            // without inferring it from span distributions after
                            // the fact, which is what Session 002 required.
                            let evictions = collector.capacity_evictions();
                            if evictions > 0 {
                                warn!(
                                    capacity_evictions = evictions,
                                    pending_peak = collector.pending_peak(),
                                    pending_capacity = collector.pending_capacity(),
                                    "measurement pending capacity bound during this session; \
                                     long-horizon outcomes are capacity-censored, not market behaviour"
                                );
                            } else {
                                info!(
                                    pending_peak = collector.pending_peak(),
                                    pending_capacity = collector.pending_capacity(),
                                    "measurement pending capacity never bound"
                                );
                            }
                            break;
                        }
                    }
                }
            })
        });

    let http_addr = std::env::var("HTTP_SERVER_ADDR").unwrap_or_else(|_| DEFAULT_HTTP_ADDR.to_string());
    // Same env var + default `market_data::live::run_live_scan` already
    // reads for its own server-to-server /qualify calls -- one source of
    // truth for "where does the Python qualitative layer run", not a
    // second copy of the setting.
    let qualify_url = std::env::var("QUALIFY_SERVICE_URL").unwrap_or_else(|_| "http://localhost:8000".to_string());
    let http_cfg = cfg.clone();
    let http_addr_for_spawn = http_addr.clone();
    let http_movers = today_movers.clone();
    let http_catalysts = catalysts.clone();
    let http_push_tokens = push_tokens.clone();
    let http_auth = auth_limiter.clone();
    let http_handle = tokio::spawn(async move {
        if let Err(e) = http::run(&http_addr_for_spawn, http_cfg, http_movers, http_catalysts, qualify_url, http_push_tokens, http_auth).await {
            error!(error = %e, "historical-bars http server exited with an error");
        }
    });

    info!(addr, http_addr, "starting ws server — watchlist is self-discovered via the universe scan, not fixed");
    server::run(&addr, tx, auth_limiter).await?;

    if let Some(handle) = measurement_handle {
        handle.abort();
    }
    http_handle.abort();
    movers_handle.abort();
    scan_handle.abort();
    signal_handle.abort();
    push_handle.abort();
    Ok(())
}
