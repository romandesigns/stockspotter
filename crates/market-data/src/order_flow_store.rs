//! Feeds live trades and quotes into `order_flow::OrderFlowEngine`, one per
//! tracked symbol, and hands out summaries on request.
//!
//! **Observational only, and off unless asked for.** Nothing here emits a
//! `ScanEvent`, gates an alert or touches ranking, and with
//! `ORDER_FLOW_ENABLED` unset every function in this module returns
//! immediately. That default is deliberate: this runs inside the live scan
//! loop production depends on, and the order-flow specification requires
//! the features to be recorded and validated before anything relies on them.
//!
//! Why a process-wide store instead of a new `ScanEvent` variant or a
//! parameter threaded through `run_live_scan`:
//!
//! - A `ScanEvent` would be broadcast to every client for every tracked
//!   symbol, when only the one symbol a user has open is ever looked at.
//!   A summary is computed here only when something asks for it.
//! - `ws-server`'s observation layer matches `ScanEvent` exhaustively on
//!   purpose, and its implementation is frozen under a preregistration. A
//!   new variant would force a change there; this does not touch it.
//! - `run_live_scan`'s signature is shared with other in-flight work, and
//!   two hook calls in the loop are a far smaller surface than a new
//!   argument through every caller.
//!
//! The cost of that choice is global state, kept small: one mutex, one map,
//! cleared when the market day rolls.
//!
//! Engines exist only for symbols that have had a quote, which means the
//! ones subscribed individually (funnel-tracked names and tracked movers).
//! Quotes are not subscribed for the rest of the market (see
//! `ws::subscribe_all_trades`), and without quotes the aggressor side
//! would fall back to the tick rule alone.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, NaiveDate, Utc};
use order_flow::{OrderFlowEngine, OrderFlowSummary, Session, TradeFlags};

use crate::bar::{Quote, Trade};
use crate::trading_session::{classify_session, market_day, TradingSession};

pub const ORDER_FLOW_ENV: &str = "ORDER_FLOW_ENABLED";

/// Prints with no aggressor to infer: auctions, official opens/closes,
/// crosses, late and out-of-sequence reports, average-price and
/// contingent trades. They still count as volume; they just get no side.
///
/// Trade conditions are tape-specific and `Trade` does not carry its tape,
/// so this list holds only codes that mean the same thing on tapes A/B and
/// C, checked against the tables snapshotted in
/// `ops/observation/metadata/alpaca-conditions-trade-tape*-20260928.json`.
/// `B` is deliberately absent: it is "Average Price Trade" on A/B but an
/// ordinary "Bunched Trade" on C, and excluding it would drop real
/// aggressive prints on every Nasdaq-listed name. The price of that is
/// A/B average-price prints being classified like regular ones until the
/// parser keeps the tape.
const UNCLASSIFIABLE_CONDITIONS: [&str; 20] =
    ["4", "5", "6", "7", "9", "C", "G", "H", "L", "M", "N", "O", "P", "Q", "R", "U", "V", "W", "X", "Z"];

/// Intermarket sweep order: `F` on every tape.
const SWEEP_CONDITION: &str = "F";

/// One thing that happened to a symbol, in the engine's own types. Kept
/// as a value so the same event can be applied to the live engine now and
/// replayed into a backfilled one later.
#[derive(Debug, Clone, Copy)]
pub enum Event {
    Quote(order_flow::Quote),
    Trade(order_flow::Trade, Session, TradeFlags),
}

impl Event {
    pub fn timestamp_secs(&self) -> f64 {
        match self {
            Event::Quote(q) => q.timestamp_secs,
            Event::Trade(t, _, _) => t.timestamp_secs,
        }
    }

    /// Applies this event to an engine. Public so an offline replay
    /// (replay-engine's `order_flow_validate`) feeds an engine exactly
    /// the way the live store does.
    pub fn apply(self, engine: &mut OrderFlowEngine) {
        match self {
            Event::Quote(q) => engine.on_quote(q),
            Event::Trade(t, session, flags) => {
                engine.on_trade(t, session, flags);
            }
        }
    }
}

/// Where a symbol's session backfill stands. See `begin_backfill`.
#[derive(Debug, Default)]
enum Backfill {
    #[default]
    NotStarted,
    /// History is being fetched. Live events keep going to the live
    /// engine AND are kept here, so they can be replayed on top of the
    /// history when it arrives.
    Running(Vec<Event>),
    Done,
    Failed,
}

/// Live events buffered during one backfill before it is abandoned. A
/// fetch that takes this long on a symbol this busy is not worth the
/// memory; the live engine simply carries on without history.
const MAX_BUFFERED_EVENTS: usize = 200_000;

#[derive(Debug, Default)]
struct Entry {
    last_seen: f64,
    engine: OrderFlowEngine,
    backfill: Backfill,
    /// Exchange codes for trades that have been read off the socket but
    /// not yet delivered to `on_trade`, oldest first (`note_venues`).
    venues: VecDeque<(i64, char)>,
}

