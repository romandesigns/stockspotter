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
use market_data::{fetch_gainers_for_date, fetch_recent_minute_bars, AlpacaConfig, Mover, SharedTodayMovers, TodayMovers};
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
        .route("/movers/today", get(get_today_movers))
        .route("/movers/gainers", get(get_gainers_for_date))
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

pub async fn run(addr: &str, cfg: AlpacaConfig, today_movers: SharedTodayMovers) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router(cfg, today_movers)).await?;
    Ok(())
}
