//! In-memory 1-minute bar history, so opening a chart does not wait on the
//! data provider.
//!
//! Why this exists (2026-10-06, Roman: the candles should form "as soon as
//! I click", not lag behind Robinhood or TradingView). Every chart open
//! costs a full round trip to Alpaca: measured at 150-180 ms from this
//! server alone, before the client's own hop, on every click, for data
//! that had barely changed since the last one.
//!
//! **Off unless `BAR_CACHE_ENABLED` is set.** With it unset -- which is
//! every deployment today -- `lookup` always misses and nothing is warmed,
//! so `/bars/:symbol` fetches from the provider exactly as it always has.
//! This is a change from the original implementation of this module, which
//! was on by default with an opt-out: a cache that silently answers a
//! chart's history request from a copy this process assembled is not a
//! thing to switch on for a running app without being asked.
//!
//! How it works when it is on:
//! - The first request for a symbol fetches a whole market day of bars
//!   from the provider and keeps them.
//! - From then on the live scan's own finished 1-minute bars keep that
//!   copy current, so later requests are answered from memory.
//! - **Tracked symbols are warmed before anyone asks.** The moment the
//!   live feed produces a bar for a symbol that is not cached, its history
//!   is fetched in the background. Those are the symbols on screen, which
//!   are the ones that get clicked.
//!
//! When the copy is NOT used, and the provider is asked instead:
//! - nothing has refreshed it for `STALE_AFTER` (a symbol that is no
//!   longer tracked gets no live bars, so its copy ages out);
//! - **it is known to have a hole in it.** A live bar does not repair a
//!   copy that already went stale: live bars arrive once a minute, so a
//!   copy nothing refreshed for longer than that is missing a minute, and
//!   the arriving bar is not the one that is missing. Such a copy is
//!   marked as needing the provider again and is not served until a fetch
//!   replaces it -- it would otherwise look current for ever, which is the
//!   one thing a cache must never do. The same applies, for every symbol
//!   at once, when the warmer falls behind the live broadcast and bars are
//!   dropped before it sees them;
//! - the request wants more than a market day;
//! - the request says `fresh` -- the client's repair path after a feed
//!   gap, which must get the provider's own answer, not a copy this
//!   process built from a stream that may have had the same gap. This is
//!   what keeps the chart's gap repair authoritative.
//!
//! **What bounds the warming.** Warm-up fetches are requests nobody asked
//! for, spending an allowance shared with discovery, the movers scan and
//! real chart backfills. So:
//! - at most `WARM_PER_MINUTE` warm-up fetches per minute across all
//!   symbols;
//! - at most `WARM_CONCURRENCY` in flight at once, and a warm-up that
//!   finds all of them busy is **dropped rather than queued**. Queuing
//!   would turn a bounded rate into an unbounded backlog of provider
//!   requests that outlives the reason for making them; the symbol's next
//!   live bar asks again a minute later;
//! - a symbol whose fetch fails enters a `RETRY_COOLDOWN` during which no
//!   further warm-up is started for it. Without that, a symbol the
//!   provider has no data for (a brand-new listing, a bad feed tier)
//!   started a fresh fetch on every live bar, once a minute, for as long
//!   as it stayed tracked.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::Utc;
use market_data::{fetch_recent_minute_bars, AlpacaConfig, ScanEvent};
use serde::Serialize;
use tokio::sync::{broadcast, Semaphore};
use tracing::{debug, warn};

pub const ENABLE_ENV: &str = "BAR_CACHE_ENABLED";
/// How much history is kept per symbol: a whole market day (04:00-20:00 ET).
pub const SEED_MINUTES: i64 = 16 * 60;
/// A copy nothing has refreshed for this long is not trusted. Live bars
/// arrive once a minute for a tracked symbol, so this allows one late bar.
const STALE_AFTER: Duration = Duration::from_secs(90);
/// Symbols kept. Past this the least recently refreshed is dropped.
const MAX_SYMBOLS: usize = 800;
/// Background history fetches allowed at once while warming.
const WARM_CONCURRENCY: usize = 4;
/// Background history fetches started per minute, across all symbols.
const WARM_PER_MINUTE: u32 = 20;
/// How long a symbol whose warm-up failed is left alone.
const RETRY_COOLDOWN: Duration = Duration::from_secs(600);