/// Far more than one frame ever carries for one symbol; a bound only so a
/// symbol whose trades stop being delivered cannot grow this forever.
const MAX_PENDING_VENUES: usize = 4096;

impl Entry {
    fn apply(&mut self, event: Event) {
        self.last_seen = self.last_seen.max(event.timestamp_secs());
        if let Backfill::Running(buffer) = &mut self.backfill {
            if buffer.len() < MAX_BUFFERED_EVENTS {
                buffer.push(event);
            } else {
                self.backfill = Backfill::Failed;
            }
        }
        event.apply(&mut self.engine);
    }
}

/// Bounds are a conservative research resource policy, not efficacy thresholds.
const MAX_ENGINES: usize = 256;
const IDLE_SECS: f64 = 600.0;

#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationHealth {
    pub engines: usize,
    pub evicted: u64,
    pub capacity_rejected: u64,
    pub late_discarded: u64,
    pub corrections_seen: u64,
    pub cancels_seen: u64,
}

/// No correction is applied by this observational layer. Counters disclose
/// receipt, not successful reconciliation or a corrected volume/profile.
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TapeProvenance { Uncorrected, CorrectionsPending }

#[derive(Default)]
struct Store {
    day: Option<NaiveDate>,
    engines: HashMap<String, Entry>,
    health: ObservationHealth,
    last_sweep: f64,
}

impl Store {
    /// Everything in an engine is per trading day (cumulative delta, the
    /// volume profile, the "normal" baseline), so a new market day starts
    /// from nothing rather than carrying yesterday's state forward.
    fn roll_to(&mut self, day: NaiveDate) -> bool {
        if self.day.is_some_and(|current| day < current) {
            self.health.late_discarded += 1;
            return false;
        }
        if self.day != Some(day) {
            self.day = Some(day);
            self.engines.clear();
            self.last_sweep = 0.0;
        }
        true
    }

    fn sweep(&mut self, now: f64) {
        // At most one bounded map scan per second, not per tape event.
        if now < self.last_sweep + 1.0 { return; }
        self.last_sweep = now;
        let before = self.engines.len();
        self.engines.retain(|_, entry| now - entry.last_seen <= IDLE_SECS);
        self.health.evicted += (before - self.engines.len()) as u64;
    }
}

fn store() -> &'static Mutex<Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(Store::default()))
}

/// Read once: flipping this needs a restart, same as every other
/// behaviour switch in the live loop.
pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var(ORDER_FLOW_ENV).is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")))
}

/// Fractional epoch seconds. Unlike `live::to_secs`' whole-second use
/// elsewhere, the sub-second part matters here: classification depends on
/// which quote was prevailing at the instant of the trade.
fn to_secs(t: DateTime<Utc>) -> f64 {
    t.timestamp() as f64 + f64::from(t.timestamp_subsec_nanos()) / 1e9
}

fn session_of(t: DateTime<Utc>) -> Session {
    match classify_session(t) {
        TradingSession::Premarket => Session::Premarket,
        TradingSession::Regular => Session::Regular,
        // 20:00-04:00 ET belongs to the day it follows (`market_day`), so
        // any overnight print lands with after-hours rather than opening a
        // fourth bucket nothing would read.
        TradingSession::AfterHours | TradingSession::Overnight => Session::AfterHours,
    }
}

fn flags_of(conditions: &[String]) -> TradeFlags {
    TradeFlags {
        unclassifiable: conditions.iter().any(|c| UNCLASSIFIABLE_CONDITIONS.contains(&c.as_str())),
        sweep: conditions.iter().any(|c| c == SWEEP_CONDITION),
        exchange: None,
    }
}

/// The three fields of a raw trade row this module wants and `Trade` does
/// not keep. Borrowed, so reading them allocates nothing.
#[derive(serde::Deserialize)]
struct VenueRow<'a> {
    #[serde(rename = "T")]
    kind: &'a str,
    #[serde(rename = "S", default)]
    symbol: Option<&'a str>,
    #[serde(default)]
    x: Option<&'a str>,
    #[serde(default)]
    t: Option<&'a str>,
}

/// Call with every raw frame from the market-data socket, BEFORE the
/// messages parsed from it are processed. Remembers which exchange each
/// trade printed on, so sweep detection can count venues.
///
/// Why a second look at the raw text instead of a field on `Trade`:
/// `Trade` is the shared parser type. It is serialized into recorded
/// receipts that must stay byte-identical, and it is built by hand in
/// dozens of places across crates other work is changing right now. The
/// exchange is wanted by this observational layer only, so it is read
/// here, for symbols that have an engine, and nowhere else changes.
///
/// The cost is one extra pass over frames that contain trades, paid only
/// with `ORDER_FLOW_ENABLED` set and at least one engine alive.
pub fn note_venues(raw: &str) {
    if !enabled() {
        return;
    }
    let mut store = store().lock().unwrap_or_else(|e| e.into_inner());
    note_venues_in(&mut store, raw);
}

