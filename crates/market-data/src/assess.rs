//! HTTP client for the Python qualitative layer's `/assess` endpoint
//! (`python/app/assess.py`) — the Chart Page AI assessment feature
//! (2026-09-03). Same "Rust hands Python a small request, gets back
//! structured JSON" shape as `qualify.rs`'s `qualify_shortlist`, and
//! deliberately reuses that exact pattern rather than inventing a new
//! one for this second Python-calling client.
//!
//! Field names here are plain Rust snake_case on purpose (no
//! `rename_all` needed) — this only ever talks to the Python service,
//! which expects snake_case (`python/app/main.py`'s Pydantic models).
//! The camelCase boundary belongs to `ws-server::http`'s own route,
//! which is what actually faces web/mobile clients — this module has no
//! opinion on wire casing for anyone but Python.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize)]
pub struct MomentumReading {
    pub overall: f64,
    pub volume_confirmation: f64,
    pub structure: f64,
    pub ma_slope: f64,
    pub wick_rejection: f64,
}

#[derive(Debug, Serialize)]
struct AssessRequest<'a> {
    symbol: &'a str,
    momentum: MomentumReading,
    force_refresh: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Assessment {
    pub summary: Vec<String>,
    pub generated_at: String,
}

/// Posts one symbol's current momentum reading to the qualitative
/// layer's `/assess` endpoint and returns Claude's brief, real-time-
/// web-search-informed read on it. `base_url` is e.g. "http://qualify:8000"
/// (the deployed Docker service name) or "http://localhost:8000" (local
/// dev) — same "not a library concern" reasoning `qualify_shortlist`'s
/// own doc comment already gives for not baking a default in here.
///
/// A real Claude call with web search takes several real seconds
/// (confirmed live: ~6s uncached) — this function's timeout needs to be
/// generous accordingly; `reqwest::Client::new()`'s default has no
/// per-request timeout, which is correct here (the caller, `ws-server`'s
/// `/assess` route, is itself just proxying one client request, not on
/// a tight internal deadline).
pub async fn request_assessment(base_url: &str, symbol: &str, momentum: MomentumReading, force_refresh: bool) -> Result<Assessment> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base_url}/assess"))
        .json(&AssessRequest { symbol, momentum, force_refresh })
        .send()
        .await
        .context("requesting qualitative layer /assess endpoint")?
        .error_for_status()
        .context("qualitative layer returned an error status for /assess")?;

    let parsed: Assessment = resp.json().await.context("parsing /assess response")?;
    Ok(parsed)
}
