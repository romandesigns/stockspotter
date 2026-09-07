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
    next_page_token: Option<String>,
}

async fn daily_history(cfg: &AlpacaConfig, symbols: &[String], start: &str, end: &str) -> Result<HashMap<String, Vec<DailyBarRaw>>> {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build()?;
    let mut out: HashMap<String, Vec<DailyBarRaw>> = HashMap::new();
    let mut token: Option<String> = None;
    let mut seen = std::collections::HashSet::new();
    for chunk in symbols.chunks(100) {
        loop {
            let mut query = vec![("symbols", chunk.join(",")), ("timeframe", "1Day".into()),
                ("start", start.into()), ("end", end.into()), ("limit", "10000".into()),
                ("feed", cfg.feed.clone()), ("adjustment", "raw".into()), ("sort", "asc".into())];
            if let Some(t) = &token { query.push(("page_token", t.clone())); }
            let page: BarsResponse = client.get(format!("{}/v2/stocks/bars", cfg.data_base))
                .header("APCA-API-KEY-ID", &cfg.api_key).header("APCA-API-SECRET-KEY", &cfg.api_secret)
                .query(&query).send().await?.error_for_status()?.json().await?;
            for (symbol, bars) in page.bars { out.entry(symbol).or_default().extend(bars); }
            match page.next_page_token.filter(|t| !t.is_empty()) {
                Some(t) => {
                    anyhow::ensure!(seen.insert(t.clone()), "Alpaca repeated a daily-bar pagination token");
                    token = Some(t);
                }
                None => { token = None; seen.clear(); break; }
            }
        }
    }
    for bars in out.values_mut() {
        bars.sort_by_key(|b| b.timestamp);
        bars.dedup_by_key(|b| b.timestamp);
    }
    Ok(out)
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
    let parsed = daily_history(cfg, &[symbol.to_string()], start, end).await?;

    Ok(parsed
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

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
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
/// The boundary is midnight in New York, excluding the active trading date.
/// All pages are consumed before choosing the trailing sessions per symbol.
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
    use chrono::TimeZone;
    let date = as_of.with_timezone(&chrono_tz::America::New_York).date_naive();
    let end = chrono_tz::America::New_York.from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
        .single().unwrap().with_timezone(&Utc);
    let start = end - chrono::Duration::days(lookback_days as i64 * 3);

    anyhow::ensure!(lookback_days > 0, "daily lookback must be positive");
    let parsed = daily_history(cfg, symbols, &start.to_rfc3339(), &end.to_rfc3339()).await?;

    let mut out = HashMap::new();
    for symbol in symbols {
        let Some(bars) = parsed.get(symbol) else {
            tracing::warn!(symbol, "no daily bars returned for symbol; skipping seed");
            continue;
        };
        let bars: Vec<_> = bars.iter().filter(|b| b.timestamp < end).collect();
        let Some(last) = bars.last() else { continue };
        // Select the last N complete sessions after merging all pages.
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

pub async fn fetch_session_bars(cfg: &AlpacaConfig, symbol: &str) -> Result<Vec<Bar>> {
    use chrono::TimeZone;
    let now = Utc::now();
    let date = now.with_timezone(&chrono_tz::America::New_York).date_naive();
    let start = chrono_tz::America::New_York.from_local_datetime(&date.and_hms_opt(4, 0, 0).unwrap()).single().unwrap();
    if now <= start { return Ok(Vec::new()); }
    let mut bars = fetch_recent_minute_bars(cfg, symbol, &start.to_rfc3339(), &now.to_rfc3339()).await?;
    bars.retain(|b| b.timestamp + chrono::Duration::minutes(1) <= now);
    Ok(bars)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn daily_seeds_follow_pages_and_exclude_the_current_new_york_session() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for (index, body) in [
                r#"{"bars":{"AAA":[{"t":"2026-09-02T04:00:00Z","c":10,"v":100}]},"next_page_token":"page2"}"#,
                r#"{"bars":{"AAA":[{"t":"2026-09-03T04:00:00Z","c":11,"v":300}],"ZZZ":[{"t":"2026-09-03T04:00:00Z","c":20,"v":400},{"t":"2026-09-04T04:00:00Z","c":99,"v":9999}]},"next_page_token":null}"#,
            ].iter().enumerate() {
                let (mut stream, _) = listener.accept().unwrap();
                let mut bytes = [0; 8192];
                let n = stream.read(&mut bytes).unwrap();
                let request = String::from_utf8_lossy(&bytes[..n]);
                if index == 1 { assert!(request.contains("page_token=page2")); }
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        let cfg = AlpacaConfig { api_key:"test".into(),api_secret:"test".into(),feed:"sip".into(),
            market_ws:String::new(),data_base:format!("http://{address}"),trading_base:String::new(),fmp_api_key:None };
        // Sept 5 UTC is still Sept 4 in New York. Sept 4 must remain excluded.
        let seeds = fetch_daily_seeds_as_of(&cfg,&["AAA".into(),"ZZZ".into()],2,"2026-09-05T00:30:00Z".parse().unwrap()).await.unwrap();
        assert_eq!(seeds["AAA"].prior_close,11.0);
        assert_eq!(seeds["AAA"].avg_daily_volume,200);
        assert_eq!(seeds["ZZZ"].prior_close,20.0);
        assert_eq!(seeds["ZZZ"].avg_daily_volume,400);
        server.join().unwrap();
    }
}