fn note_venues_in(store: &mut Store, raw: &str) {
    // A frame this cannot read costs the venue count for its trades and
    // nothing else; the real parse reports real problems.
    let Ok(rows) = serde_json::from_str::<Vec<VenueRow>>(raw) else {
        return;
    };
    for row in &rows {
        if row.kind == "c" { store.health.corrections_seen += 1; }
        if row.kind == "x" { store.health.cancels_seen += 1; }
    }
    for row in rows.iter().filter(|r| r.kind == "t") {
        let (Some(symbol), Some(x), Some(t)) = (row.symbol, row.x, row.t) else {
            continue;
        };
        let Some(entry) = store.engines.get_mut(symbol) else {
            continue;
        };
        let mut code = x.chars();
        let (Some(exchange), None) = (code.next(), code.next()) else {
            continue;
        };
        let Some(nanos) = DateTime::parse_from_rfc3339(t).ok().and_then(|t| t.timestamp_nanos_opt()) else {
            continue;
        };
        if entry.venues.len() >= MAX_PENDING_VENUES {
            entry.venues.pop_front();
        }
        entry.venues.push_back((nanos, exchange));
    }
}

/// The exchange noted for the trade at `nanos`, if any. Matched by exact
/// timestamp rather than by position, so a trade that never reaches
/// `on_trade` (or a frame `note_venues` could not read) cannot shift every
/// later trade onto the wrong exchange: older notes are discarded, and a
/// trade with no note of its own simply gets none.
fn take_venue(venues: &mut VecDeque<(i64, char)>, nanos: i64) -> Option<char> {
    while venues.front().is_some_and(|v| v.0 < nanos) {
        venues.pop_front();
    }
    match venues.front() {
        Some(&(at, exchange)) if at == nanos => {
            venues.pop_front();
            Some(exchange)
        }
        _ => None,
    }
}

/// Call for every quote received.
pub fn on_quote(quote: &Quote) {
    if !enabled() {
        return;
    }
    record_quote(&mut store().lock().unwrap_or_else(|e| e.into_inner()), quote);
}

/// Call for every trade received; ignored for symbols with no engine.
pub fn on_trade(trade: &Trade) {
    if !enabled() {
        return;
    }
    record_trade(&mut store().lock().unwrap_or_else(|e| e.into_inner()), trade);
}

/// Diagnostics only; no HTTP or wire consumer is introduced in this slice.
pub fn observation_health() -> Option<(ObservationHealth, TapeProvenance)> {
    if !enabled() { return None; }
    let store = store().lock().unwrap_or_else(|e| e.into_inner());
    let mut health = store.health;
    health.engines = store.engines.len();
    let provenance = if health.corrections_seen + health.cancels_seen > 0 {
        TapeProvenance::CorrectionsPending
    } else { TapeProvenance::Uncorrected };
    Some((health, provenance))
}

/// Current summary for `symbol`, or `None` if it has not traded while
/// tracked today (or the feature is off).
pub fn summary(symbol: &str, now: DateTime<Utc>) -> Option<OrderFlowSummary> {
    if !enabled() {
        return None;
    }
    let store = store().lock().unwrap_or_else(|e| e.into_inner());
    store.engines.get(symbol).map(|entry| entry.engine.summary(to_secs(now)))
}

/// Summaries for every symbol that traded within the last
/// `max_idle_secs`, for a recorder. Quiet symbols are left out: their
/// summary has not changed in any way worth writing down again.
pub fn active_summaries(now: DateTime<Utc>, max_idle_secs: f64) -> Vec<(String, OrderFlowSummary)> {
    if !enabled() {
        return Vec::new();
    }
    active_in(&store().lock().unwrap_or_else(|e| e.into_inner()), now, max_idle_secs)
}

fn active_in(store: &Store, now: DateTime<Utc>, max_idle_secs: f64) -> Vec<(String, OrderFlowSummary)> {
    let now_secs = to_secs(now);
    let mut rows: Vec<(String, OrderFlowSummary)> = store
        .engines
        .iter()
        .filter(|(_, entry)| entry.engine.last_trade_secs().is_some_and(|t| now_secs - t <= max_idle_secs))
        .map(|(symbol, entry)| (symbol.clone(), entry.engine.summary(now_secs)))
        .collect();
    // Stable output: a HashMap's order would make two runs' files differ
    // for no reason.
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

/// What `GET /orderflow/:symbol` reports about history for a symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackfillStatus {
    /// Live data only, from when the symbol became tracked.
    None,
    Running,
    /// The session's earlier trades (and recent quotes) are included.
    Done,
    Failed,
}

pub fn backfill_status(symbol: &str) -> BackfillStatus {
    if !enabled() {
        return BackfillStatus::None;
    }
    status_in(&store().lock().unwrap_or_else(|e| e.into_inner()), symbol)
}