/// Same wire shape as `/bars/:symbol` has always returned.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CachedBar {
    pub time: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: u64,
}

#[derive(Debug)]
struct Entry {
    /// Ascending by time, one per minute.
    bars: Vec<CachedBar>,
    refreshed: Instant,
    /// Known to be missing bars -- the live stream feeding it had a gap,
    /// or the warmer fell behind the broadcast. Only a provider fetch
    /// clears it; no arrangement of later live bars can, because the
    /// missing minutes are already past.
    holed: bool,
}

#[derive(Debug, Default)]
struct Cache {
    entries: HashMap<String, Entry>,
    /// Symbols with a warm-up fetch in flight, so a burst of live bars
    /// for a new symbol starts one fetch, not one per bar.
    warming: HashSet<String>,
    /// Symbols whose warm-up failed, and when they may be tried again.
    cooling: HashMap<String, Instant>,
    /// Start of the current warm-up rate window, and how many have been
    /// started in it.
    window: Option<Instant>,
    started_in_window: u32,
}

impl Cache {
    fn lookup(&self, symbol: &str, minutes: i64, now: Instant, now_unix: i64) -> Option<Vec<CachedBar>> {
        if minutes > SEED_MINUTES {
            return None;
        }
        let entry = self.entries.get(symbol)?;
        if entry.holed || now.duration_since(entry.refreshed) > STALE_AFTER {
            return None;
        }
        let from = now_unix - minutes * 60;
        Some(entry.bars.iter().copied().filter(|b| b.time >= from).collect())
    }

    /// Replaces a symbol's copy with a provider fetch.
    ///
    /// Live bars that arrived while that fetch was in flight are kept. A
    /// fetch takes 150-180 ms from this process and a minute bar can land
    /// inside that window; the provider's snapshot was taken before it, so
    /// taking the snapshot wholesale would roll the copy backwards and
    /// lose a bar that no later event re-sends. An empty answer (a symbol
    /// the provider has nothing for) therefore keeps the live bars rather
    /// than wiping them.
    fn seed(&mut self, symbol: &str, mut bars: Vec<CachedBar>, now: Instant) {
        self.warming.remove(symbol);
        self.cooling.remove(symbol);
        if let Some(existing) = self.entries.get(symbol) {
            let newest_fetched = bars.last().map(|b| b.time);
            let newer: Vec<CachedBar> = existing.bars.iter().copied().filter(|b| newest_fetched.is_none_or(|t| b.time > t)).collect();
            bars.extend(newer);
            // Both halves are already ascending and disjoint; this is a
            // guard, not the ordering.
            bars.sort_by_key(|b| b.time);
            bars.dedup_by_key(|b| b.time);
        }
        self.entries.insert(symbol.to_string(), Entry { bars, refreshed: now, holed: false });
        if self.entries.len() > MAX_SYMBOLS {
            if let Some(oldest) = self.entries.iter().min_by_key(|(_, e)| e.refreshed).map(|(s, _)| s.clone()) {
                self.entries.remove(&oldest);
            }
        }
    }

    /// Applies a finished live bar. Returns false when this symbol is not
    /// being served from memory -- it is not cached, or its copy is known
    /// to have a hole -- which is the warmer's cue to consider fetching
    /// its history from the provider.
    fn on_final_bar(&mut self, symbol: &str, bar: CachedBar, now: Instant, now_unix: i64) -> bool {
        let Some(entry) = self.entries.get_mut(symbol) else {
            return false;
        };
        // A tracked symbol produces a bar a minute, so a copy nothing has
        // refreshed for longer than `STALE_AFTER` is missing at least one.
        // This bar is not the missing one, and applying it would make the
        // copy look current again -- for ever, since the next bar is
        // always a minute away. The hole is recorded instead, and only a
        // provider fetch clears it.
        if now.duration_since(entry.refreshed) > STALE_AFTER {
            entry.holed = true;
        }
        match entry.bars.binary_search_by_key(&bar.time, |b| b.time) {
            // The provider revised a bar it already sent: the later one wins.
            Ok(index) => entry.bars[index] = bar,
            Err(index) => entry.bars.insert(index, bar),
        }
        let keep_from = now_unix - SEED_MINUTES * 60;
        entry.bars.retain(|b| b.time >= keep_from);
        entry.refreshed = now;
        !entry.holed
    }

