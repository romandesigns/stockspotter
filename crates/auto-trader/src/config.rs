//! Tunables, each overridable via env var with a hardcoded, doc-commented
//! default — mirrors `market_data::live`'s `QUALIFY_SERVICE_URL` pattern
//! exactly (`const DEFAULT_*` + `std::env::var(...).unwrap_or_else(...)`),
//! not a new idiom.
//!
//! Deliberately **not** built on `market_data::config::AlpacaConfig` —
//! this service never calls Alpaca directly (see `client.rs`'s own doc
//! comment on why v1 has no HTTP client at all), so it needs none of
//! those credentials. It only needs to reach `ws-server`, which is why
//! every var here has a safe default: this service runs correctly with
//! zero `.env` changes on the VPS.

/// Internal Docker-network address for local dev vs. the VPS compose
/// override (`ws://ws:8787`, set via `AUTO_TRADER_WS_URL` in
/// `ops/vps/docker-compose.yml`, same as `ws`'s own `QUALIFY_SERVICE_URL`
/// override).
const DEFAULT_WS_URL: &str = "ws://localhost:8787";

/// Simulated position size in dollars per entry — Roman's own explicit
/// choice ("small fixed size... e.g. $500-$1,000 per trade"), picked the
/// conservative end of that range as the default.
const DEFAULT_POSITION_SIZE_USD: f64 = 500.0;

/// Max simultaneously-open simulated positions — the middle of Roman's
/// own stated "max 3-5 concurrent positions" range.
const DEFAULT_MAX_CONCURRENT_POSITIONS: usize = 4;

/// Same gitignored `data/` dir the live-efficiency benchmark's JSONL logs
/// already live in (`data/live_pending_signals.jsonl` etc.) — one shared
/// mount (`../../data:/app/data` in `ops/vps/docker-compose.yml`), not a
/// new volume concept.
const DEFAULT_JOURNAL_PATH: &str = "data/auto_trader_journal.jsonl";

#[derive(Debug, Clone)]
pub struct Config {
    pub ws_url: String,
    pub position_size_usd: f64,
    pub max_concurrent_positions: usize,
    pub journal_path: String,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            ws_url: std::env::var("AUTO_TRADER_WS_URL").unwrap_or_else(|_| DEFAULT_WS_URL.to_string()),
            position_size_usd: std::env::var("AUTO_TRADER_POSITION_SIZE_USD")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_POSITION_SIZE_USD),
            max_concurrent_positions: std::env::var("AUTO_TRADER_MAX_CONCURRENT_POSITIONS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_MAX_CONCURRENT_POSITIONS),
            journal_path: std::env::var("AUTO_TRADER_JOURNAL_PATH").unwrap_or_else(|_| DEFAULT_JOURNAL_PATH.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_romans_own_stated_risk_caps_when_unset() {
        // Doesn't touch real env vars (parallel test runs would race each
        // other) -- just pins the hardcoded fallback constants themselves,
        // since those are what actually governs behavior on a fresh VPS
        // deploy with no `.env` changes at all.
        assert_eq!(DEFAULT_POSITION_SIZE_USD, 500.0);
        assert_eq!(DEFAULT_MAX_CONCURRENT_POSITIONS, 4);
    }
}
