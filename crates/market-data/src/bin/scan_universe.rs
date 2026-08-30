//! Runnable proof of the doc's actual Stage 1/2 "funnel" shape: the whole
//! tradable universe via REST (not a live WS stream — that doesn't scale
//! to thousands of symbols), shrunk down to a shortlist. Distinct from
//! `bin/scan.rs`, which tracks a handful of *already-chosen* symbols live
//! — this is what picks that handful in the first place.
//!
//! Stage 1 (price + float) and Stage 2 (rel-vol + gap) both run, but
//! float lookups only happen for symbols that already cleared price +
//! Stage 2 — not the whole universe, since FMP's free tier (250 req/day)
//! couldn't cover that. Everything else fails Stage 1 closed on unknown
//! float, same as it does everywhere else in this codebase.
//!
//! Run with: `cargo run -p market-data --bin scan_universe`

use anyhow::Result;
use fast_funnel::{explain, run_fast_funnel, FilterThresholds};
use market_data::{fetch_float_shares, fetch_snapshots, fetch_universe, AlpacaConfig};
use tracing::{info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .init();
    dotenvy::dotenv().ok();

    let cfg = AlpacaConfig::from_env()?;
    let thresholds = FilterThresholds::default();

    info!("fetching tradable universe from alpaca");
    let universe = fetch_universe(&cfg).await?;
    info!(count = universe.len(), "universe fetched");

    info!("fetching snapshots (batched)");
    let snapshots = fetch_snapshots(&cfg, &universe).await?;
    info!(
        snapshots_returned = snapshots.len(),
        "snapshots fetched"
    );

    let mut price_ok_count = 0u32;
    let mut stage2_ok_count = 0u32; // price_ok && rel_vol_ok && gap_ok, float notwithstanding
    let mut float_candidates: Vec<String> = Vec::new();

    for snapshot in snapshots.values() {
        let verdict = explain(snapshot, &thresholds);
        if verdict.price_ok {
            price_ok_count += 1;
        }
        if verdict.price_ok && verdict.rel_vol_ok && verdict.gap_ok {
            stage2_ok_count += 1;
            float_candidates.push(snapshot.symbol.clone());
        }
    }

    info!(
        universe_size = universe.len(),
        price_ok = price_ok_count,
        stage2_ok_pending_float = stage2_ok_count,
        "stage 1 (price) / stage 2 (rel-vol + gap) evaluated across the full universe"
    );

    if float_candidates.is_empty() {
        info!("no candidates cleared price + stage 2 this run — nothing to check float for");
        return Ok(());
    }

    let Some(fmp_key) = cfg.fmp_api_key.as_deref() else {
        warn!(
            candidates = float_candidates.len(),
            "FMP_API_KEY not set — these candidates can't clear Stage 1 without float data"
        );
        return Ok(());
    };

    info!(
        candidates = float_candidates.len(),
        "looking up float for stage-2 survivors"
    );
    let mut float_checked_snapshots = Vec::new();
    for symbol in &float_candidates {
        let float_shares = match fetch_float_shares(fmp_key, symbol).await {
            Ok(f) => f,
            Err(e) => {
                warn!(symbol, error = %e, "float lookup failed, treating as unknown");
                None
            }
        };

        let Some(mut snapshot) = snapshots.get(symbol).cloned() else {
            continue;
        };
        snapshot.float_shares = float_shares;

        let verdict = explain(&snapshot, &thresholds);
        info!(
            symbol,
            price = snapshot.price,
            float_shares = ?float_shares,
            passed = verdict.passed(),
            "float-checked candidate"
        );
        float_checked_snapshots.push(snapshot);
    }

    // Cross-check against fast_funnel's own combined entry point, not
    // just the per-condition explain() calls above — same snapshots,
    // float included this time.
    let qualified = run_fast_funnel(&float_checked_snapshots, &thresholds);
    info!(
        symbols = ?qualified.iter().map(|s| &s.symbol).collect::<Vec<_>>(),
        count = qualified.len(),
        "final shortlist"
    );

    Ok(())
}
