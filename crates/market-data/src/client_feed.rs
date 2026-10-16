//! What the live scan shares with ws-server's client connections beyond the
//! `ScanEvent` broadcast: per-connection chart subscriptions, the bars built
//! for them, the upstream feed's up/down state, and which symbols' ignition
//! chatter UI clients are sent.
//!
//! None of it goes through the `ScanEvent` broadcast. That stream feeds the
//! observation, measurement and research captures, and a chart someone
//! happens to open must not change what they record. Process-wide state, in
//! the same way as `status_tap` and `order_flow_store`: one live scan and one
//! server per process.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use chrono::{DateTime, Utc};
use tokio::sync::{broadcast, watch};

use crate::events::{IgnitionEventKind, ScanEvent};

/// Symbols one connection may chart live at once. Mirrors
/// `CHART_SUBSCRIPTION_LIMIT` in `@stockspotter/shared-types`; extras are
/// ignored rather than rejected.
pub const CHART_SUBSCRIPTION_LIMIT: usize = 8;

/// Distinct chart symbols across all connections. Each costs the upstream a
/// bars (and, without the full-market tape, trades) subscription and two
/// forming bars here, so a fan-out of clients cannot grow either unbounded.
pub const MAX_CHART_SYMBOLS: usize = 64;

/// Chart bars are a few per second per symbol; a connection that falls this
/// far behind just skips to the next update, which overwrites by timestamp.
const CHART_BAR_CAPACITY: usize = 4096;

/// Uppercases and validates a requested chart list: plausible tickers only,
/// no duplicates, at most `CHART_SUBSCRIPTION_LIMIT`.
pub fn chart_symbols(requested: Vec<String>) -> Vec<String> {
    let mut symbols: Vec<String> = Vec::new();
    for symbol in requested {
        let symbol = symbol.trim().to_ascii_uppercase();
        let ok = !symbol.is_empty()
            && symbol.len() <= 12
            && symbol.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '.');
        if ok && !symbols.contains(&symbol) {
            symbols.push(symbol);
        }
        if symbols.len() == CHART_SUBSCRIPTION_LIMIT {
            break;
        }
    }
    symbols
}

/// The upstream market-data stream's state as clients are told it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedState {
    pub up: bool,
    pub since: DateTime<Utc>,
    pub reason: Option<String>,
}

impl FeedState {
    /// The next state, or `None` when `up` is the current state already --
    /// a repeated "down" keeps the first reason and its `since`, which is
    /// when data actually stopped.
    pub fn transition(&self, up: bool, now: DateTime<Utc>, reason: Option<String>) -> Option<FeedState> {
        (self.up != up).then(|| FeedState { up, since: now, reason })
    }
}

pub struct Hub {
    bars: broadcast::Sender<ScanEvent>,
    charted: Mutex<HashMap<String, usize>>,
    version: AtomicU64,
    feed: watch::Sender<FeedState>,
    tiered: RwLock<Arc<HashSet<String>>>,
}

impl Hub {
    pub fn new(now: DateTime<Utc>) -> Self {
        let (bars, _) = broadcast::channel(CHART_BAR_CAPACITY);
        let (feed, _) = watch::channel(FeedState { up: false, since: now, reason: Some("starting".into()) });
        Self { bars, charted: Mutex::new(HashMap::new()), version: AtomicU64::new(0), feed, tiered: RwLock::new(Arc::new(HashSet::new())) }
    }

    fn charted(&self) -> std::sync::MutexGuard<'_, HashMap<String, usize>> {
        self.charted.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Releases `old` and takes `wanted` for one connection, returning what
    /// it actually holds now: a symbol nobody charts yet is refused once
    /// `MAX_CHART_SYMBOLS` are charted.
    pub fn replace(&self, old: &[String], wanted: Vec<String>) -> Vec<String> {
        let mut charted = self.charted();
        let before: HashSet<String> = charted.keys().cloned().collect();
        for symbol in old {
            if let Some(n) = charted.get_mut(symbol) {
                *n -= 1;
                if *n == 0 {
                    charted.remove(symbol);
                }
            }
        }
        let mut held = Vec::new();
        for symbol in wanted {
            if let Some(n) = charted.get_mut(&symbol) {
                *n += 1;
            } else if charted.len() < MAX_CHART_SYMBOLS {
                charted.insert(symbol.clone(), 1);
            } else {
                continue;
            }
            held.push(symbol);
        }
        if charted.len() != before.len() || charted.keys().any(|s| !before.contains(s)) {
            self.version.fetch_add(1, Ordering::Relaxed);
        }
        held
    }

