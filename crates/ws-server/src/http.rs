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
use axum::routing::get;
use axum::Router;
use chrono::{NaiveDate, Utc};
use market_data::{
    fetch_gainers_for_date, fetch_markets_today, fetch_recent_minute_bars, AlpacaConfig, Mover, SharedTodayMovers, TodayMovers,
};
use replay_engine::fetch_historical_bars;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;
use tracing::warn;

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
}

pub fn router(cfg: AlpacaConfig, today_movers: SharedTodayMovers) -> Router {
    let state = AppState {
        cfg: Arc::new(cfg),
        today_movers,
        gainers_cache: Arc::new(RwLock::new(HashMap::new())),
    };
    Router::new()
        .route("/bars/:symbol", get(get_bars))
        .route("/replay/bars/:symbol", get(get_replay_bars))
        .route("/movers/today", get(get_today_movers))
        .route("/movers/gainers", get(get_gainers_for_date))
        .route("/markets/today", get(get_markets_today))
        .with_state(state)
        // Permissive on purpose: this is read-only public market data (no
        // secrets, no mutation), fetched cross-origin from whatever host
        // is serving apps/client (dev localhost, or the deployed site).
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

pub async fn run(addr: &str, cfg: AlpacaConfig, today_movers: SharedTodayMovers) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router(cfg, today_movers)).await?;
    Ok(())
}
