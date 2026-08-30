//! Loads Alpaca connection config from the environment (`.env` at the repo
//! root via `dotenvy`). Kept separate from any single binary so both the
//! live scan loop and (later) the ignition detector's tick stream can share
//! it without duplicating env-var names.

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct AlpacaConfig {
    pub api_key: String,
    pub api_secret: String,
    /// Data feed tier, e.g. "sip" — see `.env`.
    pub feed: String,
    /// Realtime bars/trades/quotes WS endpoint, feed-specific.
    pub market_ws: String,
    /// REST base for historical/reference market data (bars, snapshots).
    pub data_base: String,
    /// REST base for account/trading endpoints — this is where the
    /// tradable-asset universe list lives (`/v2/assets`), not data_base.
    pub trading_base: String,
    /// Financial Modeling Prep key, for float-share lookups Alpaca
    /// doesn't provide at all. `None` if FMP_API_KEY isn't set — callers
    /// should treat that the same as "float unknown", not panic.
    pub fmp_api_key: Option<String>,
}

impl AlpacaConfig {
    /// Reads the standard `ALPACA_*` vars. Does not print or log secret
    /// values anywhere — callers should avoid doing so too.
    pub fn from_env() -> Result<Self> {
        let get = |name: &str| -> Result<String> {
            std::env::var(name).with_context(|| format!("missing env var {name}"))
        };
        Ok(Self {
            api_key: get("ALPACA_API_KEY")?,
            api_secret: get("ALPACA_API_SECRET")?,
            feed: get("ALPACA_FEED")?,
            market_ws: get("ALPACA_MARKET_WS")?,
            data_base: get("ALPACA_DATA_BASE")?,
            trading_base: get("ALPACA_TRADING_BASE")?,
            fmp_api_key: std::env::var("FMP_API_KEY").ok(),
        })
    }
}