    /// Bumped whenever the set of charted symbols changes, so the live scan
    /// re-reads it only then.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    pub fn charted_symbols(&self) -> HashSet<String> {
        self.charted().keys().cloned().collect()
    }

    pub fn publish_bar(&self, event: ScanEvent) {
        let _ = self.bars.send(event);
    }

    pub fn subscribe_bars(&self) -> broadcast::Receiver<ScanEvent> {
        self.bars.subscribe()
    }

    /// Records a change of feed state. Returns whether it was one.
    pub fn set_feed(&self, up: bool, now: DateTime<Utc>, reason: Option<String>) -> bool {
        self.feed.send_if_modified(|state| match state.transition(up, now, reason) {
            Some(next) => {
                *state = next;
                true
            }
            None => false,
        })
    }

    pub fn feed(&self) -> watch::Receiver<FeedState> {
        self.feed.subscribe()
    }

    /// The symbols with a full (funnel, movers or quiet-watch) ignition
    /// monitor, replaced wholesale by the live scan.
    pub fn set_tiered(&self, symbols: HashSet<String>) {
        *self.tiered.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(symbols);
    }

    /// Whether a UI client (web, desktop, mobile) is sent this event.
    ///
    /// Everything is, except unconfirmed ignition chatter -- `candidate_opened`
    /// and `follow_through_rejected` -- for symbols only the full-market tier
    /// watches. That chatter was ~230 events a second in the regular session
    /// (2026-10-15), about 38 for every confirmation, and no UI shows it by
    /// default: the web Ignition panel and the mobile alert list both filter to
    /// confirmations (`qualifiesForIgnitionAttention`). What still reads the
    /// unconfirmed kinds is kept: mobile's focus rows label a funnel symbol
    /// "ignition candidate", and the web panel's diagnostic "All" view lists
    /// them, so they still arrive for tiered symbols -- only the market-wide
    /// remainder stops. The auto-trader and every in-process capture read the
    /// unfiltered stream; this decides one client socket's send and nothing
    /// else.
    pub fn ui_delivers(&self, event: &ScanEvent) -> bool {
        match event {
            ScanEvent::IgnitionEvent { symbol, kind: IgnitionEventKind::CandidateOpened | IgnitionEventKind::FollowThroughRejected, .. } => {
                self.tiered.read().unwrap_or_else(|p| p.into_inner()).contains(symbol)
            }
            _ => true,
        }
    }
}

/// The process's hub.
pub fn hub() -> &'static Hub {
    static HUB: OnceLock<Hub> = OnceLock::new();
    HUB.get_or_init(|| Hub::new(Utc::now()))
}

/// Records the upstream feed going up or down on the process's hub, and logs
/// each real change under the `feed` target -- the line to look for when a
/// session's charts froze.
pub fn mark_feed(up: bool, reason: Option<String>) {
    if hub().set_feed(up, Utc::now(), reason.clone()) {
        tracing::info!(target: "feed", up, reason = reason.as_deref().unwrap_or(""), "market-data feed state changed");
    }
}

/// One connection's chart subscription. Released when dropped, so a closed
/// socket cannot leave its symbols charted.
pub struct ChartSubscription {
    hub: &'static Hub,
    symbols: Vec<String>,
}

impl ChartSubscription {
    pub fn new(hub: &'static Hub) -> Self {
        Self { hub, symbols: Vec::new() }
    }

    /// Replaces this connection's list (validated with `chart_symbols`); an
    /// empty list clears it. Returns what is held.
    pub fn replace(&mut self, requested: Vec<String>) -> &[String] {
        let wanted = chart_symbols(requested);
        self.symbols = self.hub.replace(&self.symbols, wanted);
        &self.symbols
    }

    pub fn contains(&self, symbol: &str) -> bool {
        self.symbols.iter().any(|s| s == symbol)
    }

    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }
}

