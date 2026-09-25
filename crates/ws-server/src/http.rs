//! Read-only HTTP endpoints alongside the WS server -- historical bars
//! for the Super Chart backfill, plus the Top Gainers / Highly Trading
//! panels' data (today's live rankings, and one-off historical lookups
//! for a picked past date).
//!
//! Separate from the WS server (server.rs) entirely -- tokio-tungstenite's
//! accept_async assumes every incoming connection is a WS upgrade
//! attempt, so a real HTTP GET route needs its own listener. Runs
//! alongside it on a second port in the same process (see main.rs).

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use chrono::{NaiveDate, Utc};
use market_data::{
    fetch_gainers_for_date, fetch_markets_today, fetch_recent_minute_bars, request_assessment, AlpacaConfig, CatalystRecord, Mover,
    MomentumReading, SharedCatalysts, SharedTodayMovers, TodayMovers,
};
use backtest_metrics::extract_signals;
use replay_engine::{fetch_historical_bars, fetch_replay_data, run_replay, ReplayConfig};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;
use tracing::warn;

use crate::auto_trader_status;
use crate::push::PushTokenStore;

#[derive(Debug, Deserialize)]
pub struct BarsQuery {
    /// How far back to fetch, in minutes. Capped at MAX_LOOKBACK_MINUTES
    /// -- this is a chart backfill, not a general historical-data API.
    #[serde(default = "default_minutes")]
    minutes: i64,
}

fn default_minutes() -> i64 {
    240
}

const MAX_LOOKBACK_MINUTES: i64 = 60 * 24; // one day, matches the prototype's own single-session demo scope

/// Wire shape matches BarUpdate's own convention (unix seconds, raw
/// OHLCV) so the client can feed this straight into the same CandleBar
/// shape the live feed already produces.
#[derive(Debug, Serialize)]
pub struct BarOut {
    time: i64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: u64,
}

/// A past trading day's top-gainers lookup never changes once that day's
/// market has closed -- cached here so re-picking a date the user (or a
/// page reload) already looked up doesn't re-run a ~13,378-symbol
/// historical scan every time. Unbounded for now: a realistic session's
/// worth of distinct dates picked is small, and each entry is only 25
/// small rows.
type GainersCache = Arc<RwLock<HashMap<NaiveDate, Vec<Mover>>>>;

#[derive(Clone)]
struct AppState {
    replay_slots: Arc<tokio::sync::Semaphore>,
    cfg: Arc<AlpacaConfig>,
    today_movers: SharedTodayMovers,
    gainers_cache: GainersCache,
    catalysts: SharedCatalysts,
    /// Where the Python qualitative layer runs — same env var/default
    /// `market_data::live::run_live_scan` already reads for its own
    /// (fire-and-forget, server-to-server) `/qualify` calls. This is the
    /// client-facing counterpart: `/assess` proxies a real request/
    /// response round trip on behalf of whichever web/mobile client
    /// asked for a symbol's AI assessment.
    qualify_url: Arc<String>,
    /// Registered device push tokens for the real ignition-confirmed
    /// push (2026-09-04) -- see push.rs's own doc comment. Cheap to
    /// clone (it's an `Arc<RwLock<..>>` internally, same shape as every
    /// other shared-state field on this struct).
    push_tokens: PushTokenStore,
    /// Shared handles onto each research capture's own accounting. Read-only;
    /// this router can observe capture health but cannot alter it.
    research: Arc<crate::research_health::ResearchHealth>,
}

