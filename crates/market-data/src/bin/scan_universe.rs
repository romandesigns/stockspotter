//! Runnable, manual-trigger proof of `market_data::scan_shortlist` — the
//! same wide Stage 1/2 universe scan `live::run_live_scan` now runs on
//! its own schedule internally (see that module's doc comment). Useful
//! as a standalone diagnostic (checking what the funnel finds right now,
//! independent of a running `ws-server`) and for handing a shortlist to
//! the Python qualitative layer directly.
//!
//! Run with: `cargo run -p market-data --bin scan_universe`

use anyhow::Result;
use fast_funnel::FilterThresholds;
use market_data::{qualify_shortlist, scan_shortlist, AlpacaConfig};
use tracing::{info, warn};

/// Where the Python qualitative layer (python/app/main.py) is expected to
/// be running. Dev-only default — a real deployment would make this
/// configurable, but this binary is itself still a dev/proof tool.
const QUALIFY_SERVICE_URL: &str = "http://localhost:8000";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .init();
    dotenvy::dotenv().ok();

    let cfg = AlpacaConfig::from_env()?;
    let thresholds = FilterThresholds::default();

    info!("running the full universe Stage 1/2 scan");
    let symbols = scan_shortlist(&cfg, &thresholds).await?;
    info!(symbols = ?symbols, count = symbols.len(), "final shortlist");

    if symbols.is_empty() {
        return Ok(());
    }

    // Hand the shortlist to Python for the qualitative pass — this is the
    // doc's actual boundary (4.4): Python only ever sees this small
    // shortlist, never the full universe or raw ticks.
    info!(url = QUALIFY_SERVICE_URL, symbols = ?symbols, "handing shortlist to the qualitative layer");
    match qualify_shortlist(QUALIFY_SERVICE_URL, &symbols).await {
        Ok(results) => {
            for r in results {
                if let Some(err) = &r.error {
                    warn!(symbol = %r.symbol, error = %err, "qualitative lookup failed for this symbol");
                    continue;
                }
                info!(
                    symbol = %r.symbol,
                    catalyst_tags = ?r.catalyst_tags,
                    headline_count = r.headline_count,
                    most_recent_headline = ?r.most_recent_headline,
                    "qualitative result"
                );
            }
        }
        Err(e) => {
            warn!(error = %e, "qualitative layer unreachable — is `uvicorn app.main:app` running in python/?");
        }
    }

    Ok(())
}
