//! Read-only HTTP endpoint for backfilling a Super Chart with real
//! historical bars the moment a symbol is selected -- the live WS feed
//! only ever sends bars going forward from whenever a symbol started
//! being tracked this session, so a freshly-selected symbol looks sparse
//! next to a chart that pre-fetched a full day the way the Artifact
//! prototype's demo did. This fills that gap with real Alpaca data, not
//! synthetic backfill.
//!
//! Separate from the WS server (server.rs) entirely -- tokio-tungstenite's
//! accept_async assumes every incoming connection is a WS upgrade
//! attempt, so a real HTTP GET route needs its own listener. Runs
//! alongside it on a second port in the same process (see main.rs).

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use chrono::Utc;
use market_data::{fetch_recent_minute_bars, AlpacaConfig};
use serde::{Deserialize, Serialize};
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

pub fn router(cfg: AlpacaConfig) -> Router {
    Router::new()
        .route("/bars/:symbol", get(get_bars))
        .with_state(Arc::new(cfg))
        // Permissive on purpose: this is read-only public market data (no
        // secrets, no mutation), fetched cross-origin from whatever host
        // is serving apps/client (dev localhost, or the deployed site).
        .layer(CorsLayer::permissive())
}

async fn get_bars(State(cfg): State<Arc<AlpacaConfig>>, Path(symbol): Path<String>, Query(q): Query<BarsQuery>) -> impl IntoResponse {
    let minutes = q.minutes.clamp(1, MAX_LOOKBACK_MINUTES);
    let end = Utc::now();
    let start = end - chrono::Duration::minutes(minutes);

    match fetch_recent_minute_bars(&cfg, &symbol, &start.to_rfc3339(), &end.to_rfc3339()).await {
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

pub async fn run(addr: &str, cfg: AlpacaConfig) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router(cfg)).await?;
    Ok(())
}