impl Drop for ChartSubscription {
    fn drop(&mut self) {
        if !self.symbols.is_empty() {
            self.hub.replace(&self.symbols, Vec::new());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leak() -> &'static Hub {
        Box::leak(Box::new(Hub::new(Utc::now())))
    }

    fn names(symbols: &[&str]) -> Vec<String> {
        symbols.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn requested_symbols_are_uppercased_validated_and_capped() {
        let requested = names(&["aapl", "AAPL", " brk.b ", "", "TOOLONGTICKER1", "A B", "c", "d", "e", "f", "g", "h", "i"]);
        assert_eq!(chart_symbols(requested), names(&["AAPL", "BRK.B", "C", "D", "E", "F", "G", "H"]));
        assert!(chart_symbols(Vec::new()).is_empty());
    }

    #[test]
    fn subscriptions_are_counted_across_connections_and_released_on_drop() {
        let hub = leak();
        let mut a = ChartSubscription::new(hub);
        let mut b = ChartSubscription::new(hub);
        a.replace(names(&["AAA", "BBB"]));
        let v = hub.version();
        b.replace(names(&["bbb"]));
        assert_eq!(hub.version(), v, "BBB was already charted");
        assert_eq!(hub.charted_symbols(), ["AAA", "BBB"].map(String::from).into());
        a.replace(Vec::new());
        assert!(a.is_empty());
        assert_eq!(hub.charted_symbols(), ["BBB"].map(String::from).into(), "b still holds BBB");
        assert!(hub.version() > v);
        drop(b);
        assert!(hub.charted_symbols().is_empty());
    }

    #[test]
    fn a_replacement_swaps_the_whole_set() {
        let hub = leak();
        let mut a = ChartSubscription::new(hub);
        a.replace(names(&["AAA", "BBB"]));
        a.replace(names(&["CCC"]));
        assert!(a.contains("CCC") && !a.contains("AAA"));
        assert_eq!(hub.charted_symbols(), ["CCC"].map(String::from).into());
    }

    #[test]
    fn the_process_wide_cap_refuses_new_symbols_but_shares_existing_ones() {
        let hub = leak();
        let mut holders: Vec<ChartSubscription> = Vec::new();
        for i in 0..MAX_CHART_SYMBOLS / CHART_SUBSCRIPTION_LIMIT {
            let mut s = ChartSubscription::new(hub);
            let wanted = (0..CHART_SUBSCRIPTION_LIMIT).map(|j| format!("S{i}X{j}")).collect();
            assert_eq!(s.replace(wanted).len(), CHART_SUBSCRIPTION_LIMIT);
            holders.push(s);
        }
        let mut late = ChartSubscription::new(hub);
        assert_eq!(late.replace(names(&["NEW", "S0X0"])), names(&["S0X0"]));
        assert_eq!(hub.charted_symbols().len(), MAX_CHART_SYMBOLS);
    }

    #[test]
    fn feed_state_changes_only_on_a_real_transition() {
        let hub = leak();
        let mut rx = hub.feed();
        let t0 = Utc::now();
        assert!(!rx.borrow_and_update().up);
        assert!(hub.set_feed(true, t0, None));
        assert!(rx.has_changed().unwrap());
        assert!(rx.borrow_and_update().up);
        assert!(!hub.set_feed(true, t0 + chrono::Duration::seconds(5), None), "already up");
        assert!(!rx.has_changed().unwrap());
        let t1 = t0 + chrono::Duration::seconds(9);
        assert!(hub.set_feed(false, t1, Some("no market data for 60s".into())));
        assert!(!hub.set_feed(false, t1 + chrono::Duration::seconds(5), Some("upstream closed".into())));
        let state = rx.borrow_and_update().clone();
        assert_eq!(state, FeedState { up: false, since: t1, reason: Some("no market data for 60s".into()) }, "the first reason and time stand");
    }

    #[test]
    fn ui_clients_get_unconfirmed_ignition_only_for_tiered_symbols() {
        let hub = leak();
        hub.set_tiered(["FUNNEL".to_string()].into());
        let ignition = |symbol: &str, kind| ScanEvent::IgnitionEvent { symbol: symbol.into(), timestamp: Utc::now(), price: 1.0, kind };
        assert!(hub.ui_delivers(&ignition("FUNNEL", IgnitionEventKind::CandidateOpened)));
        assert!(hub.ui_delivers(&ignition("FUNNEL", IgnitionEventKind::FollowThroughRejected)));
        assert!(!hub.ui_delivers(&ignition("WIDE", IgnitionEventKind::CandidateOpened)));
        assert!(!hub.ui_delivers(&ignition("WIDE", IgnitionEventKind::FollowThroughRejected)));
        assert!(hub.ui_delivers(&ignition("WIDE", IgnitionEventKind::FollowThroughConfirmed)), "confirmations always go out");
        let bar = ScanEvent::BarUpdate { symbol: "WIDE".into(), timestamp: Utc::now(), open: 1.0, high: 1.0, low: 1.0, close: 1.0, volume: 1, interval_secs: 60, is_final: true };
        assert!(hub.ui_delivers(&bar));
    }
}
