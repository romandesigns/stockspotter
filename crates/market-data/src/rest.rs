//! One-shot REST calls against Alpaca's historical data API, used to seed
//! `SessionTracker`s before the realtime stream starts. Not a general
//! historical-bars client — the replay engine (build-order item 7) will
//! need its own, richer version of this; kept minimal here on purpose.

use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::bar::Bar;
use crate::config::AlpacaConfig;

#[derive(Debug, Deserialize)]
struct DailyBarRaw {
    #[serde(rename = "c")]
    close: f64,
    #[serde(rename = "v")]
    volume: u64,
    #[serde(rename = "t")]
    timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
struct BarsResponse {
    #[serde(default, deserialize_with = "crate::alpaca_json::null_values_as_empty_vecs")]
    bars: HashMap<String, Vec<DailyBarRaw>>,
}

/// One trading day's daily bar — the raw material for screening a
/// symbol's history for interesting (or deliberately quiet) sessions to
/// replay, rather than picking dates by hand. Distinct from `DailySeed`,
/// which collapses a trailing window into one aggregate; this keeps the
/// full per-day series.
#[derive(Debug, Clone, Copy)]
pub struct DailyBar {
    pub date: chrono::NaiveDate,
    pub close: f64,
    pub volume: u64,
}

/// Fetches the raw per-day bar series for one symbol over `[start, end)`
/// (RFC3339 strings) — unlike `fetch_daily_seeds*`, which collapses a
/// trailing window into one `DailySeed`, this hands back every day so a
/// caller can screen the history itself (gap%, relative volume) to pick
/// which specific sessions are worth a full intraday replay.
pub async fn fetch_daily_bar_series(
    cfg: &AlpacaConfig,
    symbol: &str,
    start: &str,
    end: &str,
) -> Result<Vec<DailyBar>> {
    let client = reqwest::Client::new();
    let url = format!("{}/v2/stocks/bars", cfg.data_base);
    let resp = client
        .get(&url)
        .header("APCA-API-KEY-ID", &cfg.api_key)
        .header("APCA-API-SECRET-KEY", &cfg.api_secret)
        .query(&[
            ("symbols", symbol.to_string()),
            ("timeframe", "1Day".to_string()),
            ("start", start.to_string()),
            ("end", end.to_string()),
            ("limit", "10000".to_string()),
            ("feed", cfg.feed.clone()),
            ("adjustment", "raw".to_string()),
        ])
        .send()
        .await
        .with_context(|| format!("requesting daily bar series for {symbol}"))?
        .error_for_status()
        .with_context(|| format!("alpaca daily bars endpoint returned an error status for {symbol}"))?;

    let parsed: BarsResponse = resp
        .json()
        .await
        .with_context(|| format!("parsing daily bar series response for {symbol}"))?;

    Ok(parsed
        .bars
        .get(symbol)
        .map(|bars| {
            bars.iter()
                .map(|b| DailyBar {
                    date: b.timestamp.date_naive(),
                    close: b.close,
                    volume: b.volume,
                })
                .collect()
        })
        .unwrap_or_default())
}

#[derive(Debug, Clone, Copy)]
pub struct DailySeed {
    pub prior_close: f64,
    pub avg_daily_volume: u64,
}

/// Fetches the last `lookback_days` daily bars for each symbol, anchored
/// to right now — the correct anchor for seeding a *live* session, where
/// "trailing average as of this moment" and "trailing average as of
/// today's session start" are the same thing that matters.
pub async fn fetch_daily_seeds(
    cfg: &AlpacaConfig,
    symbols: &[String],
    lookback_days: u32,
) -> Result<HashMap<String, DailySeed>> {
    fetch_daily_seeds_as_of(cfg, symbols, lookback_days, chrono::Utc::now()).await
}

/// Same as `fetch_daily_seeds`, but anchored to `as_of` instead of the
/// real current moment — what a replay/backtest actually needs: the
/// trailing average as it would have looked at the *start* of the
/// historical session being replayed, not as of whenever the backtest
/// happens to be run for real.
///
/// This was a genuine lookahead-bias bug before this function existed —
/// `replay_engine::fetch_replay_data` used to call the `Utc::now()`
/// version directly, so a backtest of a past date computed today could
/// silently pull in data from after that date, and the same historical
/// window replayed on different days would produce different seed
/// numbers. Confirmed empirically: SWVL's avg_daily_volume read ~4.16M
/// earlier in the same session this was found, then 17,990 replaying the
/// *identical* Aug 28 window hours later — because real time had moved
/// past the point where Aug 28's own huge-volume day still counted in
/// "the most recent 20 days as of right now".
///
/// `as_of` is truncated to midnight UTC of its own calendar day before
/// use, in both this function and `fetch_daily_seeds` above — even for
/// the live case, this stops a partial, still-forming "today" daily bar
/// from leaking into its own trailing baseline (avg_daily_volume and
/// prior_close should both reflect *prior* days only, never the day
/// currently being evaluated against them).
pub async fn fetch_daily_seeds_as_of(
    cfg: &AlpacaConfig,
    symbols: &[String],
    lookback_days: u32,
    as_of: chrono::DateTime<chrono::Utc>,
) -> Result<HashMap<String, DailySeed>> {
    if symbols.is_empty() {
        return Ok(HashMap::new());
    }

    // `limit` alone, with no `start`/`end`, empirically comes back with
    // zero bars (confirmed against the live endpoint) — Alpaca needs an
    // explicit window, not just a count. Use as_of's own day back
    // `lookback_days`, padded a further 3x for weekends/holidays so
    // `lookback_days` trading sessions actually fit inside the window.
    let end = as_of
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight is always a valid time")
        .and_utc();
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

#[derive(Debug, Deserialize)]
struct IntradayBarRaw {
    #[serde(rename = "o")]
    open: f64,
    #[serde(rename = "h")]
    high: f64,
    #[serde(rename = "l")]
    low: f64,
    #[serde(rename = "c")]
    close: f64,
    #[serde(rename = "v")]
    volume: u64,
    #[serde(rename = "t")]
    timestamp: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct IntradayBarsPage {
    #[serde(default, deserialize_with = "crate::alpaca_json::null_as_empty_vec")]
    bars: Vec<IntradayBarRaw>,
    next_page_token: Option<String>,
}

/// Real 1-minute historical bars for one symbol over `[start, end)`
/// (RFC3339 strings), paginating via `next_page_token` until exhausted --
/// used to backfill a Super Chart with real history the moment a symbol
/// is selected, rather than only whatever's accumulated live since
/// `ws-server` started tracking it this session. This module's own doc
/// comment says "not a general historical-bars client, the replay engine
/// will need its own" -- and it did (`replay_engine::historical::
/// fetch_historical_bars`, nearly identical to this), but that crate
/// isn't a dependency of `ws-server`/the live path, and pulling it in
/// just for this would drag in backtest-only scope. This is a deliberate,
/// small duplication of that function rather than a shared dependency,
/// flagged here rather than silently left unexplained; a real
/// consolidation candidate if a third caller ever needs the same thing.
pub async fn fetch_recent_minute_bars(cfg: &AlpacaConfig, symbol: &str, start: &str, end: &str) -> Result<Vec<Bar>> {
    let client = reqwest::Client::new();
    let mut out = Vec::new();
    let mut page_token: Option<String> = None;

    loop {
        let mut query = vec![
            ("start", start.to_string()),
            ("end", end.to_string()),
            ("timeframe", "1Min".to_string()),
            ("feed", cfg.feed.clone()),
            ("limit", "10000".to_string()),
        ];
        if let Some(token) = &page_token {
            query.push(("page_token", token.clone()));
        }

        let resp = client
            .get(format!("{}/v2/stocks/{symbol}/bars", cfg.data_base))
            .header("APCA-API-KEY-ID", &cfg.api_key)
            .header("APCA-API-SECRET-KEY", &cfg.api_secret)
            .query(&query)
            .send()
            .await
            .with_context(|| format!("requesting recent minute bars for {symbol}"))?
            .error_for_status()
            .with_context(|| format!("alpaca bars endpoint returned an error status for {symbol}"))?;

        let page: IntradayBarsPage = resp
            .json()
            .await
            .with_context(|| format!("parsing recent minute bars response for {symbol}"))?;

        out.extend(page.bars.into_iter().map(|b| Bar {
            symbol: symbol.to_string(),
            open: b.open,
            high: b.high,
            low: b.low,
            close: b.close,
            volume: b.volume,
            timestamp: b.timestamp,
        }));

        match page.next_page_token {
            Some(token) => page_token = Some(token),
            None => break,
        }
    }

    Ok(out)
}