// One parameter over clippy's threshold, and deliberately so: the alternative
// is a parameter struct that exists only to satisfy a lint, which would make
// this call site harder to read rather than easier. The added handle is the
// research health surface, and it is read-only.
#[allow(clippy::too_many_arguments)]
pub fn router(cfg: AlpacaConfig, today_movers: SharedTodayMovers, catalysts: SharedCatalysts, qualify_url: String, push_tokens: PushTokenStore, auth: Arc<crate::access::AuthLimiter>, research: Arc<crate::research_health::ResearchHealth>) -> Router {
    let state = AppState {
        replay_slots: Arc::new(tokio::sync::Semaphore::new(2)),
        cfg: Arc::new(cfg),
        today_movers,
        gainers_cache: Arc::new(RwLock::new(HashMap::new())),
        catalysts,
        qualify_url: Arc::new(qualify_url),
        push_tokens,
        research,
    };
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/bars/:symbol", get(get_bars))
        .route("/replay/bars/:symbol", get(get_replay_bars))
        .route("/replay/signals/:symbol", get(get_replay_signals))
        .route("/movers/today", get(get_today_movers))
        .route("/movers/gainers", get(get_gainers_for_date))
        .route("/markets/today", get(get_markets_today))
        .route("/catalysts/today", get(get_catalysts_today))
        .route("/assess", post(post_assess))
        // No AppState needed -- reads the shared JSONL journal file
        // directly (see auto_trader_status.rs's own doc comment), not
        // any in-process cache this router already carries.
        .route("/auto-trader/status", get(get_auto_trader_status))
        // Real push registration (2026-09-04) -- register on the phone
        // when the in-app toggle is on, unregister when it's switched
        // off. This is the actual mechanism behind "turn this feature
        // off on the phone" -- the app just stops appearing in future
        // sends, no server-side account/auth system needed for it.
        .route("/push/register", post(post_push_register))
        .route("/push/unregister", post(post_push_unregister))
        // Research completeness (Alpha OI V1). One authenticated read answers
        // whether this session has lost scientific evidence. Behind the same
        // fail-closed `protect` middleware as everything else on this router --
        // it exposes counters and file names, never credentials or market data.
        .route("/research/completeness", get(get_research_completeness))
        .with_state(state)
        // Cross-origin desktop and mobile clients supply an explicit bearer
        // credential. CORS permits their preflight; middleware protects work.
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024))
        .layer(axum::middleware::from_fn_with_state(crate::access::Access::from_env(auth), crate::access::protect))
        .layer(CorsLayer::permissive())
}