    /// The warmer fell behind the live broadcast, so bars were dropped
    /// before it saw them. Which symbols they were for is not knowable
    /// from a `Lagged` count, so every copy goes: a copy that is missing
    /// minutes and says it is current is worse than no cache at all.
    /// Returns how many were dropped, for the log.
    fn on_lagged(&mut self) -> usize {
        let dropped = self.entries.len();
        self.entries.clear();
        dropped
    }

    /// Whether a warm-up for `symbol` may start now: not already in
    /// flight, not cooling off after a failure, and within the per-minute
    /// allowance. Marks it as warming when it says yes.
    fn may_warm(&mut self, symbol: &str, now: Instant) -> bool {
        self.cooling.retain(|_, until| *until > now);
        if self.warming.contains(symbol) || self.cooling.contains_key(symbol) {
            return false;
        }
        if self.window.is_none_or(|start| now.duration_since(start) >= Duration::from_secs(60)) {
            self.window = Some(now);
            self.started_in_window = 0;
        }
        if self.started_in_window >= WARM_PER_MINUTE {
            return false;
        }
        self.started_in_window += 1;
        self.warming.insert(symbol.to_string());
        true
    }

    /// A warm-up that failed: released, and left alone for a while.
    fn warm_failed(&mut self, symbol: &str, now: Instant) {
        self.warming.remove(symbol);
        self.cool_off(symbol, now);
    }

    /// A provider fetch for this symbol failed, so no warm-up should be
    /// started for it for a while. Does NOT touch the warming mark: a
    /// client's own failed request must not clear a mark belonging to a
    /// warm-up that is still in flight, which would let a second fetch for
    /// the same symbol start beside it.
    fn cool_off(&mut self, symbol: &str, now: Instant) {
        if self.cooling.len() < MAX_SYMBOLS {
            self.cooling.insert(symbol.to_string(), now + RETRY_COOLDOWN);
        }
    }

    /// A warm-up `may_warm` allowed but that could not run, because every
    /// concurrent slot was busy. No request was made, so the mark and the
    /// minute's allowance both go back: this is not a failure to cool off
    /// from, it is a fetch that never happened.
    fn warm_refused(&mut self, symbol: &str) {
        self.warming.remove(symbol);
        self.started_in_window = self.started_in_window.saturating_sub(1);
    }
}

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Cache::default()))
}

fn locked() -> std::sync::MutexGuard<'static, Cache> {
    cache().lock().unwrap_or_else(|e| e.into_inner())
}

pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var(ENABLE_ENV).is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")))
}

/// Bars for the last `minutes`, from memory if a current copy exists.
/// Always `None` with the cache off, so a caller needs no flag of its own.
pub fn lookup(symbol: &str, minutes: i64) -> Option<Vec<CachedBar>> {
    if !enabled() {
        return None;
    }
    locked().lookup(symbol, minutes, Instant::now(), Utc::now().timestamp())
}

/// Who a fetch is for, which decides what a failure means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// A client asked for these bars. A failure is the client's to see;
    /// it still starts the cooldown, because the provider has just said no
    /// for this symbol, but it is not a warm-up to release.
    ClientRequest,
    /// Background warm-up, which nobody asked for.
    WarmUp,
}

