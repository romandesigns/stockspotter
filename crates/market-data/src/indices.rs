//! Markets Today panel's data -- "a broad index/market snapshot, not
//! tied to any single-symbol strategy" (App.tsx's own placeholder note
//! before this existed). Alpaca's stock API has no raw index tickers
//! (^GSPC etc. aren't tradable equities/ETFs), so this uses the standard
//! real-world proxy: the four most commonly quoted index-tracking ETFs.
//! Deliberately its own tiny module, decoupled from the funnel and from
//! `movers.rs`'s leaderboards -- same "Strategy Isolation" reasoning
//! used everywhere else in this codebase.

use anyhow::Result;
use serde::Serialize;

use crate::config::AlpacaConfig;
use crate::universe::fetch_snapshots;

/// (ticker, display name) -- fixed, not user-configurable. Same four ETFs
/// most financial sites use as "the market" when the raw index itself
/// isn't directly quotable: S&P 500, Nasdaq 100, Dow Jones, Russell 2000.
pub const MARKET_INDEX_PROXIES: &[(&str, &str)] = &[
    ("SPY", "S&P 500"),
    ("QQQ", "Nasdaq 100"),
    ("DIA", "Dow Jones"),
    ("IWM", "Russell 2000"),
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketIndexReading {
    pub symbol: String,
    pub name: String,
    pub price: f64,
    pub change_pct: f64,
}

/// Fetches a fresh snapshot for just the 4 proxy symbols, in
/// `MARKET_INDEX_PROXIES`' own fixed order. Deliberately stateless/no
/// background cache like `movers.rs`'s `TodayMovers` has -- 4 symbols is
/// one Alpaca batch request (well under `universe::SNAPSHOT_CHUNK_SIZE`),
/// sub-second, so a per-request fetch is simpler than replicating that
/// caching machinery for something this cheap.
pub async fn fetch_markets_today(cfg: &AlpacaConfig) -> Result<Vec<MarketIndexReading>> {
    let symbols: Vec<String> = MARKET_INDEX_PROXIES.iter().map(|(sym, _)| sym.to_string()).collect();
    let snapshots = fetch_snapshots(cfg, &symbols).await?;

    Ok(MARKET_INDEX_PROXIES
        .iter()
        .filter_map(|(symbol, name)| {
            snapshots.get(*symbol).map(|s| MarketIndexReading {
                symbol: symbol.to_string(),
                name: name.to_string(),
                price: s.price,
                change_pct: s.gap_pct,
            })
        })
        .collect())
}
