//! Post-session historical-trades adapter and the outcome-evidence archive.
//!
//! **Separate from the evaluator** (`analysis`), which only ever sees the
//! normalised [`TradeEvidence`] this module reconstructs from archived raw
//! pages. **Behind the outcome firewall:** fetching and loading both take an
//! [`OutcomeAccess`], which only a campaign whose capture set is closed and
//! whose outcome fetch was explicitly authorized can issue.
//!
//! **No transport is implemented here.** The adapter speaks to a
//! [`PageSource`]; this build ships only test sources. The concrete HTTP
//! source is a separate, reviewed addition at the gated fetch step, so this
//! code cannot make a real request by construction.
//!
//! Reproducibility: every HTTP exchange's exact body is archived with its
//! SHA-256, the page chain (tokens in and out) is checked against the
//! request URLs, and normalised trades are re-derived from the raw pages on
//! every load -- the archived normalised file is verified, never trusted. An
//! incomplete fetch is archived as incomplete (manifest flag plus an
//! `INCOMPLETE` marker) and loads as incomplete evidence, which the evaluator
//! censors.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::analysis::{to_micros, Micros, Trade, TradeEvidence};
use super::campaign::OutcomeAccess;
use super::prereg::sha256_hex;

pub const FETCH_CONTRACT: &str = "alpaca-v2-stocks-trades-v1";
pub const ENDPOINT: &str = "https://data.alpaca.markets/v2/stocks/{symbol}/trades";
pub const FEED: &str = "sip";
pub const PAGE_LIMIT: u32 = 10_000;
pub const EVIDENCE_SCHEMA: &str = "outcome-evidence-v1";
const MANIFEST: &str = "manifest.json";
const NORMALIZED: &str = "normalized.ndjson";
const INCOMPLETE: &str = "INCOMPLETE";

// ===========================================================================
// Request
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeRequest {
    /// Market session `YYYY-MM-DD`; must contain `[start, end)`.
    pub session: String,
    pub symbol: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub feed: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchRefusal {
    OutcomeFirewall(String),
    InvalidRequest(String),
    /// Post-session only: the session's regular close has not passed.
    SessionNotClosed,
}

fn nanos(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl TradeRequest {
    pub fn validate(&self, now: DateTime<Utc>) -> Result<(), FetchRefusal> {
        let bad = |m: &str| Err(FetchRefusal::InvalidRequest(m.to_string()));
        if self.symbol.is_empty()
            || self.symbol.len() > 12
            || !self.symbol.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'.')
        {
            return bad("symbol");
        }
        if self.feed != FEED {
            return bad("feed must be sip");
        }
        if self.start >= self.end || self.end - self.start > Duration::hours(16) {
            return bad("interval");
        }
        let day = market_data::trading_session::market_day(self.start);
        if day.to_string() != self.session
            || market_data::trading_session::market_day(self.end - Duration::nanoseconds(1)) != day
        {
            return bad("interval is not inside the stated session");
        }
        match market_data::trading_session::regular_session_close(day) {
            Some(close) if now >= close => Ok(()),
            Some(_) => Err(FetchRefusal::SessionNotClosed),
            None => bad("no regular session that day"),
        }
    }

    /// The exact URL of one page. Deterministic, so the archive can prove
    /// which request produced each archived body.
    pub fn page_url(&self, page_token: Option<&str>) -> String {
        let mut url = format!(
            "{}?start={}&end={}&limit={PAGE_LIMIT}&feed={}&sort=asc",
            ENDPOINT.replace("{symbol}", &encode(&self.symbol)),
            encode(&nanos(self.start)),
            encode(&nanos(self.end)),
            encode(&self.feed),
        );
        if let Some(t) = page_token {
            url.push_str("&page_token=");
            url.push_str(&encode(t));
        }
        url
    }
}

// ===========================================================================
// Transport seam
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageResponse {
    pub http_status: u16,
    pub retry_after_secs: Option<u64>,
    pub body: Vec<u8>,
    pub retrieved_at: DateTime<Utc>,
}

/// One HTTP GET. `Err` is a transport failure (no response).
pub trait PageSource {
    fn get(&mut self, url: &str) -> Result<PageResponse, String>;
}

#[derive(Debug, Clone, Copy)]
pub struct FetchConfig {
    pub max_pages: usize,
    pub max_rate_limit_retries: u32,
    pub default_backoff_secs: u64,
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self { max_pages: 1_000, max_rate_limit_retries: 5, default_backoff_secs: 2 }
    }
}

