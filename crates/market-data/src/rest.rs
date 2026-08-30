//! One-shot REST calls against Alpaca's historical data API, used to seed
//! `SessionTracker`s before the realtime stream starts. Not a general
//! historical-bars client — the replay engine (build-order item 7) will
//! need its own, richer version of this; kept minimal here on purpose.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::config::AlpacaConfig;

#[derive(Debug, Deserialize)]
struct DailyBarRaw {
    #[serde(rename = "c")]
    close: f64,
    #[serde(rename = "v")]
    volume: u64,
}

#[derive(Debug, Deserialize)]
struct BarsResponse {
    #[serde(default)]
    bars: HashMap<String, Vec<DailyBarRaw>>,
}

#[derive(Debug, Clone, Copy)]
pub struct DailySeed {
    pub prior_close: f64,
    pub avg_daily_volume: u64,
}

/// Fetches the last `lookback_days` daily bars for each symbol and derives
/// a prior-close + trailing-average-volume seed from them. Symbols with no
/// returned bars (delisted, bad ticker, etc.) are skipped with a warning
/// rather than failing the whole batch.
pub async fn fetch_daily_seeds(
    cfg: &AlpacaConfig,
    symbols: &[String],
    lookback_days: u32,
) -> Result<HashMap<String, DailySeed>> {
    if symbols.is_empty() {
        return Ok(HashMap::new());
    }

    // `limit` alone, with no `start`/`end`, empirically comes back with
    // zero bars (confirmed against the live endpoint) — Alpaca needs an
    // explicit window, not just a count. Use today back `lookback_days`,
    // padded a further 3x for weekends/holidays so `lookback_days` trading
    // sessions actually fit inside the window.
    let end = chrono::Utc::now();
    let start = end - chrono::Duration::days(lookback_days as i64 * 3);

    let client = reqwest::Client::new();
    let url = format!("{}/v2/stocks/bars", cfg.data_base);
    let resp = client
        .get(&url)
        .header("APCA-API-KEY-ID", &cfg.api_key)
        .header("APCA-API-SECRET-KEY", &cfg.api_secret)
        .query(&[
            ("symbols", symbols.join(",")),
            ("timeframe", "1Day".to_string()),
            ("start", start.to_rfc3339()),
            ("end", end.to_rfc3339()),
            ("limit", (lookback_days * symbols.len().max(1) as u32).to_string()),
            ("feed", cfg.feed.clone()),
            ("adjustment", "raw".to_string()),
        ])
        .send()
        .await
        .context("requesting daily bars from alpaca")?
        .error_for_status()
        .context("alpaca daily bars endpoint returned an error status")?;

    let parsed: BarsResponse = resp
        .json()
        .await
        .context("parsing alpaca daily bars response")?;

    let mut out = HashMap::new();
    for symbol in symbols {
        let Some(bars) = parsed.bars.get(symbol) else {
            tracing::warn!(symbol, "no daily bars returned for symbol; skipping seed");
            continue;
        };
        let Some(last) = bars.last() else { continue };
        // `limit` above is a total cap shared across all requested symbols,
        // not a guaranteed per-symbol count, so trim explicitly here rather
        // than trust however many bars this particular symbol got back.
        let trailing = &bars[bars.len().saturating_sub(lookback_days as usize)..];
        let avg_daily_volume = trailing.iter().map(|b| b.volume).sum::<u64>() / trailing.len() as u64;
        out.insert(
            symbol.clone(),
            DailySeed {
                prior_close: last.close,
                avg_daily_volume,
            },
        );
    }
    Ok(out)
}
