//! Historical top-gainers lookup for one specific past trading day —
//! `movers::TodayMovers` only ever reflects the live/current session;
//! this is the separate, on-demand path the Top Gainers panel's date
//! toggle calls into. Deliberately *not* run on any periodic schedule —
//! a past day's data never changes once the market has closed for it, so
//! `ws-server`'s HTTP layer fetches this once per requested date and
//! caches the result, rather than re-polling it the way `movers.rs` does
//! for today.

use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{NaiveDate, Utc};
use serde::Deserialize;

use crate::config::AlpacaConfig;
use crate::movers::Mover;
use crate::universe::fetch_universe;

/// Same batching `universe::fetch_snapshots` uses for the live path —
/// keeps request URLs/response sizes reasonable across a ~13,378-symbol
/// universe, not a documented Alpaca limit.
const CHUNK_SIZE: usize = 200;
const TOP_N: usize = 25;

#[derive(Debug, Deserialize)]
struct DailyBarRaw {
    #[serde(rename = "c")]
    close: f64,
    #[serde(rename = "v")]
    volume: u64,
    #[serde(rename = "t")]
    timestamp: chrono::DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct BarsResponse {
    #[serde(default, deserialize_with = "crate::alpaca_json::null_values_as_empty_vecs")]
    bars: HashMap<String, Vec<DailyBarRaw>>,
}

/// Top gainers for one specific past trading `date`, ranked by that day's
/// close vs the immediately-prior trading day's close — the same
/// definition `gap_pct` uses everywhere else in this codebase, just
/// computed from historical daily bars instead of a live snapshot.
///
/// Fetches the real ~13,378-symbol universe in `CHUNK_SIZE`-symbol
/// batches (same batching `universe::fetch_snapshots` uses for the live
/// path), each asking for a short window ending at `date` so both that
/// day's bar and the prior trading day's bar come back in one request per
/// chunk — no per-symbol calls. Chunks run concurrently (this is a
/// one-off, user-waited-on lookup triggered by picking a date, not a
/// background poll, so it's worth parallelizing rather than the funnel's
/// sequential chunking).
pub async fn fetch_gainers_for_date(cfg: &AlpacaConfig, date: NaiveDate) -> Result<Vec<Mover>> {
    let universe = fetch_universe(cfg).await?;
    let end = date.and_hms_opt(23, 59, 59).expect("valid time").and_utc();
    // A week of padding comfortably covers weekends/holidays so the prior
    // trading day's bar is always in-window, without hardcoding a market
    // holiday calendar.
    let start = (date - chrono::Duration::days(7)).and_hms_opt(0, 0, 0).expect("valid time").and_utc();

    let client = reqwest::Client::new();
    let chunks: Vec<Vec<String>> = universe.chunks(CHUNK_SIZE).map(|c| c.to_vec()).collect();

    let fetches = chunks.into_iter().map(|chunk| {
        let client = client.clone();
        let cfg = cfg.clone();
        let start = start.to_rfc3339();
        let end = end.to_rfc3339();
        async move { fetch_chunk(&client, &cfg, &chunk, &start, &end, date).await }
    });

    let results = futures_util::future::join_all(fetches).await;

    let mut rows: Vec<Mover> = Vec::new();
    for result in results {
        match result {
            Ok(chunk_rows) => rows.extend(chunk_rows),
            Err(e) => tracing::warn!(error = %e, "historical gainers: one universe chunk failed; rankings from remaining chunks are still real, just incomplete"),
        }
    }

    rows.sort_by(|a, b| b.change_pct.partial_cmp(&a.change_pct).unwrap_or(std::cmp::Ordering::Equal));
    rows.truncate(TOP_N);
    Ok(rows)
}

async fn fetch_chunk(
    client: &reqwest::Client,
    cfg: &AlpacaConfig,
    chunk: &[String],
    start: &str,
    end: &str,
    target_date: NaiveDate,
) -> Result<Vec<Mover>> {
    let resp = client
        .get(format!("{}/v2/stocks/bars", cfg.data_base))
        .header("APCA-API-KEY-ID", &cfg.api_key)
        .header("APCA-API-SECRET-KEY", &cfg.api_secret)
        .query(&[
            ("symbols", chunk.join(",")),
            ("timeframe", "1Day".to_string()),
            ("start", start.to_string()),
            ("end", end.to_string()),
            ("limit", (chunk.len() * 10).to_string()),
            ("feed", cfg.feed.clone()),
            ("adjustment", "raw".to_string()),
        ])
        .send()
        .await
        .context("requesting historical daily bars chunk")?
        .error_for_status()
        .context("alpaca daily bars endpoint returned an error status")?;

    let parsed: BarsResponse = resp.json().await.context("parsing historical daily bars chunk")?;

    let mut rows = Vec::new();
    for (symbol, bars) in parsed.bars {
        if bars.len() < 2 {
            continue; // need both the target day and a prior day to compute change
        }
        // Bars come back oldest-first; the last bar in the requested
        // window is the target date's bar, the one immediately before it
        // is the prior trading day.
        let target = &bars[bars.len() - 1];
        let prior = &bars[bars.len() - 2];
        if target.timestamp.date_naive() != target_date {
            continue; // no trade printed on the requested date (holiday/no data) -- skip, don't fabricate
        }
        if prior.close <= 0.0 {
            continue;
        }
        let change_pct = (target.close - prior.close) / prior.close * 100.0;
        // No session label here -- this path only ever fetches *daily*
        // bars for one past date (see this module's own doc comment), so
        // there's no intraday resolution to classify a session from. See
        // Mover::session's own doc comment.
        rows.push(Mover { symbol, price: target.close, change_pct, volume: target.volume, session: None });
    }
    Ok(rows)
}
