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
//!
//! **Rolling 24h "best reading", not a live-only snapshot.** Every tick
//! used to just re-rank the *current* snapshot and throw the previous
//! ranking away — a stock that gapped hard at 5am premarket and has since
//! cooled off would simply vanish from Top Gainers once something else
//! was live-gaining harder. `update_rolling_best` now keeps each symbol's
//! best-observed reading (by whichever metric a given list ranks on) for
//! a trailing 24h window, tagged with the `TradingSession` it happened
//! in, so an earlier session's real mover stays visible (with a label
//! saying when) instead of disappearing the moment it's no longer live.
//! For "Highly Trading" specifically, `session_volume` resets to 0 at
//! each new session, so a session that already ended with a high count
//! can keep out-ranking a newer, still-accumulating one until it ages out
//! of the window — accepted deliberately (the code and shared-types
//! comment call this out): "which session printed the most shares in the
//! last 24h" is itself a meaningful, honestly-labeled question, the same
//! shape as Top Gainers' peak-%-gain framing.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use fast_funnel::TickerSnapshot;
use serde::Serialize;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::config::AlpacaConfig;
use crate::trading_session::{classify_session, TradingSession};
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
    /// Which trading session produced this reading. `None` for the
    /// historical date-lookup path (`history::fetch_gainers_for_date`) --
    /// that path only ever fetches **daily** bars for one past date, so it
    /// has no intraday resolution to classify a session from; the
    /// frontend simply omits the label rather than showing a fabricated
    /// one. `Some` for the live rolling-24h tracker below, stamped at
    /// whatever moment produced this symbol's best-observed reading.
    pub session: Option<TradingSession>,
}

/// How many rows each ranked list keeps — a leaderboard, not a full
/// universe dump.
const TOP_N: usize = 25;

/// How long a symbol's recorded best reading stays eligible before a
/// fresh live reading replaces it outright regardless of magnitude — this
/// is what makes the leaderboard "rolling 24h" rather than "ever".
const ROLLING_WINDOW: chrono::Duration = chrono::Duration::hours(24);

/// One symbol's best-observed `Mover` (by whichever metric the caller is
/// tracking) within the rolling window, and when it was observed.
#[derive(Debug, Clone)]
struct BestObservation {
    mover: Mover,
    observed_at: DateTime<Utc>,
}

/// Per-symbol rolling-best state for one ranked list (gainers or
/// most-active) — lives only inside `spawn_periodic_movers_scan`'s own
/// task, not shared: nothing outside that task ever needs to read or
/// write it directly, only the ranked `TodayMovers` snapshot it produces
/// each tick (see `SharedTodayMovers` below).
type RollingBest = HashMap<String, BestObservation>;

/// Folds this tick's fresh universe snapshot into `state` in place. A
/// symbol's recorded best is replaced when there isn't one yet, when the
/// existing one has aged out of `ROLLING_WINDOW`, or when the live
/// reading now exceeds it by `metric`'s measure — otherwise the existing
/// (better, still-fresh) reading is left untouched. `metric` is what
/// makes this one function drive both trackers: `|m| m.change_pct` for
/// Top Gainers, `|m| m.volume as f64` for Highly Trading.
fn update_rolling_best(state: &mut RollingBest, snapshots: &HashMap<String, TickerSnapshot>, now: DateTime<Utc>, metric: impl Fn(&Mover) -> f64) {
    let session = classify_session(now);
    for s in snapshots.values() {
        let live = Mover { symbol: s.symbol.clone(), price: s.price, change_pct: s.gap_pct, volume: s.session_volume, session: Some(session) };
        let live_value = metric(&live);
        let replace = match state.get(&s.symbol) {
            None => true,
            Some(existing) => now.signed_duration_since(existing.observed_at) >= ROLLING_WINDOW || live_value > metric(&existing.mover),
        };
        if replace {
            state.insert(s.symbol.clone(), BestObservation { mover: live, observed_at: now });
        }
    }
}