// ===========================================================================
// Normalisation
// ===========================================================================

/// One trade exactly as normalised; its JSON line is the hashed form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedTrade {
    /// Exchange timestamp, RFC 3339 with nanoseconds.
    pub t: String,
    /// Price in micro-dollars.
    pub p: Micros,
    pub s: u64,
    pub x: String,
    pub i: u64,
    pub z: String,
    pub c: Vec<String>,
}

impl NormalizedTrade {
    fn to_trade(&self) -> Trade {
        Trade {
            exchange_at: self.t.parse().expect("normalised timestamp"),
            price: self.p,
            tape: self.z.clone(),
            conditions: self.c.clone(),
        }
    }
}

/// Parses one page body. Any structural defect is an error: a partial or
/// malformed page never contributes trades.
pub fn parse_page(body: &[u8], req: &TradeRequest) -> Result<(Vec<NormalizedTrade>, Option<String>), String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("unparseable page: {e}"))?;
    if !v.is_object() {
        return Err("page is not an object".into());
    }
    if let Some(sym) = v.get("symbol") {
        if sym.as_str() != Some(req.symbol.as_str()) {
            return Err(format!("page symbol {sym} is not {}", req.symbol));
        }
    }
    let next = match v.get("next_page_token") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(other) => return Err(format!("bad next_page_token {other}")),
    };
    let empty = Vec::new();
    let list = match v.get("trades") {
        None | Some(Value::Null) => &empty,
        Some(Value::Array(a)) => a,
        Some(_) => return Err("trades is not an array".into()),
    };
    let mut out = Vec::with_capacity(list.len());
    for (n, t) in list.iter().enumerate() {
        let at: DateTime<Utc> = t["t"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or(format!("trade {n}: timestamp"))?;
        if at < req.start || at >= req.end {
            return Err(format!("trade {n}: outside the requested interval"));
        }
        let price = t["p"].as_f64().and_then(to_micros).ok_or(format!("trade {n}: price"))?;
        let size = t["s"].as_u64().ok_or(format!("trade {n}: size"))?;
        let conditions = match &t["c"] {
            Value::Null => Vec::new(),
            Value::Array(a) => a
                .iter()
                .map(|c| c.as_str().map(str::to_string).ok_or(format!("trade {n}: condition")))
                .collect::<Result<_, _>>()?,
            _ => return Err(format!("trade {n}: conditions")),
        };
        out.push(NormalizedTrade {
            t: nanos(at),
            p: price,
            s: size,
            x: t["x"].as_str().unwrap_or_default().to_string(),
            i: t["i"].as_u64().ok_or(format!("trade {n}: id"))?,
            z: t["z"].as_str().unwrap_or_default().to_string(),
            c: conditions,
        });
    }
    Ok((out, next))
}

fn normalized_bytes(trades: &[NormalizedTrade]) -> Vec<u8> {
    let mut b = Vec::new();
    for t in trades {
        b.extend(serde_json::to_vec(t).expect("serialise"));
        b.push(b'\n');
    }
    b
}

// ===========================================================================
// Fetch
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exchange {
    pub url: String,
    pub page_token_in: Option<String>,
    /// `page`, `rate-limited`, `http-error`, `unparseable` or `transport-error`.
    pub role: &'static str,
    pub http_status: Option<u16>,
    pub transport_error: Option<String>,
    pub retrieved_at: DateTime<Utc>,
    pub body: Vec<u8>,
    pub next_page_token: Option<String>,
    pub trade_count: usize,
}

#[derive(Debug, Clone)]
pub struct FetchResult {
    pub request: TradeRequest,
    pub exchanges: Vec<Exchange>,
    pub complete: bool,
    pub errors: Vec<String>,
    pub trades: Vec<NormalizedTrade>,
}

impl FetchResult {
    pub fn evidence(&self) -> TradeEvidence {
        evidence_of(&self.request, self.complete, &self.trades)
    }
}

