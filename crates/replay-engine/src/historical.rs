//! Fetches historical bars/trades/quotes for one symbol over one
//! date/time range from Alpaca's REST API, paginating via
//! `next_page_token` until exhausted. Maps straight into
//! `market_data::{Bar, Trade, Quote}` — the same wire types the live
//! WebSocket path uses — since the per-symbol REST endpoints return items
//! without a `S` (symbol) field (it's implied by the URL), unlike the
//! WS messages which carry it inline.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use market_data::{AlpacaConfig, Bar, Quote, Trade};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct BarRaw {
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
struct BarsPage {
    #[serde(default, deserialize_with = "market_data::alpaca_json::null_as_empty_vec")]
    bars: Vec<BarRaw>,
    next_page_token: Option<String>,
}

/// `timeframe` is Alpaca's own format, e.g. "1Min" or "1Day". `start`/`end`
/// are RFC3339 strings, passed straight through as query params.
pub async fn fetch_historical_bars(
    cfg: &AlpacaConfig,
    symbol: &str,
    start: &str,
    end: &str,
    timeframe: &str,
) -> Result<Vec<Bar>> {
    let client = reqwest::Client::new();
    let mut out = Vec::new();
    let mut page_token: Option<String> = None;

    loop {
        let mut query = vec![
            ("start", start.to_string()),
            ("end", end.to_string()),
            ("timeframe", timeframe.to_string()),
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
            .with_context(|| format!("requesting historical bars for {symbol}"))?
            .error_for_status()
            .with_context(|| format!("alpaca bars endpoint returned an error status for {symbol}"))?;

        let page: BarsPage = resp
            .json()
            .await
            .with_context(|| format!("parsing historical bars response for {symbol}"))?;

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

#[derive(Debug, Deserialize)]
struct TradeRaw {
    #[serde(rename = "p")]
    price: f64,
    #[serde(rename = "s")]
    size: u64,
    #[serde(rename = "t")]
    timestamp: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct TradesPage {
    trades: Vec<TradeRaw>,
    next_page_token: Option<String>,
}

pub async fn fetch_historical_trades(
    cfg: &AlpacaConfig,
    symbol: &str,
    start: &str,
    end: &str,
) -> Result<Vec<Trade>> {
    let client = reqwest::Client::new();
    let mut out = Vec::new();
    let mut page_token: Option<String> = None;

    loop {
        let mut query = vec![
            ("start", start.to_string()),
            ("end", end.to_string()),
            ("feed", cfg.feed.clone()),
            ("limit", "10000".to_string()),
        ];
        if let Some(token) = &page_token {
            query.push(("page_token", token.clone()));
        }

        let resp = client
            .get(format!("{}/v2/stocks/{symbol}/trades", cfg.data_base))
            .header("APCA-API-KEY-ID", &cfg.api_key)
            .header("APCA-API-SECRET-KEY", &cfg.api_secret)
            .query(&query)
            .send()
            .await
            .with_context(|| format!("requesting historical trades for {symbol}"))?
            .error_for_status()
            .with_context(|| format!("alpaca trades endpoint returned an error status for {symbol}"))?;

        let page: TradesPage = resp
            .json()
            .await
            .with_context(|| format!("parsing historical trades response for {symbol}"))?;

        out.extend(page.trades.into_iter().map(|t| Trade {
            symbol: symbol.to_string(),
            price: t.price,
            size: t.size,
            timestamp: t.timestamp,
        }));

        match page.next_page_token {
            Some(token) => page_token = Some(token),
            None => break,
        }
    }

    Ok(out)
}

#[derive(Debug, Deserialize)]
struct QuoteRaw {
    #[serde(rename = "bp")]
    bid_price: f64,
    #[serde(rename = "bs")]
    bid_size: u64,
    #[serde(rename = "ap")]
    ask_price: f64,
    #[serde(rename = "as")]
    ask_size: u64,
    #[serde(rename = "t")]
    timestamp: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct QuotesPage {
    quotes: Vec<QuoteRaw>,
    next_page_token: Option<String>,
}

pub async fn fetch_historical_quotes(
    cfg: &AlpacaConfig,
    symbol: &str,
    start: &str,
    end: &str,
) -> Result<Vec<Quote>> {
    let client = reqwest::Client::new();
    let mut out = Vec::new();
    let mut page_token: Option<String> = None;

    loop {
        let mut query = vec![
            ("start", start.to_string()),
            ("end", end.to_string()),
            ("feed", cfg.feed.clone()),
            ("limit", "10000".to_string()),
        ];
        if let Some(token) = &page_token {
            query.push(("page_token", token.clone()));
        }

        let resp = client
            .get(format!("{}/v2/stocks/{symbol}/quotes", cfg.data_base))
            .header("APCA-API-KEY-ID", &cfg.api_key)
            .header("APCA-API-SECRET-KEY", &cfg.api_secret)
            .query(&query)
            .send()
            .await
            .with_context(|| format!("requesting historical quotes for {symbol}"))?
            .error_for_status()
            .with_context(|| format!("alpaca quotes endpoint returned an error status for {symbol}"))?;

        let page: QuotesPage = resp
            .json()
            .await
            .with_context(|| format!("parsing historical quotes response for {symbol}"))?;

        out.extend(page.quotes.into_iter().map(|q| Quote {
            symbol: symbol.to_string(),
            bid_price: q.bid_price,
            bid_size: q.bid_size,
            ask_price: q.ask_price,
            ask_size: q.ask_size,
            timestamp: q.timestamp,
        }));

        match page.next_page_token {
            Some(token) => page_token = Some(token),
            None => break,
        }
    }

    Ok(out)
}