/// Fetches bars from the provider and returns the last `minutes` of them.
/// With the cache on it fetches a whole market day and keeps the rest;
/// with it off it fetches exactly what was asked for and keeps nothing,
/// which is the behaviour `/bars/:symbol` has always had.
pub async fn fetch_and_seed(cfg: &AlpacaConfig, symbol: &str, minutes: i64, purpose: Purpose) -> anyhow::Result<Vec<CachedBar>> {
    let end = Utc::now();
    let span = if enabled() { minutes.max(SEED_MINUTES) } else { minutes };
    let start = end - chrono::Duration::minutes(span);
    let fetched = fetch_recent_minute_bars(cfg, symbol, &start.to_rfc3339(), &end.to_rfc3339()).await;
    let bars: Vec<CachedBar> = match fetched {
        Ok(bars) => bars
            .into_iter()
            .map(|b| CachedBar { time: b.timestamp.timestamp(), open: b.open, high: b.high, low: b.low, close: b.close, volume: b.volume })
            .collect(),
        Err(error) => {
            // A failed warm-up must not leave the symbol marked as
            // warming for ever, and no fetch failure may be retried on the
            // next live bar.
            if enabled() {
                let mut cache = locked();
                match purpose {
                    Purpose::WarmUp => cache.warm_failed(symbol, Instant::now()),
                    Purpose::ClientRequest => cache.cool_off(symbol, Instant::now()),
                }
            }
            return Err(error);
        }
    };
    let from = end.timestamp() - minutes * 60;
    let wanted = bars.iter().copied().filter(|b| b.time >= from).collect();
    if enabled() {
        locked().seed(symbol, bars, Instant::now());
    }
    Ok(wanted)
}