fn evidence_of(req: &TradeRequest, complete: bool, trades: &[NormalizedTrade]) -> TradeEvidence {
    TradeEvidence {
        symbol: req.symbol.clone(),
        covered_from: req.start,
        covered_to: req.end,
        response_complete: complete,
        gaps: Vec::new(),
        trades: trades.iter().map(NormalizedTrade::to_trade).collect(),
    }
}

/// Fetches every page of one request. Refusals (firewall, invalid or
/// pre-close request) happen before any request is made. Every failure after
/// that -- transport, HTTP error, exhausted rate limit, malformed or partial
/// page, repeated page token, page cap -- ends the fetch **incomplete** with
/// the reason recorded; nothing is retried silently except a bounded,
/// logged rate-limit backoff.
pub fn fetch_trades(
    access: &OutcomeAccess,
    request: &TradeRequest,
    source: &mut dyn PageSource,
    cfg: FetchConfig,
    now: DateTime<Utc>,
    sleep: &mut dyn FnMut(u64),
) -> Result<FetchResult, FetchRefusal> {
    if !access.allows(&request.session) {
        return Err(FetchRefusal::OutcomeFirewall(request.session.clone()));
    }
    request.validate(now)?;
    let mut r = FetchResult { request: request.clone(), exchanges: Vec::new(), complete: false, errors: Vec::new(), trades: Vec::new() };
    let mut token: Option<String> = None;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut pages = 0usize;
    let mut last_at: Option<String> = None;
    'pages: loop {
        if pages >= cfg.max_pages {
            r.errors.push(format!("page cap {} reached", cfg.max_pages));
            break;
        }
        let url = request.page_url(token.as_deref());
        let mut retries = 0u32;
        let resp = loop {
            match source.get(&url) {
                Err(e) => {
                    r.exchanges.push(Exchange {
                        url: url.clone(),
                        page_token_in: token.clone(),
                        role: "transport-error",
                        http_status: None,
                        transport_error: Some(e.clone()),
                        retrieved_at: now,
                        body: Vec::new(),
                        next_page_token: None,
                        trade_count: 0,
                    });
                    r.errors.push(format!("transport: {e}"));
                    break 'pages;
                }
                Ok(resp) if resp.http_status == 429 => {
                    r.exchanges.push(Exchange {
                        url: url.clone(),
                        page_token_in: token.clone(),
                        role: "rate-limited",
                        http_status: Some(429),
                        transport_error: None,
                        retrieved_at: resp.retrieved_at,
                        body: resp.body.clone(),
                        next_page_token: None,
                        trade_count: 0,
                    });
                    if retries >= cfg.max_rate_limit_retries {
                        r.errors.push("rate limit: retries exhausted".into());
                        break 'pages;
                    }
                    retries += 1;
                    sleep(resp.retry_after_secs.unwrap_or(cfg.default_backoff_secs << (retries - 1)));
                }
                Ok(resp) => break resp,
            }
        };
        let mut ex = Exchange {
            url: url.clone(),
            page_token_in: token.clone(),
            role: "page",
            http_status: Some(resp.http_status),
            transport_error: None,
            retrieved_at: resp.retrieved_at,
            body: resp.body,
            next_page_token: None,
            trade_count: 0,
        };
        if ex.http_status != Some(200) {
            ex.role = "http-error";
            r.errors.push(format!("HTTP {}", resp.http_status));
            r.exchanges.push(ex);
            break;
        }
        match parse_page(&ex.body, request) {
            Err(e) => {
                ex.role = "unparseable";
                r.errors.push(e);
                r.exchanges.push(ex);
                break;
            }
            Ok((trades, next)) => {
                if let (Some(prev), Some(first)) = (&last_at, trades.first()) {
                    // Normalised timestamps are fixed-width RFC 3339 (UTC,
                    // nanoseconds), so string order is time order.
                    if first.t < *prev {
                        r.errors.push("pages out of time order".into());
                    }
                }
                if trades.windows(2).any(|w| w[1].t < w[0].t) {
                    r.errors.push("trades out of time order within a page".into());
                }
                last_at = trades.last().map(|t| t.t.clone()).or(last_at);
                ex.trade_count = trades.len();
                ex.next_page_token = next.clone();
                r.trades.extend(trades);
                r.exchanges.push(ex);
                pages += 1;
                if !r.errors.is_empty() {
                    break;
                }
                match next {
                    None => {
                        r.complete = true;
                        break;
                    }
                    Some(t) => {
                        if !seen.insert(t.clone()) {
                            r.errors.push("repeated page token".into());
                            break;
                        }
                        token = Some(t);
                    }
                }
            }
        }
    }
    Ok(r)
}

