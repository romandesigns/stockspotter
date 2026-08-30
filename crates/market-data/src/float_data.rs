//! Float-share lookups via Financial Modeling Prep (FMP) — Alpaca has no
//! float endpoint at all, so this fills that specific gap for
//! `fast_funnel`'s Stage 1 filter. Free tier: 250 requests/day, fine for
//! looking up individual shortlisted symbols, not yet enough for a daily
//! full-universe scan (see `.env`'s `FMP_API_KEY` comment for the $19/mo
//! unlimited upgrade path once that's needed).

use anyhow::{Context, Result};
use serde::Deserialize;

const FMP_BASE: &str = "https://financialmodelingprep.com/stable";

#[derive(Debug, Deserialize)]
struct SharesFloatRaw {
    symbol: String,
    #[serde(rename = "floatShares")]
    float_shares: Option<f64>,
}

/// Looks up float-share count for one symbol. `Ok(None)` means FMP simply
/// has no float data for it (not every ticker is covered) — same "unknown
/// float, fail Stage 1 closed" handling as a missing source entirely, not
/// an error. An actual request/parse failure is still `Err`.
pub async fn fetch_float_shares(api_key: &str, symbol: &str) -> Result<Option<u64>> {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{FMP_BASE}/shares-float"))
        .query(&[("symbol", symbol), ("apikey", api_key)])
        .send()
        .await
        .with_context(|| format!("requesting float data for {symbol} from FMP"))?
        .error_for_status()
        .with_context(|| format!("FMP shares-float endpoint returned an error status for {symbol}"))?;

    let parsed: Vec<SharesFloatRaw> = resp
        .json()
        .await
        .with_context(|| format!("parsing FMP shares-float response for {symbol}"))?;

    Ok(parsed
        .into_iter()
        .find(|r| r.symbol == symbol)
        .and_then(|r| r.float_shares)
        .map(|f| f.round() as u64))
}
