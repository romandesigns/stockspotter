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
    /// Trade condition codes. Needed here specifically so replay can see
    /// halt resumptions -- see `market_data::Trade::conditions`.
    #[serde(rename = "c", default)]
    conditions: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct TradesPage {
    #[serde(default, deserialize_with = "market_data::alpaca_json::null_as_empty_vec")]
    trades: Vec<TradeRaw>,
    next_page_token: Option<String>,
}

pub async fn fetch_historical_trades(
    cfg: &AlpacaConfig,
    symbol: &str,
    start: &str,
    end: &str,
) -> Result<Vec<Trade>> {
    fetch_historical_trades_bounded(cfg, symbol, start, end, PageBudget::Unbounded).await
}

/// Rows asked for per page, and the page size every bound below is
/// expressed in.
const ROWS_PER_PAGE: usize = 10_000;
/// How long one bounded request may take. Shorter than the caller's own
/// overall timeout, so a single stalled page is what fails rather than the
/// whole fetch. The same 30 seconds `market_data::rest` uses.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Body bytes one page of a bounded fetch may carry. A full 10,000-row
/// trade page is a couple of megabytes; past this the answer is not this
/// endpoint's shape any more, and it is refused *while* it arrives rather
/// than buffered and parsed first.
const MAX_PAGE_BYTES: u64 = 16 * 1024 * 1024;

/// How many pages one historical fetch may walk.
///
/// The offline replay CLI wants whatever the session holds and runs under
/// a human's supervision, so it is `Unbounded`. A request made on a live
/// client's behalf is not: it shares the process's provider allowance with
/// discovery and the movers scan, and a symbol whose tape is far larger
/// than expected must fail rather than keep paging. See
/// `ws_server::order_flow_backfill`.
///
/// The budget also decides *how* the request is made. A bounded fetch is
/// one a client triggered, so it:
///
/// - **refuses redirects.** The provider key and secret travel in request
///   headers, and `reqwest`'s default policy follows up to ten hops,
///   re-sending those headers to wherever it is sent. Same
///   `redirect::Policy::none()` as `market_data::rest::daily_history_bounded`;
/// - **times out**, so a stalled page fails instead of holding the slot;
/// - **caps the body it will read and the rows it will keep**, both as the
///   answer arrives. 30 pages of 10,000 trades is 300,000 rows, each with
///   its own allocations; discovering that after buffering and parsing
///   them is too late for a bound to be worth having.
///
/// `Unbounded` keeps the research path's existing behaviour exactly,
/// including its client, because the offline validator depends on reading
/// a whole session however large it is.
#[derive(Debug, Clone, Copy)]
pub enum PageBudget {
    Unbounded,
    Pages(usize),
}

impl PageBudget {
    fn allows(self, walked: usize) -> bool {
        match self {
            PageBudget::Unbounded => true,
            PageBudget::Pages(max) => walked < max,
        }
    }

    /// Rows this budget's worth of pages can legitimately hold. A provider
    /// answering with more than it was asked for does not get to decide
    /// how much memory this process spends.
    fn max_rows(self) -> usize {
        match self {
            PageBudget::Unbounded => usize::MAX,
            PageBudget::Pages(pages) => pages.saturating_mul(ROWS_PER_PAGE),
        }
    }

    /// Body bytes per page, or `None` for the research path, which reads
    /// whatever the endpoint sends.
    fn max_page_bytes(self) -> Option<u64> {
        match self {
            PageBudget::Unbounded => None,
            PageBudget::Pages(_) => Some(MAX_PAGE_BYTES),
        }
    }

    fn client(self) -> Result<reqwest::Client> {
        match self {
            PageBudget::Unbounded => Ok(reqwest::Client::new()),
            PageBudget::Pages(_) => reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .context("building the bounded historical-fetch client"),
        }
    }
}

/// Reads one page's body and parses it, refusing an oversized one as it
/// arrives. `None` means no cap: the research path's own behaviour.
async fn read_page<T: serde::de::DeserializeOwned>(resp: reqwest::Response, max_bytes: Option<u64>, what: &str) -> Result<T> {
    let Some(max_bytes) = max_bytes else {
        return resp.json().await.with_context(|| format!("parsing {what}"));
    };
    if let Some(declared) = resp.content_length() {
        anyhow::ensure!(declared <= max_bytes, "{what}: response declares {declared} bytes, over the {max_bytes}-byte cap");
    }
    let mut resp = resp;
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await.with_context(|| format!("reading {what}"))? {
        anyhow::ensure!(body.len() as u64 + chunk.len() as u64 <= max_bytes, "{what}: response passed the {max_bytes}-byte cap while being read");
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).with_context(|| format!("parsing {what}"))
}

