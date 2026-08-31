//! Universe-wide Stage 1/2 scanning — architecture doc section 4.1's
//! actual "funnel" step. Everything else in this crate (`SessionTracker`,
//! `IgnitionMonitor`) tracks a handful of *already-chosen* symbols over a
//! live WS stream. That doesn't scale to the full tradable universe
//! (thousands of tickers) — the doc's own design is a periodic REST pass
//! over everything to shrink it down to a shortlist, and only *that*
//! shortlist gets promoted to live streaming. This module is the wide
//! net; `bin/scan.rs`'s tracked symbols are what a promoted shortlist
//! would look like.

use std::collections::HashMap;

use anyhow::{Context, Result};
use fast_funnel::{explain, run_fast_funnel, FilterThresholds, TickerSnapshot};
use serde::Deserialize;
use tracing::warn;

use crate::config::AlpacaConfig;
use crate::float_data::fetch_float_shares;

#[derive(Debug, Deserialize)]
struct AssetRaw {
    symbol: String,
    tradable: bool,
    status: String,
}

/// Every active, tradable US-equity symbol Alpaca knows about.
pub async fn fetch_universe(cfg: &AlpacaConfig) -> Result<Vec<String>> {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/v2/assets", cfg.trading_base))
        .header("APCA-API-KEY-ID", &cfg.api_key)
        .header("APCA-API-SECRET-KEY", &cfg.api_secret)
        .query(&[("status", "active"), ("asset_class", "us_equity")])
        .send()
        .await
        .context("requesting tradable asset universe from alpaca")?
        .error_for_status()
        .context("alpaca assets endpoint returned an error status")?;

    let assets: Vec<AssetRaw> = resp
        .json()
        .await
        .context("parsing alpaca assets response")?;

    Ok(assets
        .into_iter()
        .filter(|a| a.tradable && a.status == "active")
        .map(|a| a.symbol)
        .collect())
}

#[derive(Debug, Deserialize)]
struct SnapshotBar {
    #[serde(rename = "c")]
    close: f64,
    #[serde(rename = "v")]
    volume: u64,
}

#[derive(Debug, Deserialize)]
struct SnapshotTrade {
    #[serde(rename = "p")]
    price: f64,
}

#[derive(Debug, Default, Deserialize)]
struct SnapshotRaw {
    #[serde(rename = "latestTrade")]
    latest_trade: Option<SnapshotTrade>,
    #[serde(rename = "dailyBar")]
    daily_bar: Option<SnapshotBar>,
    #[serde(rename = "prevDailyBar")]
    prev_daily_bar: Option<SnapshotBar>,
}

/// Symbols per snapshot request — batched to keep request URLs and
/// response sizes reasonable across thousands of tickers, not because of
/// any documented Alpaca limit.
const SNAPSHOT_CHUNK_SIZE: usize = 200;

/// Batched snapshot fetch, turned directly into `fast_funnel`-ready
/// snapshots. `float_shares` is always `None` here — this module only
/// ever sees price/volume/gap; float is a separate lookup
/// (`float_data::fetch_float_shares`) applied only to whatever survives
/// Stage 1's other checks, not to the whole universe (FMP's free tier
/// couldn't cover that volume anyway).
///
/// `avg_daily_volume` is approximated from a single prior day
/// (`prevDailyBar.v`) rather than a true multi-day trailing average — a
/// deliberate shortcut for a fast, wide, periodic pass. A symbol that
/// survives and gets promoted to individual tracking uses
/// `rest::fetch_daily_seeds`'s real multi-day average instead.
pub async fn fetch_snapshots(
    cfg: &AlpacaConfig,
    symbols: &[String],
) -> Result<HashMap<String, TickerSnapshot>> {
    let client = reqwest::Client::new();
    let mut out = HashMap::new();

    for chunk in symbols.chunks(SNAPSHOT_CHUNK_SIZE) {
        let resp = client
            .get(format!("{}/v2/stocks/snapshots", cfg.data_base))
            .header("APCA-API-KEY-ID", &cfg.api_key)
            .header("APCA-API-SECRET-KEY", &cfg.api_secret)
            .query(&[("symbols", chunk.join(",")), ("feed", cfg.feed.clone())])
            .send()
            .await
            .context("requesting snapshots from alpaca")?
            .error_for_status()
            .context("alpaca snapshots endpoint returned an error status")?;

        let parsed: HashMap<String, SnapshotRaw> = resp
            .json()
            .await
            .context("parsing alpaca snapshots response")?;

        for (symbol, snap) in parsed {
            let Some(prev) = snap.prev_daily_bar else {
                continue;
            };
            let price = snap
                .latest_trade
                .map(|t| t.price)
                .or_else(|| snap.daily_bar.as_ref().map(|b| b.close));
            let Some(price) = price else { continue };
            let session_volume = snap.daily_bar.map(|b| b.volume).unwrap_or(0);
            let gap_pct = if prev.close > 0.0 {
                (price - prev.close) / prev.close * 100.0
            } else {
                0.0
            };

            out.insert(
                symbol.clone(),
                TickerSnapshot {
                    symbol,
                    price,
                    float_shares: None,
                    avg_daily_volume: prev.volume,
                    session_volume,
                    gap_pct,
                },
            );
        }
    }

    Ok(out)
}

/// The full Stage 1/2 funnel scan across the whole tradable universe,
/// returning just the symbols that qualify — the "wide, cheap, periodic"
/// half of the live architecture (see `live::run_live_scan`'s doc
/// comment for the other half, and the isolated core logic
/// `bin/scan_universe.rs` used to duplicate before this was factored
/// out). Measured 2026-08-31: ~3s for the full ~13,378-symbol universe
/// via Alpaca's batched snapshot endpoint — cheap enough to run every
/// couple of minutes continuously, not just as a one-off manual pass.
///
/// Float lookups only run on symbols that already cleared price + Stage
/// 2 (not the whole universe — FMP's rate limits couldn't cover that),
/// same fail-closed-on-unknown-float handling as everywhere else float
/// appears in this codebase.
pub async fn scan_shortlist(cfg: &AlpacaConfig, thresholds: &FilterThresholds) -> Result<Vec<String>> {
    let universe = fetch_universe(cfg).await?;
    let snapshots = fetch_snapshots(cfg, &universe).await?;

    let mut float_candidates: Vec<String> = Vec::new();
    for snapshot in snapshots.values() {
        let verdict = explain(snapshot, thresholds);
        if verdict.price_ok && verdict.rel_vol_ok && verdict.gap_ok {
            float_candidates.push(snapshot.symbol.clone());
        }
    }
    if float_candidates.is_empty() {
        return Ok(Vec::new());
    }

    let Some(fmp_key) = cfg.fmp_api_key.as_deref() else {
        warn!(
            candidates = float_candidates.len(),
            "FMP_API_KEY not set — these candidates can't clear Stage 1 without float data"
        );
        return Ok(Vec::new());
    };

    let mut float_checked_snapshots = Vec::new();
    for symbol in &float_candidates {
        let float_shares = match fetch_float_shares(fmp_key, symbol).await {
            Ok(f) => f,
            Err(e) => {
                warn!(symbol, error = %e, "float lookup failed for universe scan; treating as unknown");
                None
            }
        };
        let Some(mut snapshot) = snapshots.get(symbol).cloned() else {
            continue;
        };
        snapshot.float_shares = float_shares;
        float_checked_snapshots.push(snapshot);
    }

    let qualified = run_fast_funnel(&float_checked_snapshots, thresholds);
    Ok(qualified.into_iter().map(|s| s.symbol.clone()).collect())
}