/// Keeps cached symbols current from the live feed and warms new ones.
/// A no-op when the cache is off, which is the default -- the subscription
/// itself is not even taken, so there is no task and no broadcast
/// receiver to fall behind.
pub fn spawn_warmer(events: broadcast::Receiver<ScanEvent>, cfg: Arc<AlpacaConfig>) {
    if !enabled() {
        drop(events);
        return;
    }
    let mut events = events;
    let slots = Arc::new(Semaphore::new(WARM_CONCURRENCY));
    tokio::spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                // Falling behind loses live bars, and a later bar for the
                // same symbol would otherwise mark the copy fresh again
                // with a minute missing from it. The lost bars cannot be
                // attributed to symbols, so every copy is dropped and
                // refetched on its next request.
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    let dropped = locked().on_lagged();
                    warn!(missed, dropped, "bar cache: fell behind the live feed; dropped every cached copy rather than serve one with a hole in it");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => return,
            };
            let ScanEvent::BarUpdate { is_final: true, interval_secs: 60, symbol, timestamp, open, high, low, close, volume } = event else {
                continue;
            };
            let bar = CachedBar { time: timestamp.timestamp(), open, high, low, close, volume };
            let now = Instant::now();
            let start_warming = {
                let mut cache = locked();
                !cache.on_final_bar(&symbol, bar, now, Utc::now().timestamp()) && cache.may_warm(&symbol, now)
            };
            if !start_warming {
                continue;
            }
            // Taken BEFORE spawning, not awaited inside the task: a task
            // per wanted warm-up, each waiting its turn, is an unbounded
            // queue of provider requests built out of a bounded rate.
            let Ok(permit) = slots.clone().try_acquire_owned() else {
                locked().warm_refused(&symbol);
                debug!(%symbol, "bar cache: warm-up slots all busy; dropped this one rather than queue it");
                continue;
            };
            let cfg = cfg.clone();
            tokio::spawn(async move {
                let _permit = permit;
                match fetch_and_seed(&cfg, &symbol, SEED_MINUTES, Purpose::WarmUp).await {
                    Ok(bars) => debug!(%symbol, bars = bars.len(), "bar cache: warmed"),
                    // fetch_and_seed has already started this symbol's
                    // cooldown.
                    Err(error) => warn!(%symbol, %error, cooldown = ?RETRY_COOLDOWN, "bar cache: warm-up fetch failed; leaving this symbol alone for a while"),
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(time: i64, close: f64) -> CachedBar {
        CachedBar { time, open: close, high: close, low: close, close, volume: 100 }
    }

    const NOW: i64 = 1_000_000;

    fn seeded() -> (Cache, Instant) {
        let mut cache = Cache::default();
        let t0 = Instant::now();
        cache.seed("AAA", vec![bar(NOW - 600, 1.0), bar(NOW - 300, 2.0), bar(NOW - 60, 3.0)], t0);
        (cache, t0)
    }

    #[test]
    fn the_cache_is_off_until_its_own_switch_is_set() {
        // BAR_CACHE_ENABLED is unset under test, so /bars must behave
        // exactly as it did before this module existed.
        assert!(!enabled());
        assert!(lookup("AAA", 240).is_none());
    }

    #[test]
    fn a_current_copy_answers_and_is_trimmed_to_the_requested_minutes() {
        let (cache, t0) = seeded();
        assert_eq!(cache.lookup("AAA", 240, t0, NOW).unwrap().len(), 3);
        let last_five = cache.lookup("AAA", 5, t0, NOW).unwrap();
        assert_eq!(last_five.iter().map(|b| b.time).collect::<Vec<_>>(), vec![NOW - 300, NOW - 60]);
    }

    #[test]
    fn unknown_symbols_and_oversized_requests_miss() {
        let (cache, t0) = seeded();
        assert!(cache.lookup("ZZZ", 240, t0, NOW).is_none());
        assert!(cache.lookup("AAA", SEED_MINUTES + 1, t0, NOW).is_none());
    }

    #[test]
    fn a_live_bar_renews_a_copy_that_was_kept_current() {
        let (mut cache, t0) = seeded();
        let later = t0 + STALE_AFTER - Duration::from_secs(1);
        assert!(cache.on_final_bar("AAA", bar(NOW + 60, 4.0), later, NOW + 91));
        let bars = cache.lookup("AAA", 240, later, NOW + 91).unwrap();
        assert_eq!(bars.last().unwrap().close, 4.0);
    }

    #[test]
    fn a_copy_that_went_stale_is_not_renewed_by_a_live_bar_alone() {
        let (mut cache, t0) = seeded();
        let later = t0 + STALE_AFTER + Duration::from_secs(1);
        assert!(cache.lookup("AAA", 240, later, NOW + 91).is_none());
        // Live bars arrive once a minute, so nothing refreshing this copy
        // for longer than that means a minute is missing from it -- and it
        // is not the minute this bar carries. Renewing on it marked an
        // incomplete history as current, and the next bar did it again, so
        // the copy was served as fresh for the rest of the session.
        assert!(!cache.on_final_bar("AAA", bar(NOW + 60, 4.0), later, NOW + 91));
        assert!(cache.entries["AAA"].holed);
        assert!(cache.lookup("AAA", 240, later, NOW + 91).is_none());
        // The bar is still applied -- the copy is not wrong, it is
        // incomplete -- and a later bar does not quietly clear the hole.
        assert!(!cache.on_final_bar("AAA", bar(NOW + 120, 5.0), later + Duration::from_secs(60), NOW + 151));
        assert!(cache.lookup("AAA", 240, later + Duration::from_secs(60), NOW + 151).is_none());
        // Only the provider's own answer clears it.
        cache.seed("AAA", vec![bar(NOW + 60, 4.0), bar(NOW + 120, 5.0)], later + Duration::from_secs(61));
        assert!(!cache.entries["AAA"].holed);
        assert!(cache.lookup("AAA", 240, later + Duration::from_secs(61), NOW + 151).is_some());
    }

    #[test]
    fn falling_behind_the_live_feed_drops_every_copy() {
        let (mut cache, t0) = seeded();
        cache.seed("BBB", vec![bar(NOW - 60, 1.0)], t0);
        // A `Lagged` count says how many messages were missed, never which
        // symbols they were for, so no copy can be trusted.
        assert_eq!(cache.on_lagged(), 2);
        assert!(cache.lookup("AAA", 240, t0, NOW).is_none());
        assert!(cache.lookup("BBB", 240, t0, NOW).is_none());
        // And the next live bar for one of them reports a miss, so the
        // warmer refetches rather than rebuilding a day from live bars.
        assert!(!cache.on_final_bar("AAA", bar(NOW + 60, 4.0), t0, NOW + 60));
    }

    #[test]
    fn a_fetch_in_flight_does_not_roll_the_copy_back_over_newer_live_bars() {
        let mut cache = Cache::default();
        let t0 = Instant::now();
        // A fetch for AAA starts here; the provider's snapshot holds
        // everything up to NOW - 60.
        let snapshot = vec![bar(NOW - 600, 1.0), bar(NOW - 300, 2.0), bar(NOW - 60, 3.0)];
        cache.seed("AAA", snapshot.clone(), t0);
        // While it was in flight, the live feed delivered the next minute.
        assert!(cache.on_final_bar("AAA", bar(NOW, 4.0), t0, NOW));
        // The fetch completes and seeds its (older) snapshot. The live bar
        // must survive it: the provider will not send it again, and the
        // chart would lose a candle it had already drawn.
        cache.seed("AAA", snapshot, t0 + Duration::from_millis(180));
        let bars = cache.lookup("AAA", 240, t0 + Duration::from_millis(180), NOW).unwrap();
        assert_eq!(bars.iter().map(|b| (b.time, b.close)).collect::<Vec<_>>(), vec![(NOW - 600, 1.0), (NOW - 300, 2.0), (NOW - 60, 3.0), (NOW, 4.0)]);
        // An empty answer -- a symbol the provider has nothing for -- does
        // not wipe the live bars either.
        cache.seed("AAA", Vec::new(), t0 + Duration::from_millis(360));
        assert_eq!(cache.lookup("AAA", 240, t0 + Duration::from_millis(360), NOW).unwrap().len(), 4);
        // Where both have the same minute, the provider's is authoritative.
        cache.seed("AAA", vec![bar(NOW, 9.0)], t0 + Duration::from_millis(400));
        let bars = cache.lookup("AAA", 240, t0 + Duration::from_millis(400), NOW).unwrap();
        assert_eq!(bars.iter().filter(|b| b.time == NOW).map(|b| b.close).collect::<Vec<_>>(), vec![9.0]);
    }

    #[test]
    fn live_bars_append_in_order_and_a_revision_replaces_its_minute() {
        let (mut cache, t0) = seeded();
        assert!(cache.on_final_bar("AAA", bar(NOW, 5.0), t0, NOW));
        // Out of order, and a revision of an existing minute.
        assert!(cache.on_final_bar("AAA", bar(NOW - 180, 2.5), t0, NOW));
        assert!(cache.on_final_bar("AAA", bar(NOW - 60, 3.3), t0, NOW));
        let bars = cache.lookup("AAA", 240, t0, NOW).unwrap();
        assert_eq!(bars.iter().map(|b| (b.time, b.close)).collect::<Vec<_>>(), vec![(NOW - 600, 1.0), (NOW - 300, 2.0), (NOW - 180, 2.5), (NOW - 60, 3.3), (NOW, 5.0)]);
    }

    #[test]
    fn a_live_bar_for_an_uncached_symbol_reports_a_miss_so_it_can_be_warmed() {
        let (mut cache, t0) = seeded();
        assert!(!cache.on_final_bar("NEW", bar(NOW, 9.0), t0, NOW));
        assert!(cache.lookup("NEW", 240, t0, NOW).is_none());
    }

    #[test]
    fn bars_older_than_a_market_day_are_dropped_as_new_ones_arrive() {
        let mut cache = Cache::default();
        let t0 = Instant::now();
        cache.seed("AAA", vec![bar(NOW - SEED_MINUTES * 60 - 600, 1.0), bar(NOW - 60, 2.0)], t0);
        cache.on_final_bar("AAA", bar(NOW, 3.0), t0, NOW);
        assert_eq!(cache.entries["AAA"].bars.len(), 2);
    }

    #[test]
    fn the_least_recently_refreshed_symbol_is_dropped_past_the_limit() {
        let mut cache = Cache::default();
        let t0 = Instant::now();
        for i in 0..=MAX_SYMBOLS {
            cache.seed(&format!("S{i}"), vec![bar(NOW, 1.0)], t0 + Duration::from_millis(i as u64));
        }
        assert_eq!(cache.entries.len(), MAX_SYMBOLS);
        assert!(!cache.entries.contains_key("S0"));
        assert!(cache.entries.contains_key(&format!("S{MAX_SYMBOLS}")));
    }

    #[test]
    fn seeding_clears_the_warming_mark_and_any_cooldown() {
        let mut cache = Cache::default();
        let t0 = Instant::now();
        cache.warm_failed("AAA", t0);
        assert!(cache.cooling.contains_key("AAA"));
        cache.seed("AAA", vec![bar(NOW, 1.0)], t0);
        assert!(cache.warming.is_empty() && cache.cooling.is_empty());
    }

    #[test]
    fn one_warm_up_per_symbol_at_a_time() {
        let mut cache = Cache::default();
        let t0 = Instant::now();
        assert!(cache.may_warm("AAA", t0));
        // A burst of live bars for the same new symbol starts one fetch.
        assert!(!cache.may_warm("AAA", t0));
        assert_eq!(cache.started_in_window, 1);
    }

    #[test]
    fn a_failed_warm_up_is_left_alone_until_its_cooldown_expires() {
        let mut cache = Cache::default();
        let t0 = Instant::now();
        assert!(cache.may_warm("AAA", t0));
        cache.warm_failed("AAA", t0);
        // Every later bar for this symbol would otherwise start another
        // fetch, once a minute, for as long as it stays tracked.
        assert!(!cache.may_warm("AAA", t0 + Duration::from_secs(60)));
        assert!(!cache.may_warm("AAA", t0 + RETRY_COOLDOWN - Duration::from_secs(1)));
        assert!(cache.may_warm("AAA", t0 + RETRY_COOLDOWN + Duration::from_secs(1)));
        // The expired entry is not kept around.
        assert!(!cache.cooling.contains_key("AAA"));
    }

    #[test]
    fn warm_ups_are_rate_limited_and_the_allowance_refills() {
        let mut cache = Cache::default();
        let t0 = Instant::now();
        for i in 0..WARM_PER_MINUTE {
            assert!(cache.may_warm(&format!("S{i}"), t0), "{i}");
        }
        assert!(!cache.may_warm("OVER", t0));
        // Still refused later in the same window.
        assert!(!cache.may_warm("OVER", t0 + Duration::from_secs(59)));
        assert!(cache.may_warm("OVER", t0 + Duration::from_secs(61)));
    }

    #[test]
    fn a_clients_own_failed_fetch_does_not_release_a_warm_up_in_flight() {
        let mut cache = Cache::default();
        let t0 = Instant::now();
        assert!(cache.may_warm("AAA", t0));
        // `/bars/AAA?fresh` fails at the provider while that warm-up is
        // still running. It starts the cooldown -- the provider has just
        // said no for this symbol -- but clearing the warming mark would
        // let a second fetch for the same symbol start beside the first.
        cache.cool_off("AAA", t0);
        assert!(cache.warming.contains("AAA"));
        assert!(cache.cooling.contains_key("AAA"));
        assert!(!cache.may_warm("AAA", t0));
        // The warm-up's own failure is what releases it.
        cache.warm_failed("AAA", t0);
        assert!(cache.warming.is_empty());
    }

    #[test]
    fn a_warm_up_that_could_not_run_gives_back_its_mark_and_its_allowance() {
        let mut cache = Cache::default();
        let t0 = Instant::now();
        assert!(cache.may_warm("AAA", t0));
        // Every concurrent slot was busy, so no request was made. This is
        // not a failure to cool off from: the symbol may be tried again on
        // its next bar, and the minute's allowance was not spent.
        cache.warm_refused("AAA");
        assert_eq!(cache.started_in_window, 0);
        assert!(!cache.cooling.contains_key("AAA"));
        assert!(cache.may_warm("AAA", t0));
    }

    #[test]
    fn the_cooldown_map_is_bounded() {
        let mut cache = Cache::default();
        let t0 = Instant::now();
        for i in 0..=MAX_SYMBOLS {
            cache.warm_failed(&format!("S{i}"), t0);
        }
        assert_eq!(cache.cooling.len(), MAX_SYMBOLS);
    }
}