/// A bounded fetch does not follow redirects, so a redirection status is
/// refused here rather than handed on: nothing downstream should try to
/// read a tape out of whatever body a redirector chose to send, and the
/// quiet failure mode -- following it with the provider secret in the
/// headers -- is the one this exists to prevent.
fn reject_redirect(resp: reqwest::Response, what: &str) -> Result<reqwest::Response> {
    anyhow::ensure!(
        !resp.status().is_redirection(),
        "{what}: the provider answered {} and a bounded fetch does not follow redirects (the API secret travels in the request headers)",
        resp.status()
    );
    Ok(resp)
}

/// The next page token, refusing one already walked. A provider that
/// repeats a token is otherwise an endless loop -- bounded only by the
/// page budget, which the research path does not have.
fn next_token(token: Option<String>, seen: &mut std::collections::HashSet<String>, what: &str) -> Result<Option<String>> {
    let Some(token) = token.filter(|t| !t.is_empty()) else {
        return Ok(None);
    };
    anyhow::ensure!(seen.insert(token.clone()), "{what}: the provider repeated a pagination token");
    Ok(Some(token))
}

/// `fetch_historical_trades` with an explicit page budget. Exhausting the
/// budget is an error, never a short answer: a partial tape silently
/// treated as a whole session is exactly the kind of quiet wrongness the
/// order-flow coverage fields exist to prevent.
pub async fn fetch_historical_trades_bounded(
    cfg: &AlpacaConfig,
    symbol: &str,
    start: &str,
    end: &str,
    budget: PageBudget,
) -> Result<Vec<Trade>> {
    let client = budget.client()?;
    let mut out = Vec::new();
    let mut page_token: Option<String> = None;
    let mut pages = 0usize;
    let mut seen = std::collections::HashSet::new();

    loop {
        anyhow::ensure!(budget.allows(pages), "historical trades page budget exhausted for {symbol}");
        pages += 1;
        let mut query = vec![
            ("start", start.to_string()),
            ("end", end.to_string()),
            ("feed", cfg.feed.clone()),
            ("limit", ROWS_PER_PAGE.to_string()),
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
            .with_context(|| format!("requesting historical trades for {symbol}"))?;
        let resp = reject_redirect(resp, &format!("historical trades for {symbol}"))?
            .error_for_status()
            .with_context(|| format!("alpaca trades endpoint returned an error status for {symbol}"))?;

        let page: TradesPage = read_page(resp, budget.max_page_bytes(), &format!("historical trades response for {symbol}")).await?;

        // Checked before the rows are mapped and kept, not after: the
        // allocation this bound exists to prevent is the one below it.
        anyhow::ensure!(
            out.len().saturating_add(page.trades.len()) <= budget.max_rows(),
            "historical trades row budget exhausted for {symbol} ({} rows, cap {})",
            out.len().saturating_add(page.trades.len()),
            budget.max_rows()
        );
        out.extend(page.trades.into_iter().map(|t| Trade {
            symbol: symbol.to_string(),
            price: t.price,
            size: t.size,
            timestamp: t.timestamp,
            conditions: t.conditions,
        }));

        match next_token(page.next_page_token, &mut seen, &format!("historical trades for {symbol}"))? {
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
    #[serde(default, deserialize_with = "market_data::alpaca_json::null_as_empty_vec")]
    quotes: Vec<QuoteRaw>,
    next_page_token: Option<String>,
}

pub async fn fetch_historical_quotes(
    cfg: &AlpacaConfig,
    symbol: &str,
    start: &str,
    end: &str,
) -> Result<Vec<Quote>> {
    fetch_historical_quotes_bounded(cfg, symbol, start, end, PageBudget::Unbounded).await
}

/// `fetch_historical_quotes` with an explicit page budget. The quote tape
/// is roughly ten times the trade tape, so this is the one a caller on a
/// request path must use. See `PageBudget`.
pub async fn fetch_historical_quotes_bounded(
    cfg: &AlpacaConfig,
    symbol: &str,
    start: &str,
    end: &str,
    budget: PageBudget,
) -> Result<Vec<Quote>> {
    let client = budget.client()?;
    let mut out = Vec::new();
    let mut page_token: Option<String> = None;
    let mut pages = 0usize;
    let mut seen = std::collections::HashSet::new();

    loop {
        anyhow::ensure!(budget.allows(pages), "historical quotes page budget exhausted for {symbol}");
        pages += 1;
        let mut query = vec![
            ("start", start.to_string()),
            ("end", end.to_string()),
            ("feed", cfg.feed.clone()),
            ("limit", ROWS_PER_PAGE.to_string()),
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
            .with_context(|| format!("requesting historical quotes for {symbol}"))?;
        let resp = reject_redirect(resp, &format!("historical quotes for {symbol}"))?
            .error_for_status()
            .with_context(|| format!("alpaca quotes endpoint returned an error status for {symbol}"))?;

        let page: QuotesPage = read_page(resp, budget.max_page_bytes(), &format!("historical quotes response for {symbol}")).await?;

        anyhow::ensure!(
            out.len().saturating_add(page.quotes.len()) <= budget.max_rows(),
            "historical quotes row budget exhausted for {symbol} ({} rows, cap {})",
            out.len().saturating_add(page.quotes.len()),
            budget.max_rows()
        );
        out.extend(page.quotes.into_iter().map(|q| Quote {
            symbol: symbol.to_string(),
            bid_price: q.bid_price,
            bid_size: q.bid_size,
            ask_price: q.ask_price,
            ask_size: q.ask_size,
            timestamp: q.timestamp,
        }));

        match next_token(page.next_page_token, &mut seen, &format!("historical quotes for {symbol}"))? {
            Some(token) => page_token = Some(token),
            None => break,
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn quiet_sessions_accept_null_trade_and_quote_pages() {
        let trades: TradesPage = serde_json::from_str(r#"{"symbol":"AREN","trades":null,"next_page_token":null}"#).unwrap();
        let quotes: QuotesPage = serde_json::from_str(r#"{"symbol":"AREN","quotes":null,"next_page_token":null}"#).unwrap();
        assert!(trades.trades.is_empty());
        assert!(quotes.quotes.is_empty());
        assert!(trades.next_page_token.is_none());
        assert!(quotes.next_page_token.is_none());
    }

    #[test]
    fn malformed_pages_are_not_silently_treated_as_quiet_sessions() {
        assert!(serde_json::from_str::<TradesPage>(r#"{"trades":123}"#).is_err());
        assert!(serde_json::from_str::<QuotesPage>(r#"{"quotes":"broken"}"#).is_err());
    }

    #[test]
    fn a_page_budget_stops_at_its_limit_and_unbounded_never_does() {
        let budget = PageBudget::Pages(3);
        assert!(budget.allows(0) && budget.allows(2));
        assert!(!budget.allows(3));
        assert!(PageBudget::Unbounded.allows(usize::MAX - 1));
        // Zero means "not even one request".
        assert!(!PageBudget::Pages(0).allows(0));
    }

    #[test]
    fn a_bounded_budget_caps_rows_and_bytes_and_the_research_path_does_not() {
        assert_eq!(PageBudget::Pages(30).max_rows(), 300_000);
        assert_eq!(PageBudget::Pages(1).max_rows(), ROWS_PER_PAGE);
        assert_eq!(PageBudget::Unbounded.max_rows(), usize::MAX);
        // A pathological page count must not wrap into a tiny cap.
        assert_eq!(PageBudget::Pages(usize::MAX).max_rows(), usize::MAX);
        assert_eq!(PageBudget::Pages(1).max_page_bytes(), Some(MAX_PAGE_BYTES));
        assert_eq!(PageBudget::Unbounded.max_page_bytes(), None);
    }

    #[test]
    fn a_repeated_pagination_token_is_refused_rather_than_walked_again() {
        let mut seen = std::collections::HashSet::new();
        assert_eq!(next_token(Some("a".into()), &mut seen, "trades").unwrap(), Some("a".into()));
        assert_eq!(next_token(Some("b".into()), &mut seen, "trades").unwrap(), Some("b".into()));
        // The same token again: an endless loop, bounded only by a page
        // budget the research path does not have.
        assert!(next_token(Some("a".into()), &mut seen, "trades").is_err());
        // An empty token is "no more pages", the same as its absence.
        assert_eq!(next_token(Some(String::new()), &mut seen, "trades").unwrap(), None);
        assert_eq!(next_token(None, &mut seen, "trades").unwrap(), None);
    }

    // The rules above are what a bounded fetch promises; these exercise
    // them through a real `reqwest` call to a localhost fixture, because
    // "refuses redirects" and "stops reading an oversized body" are
    // properties of the HTTP client and not of the budget type.

    /// A request as the fixture saw it: the target, and whether it carried
    /// the provider credentials.
    #[derive(Debug, Clone)]
    struct Seen {
        target: String,
        sent_secret: bool,
    }

    /// Serves `route` on 127.0.0.1 until the test ends, and records every
    /// request. `route` returns (status, body) for a request target.
    fn fixture(route: impl Fn(&str) -> (u16, String) + Send + 'static) -> (String, Arc<Mutex<Vec<Seen>>>) {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let log = Arc::new(Mutex::new(Vec::<Seen>::new()));
        let seen = Arc::clone(&log);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 8192];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                let Some(target) = head.lines().next().and_then(|l| l.split_whitespace().nth(1)).map(str::to_string) else { continue };
                let sent_secret = head.to_ascii_lowercase().contains("apca-api-secret-key");
                let (status, body) = route(&target);
                let nolength = target.contains("nolength");
                seen.lock().unwrap().push(Seen { target, sent_secret });
                let location = if (300..400).contains(&status) { "Location: /v2/stocks/ELSEWHERE/trades\r\n" } else { "" };
                // A target asking for `nolength` is answered without a
                // Content-Length, so the body is only bounded by the
                // connection closing -- which is how the byte cap has to
                // hold while the body arrives, not from a declared size.
                let length = if nolength { String::new() } else { format!("Content-Length: {}\r\n", body.len()) };
                let _ = write!(stream, "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{location}{length}Connection: close\r\n\r\n{body}");
            }
        });
        (base, log)
    }

    fn config(base: &str) -> AlpacaConfig {
        AlpacaConfig {
            api_key: "test-key".into(),
            api_secret: "test-secret".into(),
            feed: "sip".into(),
            market_ws: String::new(),
            data_base: base.to_string(),
            trading_base: base.to_string(),
            fmp_api_key: None,
        }
    }

    fn trade_rows(n: usize) -> String {
        let rows: Vec<String> = (0..n).map(|i| format!(r#"{{"p":2.0,"s":1,"t":"2026-10-06T14:00:{:02}Z","c":["@"]}}"#, i % 60)).collect();
        rows.join(",")
    }

    #[tokio::test]
    async fn a_bounded_fetch_refuses_a_redirect_instead_of_re_sending_the_api_secret() {
        let (base, log) = fixture(|_| (302, "{}".to_string()));
        let cfg = config(&base);
        let error = fetch_historical_trades_bounded(&cfg, "AAA", "s", "e", PageBudget::Pages(2)).await.unwrap_err();
        assert!(format!("{error:#}").contains("does not follow redirects"), "{error:#}");
        let seen = log.lock().unwrap().clone();
        // Exactly one request: the redirect target was never asked for,
        // which is the point -- reqwest's default policy would have
        // followed it and taken these headers along.
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert!(seen[0].target.contains("/v2/stocks/AAA/trades"));
        assert!(seen[0].sent_secret, "the original request is the one that legitimately carries the secret");
    }

    #[tokio::test]
    async fn a_bounded_fetch_stops_at_its_page_budget_after_making_exactly_that_many_requests() {
        // Every page offers another token, so only the budget stops this.
        let (base, log) = fixture(|target| {
            let page = target.split("page_token=").nth(1).map_or(0, |t| t.split('&').next().unwrap_or("0").parse::<u32>().unwrap_or(0));
            (200, format!(r#"{{"trades":[{}],"next_page_token":"{}"}}"#, trade_rows(1), page + 1))
        });
        let cfg = config(&base);
        let error = fetch_historical_trades_bounded(&cfg, "AAA", "s", "e", PageBudget::Pages(3)).await.unwrap_err();
        assert!(format!("{error:#}").contains("page budget exhausted"), "{error:#}");
        assert_eq!(log.lock().unwrap().len(), 3);
        // A budget is a bound on requests, not a short answer: the partial
        // tape is not returned as if it were a session.
    }

    #[tokio::test]
    async fn a_page_with_more_rows_than_the_budget_allows_is_refused_before_it_is_kept() {
        // One page, one more row than a one-page budget can hold.
        let (base, log) = fixture(|_| (200, format!(r#"{{"trades":[{}],"next_page_token":null}}"#, trade_rows(ROWS_PER_PAGE + 1))));
        let cfg = config(&base);
        let error = fetch_historical_trades_bounded(&cfg, "AAA", "s", "e", PageBudget::Pages(1)).await.unwrap_err();
        assert!(format!("{error:#}").contains("row budget exhausted"), "{error:#}");
        assert_eq!(log.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_bounded_fetch_reads_a_whole_session_when_the_provider_behaves() {
        let (base, log) = fixture(|target| {
            let body = if target.contains("page_token=second") {
                format!(r#"{{"trades":[{}],"next_page_token":null}}"#, trade_rows(2))
            } else {
                format!(r#"{{"trades":[{}],"next_page_token":"second"}}"#, trade_rows(3))
            };
            (200, body)
        });
        let cfg = config(&base);
        let trades = fetch_historical_trades_bounded(&cfg, "AAA", "s", "e", PageBudget::Pages(5)).await.unwrap();
        assert_eq!(trades.len(), 5);
        assert!(trades.iter().all(|t| t.symbol == "AAA"));
        let seen = log.lock().unwrap().clone();
        assert_eq!(seen.len(), 2);
        // The bound is on how much is read, not on how it is read: the
        // request still carries the credentials and the asked-for limit.
        assert!(seen.iter().all(|s| s.sent_secret));
        assert!(seen[0].target.contains(&format!("limit={ROWS_PER_PAGE}")));
    }

    #[tokio::test]
    async fn a_repeated_token_from_a_real_response_stops_the_walk() {
        let (base, log) = fixture(|_| (200, format!(r#"{{"trades":[{}],"next_page_token":"same"}}"#, trade_rows(1))));
        let cfg = config(&base);
        // The page budget is larger than the number of requests this
        // should make: what stops it is the repeated token.
        let error = fetch_historical_trades_bounded(&cfg, "AAA", "s", "e", PageBudget::Pages(20)).await.unwrap_err();
        assert!(format!("{error:#}").contains("repeated a pagination token"), "{error:#}");
        assert_eq!(log.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn an_oversized_body_is_refused_while_it_arrives() {
        let (base, _log) = fixture(|_| (200, format!(r#"{{"trades":[{}],"next_page_token":null}}"#, trade_rows(200))));
        let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
        let resp = client.get(format!("{base}/v2/stocks/AAA/trades")).send().await.unwrap();
        // A cap smaller than the body: refused on the declared length,
        // before a byte of it is parsed.
        let read: Result<TradesPage> = read_page(resp, Some(64), "historical trades response for AAA").await;
        let error = read.unwrap_err();
        assert!(format!("{error:#}").contains("declares"), "{error:#}");
        // The same cap holds for a response that declares no length at
        // all, where it is the bytes arriving that have to be counted.
        let resp = client.get(format!("{base}/v2/stocks/AAA/trades?nolength=1")).send().await.unwrap();
        assert_eq!(resp.content_length(), None);
        let read: Result<TradesPage> = read_page(resp, Some(64), "historical trades response for AAA").await;
        let error = read.unwrap_err();
        assert!(format!("{error:#}").contains("while being read"), "{error:#}");
        // And the same body under a cap that fits parses normally.
        let resp = client.get(format!("{base}/v2/stocks/AAA/trades")).send().await.unwrap();
        let page: TradesPage = read_page(resp, Some(MAX_PAGE_BYTES), "historical trades response for AAA").await.unwrap();
        assert_eq!(page.trades.len(), 200);
    }

    #[tokio::test]
    async fn the_quote_path_is_bounded_the_same_way() {
        let (base, log) = fixture(|_| (302, "{}".to_string()));
        let cfg = config(&base);
        let error = fetch_historical_quotes_bounded(&cfg, "AAA", "s", "e", PageBudget::Pages(2)).await.unwrap_err();
        assert!(format!("{error:#}").contains("does not follow redirects"), "{error:#}");
        assert_eq!(log.lock().unwrap().len(), 1);

        let (base, log) = fixture(|_| (200, r#"{"quotes":[{"bp":1.0,"bs":1,"ap":1.1,"as":2,"t":"2026-10-06T14:00:00Z"}],"next_page_token":"same"}"#.to_string()));
        let cfg = config(&base);
        let error = fetch_historical_quotes_bounded(&cfg, "AAA", "s", "e", PageBudget::Pages(20)).await.unwrap_err();
        assert!(format!("{error:#}").contains("repeated a pagination token"), "{error:#}");
        assert_eq!(log.lock().unwrap().len(), 2);
    }
}
