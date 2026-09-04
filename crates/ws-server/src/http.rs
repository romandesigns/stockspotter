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
use replay_engine::fetch_historical_bars;
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
}

pub fn router(cfg: AlpacaConfig, today_movers: SharedTodayMovers, catalysts: SharedCatalysts, qualify_url: String, push_tokens: PushTokenStore) -> Router {
    let state = AppState {
        cfg: Arc::new(cfg),
        today_movers,
        gainers_cache: Arc::new(RwLock::new(HashMap::new())),
        catalysts,
        qualify_url: Arc::new(qualify_url),
        push_tokens,
    };
    Router::new()
        .route("/bars/:symbol", get(get_bars))
        .route("/replay/bars/:symbol", get(get_replay_bars))
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
        .with_state(state)
        // Permissive on purpose: this is read-only public market data (no
        // secrets, no mutation), fetched cross-origin from whatever host
        // is serving apps/client (dev localhost, or the deployed site).
        // /assess fits this too -- the real secret (ANTHROPIC_API_KEY)
        // never leaves the server side; a client only ever sends a
        // symbol + the same momentum numbers it already has, and gets
        // back a short summary, no different in kind from every other
        // read-only endpoint here.
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
    match auto_trader_status::read_journal(std::path::Path::new(auto_trader_status::AUTO_TRADER_JOURNAL_PATH)).await {
        Ok(entries) => Json(auto_trader_status::compute_status(&entries, limit)).into_response(),
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
/// is idempotent, see PushTokenStore's own doc comment). No auth beyond
/// "you have the token" -- an Expo push token is only useful to send
/// notifications TO that specific device, not to read anything back, so
/// there's no real secret here worth gating behind an account system for
/// what's still a single-user app.
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

pub async fn run(
    addr: &str,
    cfg: AlpacaConfig,
    today_movers: SharedTodayMovers,
    catalysts: SharedCatalysts,
    qualify_url: String,
    push_tokens: PushTokenStore,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router(cfg, today_movers, catalysts, qualify_url, push_tokens)).await?;
    Ok(())
}