/// Top `TOP_N` rows by `metric` descending, off the rolling-best state
/// (not the raw live snapshot) — this is what makes an earlier session's
/// real mover stay on the list after it's no longer live-leading.
fn ranked_top_n(state: &RollingBest, metric: impl Fn(&Mover) -> f64) -> Vec<Mover> {
    let mut rows: Vec<Mover> = state.values().map(|o| o.mover.clone()).collect();
    rows.sort_by(|a, b| metric(b).partial_cmp(&metric(a)).unwrap_or(std::cmp::Ordering::Equal));
    rows.truncate(TOP_N);
    rows
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
/// the funnel's own Alpaca call volume for no real benefit. Also what a
/// symbol's rolling-best reading gets compared against on each refresh.
const MOVERS_RESCAN_INTERVAL: Duration = Duration::from_secs(60);

/// Spawns the background task that keeps `shared` fresh — same
/// fire-and-forget `tokio::spawn` pattern as `live::spawn_periodic_rescan`,
/// deliberately its own separate task/schedule rather than piggybacking on
/// that one (see module doc comment on why this needs the *whole*
/// universe snapshot, not the funnel's shortlist). Owns the two rolling-
/// best trackers itself (plain local `HashMap`s, not shared/locked state)
/// since this is the only task that ever touches them — `shared` only
/// ever receives the already-ranked, already-cloneable `TodayMovers` this
/// tick produced.
pub fn spawn_periodic_movers_scan(cfg: AlpacaConfig, shared: SharedTodayMovers) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(MOVERS_RESCAN_INTERVAL);
        let mut gainers_state: RollingBest = HashMap::new();
        let mut most_active_state: RollingBest = HashMap::new();
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
            let now = Utc::now();
            update_rolling_best(&mut gainers_state, &snapshots, now, |m| m.change_pct);
            update_rolling_best(&mut most_active_state, &snapshots, now, |m| m.volume as f64);

            let gainers = ranked_top_n(&gainers_state, |m| m.change_pct);
            let most_active = ranked_top_n(&most_active_state, |m| m.volume as f64);
            info!(
                gainers = gainers.len(),
                most_active = most_active.len(),
                universe = snapshots.len(),
                tracked_gainers = gainers_state.len(),
                tracked_most_active = most_active_state.len(),
                "movers scan complete"
            );
            *shared.write().await = TodayMovers { gainers, most_active };
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(symbol: &str, gap_pct: f64, volume: u64) -> TickerSnapshot {
        TickerSnapshot { symbol: symbol.to_string(), price: 1.0, float_shares: None, avg_daily_volume: 1_000_000, session_volume: volume, gap_pct }
    }

    fn snapshots(rows: Vec<TickerSnapshot>) -> HashMap<String, TickerSnapshot> {
        rows.into_iter().map(|s| (s.symbol.clone(), s)).collect()
    }

    // EST instant, well inside Premarket (see trading_session.rs's own tests).
    fn premarket_instant() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-01-15T09:00:00Z").unwrap().with_timezone(&Utc)
    }

    fn regular_instant() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-01-15T14:30:00Z").unwrap().with_timezone(&Utc)
    }

    #[test]
    fn a_symbols_earlier_best_survives_a_later_weaker_live_reading() {
        let mut state: RollingBest = HashMap::new();
        // Premarket: FLYE prints a huge 58% gap.
        update_rolling_best(&mut state, &snapshots(vec![snapshot("FLYE", 58.1, 1_000)]), premarket_instant(), |m| m.change_pct);
        // Regular session: FLYE has cooled off to 15%, something else is live-leading.
        update_rolling_best(&mut state, &snapshots(vec![snapshot("FLYE", 15.6, 5_000), snapshot("KITT", 20.0, 2_000)]), regular_instant(), |m| m.change_pct);

        let rows = ranked_top_n(&state, |m| m.change_pct);
        let flye = rows.iter().find(|m| m.symbol == "FLYE").expect("FLYE should still be tracked");
        assert_eq!(flye.change_pct, 58.1, "the real premarket peak should survive, not the cooled-off live reading");
        assert_eq!(flye.session, Some(TradingSession::Premarket), "should be labeled with the session the peak actually happened in");
    }

    #[test]
    fn a_stale_reading_is_replaced_once_it_ages_out_of_the_rolling_window() {
        let mut state: RollingBest = HashMap::new();
        update_rolling_best(&mut state, &snapshots(vec![snapshot("FLYE", 58.1, 1_000)]), premarket_instant(), |m| m.change_pct);

        let more_than_24h_later = premarket_instant() + chrono::Duration::hours(25);
        update_rolling_best(&mut state, &snapshots(vec![snapshot("FLYE", 3.0, 500)]), more_than_24h_later, |m| m.change_pct);

        let flye = &state["FLYE"];
        assert_eq!(flye.mover.change_pct, 3.0, "a fresh live reading should replace an aged-out best, even though it's smaller");
    }

    #[test]
    fn ranked_top_n_sorts_descending_and_truncates() {
        let mut state: RollingBest = HashMap::new();
        update_rolling_best(&mut state, &snapshots(vec![snapshot("A", 5.0, 1), snapshot("B", 50.0, 1), snapshot("C", 20.0, 1)]), regular_instant(), |m| m.change_pct);

        let rows = ranked_top_n(&state, |m| m.change_pct);
        let symbols: Vec<&str> = rows.iter().map(|m| m.symbol.as_str()).collect();
        assert_eq!(symbols, vec!["B", "C", "A"]);
    }
}
