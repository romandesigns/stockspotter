//! HTTP client for the Python qualitative layer (`python/app/main.py`) —
//! architecture doc section 4.4. This crate's job stops at "hand Python
//! the shortlist"; the actual qualitative work (news catalyst tagging)
//! lives entirely on the Python side, by design — the doc is explicit
//! that Python never scans the full market or touches raw tick data,
//! only the small shortlist Rust already qualified.
//!
//! IPC is plain HTTP, not shared memory or a message queue — keeps the
//! two languages fully decoupled, each independently restartable/
//! testable, with one JSON contract as the only thing to keep in sync.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
struct QualifyRequest<'a> {
    symbols: &'a [String],
}

#[derive(Debug, Clone, Deserialize)]
pub struct SymbolQualification {
    pub symbol: String,
    pub catalyst_tags: Vec<String>,
    pub headline_count: u32,
    pub most_recent_headline: Option<String>,
    pub most_recent_published_at: Option<String>,
    /// Set if this symbol's own news lookup failed — a per-symbol
    /// failure doesn't fail the whole batch on the Python side, so this
    /// surfaces it instead of silently dropping the symbol.
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct QualifyResponse {
    results: Vec<SymbolQualification>,
}

/// Posts a shortlist to the qualitative layer's `/qualify` endpoint and
/// returns its per-symbol catalyst tags. `base_url` is e.g.
/// "http://localhost:8000" — no default baked in here, where this
/// service runs is a deployment decision, not a library concern.
pub async fn qualify_shortlist(
    base_url: &str,
    symbols: &[String],
) -> Result<Vec<SymbolQualification>> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base_url}/qualify"))
        .json(&QualifyRequest { symbols })
        .send()
        .await
        .context("requesting qualitative layer /qualify endpoint")?
        .error_for_status()
        .context("qualitative layer returned an error status")?;

    let parsed: QualifyResponse = resp
        .json()
        .await
        .context("parsing qualitative layer response")?;
    Ok(parsed.results)
}