fn status_in(store: &Store, symbol: &str) -> BackfillStatus {
    match store.engines.get(symbol).map(|e| &e.backfill) {
        Some(Backfill::Running(_)) => BackfillStatus::Running,
        Some(Backfill::Done) => BackfillStatus::Done,
        Some(Backfill::Failed) => BackfillStatus::Failed,
        Some(Backfill::NotStarted) | None => BackfillStatus::None,
    }
}

/// Session backfill, step 1 of 2. Without it, order flow for a symbol
/// starts when the server began tracking it, so a chart opened at 09:50
/// shows a cumulative delta and volume profile that silently cover only
/// part of the session.
///
/// Returns true if the caller should now fetch this symbol's history and
/// hand it to `finish_backfill` (or call `fail_backfill`). False means
/// there is nothing to do: unknown symbol, or a backfill already ran or is
/// running today. At most one backfill per symbol per market day.
///
/// From this point until `finish_backfill`, live events are buffered as
/// well as applied, which is what lets history and live data be stitched
/// together without a gap or a double count.
pub fn begin_backfill(symbol: &str) -> bool {
    if !enabled() {
        return false;
    }
    begin_in(&mut store().lock().unwrap_or_else(|e| e.into_inner()), symbol)
}

fn begin_in(store: &mut Store, symbol: &str) -> bool {
    match store.engines.get_mut(symbol) {
        Some(entry) if matches!(entry.backfill, Backfill::NotStarted) => {
            entry.backfill = Backfill::Running(Vec::new());
            true
        }
        _ => false,
    }
}

pub fn fail_backfill(symbol: &str) {
    let mut store = store().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entry) = store.engines.get_mut(symbol) {
        entry.backfill = Backfill::Failed;
    }
}

/// Turns fetched history into engine events in time order. At an equal
/// timestamp a quote goes before a trade, so the trade is classified
/// against it rather than against the quote before.
pub fn history_events(trades: &[Trade], quotes: &[Quote]) -> Vec<Event> {
    let mut events: Vec<Event> = Vec::with_capacity(trades.len() + quotes.len());
    events.extend(quotes.iter().map(quote_event));
    events.extend(trades.iter().map(trade_event));
    events.sort_by(|a, b| a.timestamp_secs().total_cmp(&b.timestamp_secs()).then_with(|| matches!(b, Event::Quote(_)).cmp(&matches!(a, Event::Quote(_)))));
    events
}

/// Session backfill, step 2 of 2: rebuilds the symbol's engine from
/// `history` followed by every live event buffered since `begin_backfill`,
/// and swaps it in.
///
/// History is cut off at the first buffered live event, so nothing is
/// counted twice and nothing between the two is lost. The replay runs
/// WITHOUT the store lock held -- it can be hundreds of thousands of
/// events, and the live scan loop takes that lock on every trade -- and
/// only the short tail of events that arrived during the replay is applied
/// under it.
pub fn finish_backfill(symbol: &str, history: Vec<Event>) {
    finish_in(store(), symbol, history);
}

fn finish_in(store: &Mutex<Store>, symbol: &str, history: Vec<Event>) {
    let buffered: Vec<Event> = {
        let guard = store.lock().unwrap_or_else(|e| e.into_inner());
        match guard.engines.get(symbol).map(|e| &e.backfill) {
            Some(Backfill::Running(buffer)) => buffer.clone(),
            // Abandoned, or the market day rolled and the entry is gone.
            _ => return,
        }
    };
    let cut = buffered.first().map(Event::timestamp_secs).unwrap_or(f64::INFINITY);
    let mut engine = OrderFlowEngine::default();
    for event in history.into_iter().filter(|e| e.timestamp_secs() < cut) {
        event.apply(&mut engine);
    }
    for event in &buffered {
        event.apply(&mut engine);
    }

    let mut guard = store.lock().unwrap_or_else(|e| e.into_inner());
    let Some(entry) = guard.engines.get_mut(symbol) else {
        return;
    };
    let Backfill::Running(buffer) = &entry.backfill else {
        return;
    };
    for event in &buffer[buffered.len().min(buffer.len())..] {
        event.apply(&mut engine);
    }
    entry.engine = engine;
    entry.backfill = Backfill::Done;
}

fn quote_event(quote: &Quote) -> Event {
    Event::Quote(order_flow::Quote {
        timestamp_secs: to_secs(quote.timestamp),
        bid_price: quote.bid_price,
        bid_size: quote.bid_size,
        ask_price: quote.ask_price,
        ask_size: quote.ask_size,
    })
}

fn trade_event(trade: &Trade) -> Event {
    Event::Trade(
        order_flow::Trade { timestamp_secs: to_secs(trade.timestamp), price: trade.price, size: trade.size },
        session_of(trade.timestamp),
        flags_of(&trade.conditions),
    )
}