// ===========================================================================
// Archive
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceBinding {
    pub condition_table_sha256: String,
    pub status_policy_sha256: String,
    pub implementation_sha: String,
    pub preregistration_sha256: String,
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut f = OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(bytes).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())
}

fn compact(t: DateTime<Utc>) -> String {
    t.format("%Y%m%dT%H%M%S%.9fZ").to_string().replace('.', "")
}

/// Archives one fetch under `root/<session>/<symbol>_<start>_<end>_a<attempt>`.
/// Never overwrites: an existing directory or file is an error. Returns the
/// directory and the manifest's SHA-256.
pub fn archive_fetch(root: &Path, result: &FetchResult, binding: &EvidenceBinding, attempt: u32) -> Result<(PathBuf, String), String> {
    let req = &result.request;
    let parent = root.join(&req.session);
    fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
    let dir = parent.join(format!("{}_{}_{}_a{attempt}", req.symbol, compact(req.start), compact(req.end)));
    fs::create_dir(&dir).map_err(|e| format!("{}: {e} (archives are never overwritten)", dir.display()))?;
    let mut exchanges = Vec::new();
    for (i, ex) in result.exchanges.iter().enumerate() {
        let file = format!("exchange-{i:04}.body");
        write_new(&dir.join(&file), &ex.body)?;
        exchanges.push(json!({
            "index": i,
            "url": ex.url,
            "pageTokenIn": ex.page_token_in,
            "role": ex.role,
            "httpStatus": ex.http_status,
            "transportError": ex.transport_error,
            "retrievedAt": nanos(ex.retrieved_at),
            "file": file,
            "sha256": sha256_hex(&ex.body),
            "bytes": ex.body.len(),
            "nextPageToken": ex.next_page_token,
            "tradeCount": ex.trade_count,
        }));
    }
    let norm = normalized_bytes(&result.trades);
    write_new(&dir.join(NORMALIZED), &norm)?;
    let manifest = json!({
        "schema": EVIDENCE_SCHEMA,
        "fetchContract": FETCH_CONTRACT,
        "session": req.session,
        "symbol": req.symbol,
        "requestedFrom": nanos(req.start),
        "requestedTo": nanos(req.end),
        "feed": req.feed,
        "endpoint": ENDPOINT,
        "pageLimit": PAGE_LIMIT,
        "exchanges": exchanges,
        "complete": result.complete,
        "errors": result.errors,
        "normalizedFile": NORMALIZED,
        "normalizedSha256": sha256_hex(&norm),
        "normalizedCount": result.trades.len(),
        "binding": binding,
    });
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    write_new(&dir.join(MANIFEST), &bytes)?;
    if !result.complete {
        write_new(&dir.join(INCOMPLETE), result.errors.join("\n").as_bytes())?;
    }
    Ok((dir, sha256_hex(&bytes)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveReport {
    pub session: String,
    pub symbol: String,
    pub complete: bool,
    pub pages: usize,
    pub normalized_count: usize,
    pub manifest_sha256: String,
    pub binding: EvidenceBinding,
}

fn reconstruct(dir: &Path) -> Result<(ArchiveReport, TradeRequest, Vec<NormalizedTrade>), String> {
    let manifest_bytes = fs::read(dir.join(MANIFEST)).map_err(|e| format!("manifest: {e}"))?;
    let m: Value = serde_json::from_slice(&manifest_bytes).map_err(|e| format!("manifest: {e}"))?;
    if m["schema"] != EVIDENCE_SCHEMA || m["fetchContract"] != FETCH_CONTRACT {
        return Err("manifest schema or fetch contract".into());
    }
    let s = |k: &str| m[k].as_str().map(str::to_string).ok_or(format!("manifest.{k}"));
    let t = |k: &str| -> Result<DateTime<Utc>, String> { s(k)?.parse().map_err(|_| format!("manifest.{k}")) };
    let req = TradeRequest { session: s("session")?, symbol: s("symbol")?, start: t("requestedFrom")?, end: t("requestedTo")?, feed: s("feed")? };
    let binding: EvidenceBinding = serde_json::from_value(m["binding"].clone()).map_err(|e| format!("binding: {e}"))?;
    let exchanges = m["exchanges"].as_array().ok_or("manifest.exchanges")?;
    let mut trades = Vec::new();
    let mut expected_token: Option<String> = None;
    let mut pages = 0usize;
    let mut last_role = "";
    let mut last_next: Option<String> = None;
    for (i, e) in exchanges.iter().enumerate() {
        if e["index"].as_u64() != Some(i as u64) {
            return Err(format!("exchange {i}: index"));
        }
        let file = e["file"].as_str().ok_or(format!("exchange {i}: file"))?;
        let body = fs::read(dir.join(file)).map_err(|err| format!("exchange {i}: {err}"))?;
        if Some(sha256_hex(&body).as_str()) != e["sha256"].as_str() || e["bytes"].as_u64() != Some(body.len() as u64) {
            return Err(format!("exchange {i}: body does not match its recorded SHA-256"));
        }
        let token_in = e["pageTokenIn"].as_str().map(str::to_string);
        if token_in != expected_token {
            return Err(format!("exchange {i}: page chain broken"));
        }
        if e["url"].as_str() != Some(req.page_url(token_in.as_deref()).as_str()) {
            return Err(format!("exchange {i}: URL is not the request's"));
        }
        last_role = match e["role"].as_str().ok_or(format!("exchange {i}: role"))? {
            "page" => "page",
            "rate-limited" => "rate-limited",
            "http-error" => "http-error",
            "unparseable" => "unparseable",
            "transport-error" => "transport-error",
            _ => return Err(format!("exchange {i}: unknown role")),
        };
        if last_role == "page" {
            let (page, next) = parse_page(&body, &req).map_err(|err| format!("exchange {i}: {err}"))?;
            if next.as_deref() != e["nextPageToken"].as_str() || e["tradeCount"].as_u64() != Some(page.len() as u64) {
                return Err(format!("exchange {i}: recorded page metadata differs from the body"));
            }
            trades.extend(page);
            pages += 1;
            last_next = next.clone();
            expected_token = next;
        }
    }
    let norm = normalized_bytes(&trades);
    let stored = fs::read(dir.join(NORMALIZED)).map_err(|e| format!("normalized: {e}"))?;
    if stored != norm || m["normalizedSha256"].as_str() != Some(sha256_hex(&norm).as_str()) {
        return Err("normalized trades do not re-derive from the raw pages".into());
    }
    let errors_empty = m["errors"].as_array().is_some_and(|a| a.is_empty());
    let derived_complete = pages > 0 && last_role == "page" && last_next.is_none() && errors_empty;
    if m["complete"].as_bool() != Some(derived_complete) {
        return Err("manifest completeness is not what the exchanges show".into());
    }
    if dir.join(INCOMPLETE).exists() == derived_complete {
        return Err("INCOMPLETE marker disagrees with the manifest".into());
    }
    let report = ArchiveReport {
        session: req.session.clone(),
        symbol: req.symbol.clone(),
        complete: derived_complete,
        pages,
        normalized_count: trades.len(),
        manifest_sha256: sha256_hex(&manifest_bytes),
        binding,
    };
    Ok((report, req, trades))
}

/// Integrity check only; read-only and idempotent. Returns no trades.
pub fn verify_archive(dir: &Path) -> Result<ArchiveReport, String> {
    reconstruct(dir).map(|(r, _, _)| r)
}

/// Loads archived evidence for the evaluator: behind the firewall, and only
/// for the identities the preregistration binds.
pub fn load_evidence(access: &OutcomeAccess, dir: &Path, expected: &EvidenceBinding) -> Result<(TradeEvidence, ArchiveReport), String> {
    let (report, req, trades) = reconstruct(dir)?;
    if !access.allows(&report.session) {
        return Err(format!("outcome firewall: session {} is not unlocked", report.session));
    }
    if report.binding != *expected {
        return Err("evidence binding differs from the preregistered identities".into());
    }
    Ok((evidence_of(&req, report.complete, &trades), report))
}
