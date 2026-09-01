//! Universe-wide "leaderboard" panels — Top Gainers and Highly Trading —
//! deliberately decoupled from the Stage 1/2 funnel/qualification
//! pipeline in `universe.rs`/`live.rs` (same "Strategy Isolation"
//! principle used throughout this codebase: these are informational
//! rankings, not detection gates, so nothing here reads or feeds funnel
//! state).
//!
//! Reuses `universe::fetch_universe` + `universe::fetch_snapshots` (the
//! same cheap ~3s/~13,378-symbol batched Alpaca pass `scan_shortlist`
//! already runs for the funnel) on its own independent schedule —
//! ranking by `gap_pct`/session volume needs the *whole* universe's raw
//! snapshot, not just whatever survives Stage 1/2's much narrower
//! thresholds (min relative volume, min gap, float), so `scan_shortlist`
//! itself can't be reused directly here.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use fast_funnel::TickerSnapshot;
use serde::Serialize;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::config::AlpacaConfig;
use crate::universe::{fetch_snapshots, fetch_universe};

/// One ranked row — shared shape for both today's live gainers/most-active
/// lists and the historical date-lookup path (`history::fetch_gainers_for_date`),
/// so the client only ever needs to render one row type regardless of
/// which endpoint it came from.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Mover {
    pub symbol: String,
    pub price: f64,
    /// Session change vs prior close, percent — same `gap_pct` figure the
    /// funnel treats as "the" gap metric everywhere else in this codebase.
    pub change_pct: f64,
    pub volume: u64,
}

/// How many rows each ranked list keeps — a leaderboard, not a full
/// universe dump.
const TOP_N: usize = 25;

/// Top `TOP_N` by `change_pct` descending — today's biggest gainers
/// across the *entire* universe, independent of whether a symbol ever
/// cleared the funnel's own float/relative-volume/gap thresholds.
pub fn rank_gainers(snapshots: &HashMap<String, TickerSnapshot>) -> Vec<Mover> {
    let mut rows = to_rows(snapshots);
    rows.sort_by(|a, b| b.change_pct.partial_cmp(&a.change_pct).unwrap_or(std::cmp::Ordering::Equal));
    rows.truncate(TOP_N);
    rows
}

/// Top `TOP_N` by raw session share volume descending — the conventional
/// "Most Active" definition (the same one major screeners use), not
/// relative volume: relative volume already has its own dedicated home in
/// the funnel/halt panels, and raw volume answers a different, equally
/// real question ("what's actually trading the most right now") without
/// depending on `avg_daily_volume` being populated at all.
pub fn rank_most_active(snapshots: &HashMap<String, TickerSnapshot>) -> Vec<Mover> {
    let mut rows = to_rows(snapshots);
    rows.sort_by_key(|a| std::cmp::Reverse(a.volume));
    rows.truncate(TOP_N);
    rows
}

fn to_rows(snapshots: &HashMap<String, TickerSnapshot>) -> Vec<Mover> {
    snapshots
        .values()
        .map(|s| Mover { symbol: s.symbol.clone(), price: s.price, change_pct: s.gap_pct, volume: s.session_volume })
        .collect()
}

/// Both ranked lists for the current/live trading session — what the
/// dashboard shows by default (Top Gainers with no date picked, and
/// Highly Trading always).
#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TodayMovers {
    pub gainers: Vec<Mover>,
    pub most_active: Vec<Mover>,
}

/// Shared, continuously-refreshed handle — `ws-server`'s HTTP layer reads
/// this on every `/movers/today` request rather than fetching on demand,
/// so a request never blocks on a live ~3s universe pass.
pub type SharedTodayMovers = Arc<RwLock<TodayMovers>>;

/// Independent of `live::UNIVERSE_RESCAN_INTERVAL` (15s, funnel
/// freshness) — these are leaderboard panels, not detection gates, so a
/// slower cadence is plenty and keeps this decoupled pass from doubling
/// the funnel's own Alpaca call volume for no real benefit.
const MOVERS_RESCAN_INTERVAL: Duration = Duration::from_secs(60);

/// Spawns the background task that keeps `shared` fresh — same
/// fire-and-forget `tokio::spawn` pattern as `live::spawn_periodic_rescan`,
/// deliberately its own separate task/schedule rather than piggybacking on
/// that one (see module doc comment on why this needs the *whole*
/// universe snapshot, not the funnel's shortlist).
pub fn spawn_periodic_movers_scan(cfg: AlpacaConfig, shared: SharedTodayMovers) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(MOVERS_RESCAN_INTERVAL);
        loop {
            ticker.tick().await;
            let universe = match fetch_universe(&cfg).await {
                Ok(u) => u,
                Err(e) => {
                    warn!(error = %e, "movers scan: failed to fetch universe; keeping stale rankings");
                    continue;
                }
            };
            let snapshots = match fetch_snapshots(&cfg, &universe).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "movers scan: failed to fetch snapshots; keeping stale rankings");
                    continue;
                }
            };
            let gainers = rank_gainers(&snapshots);
            let most_active = rank_most_active(&snapshots);
            info!(
                gainers = gainers.len(),
                most_active = most_active.len(),
                universe = snapshots.len(),
                "movers scan complete"
            );
            *shared.write().await = TodayMovers { gainers, most_active };
        }
    })
}