fn record_quote(store: &mut Store, quote: &Quote) {
    if !store.roll_to(market_day(quote.timestamp)) { return; }
    if let Some(entry) = store.engines.get_mut(&quote.symbol) {
        entry.last_seen = entry.last_seen.max(to_secs(quote.timestamp));
    }
    store.sweep(to_secs(quote.timestamp));
    if !store.engines.contains_key(&quote.symbol) && store.engines.len() >= MAX_ENGINES {
        store.health.capacity_rejected += 1;
        return;
    }
    // Recreated engines start a partial observation history, never a complete
    // session. No delivery consumer or session-completeness claim is added.
    store.engines.entry(quote.symbol.clone()).or_default().apply(quote_event(quote));
}
/// Trades never create an engine; only a quote does (`record_quote`).
/// Quotes arrive solely for symbols this process subscribed to one by one
/// -- funnel-tracked names and tracked movers alike -- so "has had a
/// quote" is exactly "has the data order flow needs", without this module
/// having to know the live loop's several notions of "tracked". It also
/// keeps the full-market trade tape (universe mode) from spawning an
/// engine per symbol in the market.
fn record_trade(store: &mut Store, trade: &Trade) {
    if !store.roll_to(market_day(trade.timestamp)) { return; }
    if let Some(entry) = store.engines.get_mut(&trade.symbol) {
        entry.last_seen = entry.last_seen.max(to_secs(trade.timestamp));
    }
    store.sweep(to_secs(trade.timestamp));
    let Some(entry) = store.engines.get_mut(&trade.symbol) else {
        return;
    };
    let mut event = trade_event(trade);
    if let (Event::Trade(_, _, flags), Some(nanos)) = (&mut event, trade.timestamp.timestamp_nanos_opt()) {
        flags.exchange = take_venue(&mut entry.venues, nanos);
    }
    entry.apply(event);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use order_flow::TradeSide;

    // 2026-10-06 is EDT: 08:00 ET = 12:00Z, 10:00 ET = 14:00Z.
    fn at(h: u32, m: u32, s: u32, millis: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 6, h, m, s).unwrap() + chrono::Duration::milliseconds(i64::from(millis))
    }

    fn quote(t: DateTime<Utc>, bid: f64, ask: f64) -> Quote {
        Quote { symbol: "AAA".into(), bid_price: bid, bid_size: 5, ask_price: ask, ask_size: 5, timestamp: t }
    }

    fn trade(t: DateTime<Utc>, price: f64, size: u64, conditions: &[&str]) -> Trade {
        Trade { symbol: "AAA".into(), price, size, timestamp: t, conditions: conditions.iter().map(|c| c.to_string()).collect() }
    }

    #[test]
    fn public_ingestion_gate_checks() {
        if let Ok(mode) = std::env::var("ORDER_FLOW_TEST_CHILD_MODE") {
            let now = at(14, 0, 0, 0);
            on_quote(&quote(now, 1.99, 2.0));
            on_trade(&trade(now, 2.0, 10, &["@"]));
            note_venues(r#"[{"T":"c","S":"AAA"},{"T":"x","S":"AAA"}]"#);
            if mode == "off" {
                assert!(observation_health().is_none());
                assert!(summary("AAA", now).is_none());
            } else {
                let (health, provenance) = observation_health().unwrap();
                assert_eq!(health.engines, 1);
                assert_eq!((health.corrections_seen, health.cancels_seen), (1, 1));
                assert!(matches!(provenance, TapeProvenance::CorrectionsPending));
                assert!(summary("AAA", now).is_some());
                for i in 0..MAX_ENGINES {
                    let mut q = quote(now, 1.99, 2.0);
                    q.symbol = format!("S{i}");
                    on_quote(&q);
                }
                let (health, _) = observation_health().unwrap();
                assert_eq!(health.engines, MAX_ENGINES);
                assert_eq!(health.capacity_rejected, 1);
                on_trade(&trade(now - chrono::Duration::days(1), 2.0, 10, &["@"]));
                assert_eq!(observation_health().unwrap().0.late_discarded, 1);
                let later = now + chrono::Duration::seconds(IDLE_SECS as i64 + 1);
                let mut q = quote(later, 1.99, 2.0);
                q.symbol = "NEW".into();
                on_quote(&q);
                let (health, _) = observation_health().unwrap();
                assert_eq!(health.engines, 1);
                assert_eq!(health.evicted, MAX_ENGINES as u64);
            }
            return;
        }
        // Fresh processes test the real OnceLock environment gate without
        // mutating shared process environment or racing other Rust tests.
        for mode in ["off", "on"] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child.args(["--exact", "order_flow_store::tests::public_ingestion_gate_checks"])
                .env("ORDER_FLOW_TEST_CHILD_MODE", mode);
            if mode == "on" { child.env(ORDER_FLOW_ENV, "true"); }
            else { child.env_remove(ORDER_FLOW_ENV); }
            let output = child.output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        }
    }

    #[test]
    fn previous_day_is_discarded_without_resetting_current_engines() {
        let mut store = Store::default();
        let current = at(14, 0, 0, 0);
        record_quote(&mut store, &quote(current, 1.99, 2.0));
        record_trade(&mut store, &trade(current - chrono::Duration::days(1), 2.0, 10, &["@"]));
        assert_eq!(store.engines.len(), 1);
        assert_eq!(store.health.late_discarded, 1);
        assert_eq!(store.day, Some(market_day(current)));
    }

    #[test]
    fn capacity_rejects_new_symbol_without_evicting_active_engines() {
        let mut store = Store::default();
        let now = at(14, 0, 0, 0);
        for i in 0..MAX_ENGINES {
            let mut q = quote(now, 1.99, 2.0);
            q.symbol = format!("S{i}");
            record_quote(&mut store, &q);
        }
        record_quote(&mut store, &quote(now, 1.99, 2.0));
        assert_eq!(store.engines.len(), MAX_ENGINES);
        assert_eq!(store.health.capacity_rejected, 1);
        assert_eq!(store.health.evicted, 0);
        assert!(!store.engines.contains_key("AAA"));
    }

    #[test]
    fn idle_eviction_allows_only_a_new_partial_engine() {
        let mut store = Store::default();
        let now = at(14, 0, 0, 0);
        record_quote(&mut store, &quote(now, 1.99, 2.0));
        record_trade(&mut store, &trade(now, 2.0, 10, &["@"]));
        let later = now + chrono::Duration::seconds(IDLE_SECS as i64 + 1);
        let mut other = quote(later, 1.99, 2.0);
        other.symbol = "BBB".into();
        record_quote(&mut store, &other);
        assert!(!store.engines.contains_key("AAA"));
        record_quote(&mut store, &quote(later, 1.99, 2.0));
        assert_eq!(store.health.evicted, 1);
        let summary = store.engines["AAA"].engine.summary(to_secs(later));
        assert_eq!(summary.cumulative_delta_regular, 0);
    }

    #[test]
    fn correction_and_cancel_frames_are_counted_without_creating_engines() {
        let mut store = Store::default();
        note_venues_in(&mut store, r#"[{"T":"c","S":"AAA"},{"T":"x","S":"AAA"}]"#);
        assert_eq!(store.health.corrections_seen, 1);
        assert_eq!(store.health.cancels_seen, 1);
        assert!(store.engines.is_empty());
    }

    #[test]
    fn the_exchange_is_read_from_the_raw_frame_and_reaches_the_sweep() {
        let mut store = Store::default();
        record_quote(&mut store, &quote(at(14, 0, 0, 0), 1.99, 2.00));
        // Three exchanges, two price levels, one second: a sweep only
        // because of the venues. BBB has no engine and is ignored; the
        // quote row is skipped.
        let frame = r#"[
            {"T":"t","S":"AAA","i":1,"x":"Q","p":2.00,"s":500,"c":["@"],"t":"2026-10-06T14:00:00.1Z","z":"C"},
            {"T":"q","S":"AAA","bx":"Q","bp":1.99,"bs":5,"ax":"P","ap":2.0,"as":5,"t":"2026-10-06T14:00:00.15Z"},
            {"T":"t","S":"BBB","i":2,"x":"N","p":9.00,"s":500,"t":"2026-10-06T14:00:00.2Z"},
            {"T":"t","S":"AAA","i":3,"x":"P","p":2.00,"s":500,"t":"2026-10-06T14:00:00.2Z"},
            {"T":"t","S":"AAA","i":4,"x":"Z","p":2.01,"s":500,"t":"2026-10-06T14:00:00.3Z"}
        ]"#;
        note_venues_in(&mut store, frame);
        for (millis, price) in [(100, 2.00), (200, 2.00)] {
            record_trade(&mut store, &trade(at(14, 0, 0, millis), price, 500, &["@"]));
        }
        record_quote(&mut store, &quote(at(14, 0, 0, 250), 2.00, 2.01));
        record_trade(&mut store, &trade(at(14, 0, 0, 300), 2.01, 500, &["@"]));
        let sweeps = store.engines["AAA"].engine.summary(to_secs(at(14, 0, 1, 0))).sweeps;
        assert_eq!((sweeps.count, sweeps.multi_venue_count), (1, 1));
        assert_eq!(sweeps.last.unwrap().venues, 3);
        assert!(store.engines["AAA"].venues.is_empty());
    }

    #[test]
    fn a_missing_or_unmatched_note_never_shifts_onto_another_trade() {
        let mut venues: VecDeque<(i64, char)> = VecDeque::from([(100, 'Q'), (200, 'P'), (200, 'Z'), (400, 'N')]);
        // The 100 trade was never delivered: its note is dropped, not
        // handed to the next trade.
        assert_eq!(take_venue(&mut venues, 200), Some('P'));
        // Two prints in the same nanosecond keep their order.
        assert_eq!(take_venue(&mut venues, 200), Some('Z'));
        // No note for 300: none, and the note for 400 is left for 400.
        assert_eq!(take_venue(&mut venues, 300), None);
        assert_eq!(take_venue(&mut venues, 400), Some('N'));
        assert_eq!(take_venue(&mut venues, 500), None);
    }

    #[test]
    fn frames_without_trades_or_that_do_not_parse_are_ignored() {
        let mut store = Store::default();
        record_quote(&mut store, &quote(at(14, 0, 0, 0), 1.99, 2.00));
        note_venues_in(&mut store, "not json");
        note_venues_in(&mut store, r#"[{"T":"success","msg":"connected"}]"#);
        note_venues_in(&mut store, r#"[{"T":"t","S":"AAA","x":"QQ","t":"2026-10-06T14:00:00Z"},{"T":"t","S":"AAA","x":"Q","t":"soon"}]"#);
        assert!(store.engines["AAA"].venues.is_empty());
    }

    #[test]
    fn sub_second_ordering_survives_the_conversion() {
        // A quote 400ms BEFORE the trade must classify it; with whole
        // seconds both would read as the same instant and still work, but
        // a quote 400ms AFTER must not, which only fractions can tell.
        let mut store = Store::default();
        record_quote(&mut store, &quote(at(14, 0, 0, 100), 2.00, 2.02));
        record_trade(&mut store, &trade(at(14, 0, 0, 500), 2.02, 300, &["@"]));
        record_quote(&mut store, &quote(at(14, 0, 1, 400), 2.10, 2.12));
        record_trade(&mut store, &trade(at(14, 0, 1, 0), 2.02, 200, &["@"]));
        let s = store.engines["AAA"].engine.summary(to_secs(at(14, 0, 2, 0)));
        let w = s.windows.iter().find(|w| w.window_secs == 5.0).unwrap();
        // First print: at the ask of its prevailing quote, a buy. Second:
        // the latest quote (2.10/2.12) postdates it by 400ms and must not
        // judge it -- used anyway, 2.02 would sit below that bid and read
        // as a sell. With no earlier price change to fall back on, it
        // stays unknown.
        assert_eq!((w.buy_volume, w.sell_volume, w.unknown_volume), (300, 0, 200));
    }

    #[test]
    fn auction_and_late_prints_get_no_side_but_keep_their_volume() {
        let mut store = Store::default();
        record_quote(&mut store, &quote(at(14, 0, 0, 0), 2.00, 2.02));
        record_trade(&mut store, &trade(at(14, 0, 0, 100), 2.02, 9_000, &["@", "5", "X"]));
        record_trade(&mut store, &trade(at(14, 0, 0, 200), 2.02, 400, &["@", "I"]));
        let s = store.engines["AAA"].engine.summary(to_secs(at(14, 0, 1, 0)));
        let w = s.windows[0];
        assert_eq!((w.buy_volume, w.unknown_volume, w.shares), (400, 9_000, 9_400));
    }

    #[test]
    fn bunched_prints_stay_classified_and_sweep_flags_are_tallied() {
        assert!(!flags_of(&["B".to_string()]).unclassifiable);
        let mut store = Store::default();
        record_quote(&mut store, &quote(at(14, 0, 0, 0), 2.00, 2.02));
        record_trade(&mut store, &trade(at(14, 0, 0, 100), 2.02, 700, &["@", "F"]));
        let s = store.engines["AAA"].engine.summary(to_secs(at(14, 0, 1, 0)));
        assert_eq!(s.sweeps.flagged_buy_volume, 700);
        assert_eq!(s.large_prints.iter().filter(|p| p.side == TradeSide::Buy).count(), 0);
    }

    #[test]
    fn cumulative_delta_is_bucketed_by_real_session() {
        let mut store = Store::default();
        record_quote(&mut store, &quote(at(12, 0, 0, 0), 2.00, 2.02));
        record_trade(&mut store, &trade(at(12, 0, 0, 100), 2.02, 300, &["@", "T"]));
        record_quote(&mut store, &quote(at(14, 0, 0, 0), 2.00, 2.02));
        record_trade(&mut store, &trade(at(14, 0, 0, 100), 2.00, 100, &["@"]));
        let s = store.engines["AAA"].engine.summary(to_secs(at(14, 0, 1, 0)));
        assert_eq!(s.cumulative_delta_premarket, 300);
        assert_eq!(s.cumulative_delta_regular, -100);
    }

    #[test]
    fn a_new_market_day_starts_from_nothing() {
        let mut store = Store::default();
        record_quote(&mut store, &quote(at(14, 0, 0, 0), 2.00, 2.02));
        assert_eq!(store.engines.len(), 1);
        let next_day = at(14, 0, 0, 0) + chrono::Duration::days(1);
        let mut other = quote(next_day, 5.00, 5.02);
        other.symbol = "BBB".into();
        record_quote(&mut store, &other);
        assert!(store.engines.contains_key("BBB") && !store.engines.contains_key("AAA"));
    }

    #[test]
    fn active_summaries_skip_quiet_symbols_and_come_back_sorted() {
        let mut store = Store::default();
        for (symbol, trade_at) in [("ZZZ", at(14, 0, 50, 0)), ("AAA", at(14, 0, 55, 0)), ("OLD", at(13, 0, 0, 0))] {
            let mut q = quote(trade_at - chrono::Duration::milliseconds(100), 2.00, 2.02);
            q.symbol = symbol.into();
            record_quote(&mut store, &q);
            let mut t = trade(trade_at, 2.02, 100, &["@"]);
            t.symbol = symbol.into();
            record_trade(&mut store, &t);
        }
        // A symbol with a quote but no trade yet has nothing to record.
        let mut silent = quote(at(14, 0, 58, 0), 9.00, 9.02);
        silent.symbol = "NEW".into();
        record_quote(&mut store, &silent);

        let rows = active_in(&store, at(14, 1, 0, 0), 30.0);
        assert_eq!(rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), vec!["AAA", "ZZZ"]);
    }

    #[test]
    fn backfill_stitches_history_and_live_without_a_gap_or_a_double_count() {
        let store = Mutex::new(Store::default());
        {
            let mut s = store.lock().unwrap();
            record_quote(&mut s, &quote(at(14, 0, 9, 0), 2.00, 2.02));
            record_trade(&mut s, &trade(at(14, 0, 10, 0), 2.02, 100, &["@"]));
            assert!(begin_in(&mut s, "AAA"));
            // Only one backfill per symbol per day.
            assert!(!begin_in(&mut s, "AAA"));
            assert_eq!(status_in(&s, "AAA"), BackfillStatus::Running);
            // Arrives while the history request is in flight.
            record_trade(&mut s, &trade(at(14, 0, 20, 0), 2.02, 300, &["@"]));
        }
        // What the provider returns overlaps both live trades.
        let history = history_events(
            &[
                trade(at(13, 31, 0, 0), 2.00, 1_000, &["@"]),
                trade(at(14, 0, 10, 0), 2.02, 100, &["@"]),
                trade(at(14, 0, 20, 0), 2.02, 300, &["@"]),
            ],
            &[quote(at(13, 30, 59, 0), 1.98, 2.00), quote(at(14, 0, 9, 0), 2.00, 2.02)],
        );
        finish_in(&store, "AAA", history);

        let s = store.lock().unwrap();
        assert_eq!(status_in(&s, "AAA"), BackfillStatus::Done);
        let summary = s.engines["AAA"].engine.summary(to_secs(at(14, 0, 21, 0)));
        // 1,000 from before tracking began, 100 seen live before the
        // backfill started (taken from history), 300 seen live during it
        // (taken from the buffer). Each exactly once.
        assert_eq!(summary.cumulative_delta_regular, 1_400);
        assert_eq!(summary.profile.unwrap().total_volume, 1_400);
    }

    #[test]
    fn backfill_is_a_no_op_for_unknown_symbols_and_after_failure() {
        let store = Mutex::new(Store::default());
        assert!(!begin_in(&mut store.lock().unwrap(), "NOPE"));
        finish_in(&store, "NOPE", Vec::new());
        {
            let mut s = store.lock().unwrap();
            record_quote(&mut s, &quote(at(14, 0, 9, 0), 2.00, 2.02));
            record_trade(&mut s, &trade(at(14, 0, 10, 0), 2.02, 100, &["@"]));
            assert_eq!(status_in(&s, "AAA"), BackfillStatus::None);
            assert!(begin_in(&mut s, "AAA"));
            s.engines.get_mut("AAA").unwrap().backfill = Backfill::Failed;
        }
        // History arriving after the backfill was abandoned changes nothing.
        finish_in(&store, "AAA", history_events(&[trade(at(13, 31, 0, 0), 2.00, 9_999, &["@"])], &[]));
        let s = store.lock().unwrap();
        assert_eq!(status_in(&s, "AAA"), BackfillStatus::Failed);
        assert_eq!(s.engines["AAA"].engine.summary(to_secs(at(14, 0, 11, 0))).profile.unwrap().total_volume, 100);
    }

    #[test]
    fn history_events_are_time_ordered_with_quotes_before_trades_at_a_tie() {
        let events = history_events(&[trade(at(14, 0, 5, 0), 2.02, 1, &[]), trade(at(14, 0, 1, 0), 2.02, 1, &[])], &[quote(at(14, 0, 5, 0), 2.00, 2.02)]);
        let kinds: Vec<&str> = events.iter().map(|e| if matches!(e, Event::Quote(_)) { "q" } else { "t" }).collect();
        assert_eq!(kinds, vec!["t", "q", "t"]);
    }

    #[test]
    fn only_a_quote_creates_an_engine() {
        // The full-market trade tape must not spawn an engine per symbol.
        let mut store = Store::default();
        record_trade(&mut store, &trade(at(14, 0, 0, 0), 2.00, 100, &["@"]));
        assert!(store.engines.is_empty());
        record_quote(&mut store, &quote(at(14, 0, 1, 0), 2.00, 2.02));
        record_trade(&mut store, &trade(at(14, 0, 1, 100), 2.02, 100, &["@"]));
        let s = store.engines["AAA"].engine.summary(to_secs(at(14, 0, 2, 0)));
        assert_eq!(s.windows.iter().find(|w| w.window_secs == 5.0).unwrap().buy_volume, 100);
    }
}