async fn get_bars(State(state): State<AppState>, Path(symbol): Path<String>, Query(q): Query<BarsQuery>) -> impl IntoResponse {
    let minutes = q.minutes.clamp(1, MAX_LOOKBACK_MINUTES);
    let end = Utc::now();
    let start = end - chrono::Duration::minutes(minutes);

    match fetch_recent_minute_bars(&state.cfg, &symbol, &start.to_rfc3339(), &end.to_rfc3339()).await {
        Ok(bars) => {
            let out: Vec<BarOut> = bars
                .into_iter()
                .map(|b| BarOut {
                    time: b.timestamp.timestamp(),
                    open: b.open,
                    high: b.high,
                    low: b.low,
                    close: b.close,
                    volume: b.volume,
                })
                .collect();
            Json(out).into_response()
        }
        Err(e) => {
            warn!(symbol = %symbol, error = %e, "historical bars backfill request failed");
            (StatusCode::BAD_GATEWAY, format!("failed to fetch historical bars for {symbol}")).into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ReplayBarsQuery {
    /// Both YYYY-MM-DD, inclusive -- the Backtest Replay dialog's own
    /// date-range picker (ReplayRangePicker.tsx).
    start: String,
    end: String,
}

/// Widest span this endpoint will fetch in one request -- generous
/// enough for the replay dialog's own largest preset ("Last 10
/// sessions", padded for weekends) while still bounding a single
/// request's worst case to something Alpaca's pagination handles
/// comfortably, rather than an unbounded historical-data API.
const MAX_REPLAY_SPAN_DAYS: i64 = 45;

/// Real multi-day 1-minute bars for the Backtest Replay dialog -- unlike
/// `get_bars` above (capped at one day, anchored to "now", built for the
/// live chart's backfill), this takes an explicit past date range of any
/// real symbol. Reuses `replay_engine::fetch_historical_bars` (the same
/// paginated Alpaca fetch `get_bars` duplicates in miniature for its own
/// narrower job -- see `market_data::rest::fetch_recent_minute_bars`'s
/// own doc comment flagging this as "a real consolidation candidate if a
/// third caller ever needs the same thing"; this is that third caller,
/// and it needed replay-engine's uncapped version, not another copy).
async fn get_replay_bars(State(state): State<AppState>, Path(symbol): Path<String>, Query(q): Query<ReplayBarsQuery>) -> impl IntoResponse {
    let start_date = match NaiveDate::parse_from_str(&q.start, "%Y-%m-%d") {
        Ok(d) => d,
        Err(_) => return (StatusCode::BAD_REQUEST, "start must be YYYY-MM-DD").into_response(),
    };
    let end_date = match NaiveDate::parse_from_str(&q.end, "%Y-%m-%d") {
        Ok(d) => d,
        Err(_) => return (StatusCode::BAD_REQUEST, "end must be YYYY-MM-DD").into_response(),
    };
    if end_date < start_date {
        return (StatusCode::BAD_REQUEST, "end must not be before start").into_response();
    }
    if end_date > Utc::now().date_naive() {
        return (StatusCode::BAD_REQUEST, "end can't be in the future").into_response();
    }
    if (end_date - start_date).num_days() > MAX_REPLAY_SPAN_DAYS {
        return (StatusCode::BAD_REQUEST, format!("range too wide -- max {MAX_REPLAY_SPAN_DAYS} days")).into_response();
    }

    // Full calendar days in UTC, padded a day past `end_date` -- comfortably
    // covers 4:00-20:00 ET (pre-market through after-hours) regardless of
    // the UTC offset shift across DST, without needing real timezone math
    // just to bound a fetch window. The bars themselves carry real
    // timestamps; the client does the actual ET session classification
    // for display (sessionClassify.ts), not this endpoint.
    let start = start_date.and_hms_opt(0, 0, 0).expect("valid time").and_utc();
    let end = (end_date + chrono::Duration::days(1)).and_hms_opt(0, 0, 0).expect("valid time").and_utc();

    match fetch_historical_bars(&state.cfg, &symbol, &start.to_rfc3339(), &end.to_rfc3339(), "1Min").await {
        Ok(bars) => {
            let out: Vec<BarOut> = bars
                .into_iter()
                .map(|b| BarOut {
                    time: b.timestamp.timestamp(),
                    open: b.open,
                    high: b.high,
                    low: b.low,
                    close: b.close,
                    volume: b.volume,
                })
                .collect();
            Json(out).into_response()
        }
        Err(e) => {
            warn!(symbol = %symbol, %start_date, %end_date, error = %e, "replay bars fetch failed");
            (StatusCode::BAD_GATEWAY, format!("failed to fetch replay bars for {symbol}")).into_response()
        }
    }
}

/// Widest span the signals endpoint will replay. Far tighter than
/// `MAX_REPLAY_SPAN_DAYS` (45) on purpose: bars are one cheap paginated
/// fetch, but a real detection replay also needs every TRADE and QUOTE
/// in the window -- a single busy session measured 174,954 trades
/// (QNRX, 2026-08-28). Multiplying that by 45 days per chart open isn't
/// a bigger request, it's a different kind of request.
const MAX_SIGNAL_SPAN_DAYS: i64 = 3;

/// Wire shape for one detection signal on the replay chart. Deliberately
/// minimal -- a marker needs a time, a price, and what fired; anything
/// more belongs in the panel that owns that strategy.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaySignalOut {
    time: i64,
    price: f64,
    /// `Strategy`'s own Debug name, e.g. "IgnitionDetector".
    strategy: String,
}

/// Detection signals for a replay window, rendered as markers on the
/// Backtest Replay chart.
///
/// **Why this exists (2026-09-06).** Architecture doc section 7 asks for
/// exactly this: "indicators and any detection signals (ignition alerts,
/// momentum panel qualifications, etc.) render on the chart at the exact
/// moments they would have fired live." The replay dialog shipped
/// without it -- it played historical bars back and nothing else, so
/// replay showed price but never showed what the scanner would have
/// DONE about that price. That is the fastest available way to build or
/// destroy trust in a strategy, and it was the missing half.
///
/// This is not a second implementation of anything: it calls the same
/// `run_replay` + `extract_signals` the backtest binaries use, which in
/// turn drive the same detector code the live path runs. The doc's "no
/// separate backtest version of the logic" rule holds through to the
/// chart.
async fn get_replay_signals(State(state): State<AppState>, Path(symbol): Path<String>, Query(q): Query<ReplayBarsQuery>) -> impl IntoResponse {
    let Ok(permit) = state.replay_slots.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let start_date = match NaiveDate::parse_from_str(&q.start, "%Y-%m-%d") {
        Ok(d) => d,
        Err(_) => return (StatusCode::BAD_REQUEST, "start must be YYYY-MM-DD").into_response(),
    };
    let end_date = match NaiveDate::parse_from_str(&q.end, "%Y-%m-%d") {
        Ok(d) => d,
        Err(_) => return (StatusCode::BAD_REQUEST, "end must be YYYY-MM-DD").into_response(),
    };
    if end_date < start_date {
        return (StatusCode::BAD_REQUEST, "end must not be before start").into_response();
    }
    if end_date > Utc::now().date_naive() {
        return (StatusCode::BAD_REQUEST, "end can't be in the future").into_response();
    }
    if (end_date - start_date).num_days() > MAX_SIGNAL_SPAN_DAYS {
        return (
            StatusCode::BAD_REQUEST,
            format!("range too wide for signal replay -- max {MAX_SIGNAL_SPAN_DAYS} days (tick data is heavy; bars alone go wider)"),
        )
            .into_response();
    }

    let start = start_date.and_hms_opt(0, 0, 0).expect("valid time").and_utc();
    let end = (end_date + chrono::Duration::days(1)).and_hms_opt(0, 0, 0).expect("valid time").and_utc();

    let data = match fetch_replay_data(&state.cfg, &symbol, &start.to_rfc3339(), &end.to_rfc3339()).await {
        Ok(d) => d,
        Err(e) => {
            warn!(symbol = %symbol, %start_date, %end_date, error = %e, "replay signal data fetch failed");
            return (StatusCode::BAD_GATEWAY, format!("failed to fetch replay data for {symbol}")).into_response();
        }
    };

    // Shipped defaults, not a tuned variant -- the chart has to show what
    // the live scanner would actually have fired, not a flattering
    // configuration of it.
    // Keep CPU work off the live-feed runtime. The permit remains owned by
    // this worker even if the HTTP request times out or the client disconnects.
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let result = run_replay(&data, &ReplayConfig::default());
        extract_signals(&result).into_iter()
            .map(|s| ReplaySignalOut { time: s.timestamp.timestamp(), price: s.price, strategy: format!("{:?}", s.strategy) })
            .collect::<Vec<_>>()
    }).await {
        Ok(out) => Json(out).into_response(),
        Err(e) => {
            warn!(error = %e, "replay worker failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Today's live Top Gainers + Highly Trading rankings -- backed by
/// `market_data::movers`'s own 60s background scan (see main.rs), so this
/// just reads whatever's currently cached rather than fetching per
/// request.
async fn get_today_movers(State(state): State<AppState>) -> impl IntoResponse {
    let movers: TodayMovers = state.today_movers.read().await.clone();
    Json(movers)
}

#[derive(Debug, Deserialize)]
pub struct GainersQuery {
    /// YYYY-MM-DD, the trading day to rank. Required -- callers wanting
    /// "today" should hit `/movers/today` instead (it's live/cached, this
    /// endpoint is a one-off historical scan and deliberately not meant
    /// for the default/no-date-picked case).
    date: String,
}

async fn get_gainers_for_date(State(state): State<AppState>, Query(q): Query<GainersQuery>) -> impl IntoResponse {
    let date = match NaiveDate::parse_from_str(&q.date, "%Y-%m-%d") {
        Ok(d) => d,
        Err(_) => return (StatusCode::BAD_REQUEST, "date must be YYYY-MM-DD").into_response(),
    };
    if date > Utc::now().date_naive() {
        return (StatusCode::BAD_REQUEST, "date can't be in the future").into_response();
    }

    if let Some(cached) = state.gainers_cache.read().await.get(&date) {
        return Json(cached.clone()).into_response();
    }

    match fetch_gainers_for_date(&state.cfg, date).await {
        Ok(rows) => {
            state.gainers_cache.write().await.insert(date, rows.clone());
            Json(rows).into_response()
        }
        Err(e) => {
            warn!(%date, error = %e, "historical gainers lookup failed");
            (StatusCode::BAD_GATEWAY, format!("failed to fetch gainers for {date}")).into_response()
        }
    }
}

/// Markets Today's 4 index-proxy readings (market_data::indices) --
/// stateless, fetched fresh per request (see that module's doc comment
/// on why no background cache is needed for something this cheap).
async fn get_markets_today(State(state): State<AppState>) -> impl IntoResponse {
    match fetch_markets_today(&state.cfg).await {
        Ok(readings) => Json(readings).into_response(),
        Err(e) => {
            warn!(error = %e, "markets-today snapshot fetch failed");
            (StatusCode::BAD_GATEWAY, "failed to fetch index snapshots").into_response()
        }
    }
}

/// Backfill for a client that connects after a currently-tracked symbol's
/// one-shot catalyst lookup already fired -- confirmed live 2026-09-01:
/// the live WS broadcast alone left a freshly-opened Catalysts panel
/// empty for 17 real, currently-tracked symbols whose catalyst tags had
/// already been looked up (and logged) 30+ minutes earlier. Backed by
/// `market_data::live::run_live_scan`'s own `SharedCatalysts` cache, kept
/// in sync with the real watchlist (populated on promotion, cleared on
/// drop) rather than a REST endpoint's own copy.
async fn get_catalysts_today(State(state): State<AppState>) -> impl IntoResponse {
    let records: Vec<CatalystRecord> = state.catalysts.read().await.values().cloned().collect();
    Json(records)
}

/// Wire shape matches shared-types' `MomentumUpdate`/`AssessRequest`
/// convention (camelCase) -- this is the one boundary in this file that
/// faces a real secret-backed external call, but the request/response
/// casing itself follows the exact same convention as every other
/// client-facing shape in this codebase, no special treatment needed.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssessRequestIn {
    symbol: String,
    overall: f64,
    volume_confirmation: f64,
    structure: f64,
    ma_slope: f64,
    wick_rejection: f64,
    #[serde(default)]
    force_refresh: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AssessResponseOut {
    summary: Vec<String>,
    generated_at: String,
}

/// Proxies to the Python qualitative layer's `/assess` endpoint
/// (`python/app/assess.py`) — see that module's own doc comment for the
/// real Claude-plus-web-search call and its server-side 10-minute cache
/// (a repeat request for the same symbol within that window returns
/// near-instantly; a genuinely new one takes several real seconds, so
/// this route's timeout is deliberately generous, see
/// `market_data::assess::request_assessment`'s own doc comment).
async fn post_assess(State(state): State<AppState>, Json(req): Json<AssessRequestIn>) -> impl IntoResponse {
    let momentum = MomentumReading {
        overall: req.overall,
        volume_confirmation: req.volume_confirmation,
        structure: req.structure,
        ma_slope: req.ma_slope,
        wick_rejection: req.wick_rejection,
    };
    match request_assessment(&state.qualify_url, &req.symbol, momentum, req.force_refresh).await {
        Ok(assessment) => Json(AssessResponseOut { summary: assessment.summary, generated_at: assessment.generated_at }).into_response(),
        Err(e) => {
            warn!(symbol = %req.symbol, error = %e, "AI assessment request failed");
            (StatusCode::BAD_GATEWAY, format!("failed to get an assessment for {}", req.symbol)).into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct AutoTraderStatusQuery {
    /// How many recent journal entries (entered/exited/skipped) to
    /// return, newest first -- a monitoring view, not a full audit-log
    /// export.
    #[serde(default = "default_recent_limit")]
    limit: usize,
}

fn default_recent_limit() -> usize {
    50
}

const MAX_RECENT_LIMIT: usize = 200;

/// Auto-trader (2026-09-04, Roman's own "how can we monitor it" ask) --
/// reads the shared journal file directly rather than proxying an HTTP
/// call to a second internal service; see auto_trader_status.rs's own
/// doc comment for why that's the right call here.
async fn get_auto_trader_status(Query(q): Query<AutoTraderStatusQuery>) -> impl IntoResponse {
    let limit = q.limit.clamp(1, MAX_RECENT_LIMIT);
    match auto_trader_status::read_current_history().await {
        Ok(entries) => {
            // Read alongside the journal so the UI can show "this
            // strategy is still trading on negative evidence" -- see
            // AutoTraderStatusOut::negative_evidence's own doc comment.
            let negative_evidence =
                auto_trader_status::read_negative_evidence(std::path::Path::new(auto_trader_status::STRATEGY_CONFIG_PATH));
            let mut status=auto_trader_status::compute_status(&entries, limit, negative_evidence);
            if std::env::var("AUTO_TRADER_EXECUTION_MODE").as_deref()==Ok("paper") {status.execution_mode="alpaca_paper";}
            Json(status).into_response()
        }
        Err(e) => {
            warn!(error = %e, "auto-trader status read failed");
            (StatusCode::BAD_GATEWAY, "failed to read the auto-trader journal").into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct PushTokenIn {
    token: String,
}

#[derive(Debug, Serialize)]
struct PushTokenOut {
    ok: bool,
}

/// Called once from the app when the "Ignition push alerts" toggle turns
/// on (or, defensively, on every launch while it's already on -- register
/// is idempotent, see PushTokenStore's own doc comment). The shared HTTP
/// middleware requires the private access key before changing registration.
async fn post_push_register(State(state): State<AppState>, Json(req): Json<PushTokenIn>) -> impl IntoResponse {
    state.push_tokens.register(req.token).await;
    Json(PushTokenOut { ok: true })
}

/// The real mechanism behind "I want to be able to turn this feature off
/// on the phone" -- called when the toggle turns off. After this call
/// returns, this device is simply excluded from every future
/// send_ignition_push, no state left behind that could silently turn
/// back on.
async fn post_push_unregister(State(state): State<AppState>, Json(req): Json<PushTokenIn>) -> impl IntoResponse {
    state.push_tokens.unregister(&req.token).await;
    Json(PushTokenOut { ok: true })
}

/// One read-only answer to "did this session lose scientific evidence".
///
/// # Why this route exists
///
/// On September 16 the Opportunity Intelligence writer discarded 2,276,531 of
/// 2,558,786 snapshots — an 11.0% capture rate — and there was no way to learn
/// that while the session ran. `capture_health()` was `#[cfg(test)]`, `/health`
/// returned the string `ok`, and the drop counters were reachable only through
/// shutdown logging. Establishing the loss required grepping power-of-two log
/// lines hours later and reconstructing the denominator from cohort sizes in
/// the records that happened to survive.
///
/// The verdict itself is deliberately *not* computed here. This returns the
/// evidence; `backtest_metrics::completeness::check` turns evidence into
/// VALID / INVALID / INDETERMINATE, offline and deterministically, against the
/// artifacts as well as these counters. A subsystem must not be the thing that
/// grades itself.
/// Assembles the response body.
///
/// Extracted from the handler so the shape can be tested without standing up
/// a server. `ops/qualify/session.sh` reads these exact paths before every
/// prospective session, and a test walks the script's paths through this
/// function's output -- so a rename here fails the build rather than silently
/// turning an operator's preflight check into a no-op that reads an absent
/// field as zero.
pub fn completeness_envelope(
    report: &backtest_metrics::completeness::CompletenessReport,
    settlement: Option<serde_json::Value>,
    retention: Option<crate::research_retention::RetentionSnapshot>,
    discovery_retention: Option<market_data::discovery_audit::DiscoveryRetention>,
) -> serde_json::Value {
    serde_json::json!({
        "report": report,
        "measurementPending": settlement,
        // Reported beside the verdict rather than inside it: reclaiming an old
        // session says nothing about whether the *current* one is complete. It
        // is an operational fact an operator needs, not a completeness input.
        "retention": retention,
        // Discovery's directory ceiling, including protected-day pressure
        // (`blockedByProtection`, `bytesOverCeiling`). `null` when discovery
        // capture is not running in this process.
        "discoveryRetention": discovery_retention,
        "anyKnownLoss": report.any_known_loss(),
    })
}

async fn get_research_completeness(State(state): State<AppState>) -> impl IntoResponse {
    let report = state.research.report();
    // The settlement half comes from the collector rather than the writer: an
    // episode that never reached an outcome was never offered to the writer at
    // all, so no writer counter can see it.
    let settlement = state.research.measurement_engine().map(|h| {
        use std::sync::atomic::Ordering::Relaxed;
        serde_json::json!({
            "pending": h.pending.load(Relaxed),
            "pendingPeak": h.pending_peak.load(Relaxed),
            "pendingCapacity": h.pending_capacity.load(Relaxed),
            "capacityEvictions": h.capacity_evictions.load(Relaxed),
            "openEpisodes": h.open_episodes.load(Relaxed),
        })
    });
    Json(completeness_envelope(
        &report,
        settlement,
        state.research.retention(),
        market_data::discovery_audit::retention_health(),
    ))
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    addr: &str,
    cfg: AlpacaConfig,
    today_movers: SharedTodayMovers,
    catalysts: SharedCatalysts,
    qualify_url: String,
    push_tokens: PushTokenStore,
    auth: Arc<crate::access::AuthLimiter>,
    research: Arc<crate::research_health::ResearchHealth>,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    // ConnectInfo is what makes the real TCP peer address reachable from the
    // `protect` middleware; without it there is no spoof-resistant identity
    // to key the per-IP authentication limiter by.
    axum::serve(
        listener,
        router(cfg, today_movers, catalysts, qualify_url, push_tokens, auth, research)
            .into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
#[path = "runbook_contract_tests.rs"]
mod runbook_contract_tests;
