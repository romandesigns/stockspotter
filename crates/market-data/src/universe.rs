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
    // Optional, not required -- if Alpaca ever omits this for some
    // asset, is_warrant() below fails OPEN (treats it as not a warrant,
    // keeps it in the universe) rather than the whole fetch_universe
    // call failing to deserialize at all over one missing field on a
    // classification-only concern.
    name: Option<String>,
    tradable: bool,
    status: String,
}

/// True if this asset's real Alpaca security name marks it as a warrant
/// rather than the company's actual common stock (e.g. "Rocket Lab USA,
/// Inc. Warrant") -- a real signal from Alpaca's own metadata, not a
/// ticker-suffix guess. A suffix convention (trailing W/.WS) is common
/// but not guaranteed across every exchange/listing, and a legitimate
/// common stock could coincidentally end the same way -- the name field
/// doesn't have that false-positive risk.
fn is_warrant(name: Option<&str>) -> bool {
    name.is_some_and(|n| n.to_lowercase().contains("warrant"))
}

/// Every active, tradable US-equity symbol Alpaca knows about --
/// warrants excluded (see is_warrant's own doc comment on why: they're a
/// leveraged, low-priced derivative of the underlying stock, not the
/// stock itself, and their outsized % swings on trivial price moves were
/// crowding out genuine common-stock movers across Top Gainers/Highly
/// Trading and, upstream of that, the funnel's own Stage 1/2 shortlist).
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
        .filter(|a| a.tradable && a.status == "active" && !is_warrant(a.name.as_deref()))
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
///
/// Capped at `MAX_FLOAT_CHECKS_PER_SCAN`: Stage-2 survivor counts were
/// only 15-25/scan during the quiet premarket hours this was measured
/// against (2026-08-31), but regular hours — especially right at the
/// open — will plausibly push that much higher. At the live 15s rescan
/// interval, uncapped survivor counts scale FMP calls 4x/min; this cap
/// keeps the worst case at MAX_FLOAT_CHECKS_PER_SCAN*4 calls/min, safely
/// under the confirmed 300/min FMP Starter ceiling even on the busiest
/// part of the session. Overflow candidates aren't lost, just deferred —
/// they get re-checked on the very next 15s cycle if still qualifying,
/// prioritized by `|gap_pct| * relative_volume` so the most extreme
/// movers get float-checked first when there's more demand than budget.
const MAX_FLOAT_CHECKS_PER_SCAN: usize = 60;

pub async fn scan_shortlist(cfg: &AlpacaConfig, thresholds: &FilterThresholds) -> Result<Vec<QualifiedSymbol>> {
    let universe = fetch_universe(cfg).await?;
    let snapshots = fetch_snapshots(cfg, &universe).await?;

    let mut float_candidates: Vec<&TickerSnapshot> = Vec::new();
    for snapshot in snapshots.values() {
        let verdict = explain(snapshot, thresholds);
        if verdict.price_ok && verdict.rel_vol_ok && verdict.gap_ok {
            float_candidates.push(snapshot);
        }
    }
    if float_candidates.is_empty() {
        return Ok(Vec::new());
    }

    let total_candidates = float_candidates.len();
    if total_candidates > MAX_FLOAT_CHECKS_PER_SCAN {
        float_candidates.sort_by(|a, b| {
            let score = |s: &TickerSnapshot| s.gap_pct.abs() * (s.session_volume as f64 / s.avg_daily_volume.max(1) as f64);
            score(b).partial_cmp(&score(a)).unwrap_or(std::cmp::Ordering::Equal)
        });
        float_candidates.truncate(MAX_FLOAT_CHECKS_PER_SCAN);
        warn!(
            total_candidates,
            checking = MAX_FLOAT_CHECKS_PER_SCAN,
            "more Stage-2 survivors than this scan's float-check budget; checking the most extreme movers now, rest deferred to next cycle"
        );
    }
    let float_candidates: Vec<String> = float_candidates.into_iter().map(|s| s.symbol.clone()).collect();

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
    Ok(qualified
        .into_iter()
        .map(|s| QualifiedSymbol { symbol: s.symbol.clone(), float_shares: s.float_shares })
        .collect())
}

/// A symbol that cleared the full Stage 1/2 funnel, carrying the float
/// value the scan already paid an FMP call to confirm — plumbed through
/// so a live-promoted symbol's `SessionTracker` doesn't have to re-fetch
/// it (or worse, silently default to `None` and show `float_ok: false`
/// forever despite having a real qualifying float; see
/// `live::track_symbol`'s doc comment for the bug this fixes).
#[derive(Debug, Clone)]
pub struct QualifiedSymbol {
    pub symbol: String,
    pub float_shares: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_warrant_names_are_detected() {
        // Real examples seen live -- RCKTW/BIAFW/DSX.WS/ARQQW/GFAIW/LHSW
        // all dominated Top Gainers/Highly Trading before this filter.
        assert!(is_warrant(Some("Rocket Lab USA, Inc. Warrant")));
        assert!(is_warrant(Some("BiOptio Inc. Warrants")));
        assert!(is_warrant(Some("Diana Shipping Inc. Warrant")));
        // Case-insensitive -- Alpaca's own casing isn't guaranteed consistent.
        assert!(is_warrant(Some("Example Corp WARRANT")));
    }

    #[test]
    fn real_common_stock_names_are_not_flagged() {
        assert!(!is_warrant(Some("Apple Inc.")));
        assert!(!is_warrant(Some("Rocket Lab USA, Inc.")));
        // A name that happens to contain "War" (not "Warrant") must not
        // false-positive on a naive substring match of just "war".
        assert!(!is_warrant(Some("Warner Bros. Discovery, Inc.")));
    }

    #[test]
    fn missing_name_fails_open_not_closed() {
        // A classification-only concern -- an asset with no name field
        // shouldn't be silently dropped from the universe over it.
        assert!(!is_warrant(None));
    }
}
