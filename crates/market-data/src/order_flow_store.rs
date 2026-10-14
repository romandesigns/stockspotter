//! Feeds live trades and quotes into `order_flow::OrderFlowEngine`, one per
//! tracked symbol, and hands out summaries on request.
//!
//! **Observational only, and off unless asked for.** Nothing here emits a
//! `ScanEvent`, gates an alert or touches ranking, and with
//! `ORDER_FLOW_ENABLED` unset the live hooks and global state-mutating
//! operations return before initializing or locking the store. That default is deliberate: this runs inside the live scan
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
//! Three things protect that single mutex from the delivery layer added on
//! top of it (HTTP poll, per-connection WS push, recorder):
//!
//! - every event is checked against the moment its frame was received
//!   (`sane_event_time`) before anything global moves, so one bad
//!   timestamp cannot roll the day or move the sweep clock;
//! - polls and pushes take `lite_read`, which serves a cached summary
//!   without the three chart-only arrays and rebuilds it at most once per
//!   `LITE_TTL_SECS` per symbol however many consumers ask;
//! - a backfill carries the engine generation it was started for, so a
//!   late replay cannot be written into an engine that replaced it.
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
    /// can feed an engine exactly the way the live store does. No offline
    /// runner is included in this ingestion slice.
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
    /// Observed event-time/volume before the history request began.
    backfill_boundary: Option<(f64, u64)>,
    /// The live feed the buffered segment was observed on, snapshotted
    /// from configuration at `begin_backfill`. History that arrives from a
    /// different feed describes a different tape -- `iex` is a single
    /// venue, `sip` is the consolidated one -- so joining the two would
    /// produce a volume profile that is neither. Refused, not reconciled.
    splice_live_feed: Option<String>,
    /// Exchange codes for trades that have been read off the socket but
    /// not yet delivered to `on_trade`, oldest first (`note_venues`).
    venues: VecDeque<(i64, char)>,
    /// Bumped for every engine ever created, so a backfill that completes
    /// after its entry was evicted and recreated under the same symbol
    /// cannot overwrite the new engine with the old one's history. See
    /// `finish_backfill`.
    generation: u64,
    /// True when this symbol has already lost an engine today (idle
    /// eviction, or a capacity rejection before it got one). Its
    /// observations have a hole in them that no backfill fills, so
    /// `coverage` can never report a whole session for it again today.
    partial: bool,
    /// Cached stripped summary and the event-time it was computed at, so a
    /// fan-out of pushes and polls does not rebuild delta buckets, profile
    /// bins and large prints per consumer while the live loop waits for
    /// the same lock. See `LITE_TTL_SECS`.
    lite: Option<(f64, OrderFlowSummary)>,
}

/// Far more than one frame ever carries for one symbol; a bound only so a
/// symbol whose trades stop being delivered cannot grow this forever.
const MAX_PENDING_VENUES: usize = 4096;

impl Entry {
    fn apply(&mut self, event: Event) -> bool {
        let mut discarded = false;
        self.last_seen = self.last_seen.max(event.timestamp_secs());
        if let Backfill::Running(buffer) = &mut self.backfill {
            let crosses_boundary = self.backfill_boundary.is_none_or(|(at, _)| event.timestamp_secs() <= at);
            let backwards = buffer.last().is_some_and(|previous| event.timestamp_secs() < previous.timestamp_secs());
            if buffer.len() < MAX_BUFFERED_EVENTS && !crosses_boundary && !backwards {
                buffer.push(event);
            } else {
                // Keep every event in the live engine. An ambiguous splice
                // must not replace it with a guessed reconstructed engine.
                self.backfill = Backfill::Failed;
                self.backfill_boundary = None;
                discarded = true;
            }
        }
        event.apply(&mut self.engine);
        discarded
    }
}

/// Bounds are a conservative research resource policy, not efficacy thresholds.
const MAX_ENGINES: usize = 256;
const IDLE_SECS: f64 = 600.0;

/// How far a feed timestamp may sit ahead of the moment the frame was
/// received before the event is refused. A real tape is behind receipt,
/// never ahead; a few seconds allows for clock skew between this box and
/// the venue's own clock.
const MAX_AHEAD_OF_RECEIPT_SECS: f64 = 5.0;
/// How far behind receipt a feed timestamp may sit. Fifteen minutes is far
/// more than normal socket lag and still well short of the idle sweep, so
/// an ordinary late batch is kept while a retro-dated or replayed frame is
/// refused before it can move the market day or the sweep clock.
const MAX_BEHIND_RECEIPT_SECS: f64 = 900.0;

/// How long a stripped summary may be served before it is rebuilt. The
/// cost it removes is real: a push fan-out of four symbols per connection
/// every two seconds otherwise rebuilt delta buckets, profile bins and
/// large prints per consumer while the live scan loop waited for the same
/// mutex. The price is that `asOfSecs` can be up to this far behind, which
/// is why it is on the wire for a reader to check.
const LITE_TTL_SECS: f64 = 1.0;

/// Defensive ceiling on history replayed into one engine. The caller has
/// its own page and event caps; this is the last line, so a provider
/// answer far larger than expected cannot be replayed under the lock's
/// shadow for minutes.
pub const MAX_HISTORY_EVENTS: usize = 1_500_000;

#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationHealth {
    pub engines: usize,
    pub evicted: u64,
    pub capacity_rejected: u64,
    pub late_discarded: u64,
    pub corrections_seen: u64,
    pub cancels_seen: u64,
    /// Frames whose feed timestamp was not plausible against the moment
    /// they were received, and so were not applied to any engine and did
    /// not advance the market day or the sweep clock.
    pub timestamp_rejected: u64,
    /// Backfill attempts abandoned instead of spliced, counted **exactly
    /// once each** -- see `refuse_splice`. One attempt is one
    /// `begin_backfill`, so this is directly comparable with the number of
    /// backfills started: every refusal reason increments it once and only
    /// once, including the ingestion-time refusal in `Entry::apply` (which
    /// is counted there, at the moment the live event made the boundary
    /// ambiguous, and must not be counted again when the history lands).
    pub backfill_discarded: u64,
    /// Trades applied to an engine whose frame carried the provider's own
    /// trade id, and those whose frame did not. Counters, not a tape: they
    /// say how much of what was ingested *could* be identified, which is
    /// what a later reader needs to know before trusting any row's
    /// identity. Nothing here retains the ids themselves -- see
    /// `SourceIdentity` and `order_flow::LargePrint::id` for what is kept.
    pub trades_with_provider_id: u64,
    pub trades_without_provider_id: u64,
}

/// No correction is applied by this observational layer. Counters disclose
/// receipt, not successful reconciliation or a corrected volume/profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TapeProvenance { Uncorrected, CorrectionsPending }

/// The provider this build speaks. A constant rather than configuration
/// because the ingestion, the REST history and the condition-code tables
/// in this crate are all Alpaca-specific: a different provider would be a
/// different code path, not a different string.
pub const PROVIDER: &str = "alpaca";

/// Which source produced the observations -- recorded **only** from
/// configuration that was actually used, never inferred and never
/// defaulted.
///
/// `None` means "this process was never told", which is a different claim
/// from any feed name and must stay distinguishable: a row stamped `sip`
/// because that is the usual value would be a fabricated statement about
/// where the numbers came from. The splice refuses rather than guesses
/// when either side is `None` (see `finish_with_hook`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceIdentity {
    /// `ALPACA_FEED` as the live socket for this process was configured
    /// with it, set by the ingestion loop via `set_live_feed`.
    pub live_feed: Option<String>,
    /// `ALPACA_FEED` as the most recent historical request was made with
    /// it, set by the backfill caller via `note_historical_feed`.
    pub historical_feed: Option<String>,
}

/// What provider identity the rows of a sample can and cannot prove.
///
/// Serialized on every row written by this build so that identity is an
/// explicit statement rather than an inference from a missing field. A row
/// **without** this field was written by a build that retained no provider
/// identity at all (the archived 2026-10-06/07 recordings are all of that
/// kind) and must be read as `unavailable`, never as `none observed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TradeIdentityScope {
    /// Recorded rows carry aggregates and no per-print ids. The full HTTP
    /// response separately retains ids only for the bounded set of large
    /// prints (`order_flow::LargePrint::id`, capped by
    /// `MAX_LARGE_PRINTS`). No other print's id is kept, so this is not a
    /// raw tape and supports no deduplication or completeness claim.
    ///
    /// And those prints are one of the three chart-only arrays, which
    /// `strip_chart_arrays` removes from every recorded row: a row stamped
    /// with this scope carries the provider and feed context and the
    /// `trades_with_provider_id` / `trades_without_provider_id` counts,
    /// and **no per-print ids**. The ids are visible on the full HTTP read
    /// only. Recording them is a materially larger scope than the
    /// recorder's byte ceilings allow and is deliberately not taken here.
    AggregateOnlyNoPrintIds,
}

/// Everything a reader needs to say honestly how much of today a summary
/// covers. Never upgraded once `Partial`: an evicted engine leaves a hole
/// in the middle of the day that no backfill of the earlier session fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    /// Only what was observed while an engine happened to exist.
    Partial,
    /// A session backfill is in flight; the totals should extend back.
    SessionPending,
    /// The session's earlier trades were fetched and replayed, and this
    /// engine has existed continuously since.
    Session,
}

/// What a delivery layer serves for one symbol. The summary's three
/// chart-only arrays are empty in a `lite` read and populated in a `full`
/// one; `lite` is also the only one that is ever cached.
#[derive(Debug, Clone)]
pub struct Read {
    pub summary: OrderFlowSummary,
    pub coverage: Coverage,
    pub backfill: BackfillStatus,
    pub provenance: TapeProvenance,
    pub corrections_seen: u64,
    pub cancels_seen: u64,
}

/// One symbol's row of a recording sample: the summary a recorder writes,
/// and the context needed to say later what it was read from.
///
/// This exists because a summary on its own is not reproducible evidence.
/// The archived recordings (2026-10-06/07) are mathematically consistent
/// and still cannot answer "how much of the session is in this row", "was
/// this engine the same one as in the previous sample", or "was the tape
/// uncorrected" -- so a decrease in a cumulative profile volume between two
/// rows cannot be told apart from an engine having been evicted and
/// recreated in between.
#[derive(Debug, Clone)]
pub struct RecordedRow {
    pub symbol: String,
    pub summary: OrderFlowSummary,
    pub coverage: Coverage,
    pub backfill: BackfillStatus,
    /// Which engine produced it. Two consecutive samples with different
    /// generations for one symbol are not one continuous observation: the
    /// entry was evicted (or capacity-rejected) and recreated in between,
    /// and the later row's cumulative totals start from nothing.
    pub generation: u64,
    /// This symbol has already lost an engine today (`Entry::partial`), so
    /// there is a hole in its day that no backfill fills.
    pub partial: bool,
}

/// One recording sample: every active symbol's row and the process-wide
/// context they share, read together in a single lock acquisition so a
/// row's coverage cannot describe a different moment than its numbers.
#[derive(Debug, Clone)]
pub struct ObservationSnapshot {
    /// The market day the store's engines are on, as the store itself has
    /// it, and `None` before any event has arrived.
    ///
    /// Deliberately not the caller's clock. The market day rolls at 04:00
    /// ET (`market_day`) but the store rolls on the first frame of the new
    /// day, so between the two a recorder's clock says today while every
    /// engine still holds the previous session. Stamping the clock's date
    /// on those rows would record a day that never traded them.
    pub market_day: Option<NaiveDate>,
    /// Process-wide counters, `engines` filled in. Global, never per
    /// symbol: they say nothing about whether any one row's tape is
    /// reconciled.
    pub health: ObservationHealth,
    pub provenance: TapeProvenance,
    /// Which provider feeds produced these rows, from configuration only.
    pub source: SourceIdentity,
    pub rows: Vec<RecordedRow>,
}

#[derive(Default)]
struct Store {
    day: Option<NaiveDate>,
    engines: HashMap<String, Entry>,
    health: ObservationHealth,
    last_sweep: f64,
    /// Symbols that have lost an engine today, so a recreated one is
    /// marked partial. Bounded, and cleared with everything else on a day
    /// roll.
    lost: std::collections::HashSet<String>,
    next_generation: u64,
    /// Process configuration, not per-day state: deliberately NOT cleared
    /// by `roll_to`, because the feed a socket was opened with does not
    /// change at the 04:00 ET boundary.
    source: SourceIdentity,
}

/// Bound on `Store::lost`. Past it the set stops growing and newly
/// recreated engines are marked partial unconditionally -- the safe
/// direction, since by then this process has evicted several times the
/// number of engines it can hold.
const MAX_LOST: usize = MAX_ENGINES * 4;

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
            self.lost.clear();
            self.last_sweep = 0.0;
        }
        true
    }

    fn sweep(&mut self, now: f64) {
        // At most one bounded map scan per second, not per tape event.
        if now < self.last_sweep + 1.0 { return; }
        self.last_sweep = now;
        let before = self.engines.len();
        let mut lost: Vec<String> = Vec::new();
        self.engines.retain(|symbol, entry| {
            let keep = now - entry.last_seen <= IDLE_SECS;
            if !keep { lost.push(symbol.clone()); }
            keep
        });
        self.health.evicted += (before - self.engines.len()) as u64;
        self.note_lost(lost);
    }

    fn note_lost(&mut self, symbols: impl IntoIterator<Item = String>) {
        for symbol in symbols {
            if self.lost.len() < MAX_LOST { self.lost.insert(symbol); }
        }
    }

    /// A fresh entry, carrying its own generation and whether this symbol
    /// has already lost one today.
    fn new_entry(&mut self, symbol: &str) -> Entry {
        self.next_generation += 1;
        Entry {
            generation: self.next_generation,
            partial: self.lost.contains(symbol) || self.lost.len() >= MAX_LOST,
            ..Entry::default()
        }
    }

    fn provenance(&self) -> TapeProvenance {
        if self.health.corrections_seen + self.health.cancels_seen > 0 {
            TapeProvenance::CorrectionsPending
        } else {
            TapeProvenance::Uncorrected
        }
    }
}

/// Whether an event's own timestamp is plausible given when its frame was
/// received, so neither an injected feed timestamp nor a stale receipt can
/// roll the market day, move the sweep clock, or land in an engine.
///
/// Receipt time is this process's clock at the moment the frame was read;
/// `event` is the venue's. The window is deliberately asymmetric: a tape
/// is behind receipt, never meaningfully ahead of it.
fn sane_event_time(event_secs: f64, receipt_secs: f64) -> bool {
    if !event_secs.is_finite() || !receipt_secs.is_finite() { return false; }
    let ahead = event_secs - receipt_secs;
    ahead <= MAX_AHEAD_OF_RECEIPT_SECS && -ahead <= MAX_BEHIND_RECEIPT_SECS
}

/// How much of today a summary covers. `partial` is "this symbol has
/// already lost an engine today", which no backfill of the earlier
/// session repairs, so it wins over every backfill state.
fn coverage_of(partial: bool, backfill: BackfillStatus) -> Coverage {
    if partial {
        return Coverage::Partial;
    }
    match backfill {
        BackfillStatus::Done => Coverage::Session,
        BackfillStatus::Running => Coverage::SessionPending,
        BackfillStatus::None | BackfillStatus::Failed => Coverage::Partial,
    }
}

/// Everything in a summary except the three arrays that exist only to
/// draw charts. See `LITE_TTL_SECS` for why this shape is the cached one.
fn strip_chart_arrays(mut summary: OrderFlowSummary) -> OrderFlowSummary {
    summary.delta_buckets = Vec::new();
    summary.large_prints = Vec::new();
    if let Some(profile) = summary.profile.as_mut() {
        profile.bins = Vec::new();
    }
    summary
}

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();

fn store() -> &'static Mutex<Store> {
    STORE.get_or_init(|| Mutex::new(Store::default()))
}

/// Read once: flipping this needs a restart, same as every other
/// behaviour switch in the live loop.
pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var(ORDER_FLOW_ENV).is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")))
}

/// Records the feed the live socket was configured with, so a recording
/// can say which tape its numbers came from.
///
/// Call it with `AlpacaConfig::feed` and nothing else. A blank value is
/// treated as "never told" rather than recorded as an empty feed name,
/// because an empty string on a row would read as a feed rather than as
/// the absence of one. A no-op when the store is switched off.
pub fn set_live_feed(feed: &str) {
    if !enabled() { return; }
    let mut store = store().lock().unwrap_or_else(|e| e.into_inner());
    store.source.live_feed = non_empty(feed);
}

/// Records the feed a historical request was actually made with. Called by
/// the backfill path with the same config the request used, so the pair of
/// feeds on a recorded row describes the two halves it was built from.
pub fn note_historical_feed(feed: &str) {
    if !enabled() { return; }
    let mut store = store().lock().unwrap_or_else(|e| e.into_inner());
    store.source.historical_feed = non_empty(feed);
}

/// Which feeds this process has been told about. `None` fields are
/// genuinely unknown and must not be filled in by a reader.
///
/// Gated like every other hook here, so that reading provenance cannot be
/// the thing that initializes the global store in a process with order
/// flow switched off.
pub fn source_identity() -> SourceIdentity {
    if !enabled() { return SourceIdentity::default(); }
    store().lock().unwrap_or_else(|e| e.into_inner()).source.clone()
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
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

/// `id` is the provider's own trade id when the frame carried one, passed
/// straight through and never synthesized. Nothing in classification reads
/// it; it exists so the bounded large prints a summary already keeps can
/// say which provider print they were.
fn flags_of(conditions: &[String], id: Option<u64>) -> TradeFlags {
    TradeFlags {
        unclassifiable: conditions.iter().any(|c| UNCLASSIFIABLE_CONDITIONS.contains(&c.as_str())),
        sweep: conditions.iter().any(|c| c == SWEEP_CONDITION),
        exchange: None,
        id,
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
/// With observation enabled, this parses every raw frame even if there are
/// no engines, so correction/cancel evidence is not silently missed. That
/// enabled full-tape parsing and locking cost requires load validation.
///
/// `received` is when the frame was read off the socket, and notes whose
/// own timestamp is not plausible against it are dropped. `take_venue`
/// matches by exact timestamp and discards only older notes, so a single
/// far-future note would otherwise sit at the front of the queue and
/// block venue attribution for that symbol until the queue's bound
/// evicted it.
pub fn note_venues(raw: &str, received: DateTime<Utc>) {
    if !enabled() {
        return;
    }
    let mut store = store().lock().unwrap_or_else(|e| e.into_inner());
    note_venues_in(&mut store, raw, to_secs(received));
}

fn note_venues_in(store: &mut Store, raw: &str, received_secs: f64) {
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
        let Some(parsed) = DateTime::parse_from_rfc3339(t).ok() else {
            continue;
        };
        let Some(nanos) = parsed.timestamp_nanos_opt() else {
            continue;
        };
        if !sane_event_time(nanos as f64 / 1e9, received_secs) {
            store.health.timestamp_rejected += 1;
            continue;
        }
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

/// Call for every quote received, with the moment the frame carrying it
/// was read off the socket.
pub fn on_quote(quote: &Quote, received: DateTime<Utc>) {
    if !enabled() {
        return;
    }
    record_quote(&mut store().lock().unwrap_or_else(|e| e.into_inner()), quote, to_secs(received));
}

/// Call for every trade received; ignored for symbols with no engine.
pub fn on_trade(trade: &Trade, received: DateTime<Utc>) {
    if !enabled() {
        return;
    }
    record_trade(&mut store().lock().unwrap_or_else(|e| e.into_inner()), trade, to_secs(received));
}

/// Process telemetry: how many engines exist, what has been rejected, and
/// how much correction evidence has arrived. Global, never per symbol --
/// it says nothing about whether any one symbol's tape is reconciled.
pub fn observation_health() -> Option<(ObservationHealth, TapeProvenance)> {
    if !enabled() { return None; }
    let store = store().lock().unwrap_or_else(|e| e.into_inner());
    let mut health = store.health;
    health.engines = store.engines.len();
    let provenance = store.provenance();
    Some((health, provenance))
}

/// Current summary for `symbol`, or `None` if it has not traded while
/// tracked today (or the feature is off).
///
/// Rebuilds everything, chart arrays included, and is the expensive shape:
/// prefer `lite_read` for anything polled or pushed.
pub fn summary(symbol: &str, now: DateTime<Utc>) -> Option<OrderFlowSummary> {
    full_read(symbol, now).map(|read| read.summary)
}

/// The full shape -- chart arrays included -- never cached.
pub fn full_read(symbol: &str, now: DateTime<Utc>) -> Option<Read> {
    if !enabled() {
        return None;
    }
    let store = store().lock().unwrap_or_else(|e| e.into_inner());
    let entry = store.engines.get(symbol)?;
    Some(Read {
        summary: entry.engine.summary(to_secs(now)),
        coverage: coverage_of(entry.partial, status_of(&entry.backfill)),
        backfill: status_of(&entry.backfill),
        provenance: store.provenance(),
        corrections_seen: store.health.corrections_seen,
        cancels_seen: store.health.cancels_seen,
    })
}

/// The shape a poll or a push gets: everything except the three chart-only
/// arrays, rebuilt at most once per `LITE_TTL_SECS` per symbol however
/// many consumers ask.
pub fn lite_read(symbol: &str, now: DateTime<Utc>) -> Option<Read> {
    if !enabled() {
        return None;
    }
    lite_in(&mut store().lock().unwrap_or_else(|e| e.into_inner()), symbol, to_secs(now))
}

fn lite_in(store: &mut Store, symbol: &str, now_secs: f64) -> Option<Read> {
    let provenance = store.provenance();
    let (corrections_seen, cancels_seen) = (store.health.corrections_seen, store.health.cancels_seen);
    let entry = store.engines.get_mut(symbol)?;
    let fresh = entry.lite.as_ref().is_some_and(|(at, _)| now_secs - at <= LITE_TTL_SECS && now_secs >= *at);
    if !fresh {
        entry.lite = Some((now_secs, strip_chart_arrays(entry.engine.summary(now_secs))));
    }
    let summary = entry.lite.as_ref().map(|(_, s)| s.clone())?;
    let backfill = status_of(&entry.backfill);
    let coverage = coverage_of(entry.partial, backfill);
    Some(Read { summary, coverage, backfill, provenance, corrections_seen, cancels_seen })
}

/// Summaries for every symbol that traded within the last
/// `max_idle_secs`, for the recorder. Quiet symbols are left out: their
/// summary has not changed in any way worth writing down again.
///
/// Stripped of the chart-only arrays here rather than by the caller: they
/// are most of the bytes, they are reconstructible, and a recording run
/// must not pay to serialize them once per symbol per sample.
pub fn active_summaries(now: DateTime<Utc>, max_idle_secs: f64) -> Vec<(String, OrderFlowSummary)> {
    if !enabled() {
        return Vec::new();
    }
    active_in(&store().lock().unwrap_or_else(|e| e.into_inner()), now, max_idle_secs)
}

fn active_in(store: &Store, now: DateTime<Utc>, max_idle_secs: f64) -> Vec<(String, OrderFlowSummary)> {
    snapshot_in(store, now, max_idle_secs).rows.into_iter().map(|row| (row.symbol, row.summary)).collect()
}

/// The same active symbols as `active_summaries`, each with the coverage,
/// backfill state and engine identity it was read with, plus the
/// process-wide health, tape provenance and market day -- all from one
/// acquisition of the store lock.
///
/// This is what a recorder writes. Taking it as one snapshot is the point:
/// reading summaries and then asking for health and coverage per symbol
/// would re-lock once per symbol while the live scan loop waits for the
/// same mutex, and would attach context from a later moment to numbers
/// from an earlier one.
///
/// `None` when the feature is off, which returns before the global store
/// is initialized at all.
pub fn active_snapshot(now: DateTime<Utc>, max_idle_secs: f64) -> Option<ObservationSnapshot> {
    if !enabled() {
        return None;
    }
    Some(snapshot_in(&store().lock().unwrap_or_else(|e| e.into_inner()), now, max_idle_secs))
}

fn snapshot_in(store: &Store, now: DateTime<Utc>, max_idle_secs: f64) -> ObservationSnapshot {
    let now_secs = to_secs(now);
    let mut rows: Vec<RecordedRow> = store
        .engines
        .iter()
        .filter(|(_, entry)| entry.engine.last_trade_secs().is_some_and(|t| now_secs - t <= max_idle_secs))
        .map(|(symbol, entry)| {
            let backfill = status_of(&entry.backfill);
            RecordedRow {
                symbol: symbol.clone(),
                summary: strip_chart_arrays(entry.engine.summary(now_secs)),
                coverage: coverage_of(entry.partial, backfill),
                backfill,
                generation: entry.generation,
                partial: entry.partial,
            }
        })
        .collect();
    // Stable output: a HashMap's order would make two runs' files differ
    // for no reason.
    rows.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    let mut health = store.health;
    health.engines = store.engines.len();
    ObservationSnapshot { market_day: store.day, health, provenance: store.provenance(), source: store.source.clone(), rows }
}

/// What `GET /orderflow/:symbol` reports about history for a symbol. Say
/// nothing about completeness from this alone: `Done` means the earlier
/// session was replayed into THIS engine, which `Coverage` combines with
/// whether the symbol has lost an engine today.
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
    store.engines.get(symbol).map_or(BackfillStatus::None, |e| status_of(&e.backfill))
}

fn status_of(backfill: &Backfill) -> BackfillStatus {
    match backfill {
        Backfill::Running(_) => BackfillStatus::Running,
        Backfill::Done => BackfillStatus::Done,
        Backfill::Failed => BackfillStatus::Failed,
        Backfill::NotStarted => BackfillStatus::None,
    }
}

/// Session backfill, step 1 of 2. Without it, order flow for a symbol
/// starts when the server began tracking it, so a chart opened at 09:50
/// shows a cumulative delta and volume profile that silently cover only
/// part of the session.
///
/// Returns the engine generation the caller must hand back, if it should
/// now fetch this symbol's history and pass it to `finish_backfill` (or
/// call `fail_backfill`). `None` means there is nothing to do: unknown
/// symbol, or a backfill already ran or is running today. At most one
/// backfill per symbol per market day.
///
/// The generation is what makes an async fetch safe: by the time history
/// arrives, the entry may have been evicted for idleness and recreated by
/// a later quote, and replaying the old session into that new engine would
/// invent a history it does not have.
///
/// From this point until `finish_backfill`, live events are buffered as
/// well as applied, which is what lets history and live data be stitched
/// together without a gap or a double count.
pub fn begin_backfill(symbol: &str) -> Option<u64> {
    if !enabled() {
        return None;
    }
    begin_in(&mut store().lock().unwrap_or_else(|e| e.into_inner()), symbol)
}

fn begin_in(store: &mut Store, symbol: &str) -> Option<u64> {
    // Snapshotted here, not read at completion: this is the feed the
    // buffered live segment is actually being observed on, and a
    // reconfiguration between now and then must not be able to make the
    // two halves look like they came from one tape.
    let live_feed = store.source.live_feed.clone();
    match store.engines.get_mut(symbol) {
        Some(entry) if matches!(entry.backfill, Backfill::NotStarted) => {
            let volume = entry.engine.summary(entry.last_seen).profile.map_or(0, |profile| profile.total_volume);
            entry.backfill_boundary = Some((entry.last_seen, volume));
            entry.splice_live_feed = live_feed;
            entry.backfill = Backfill::Running(Vec::new());
            Some(entry.generation)
        }
        _ => None,
    }
}

/// Abandons an attempt whose fetch never produced history (provider error,
/// timeout, refused budget). Counted like any other abandoned attempt, so
/// `backfill_discarded` stays comparable with the number started -- unless
/// ingestion already counted this one.
pub fn fail_backfill(symbol: &str, generation: u64) {
    if !enabled() { return; }
    fail_in(&mut store().lock().unwrap_or_else(|e| e.into_inner()), symbol, generation);
}

fn fail_in(store: &mut Store, symbol: &str, generation: u64) {
    refuse(store, symbol, generation);
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
/// counted twice. This assumes the supplied history covers the symbol's
/// earlier live observations; provider history that does not reach back
/// far enough simply leaves them out, which is why the result is reported
/// as `Coverage::Session` for THIS engine and never as a proven complete
/// session (see `Coverage`).
///
/// `generation` is the value `begin_backfill` returned. A mismatch means
/// the engine this history was fetched for no longer exists -- evicted for
/// idleness and recreated under the same symbol -- and the replay is
/// discarded rather than written into an engine it does not describe.
///
/// The replay runs WITHOUT the store lock held -- it can be hundreds of
/// thousands of events, and the live scan loop takes that lock on every
/// trade -- and only the short tail of events that arrived during the
/// replay is applied under it.
/// `historical_feed` is the feed the history was actually fetched with
/// (`AlpacaConfig::feed` of the request). It must equal the feed the live
/// segment was observed on or the splice is refused -- see `Refusal` and
/// `Entry::splice_live_feed`. `None` is "the caller cannot say", which is
/// also a refusal: a feed that cannot be shown to match is not assumed to.
pub fn finish_backfill(symbol: &str, generation: u64, history: Vec<Event>, historical_feed: Option<&str>) {
    if !enabled() { return; }
    finish_in(store(), symbol, generation, history, historical_feed);
}

fn finish_in(store: &Mutex<Store>, symbol: &str, generation: u64, history: Vec<Event>, historical_feed: Option<&str>) {
    finish_with_hook(store, symbol, generation, history, historical_feed, || {});
}

/// Why a finished history fetch was not spliced into the live engine.
///
/// Every variant abandons the attempt; what differs is whether *this* path
/// is the one that discovered it. When the entry is already `Failed`, the
/// ingestion path (`Entry::apply`) counted this same attempt at the moment
/// a live event made the boundary ambiguous, and counting it again here
/// would report two abandoned backfills where only one was ever started.
#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    /// Nothing has counted this attempt yet.
    Fresh,
    /// `Entry::apply` already counted it at ingestion.
    AlreadyCounted,
}

/// Abandons a backfill attempt, counting it **exactly once**.
///
/// The counter moves even when the entry is gone or has been replaced: the
/// attempt was still started and is still being abandoned, and skipping
/// those would under-report precisely the conservative refusals this
/// counter exists to make visible. The live engine is never touched -- a
/// refused splice loses the history, never the observations.
fn refuse_splice(guard: &mut Store, symbol: &str, generation: u64, refusal: Refusal) {
    if let Some(entry) = guard.engines.get_mut(symbol).filter(|e| e.generation == generation) {
        entry.backfill = Backfill::Failed;
        entry.backfill_boundary = None;
        entry.splice_live_feed = None;
    }
    if refusal == Refusal::Fresh {
        guard.health.backfill_discarded += 1;
    }
}

/// What should happen to an attempt that will not be spliced.
///
/// `None` means **leave the entry alone and count nothing**: there is no
/// attempt in flight to abandon (`NotStarted`), or one already finished
/// (`Done`) and a late or duplicate completion must not downgrade it to
/// `Failed` or report a discard that did not happen.
fn refusal_for(guard: &Store, symbol: &str, generation: u64) -> Option<Refusal> {
    match guard.engines.get(symbol) {
        Some(entry) if entry.generation == generation => match entry.backfill {
            Backfill::Running(_) => Some(Refusal::Fresh),
            Backfill::Failed => Some(Refusal::AlreadyCounted),
            Backfill::NotStarted | Backfill::Done => None,
        },
        // Nothing under this generation. Generations are handed out in
        // sequence and only ever increase, so one *past* the counter was
        // never issued: there is no attempt to abandon and nothing to
        // count. One at or below it was issued and its entry has since
        // been evicted or replaced -- that attempt is abandoned, and
        // nothing has counted it yet.
        _ if generation > guard.next_generation => None,
        _ => Some(Refusal::Fresh),
    }
}

/// Abandons the attempt if there is one, counting it exactly once.
fn refuse(guard: &mut Store, symbol: &str, generation: u64) {
    if let Some(refusal) = refusal_for(guard, symbol, generation) {
        refuse_splice(guard, symbol, generation, refusal);
    }
}

// The injected callback exercises arrivals during replay without timing races.
fn finish_with_hook(store: &Mutex<Store>, symbol: &str, generation: u64, history: Vec<Event>, historical_feed: Option<&str>, before_commit: impl FnOnce()) {
    if history.len() > MAX_HISTORY_EVENTS {
        let mut guard = store.lock().unwrap_or_else(|e| e.into_inner());
        refuse(&mut guard, symbol, generation);
        return;
    }
    enum Start { Go(Vec<Event>, (f64, u64)), Refuse }
    let start = {
        let guard = store.lock().unwrap_or_else(|e| e.into_inner());
        match guard.engines.get(symbol) {
            Some(entry) if entry.generation == generation => {
                // Both halves must name the same feed. `iex` is a single
                // venue and `sip` is the consolidated tape, so an engine
                // built from one of each has a volume profile that is
                // neither; and a feed nobody recorded cannot be shown to
                // match, so it is refused rather than assumed.
                let same_feed = match (entry.splice_live_feed.as_deref(), historical_feed) {
                    (Some(live), Some(historical)) => live == historical,
                    _ => false,
                };
                match (&entry.backfill, entry.backfill_boundary) {
                    (Backfill::Running(buffer), Some(boundary)) if same_feed => Start::Go(buffer.clone(), boundary),
                    _ => Start::Refuse,
                }
            }
            // Gone, or replaced under the same symbol.
            _ => Start::Refuse,
        }
    };
    let (buffered, boundary) = match start {
        Start::Go(buffer, boundary) => (buffer, boundary),
        Start::Refuse => {
            let mut guard = store.lock().unwrap_or_else(|e| e.into_inner());
            refuse(&mut guard, symbol, generation);
            return;
        }
    };
    let cut = buffered.first().map(Event::timestamp_secs).unwrap_or(f64::INFINITY);
    let history_last = history.last().map(Event::timestamp_secs);
    let previous_volume: u128 = history.iter().filter_map(|event| match event {
        Event::Trade(trade, _, _) if trade.timestamp_secs <= boundary.0 && trade.price.is_finite() && trade.price > 0.0 => Some(u128::from(trade.size)),
        _ => None,
    }).sum();
    if previous_volume < u128::from(boundary.1) || !splice_overlap_consistent(&history, &buffered, cut) {
        let mut guard = store.lock().unwrap_or_else(|e| e.into_inner());
        refuse(&mut guard, symbol, generation);
        return;
    }
    let mut engine = OrderFlowEngine::default();
    for event in history.into_iter().filter(|e| e.timestamp_secs() < cut) {
        event.apply(&mut engine);
    }
    for event in &buffered {
        event.apply(&mut engine);
    }
    before_commit();

    let mut guard = store.lock().unwrap_or_else(|e| e.into_inner());
    // Re-checked under the lock: the entry can have been evicted, replaced
    // or failed by ingestion while the replay above ran without it.
    //
    // No events were buffered at the earlier snapshot, so an event that
    // arrived during the replay and overlaps the fetched history makes
    // that boundary ambiguous. Counted here because nothing else has: the
    // buffer is still `Running`, so ingestion did not refuse it.
    let ambiguous = matches!(guard.engines.get(symbol), Some(entry)
        if entry.generation == generation
            && matches!(&entry.backfill, Backfill::Running(buffer)
                if buffered.is_empty()
                    && history_last.is_some_and(|last| buffer.first().is_some_and(|event| event.timestamp_secs() <= last))));
    if ambiguous {
        refuse_splice(&mut guard, symbol, generation, Refusal::Fresh);
        return;
    }
    let committable = matches!(guard.engines.get(symbol), Some(entry)
        if entry.generation == generation && matches!(entry.backfill, Backfill::Running(_)));
    if !committable {
        refuse(&mut guard, symbol, generation);
        return;
    }
    let Some(entry) = guard.engines.get_mut(symbol) else { return };
    let Backfill::Running(buffer) = &entry.backfill else { return };
    for event in &buffer[buffered.len().min(buffer.len())..] {
        event.apply(&mut engine);
    }
    entry.engine = engine;
    entry.backfill = Backfill::Done;
    entry.backfill_boundary = None;
    entry.splice_live_feed = None;
    entry.lite = None;
}

/// A refusal check on the observations supplied by the two paths, NOT
/// provider identity or reconciliation. Equal shapes can hide distinct prints;
/// duplicate trade shapes are therefore refused. The tape remains uncorrected.
///
/// The provider trade id that `TradeFlags::id` now carries is deliberately
/// **not** part of this shape, and the splice is no more permissive than it
/// was before that field existed. Matching on ids would be a deduplication
/// claim -- that two prints with one id are one print, and that prints with
/// different ids are distinct -- which this layer cannot support: ids are
/// absent on some frames, are not guaranteed comparable across the live
/// socket and the REST history, and nothing here reconciles them against an
/// official tape. Cross-source joining is refused by feed equality instead
/// (see `finish_with_hook`), which is a check this code can actually make.
#[derive(Debug, PartialEq, Eq, Hash)]
enum SpliceShape {
    Quote(u64, u64, u64, u64, u64),
    Trade(u64, u64, u64, u8, bool, bool),
}

fn splice_shape(event: &Event) -> SpliceShape {
    match event {
        Event::Quote(quote) => SpliceShape::Quote(quote.timestamp_secs.to_bits(), quote.bid_price.to_bits(), quote.ask_price.to_bits(), quote.bid_size, quote.ask_size),
        Event::Trade(trade, session, flags) => SpliceShape::Trade(trade.timestamp_secs.to_bits(), trade.price.to_bits(), trade.size, *session as u8, flags.unclassifiable, flags.sweep),
    }
}

fn splice_overlap_consistent(history: &[Event], buffered: &[Event], cut: f64) -> bool {
    if history.iter().any(|event| !event.timestamp_secs().is_finite()) || history.windows(2).any(|events| events[0].timestamp_secs() > events[1].timestamp_secs()) { return false; }
    let mut live = std::collections::HashSet::new();
    for event in buffered {
        if !live.insert(splice_shape(event)) && matches!(event, Event::Trade(..)) { return false; }
    }
    let mut tail = std::collections::HashSet::new();
    for event in history.iter().filter(|event| event.timestamp_secs() >= cut) {
        let shape = splice_shape(event);
        if !live.contains(&shape) { return false; }
        if !tail.insert(shape) && matches!(event, Event::Trade(..)) { return false; }
    }
    true
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
        flags_of(&trade.conditions, trade.id),
    )
}

fn record_quote(store: &mut Store, quote: &Quote, received_secs: f64) {
    let at = to_secs(quote.timestamp);
    // Checked before anything global moves: an implausible timestamp must
    // not be able to roll the market day, advance the sweep clock, or
    // extend an entry's idle life.
    if !sane_event_time(at, received_secs) {
        store.health.timestamp_rejected += 1;
        return;
    }
    if !store.roll_to(market_day(quote.timestamp)) { return; }
    if let Some(entry) = store.engines.get_mut(&quote.symbol) {
        entry.last_seen = entry.last_seen.max(at);
    }
    store.sweep(at);
    if !store.engines.contains_key(&quote.symbol) {
        if store.engines.len() >= MAX_ENGINES {
            store.health.capacity_rejected += 1;
            // A symbol turned away now and admitted later has a hole in
            // its day; remembered so `Coverage` cannot call it a session.
            store.note_lost([quote.symbol.clone()]);
            return;
        }
        // Recreated engines start a partial observation history, never a
        // complete session -- `Entry::partial` carries that.
        let entry = store.new_entry(&quote.symbol);
        store.engines.insert(quote.symbol.clone(), entry);
    }
    if let Some(entry) = store.engines.get_mut(&quote.symbol) {
        if entry.apply(quote_event(quote)) { store.health.backfill_discarded += 1; }
    }
}
/// Trades never create an engine; only a quote does (`record_quote`).
/// Quotes arrive solely for symbols this process subscribed to one by one
/// -- funnel-tracked names and tracked movers alike -- so "has had a
/// quote" is exactly "has the data order flow needs", without this module
/// having to know the live loop's several notions of "tracked". It also
/// keeps the full-market trade tape (universe mode) from spawning an
/// engine per symbol in the market.
fn record_trade(store: &mut Store, trade: &Trade, received_secs: f64) {
    let at = to_secs(trade.timestamp);
    if !sane_event_time(at, received_secs) {
        store.health.timestamp_rejected += 1;
        return;
    }
    if !store.roll_to(market_day(trade.timestamp)) { return; }
    if let Some(entry) = store.engines.get_mut(&trade.symbol) {
        entry.last_seen = entry.last_seen.max(at);
    }
    store.sweep(at);
    let Some(entry) = store.engines.get_mut(&trade.symbol) else {
        return;
    };
    let mut event = trade_event(trade);
    if let (Event::Trade(_, _, flags), Some(nanos)) = (&mut event, trade.timestamp.timestamp_nanos_opt()) {
        flags.exchange = take_venue(&mut entry.venues, nanos);
    }
    let discarded = entry.apply(event);
    // Counted for every trade that reached an engine, so a reader can see
    // what share of the tape behind a row was identifiable at all. Only
    // the bounded large prints keep the id itself.
    if trade.id.is_some() {
        store.health.trades_with_provider_id += 1;
    } else {
        store.health.trades_without_provider_id += 1;
    }
    if discarded { store.health.backfill_discarded += 1; }
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
        Trade { symbol: "AAA".into(), price, size, timestamp: t, conditions: conditions.iter().map(|c| c.to_string()).collect(), id: None }
    }

    fn trade_with_id(t: DateTime<Utc>, price: f64, size: u64, id: Option<u64>) -> Trade {
        Trade { id, ..trade(t, price, size, &["@"]) }
    }

    /// The feed every splice test runs with. A splice whose two halves
    /// cannot both name the same feed is refused by design, so a test that
    /// wants a successful splice has to record one.
    const FEED: &str = "sip";

    fn spliceable() -> Mutex<Store> {
        let mut store = Store::default();
        store.source.live_feed = Some(FEED.to_string());
        Mutex::new(store)
    }

    /// Records as if the frame carrying the event arrived the instant the
    /// venue stamped it, which is what every test below means by
    /// "received". `sane_event_time` has its own tests.
    fn rq(store: &mut Store, quote: &Quote) {
        record_quote(store, quote, to_secs(quote.timestamp));
    }

    fn rt(store: &mut Store, trade: &Trade) {
        record_trade(store, trade, to_secs(trade.timestamp));
    }

    #[test]
    fn public_ingestion_gate_checks() {
        if let Ok(mode) = std::env::var("ORDER_FLOW_TEST_CHILD_MODE") {
            let now = at(14, 0, 0, 0);
            on_quote(&quote(now, 1.99, 2.0), now);
            on_trade(&trade(now, 2.0, 10, &["@"]), now);
            note_venues(r#"[{"T":"c","S":"AAA"},{"T":"x","S":"AAA"}]"#, now);
            if mode == "off" {
                assert!(observation_health().is_none());
                assert!(summary("AAA", now).is_none());
                assert!(lite_read("AAA", now).is_none());
                assert!(full_read("AAA", now).is_none());
                assert!(begin_backfill("AAA").is_none());
                fail_backfill("AAA", 1);
                finish_backfill("AAA", 1, Vec::new(), None);
                set_live_feed("sip");
                note_historical_feed("sip");
                assert_eq!(source_identity(), SourceIdentity::default());
                assert!(active_summaries(now, 30.0).is_empty());
                assert!(active_snapshot(now, 30.0).is_none());
                assert!(STORE.get().is_none(), "disabled hooks must not initialize the global store");
            } else {
                let (health, provenance) = observation_health().unwrap();
                assert_eq!(health.engines, 1);
                assert_eq!((health.corrections_seen, health.cancels_seen), (1, 1));
                // Feeds are recorded from configuration when they are set,
                // and stay unknown until something sets them.
                assert_eq!(source_identity(), SourceIdentity::default());
                set_live_feed("sip");
                note_historical_feed("  iex  ");
                assert_eq!(source_identity().live_feed.as_deref(), Some("sip"));
                assert_eq!(source_identity().historical_feed.as_deref(), Some("iex"));
                assert!(matches!(provenance, TapeProvenance::CorrectionsPending));
                assert!(summary("AAA", now).is_some());
                // A poll/push read discloses the same uncorrected-tape
                // evidence, carries no chart arrays, and does not claim a
                // session it has not replayed.
                let read = lite_read("AAA", now).unwrap();
                assert_eq!(read.provenance, TapeProvenance::CorrectionsPending);
                assert_eq!((read.corrections_seen, read.cancels_seen), (1, 1));
                assert_eq!(read.coverage, Coverage::Partial);
                assert!(read.summary.delta_buckets.is_empty() && read.summary.large_prints.is_empty());
                assert!(full_read("AAA", now).unwrap().summary.profile.unwrap().bins.len() > 0);
                // A recording snapshot sees the same symbol with the same
                // disclosure, from one lock acquisition.
                let snapshot = active_snapshot(now, 30.0).unwrap();
                assert_eq!(snapshot.market_day, Some(market_day(now)));
                assert_eq!(snapshot.provenance, TapeProvenance::CorrectionsPending);
                assert_eq!(snapshot.rows.len(), 1);
                assert_eq!(snapshot.rows[0].coverage, Coverage::Partial);
                assert!(snapshot.rows[0].generation > 0);
                for i in 0..MAX_ENGINES {
                    let mut q = quote(now, 1.99, 2.0);
                    q.symbol = format!("S{i}");
                    on_quote(&q, now);
                }
                let (health, _) = observation_health().unwrap();
                assert_eq!(health.engines, MAX_ENGINES);
                assert_eq!(health.capacity_rejected, 1);
                on_trade(&trade(now - chrono::Duration::days(1), 2.0, 10, &["@"]), now - chrono::Duration::days(1));
                assert_eq!(observation_health().unwrap().0.late_discarded, 1);
                // An implausible feed timestamp is refused outright: the
                // day does not roll and no engine is touched.
                on_quote(&quote(now + chrono::Duration::days(3), 1.99, 2.0), now);
                let (health, _) = observation_health().unwrap();
                assert_eq!(health.timestamp_rejected, 1);
                assert_eq!(health.engines, MAX_ENGINES);
                let later = now + chrono::Duration::seconds(IDLE_SECS as i64 + 1);
                let mut q = quote(later, 1.99, 2.0);
                q.symbol = "NEW".into();
                on_quote(&q, later);
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
        rq(&mut store,&quote(current, 1.99, 2.0));
        rt(&mut store,&trade(current - chrono::Duration::days(1), 2.0, 10, &["@"]));
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
            rq(&mut store,&q);
        }
        rq(&mut store,&quote(now, 1.99, 2.0));
        assert_eq!(store.engines.len(), MAX_ENGINES);
        assert_eq!(store.health.capacity_rejected, 1);
        assert_eq!(store.health.evicted, 0);
        assert!(!store.engines.contains_key("AAA"));
    }

    #[test]
    fn idle_eviction_allows_only_a_new_partial_engine() {
        let mut store = Store::default();
        let now = at(14, 0, 0, 0);
        rq(&mut store,&quote(now, 1.99, 2.0));
        rt(&mut store,&trade(now, 2.0, 10, &["@"]));
        let later = now + chrono::Duration::seconds(IDLE_SECS as i64 + 1);
        let mut other = quote(later, 1.99, 2.0);
        other.symbol = "BBB".into();
        rq(&mut store,&other);
        assert!(!store.engines.contains_key("AAA"));
        rq(&mut store,&quote(later, 1.99, 2.0));
        assert_eq!(store.health.evicted, 1);
        let summary = store.engines["AAA"].engine.summary(to_secs(later));
        assert_eq!(summary.cumulative_delta_regular, 0);
    }

    #[test]
    fn correction_and_cancel_frames_are_counted_without_creating_engines() {
        let mut store = Store::default();
        note_venues_in(&mut store, r#"[{"T":"c","S":"AAA"},{"T":"x","S":"AAA"}]"#, to_secs(at(14, 0, 0, 0)));
        assert_eq!(store.health.corrections_seen, 1);
        assert_eq!(store.health.cancels_seen, 1);
        assert!(store.engines.is_empty());
    }

    #[test]
    fn the_exchange_is_read_from_the_raw_frame_and_reaches_the_sweep() {
        let mut store = Store::default();
        rq(&mut store,&quote(at(14, 0, 0, 0), 1.99, 2.00));
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
        note_venues_in(&mut store, frame, to_secs(at(14, 0, 0, 400)));
        for (millis, price) in [(100, 2.00), (200, 2.00)] {
            rt(&mut store,&trade(at(14, 0, 0, millis), price, 500, &["@"]));
        }
        rq(&mut store,&quote(at(14, 0, 0, 250), 2.00, 2.01));
        rt(&mut store,&trade(at(14, 0, 0, 300), 2.01, 500, &["@"]));
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
        let received = to_secs(at(14, 0, 0, 0));
        rq(&mut store,&quote(at(14, 0, 0, 0), 1.99, 2.00));
        note_venues_in(&mut store, "not json", received);
        note_venues_in(&mut store, r#"[{"T":"success","msg":"connected"}]"#, received);
        note_venues_in(&mut store, r#"[{"T":"t","S":"AAA","x":"QQ","t":"2026-10-06T14:00:00Z"},{"T":"t","S":"AAA","x":"Q","t":"soon"}]"#, received);
        assert!(store.engines["AAA"].venues.is_empty());
    }

    #[test]
    fn a_venue_note_from_the_future_cannot_block_the_queue_behind_it() {
        let mut store = Store::default();
        let received = to_secs(at(14, 0, 0, 0));
        rq(&mut store, &quote(at(14, 0, 0, 0), 1.99, 2.00));
        // `take_venue` matches by exact timestamp and only discards OLDER
        // notes, so a note stamped days out would sit at the front for the
        // next 4096 notes and give every real trade no venue at all.
        let frame = r#"[
            {"T":"t","S":"AAA","x":"Q","t":"2026-10-09T14:00:00Z"},
            {"T":"t","S":"AAA","x":"P","t":"2026-10-06T14:00:00.1Z"}
        ]"#;
        note_venues_in(&mut store, frame, received);
        assert_eq!(store.health.timestamp_rejected, 1);
        let notes: Vec<char> = store.engines["AAA"].venues.iter().map(|v| v.1).collect();
        assert_eq!(notes, vec!['P']);
        let nanos = at(14, 0, 0, 100).timestamp_nanos_opt().unwrap();
        assert_eq!(take_venue(&mut store.engines.get_mut("AAA").unwrap().venues, nanos), Some('P'));
    }

    #[test]
    fn sub_second_ordering_survives_the_conversion() {
        // A quote 400ms BEFORE the trade must classify it; with whole
        // seconds both would read as the same instant and still work, but
        // a quote 400ms AFTER must not, which only fractions can tell.
        let mut store = Store::default();
        rq(&mut store,&quote(at(14, 0, 0, 100), 2.00, 2.02));
        rt(&mut store,&trade(at(14, 0, 0, 500), 2.02, 300, &["@"]));
        rq(&mut store,&quote(at(14, 0, 1, 400), 2.10, 2.12));
        rt(&mut store,&trade(at(14, 0, 1, 0), 2.02, 200, &["@"]));
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
        rq(&mut store,&quote(at(14, 0, 0, 0), 2.00, 2.02));
        rt(&mut store,&trade(at(14, 0, 0, 100), 2.02, 9_000, &["@", "5", "X"]));
        rt(&mut store,&trade(at(14, 0, 0, 200), 2.02, 400, &["@", "I"]));
        let s = store.engines["AAA"].engine.summary(to_secs(at(14, 0, 1, 0)));
        let w = s.windows[0];
        assert_eq!((w.buy_volume, w.unknown_volume, w.shares), (400, 9_000, 9_400));
    }

    #[test]
    fn bunched_prints_stay_classified_and_sweep_flags_are_tallied() {
        assert!(!flags_of(&["B".to_string()], None).unclassifiable);
        let mut store = Store::default();
        rq(&mut store,&quote(at(14, 0, 0, 0), 2.00, 2.02));
        rt(&mut store,&trade(at(14, 0, 0, 100), 2.02, 700, &["@", "F"]));
        let s = store.engines["AAA"].engine.summary(to_secs(at(14, 0, 1, 0)));
        assert_eq!(s.sweeps.flagged_buy_volume, 700);
        assert_eq!(s.large_prints.iter().filter(|p| p.side == TradeSide::Buy).count(), 0);
    }

    #[test]
    fn cumulative_delta_is_bucketed_by_real_session() {
        let mut store = Store::default();
        rq(&mut store,&quote(at(12, 0, 0, 0), 2.00, 2.02));
        rt(&mut store,&trade(at(12, 0, 0, 100), 2.02, 300, &["@", "T"]));
        rq(&mut store,&quote(at(14, 0, 0, 0), 2.00, 2.02));
        rt(&mut store,&trade(at(14, 0, 0, 100), 2.00, 100, &["@"]));
        let s = store.engines["AAA"].engine.summary(to_secs(at(14, 0, 1, 0)));
        assert_eq!(s.cumulative_delta_premarket, 300);
        assert_eq!(s.cumulative_delta_regular, -100);
    }

    #[test]
    fn a_new_market_day_starts_from_nothing() {
        let mut store = Store::default();
        rq(&mut store,&quote(at(14, 0, 0, 0), 2.00, 2.02));
        assert_eq!(store.engines.len(), 1);
        let next_day = at(14, 0, 0, 0) + chrono::Duration::days(1);
        let mut other = quote(next_day, 5.00, 5.02);
        other.symbol = "BBB".into();
        rq(&mut store,&other);
        assert!(store.engines.contains_key("BBB") && !store.engines.contains_key("AAA"));
    }

    #[test]
    fn active_summaries_skip_quiet_symbols_and_come_back_sorted() {
        let mut store = Store::default();
        for (symbol, trade_at) in [("ZZZ", at(14, 0, 50, 0)), ("AAA", at(14, 0, 55, 0)), ("OLD", at(13, 0, 0, 0))] {
            let mut q = quote(trade_at - chrono::Duration::milliseconds(100), 2.00, 2.02);
            q.symbol = symbol.into();
            rq(&mut store,&q);
            let mut t = trade(trade_at, 2.02, 100, &["@"]);
            t.symbol = symbol.into();
            rt(&mut store,&t);
        }
        // A symbol with a quote but no trade yet has nothing to record.
        let mut silent = quote(at(14, 0, 58, 0), 9.00, 9.02);
        silent.symbol = "NEW".into();
        rq(&mut store,&silent);

        let rows = active_in(&store, at(14, 1, 0, 0), 30.0);
        assert_eq!(rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), vec!["AAA", "ZZZ"]);
    }

    #[test]
    fn a_snapshot_carries_each_rows_own_coverage_and_the_shared_context() {
        let mut store = Store::default();
        let now = at(14, 0, 0, 0);
        // AAA: an ordinary engine with a failed backfill.
        rq(&mut store, &quote(now, 2.00, 2.02));
        rt(&mut store, &trade(at(14, 0, 0, 100), 2.02, 100, &["@"]));
        store.engines.get_mut("AAA").unwrap().backfill = Backfill::Failed;
        // BBB: an engine this symbol has already lost once today, so no
        // backfill state can make its day whole again.
        let mut q = quote(at(14, 0, 0, 200), 3.00, 3.02);
        q.symbol = "BBB".into();
        rq(&mut store, &q);
        let mut t = trade(at(14, 0, 0, 300), 3.02, 50, &["@"]);
        t.symbol = "BBB".into();
        rt(&mut store, &t);
        store.engines.get_mut("BBB").unwrap().partial = true;
        store.engines.get_mut("BBB").unwrap().backfill = Backfill::Done;
        // Correction evidence is process-wide, not per symbol.
        note_venues_in(&mut store, r#"[{"T":"c","S":"AAA"},{"T":"x","S":"ZZZ"}]"#, to_secs(at(14, 0, 0, 400)));

        let snapshot = snapshot_in(&store, at(14, 0, 10, 0), 30.0);
        assert_eq!(snapshot.market_day, Some(market_day(now)));
        assert_eq!(snapshot.provenance, TapeProvenance::CorrectionsPending);
        assert_eq!((snapshot.health.corrections_seen, snapshot.health.cancels_seen), (1, 1));
        // `engines` is the live count, which `ObservationHealth::default`
        // does not carry: it has to be filled in from the map.
        assert_eq!(snapshot.health.engines, 2);

        let rows = &snapshot.rows;
        assert_eq!(rows.iter().map(|r| r.symbol.as_str()).collect::<Vec<_>>(), vec!["AAA", "BBB"]);
        assert_eq!((rows[0].coverage, rows[0].backfill, rows[0].partial), (Coverage::Partial, BackfillStatus::Failed, false));
        // Done backfill, but the symbol lost an engine today: still partial.
        assert_eq!((rows[1].coverage, rows[1].backfill, rows[1].partial), (Coverage::Partial, BackfillStatus::Done, true));
        assert_ne!(rows[0].generation, rows[1].generation);
        // The same stripping `active_summaries` does, and the same numbers.
        assert!(rows[0].summary.delta_buckets.is_empty() && rows[0].summary.large_prints.is_empty());
        assert!(rows[0].summary.profile.as_ref().unwrap().bins.is_empty());
        assert_eq!(rows[0].summary.profile.as_ref().unwrap().total_volume, 100);
        assert_eq!(rows[1].summary.profile.as_ref().unwrap().total_volume, 50);
    }

    #[test]
    fn a_snapshot_reports_the_engines_market_day_not_the_callers_clock() {
        // The store has no day at all before anything has arrived, which is
        // reported as such rather than as the caller's date.
        let mut store = Store::default();
        assert_eq!(snapshot_in(&store, at(14, 0, 0, 0), 30.0).market_day, None);

        // A session recorded after the 04:00 ET roll, asked for under the
        // following market day's clock: the rows are still the earlier
        // day's, and the snapshot says so.
        rq(&mut store, &quote(at(14, 0, 0, 0), 2.00, 2.02));
        rt(&mut store, &trade(at(14, 0, 0, 100), 2.02, 100, &["@"]));
        let yesterday = market_day(at(14, 0, 0, 0));
        let after_roll = at(14, 0, 10, 0) + chrono::Duration::days(1);
        assert_ne!(market_day(after_roll), yesterday);
        assert_eq!(snapshot_in(&store, after_roll, f64::INFINITY).market_day, Some(yesterday));
    }

    #[test]
    fn equal_or_backwards_buffer_times_refuse_history_without_losing_live_volume() {
        for first in [at(14, 0, 10, 0), at(14, 0, 9, 999)] {
            let store = spliceable();
            let generation = {
                let mut s = store.lock().unwrap();
                rq(&mut s, &quote(at(14, 0, 9, 0), 2.00, 2.02));
                rt(&mut s, &trade(at(14, 0, 10, 0), 2.02, 100, &["@"]));
                let generation = begin_in(&mut s, "AAA").unwrap();
                record_trade(&mut s, &trade(first, 2.02, 50, &["@"]), to_secs(at(14, 0, 10, 30)));
                generation
            };
            // Ingestion refused the attempt the moment the crossing event
            // arrived, and counted it there.
            assert_eq!(store.lock().unwrap().health.backfill_discarded, 1);
            finish_in(&store, "AAA", generation, Vec::new(), Some(FEED));
            let s = store.lock().unwrap();
            assert_eq!(status_in(&s, "AAA"), BackfillStatus::Failed);
            assert_eq!(s.engines["AAA"].engine.summary(to_secs(at(14, 0, 11, 0))).profile.unwrap().total_volume, 150);
            // History landing afterwards must not count the SAME abandoned
            // attempt a second time.
            assert_eq!(s.health.backfill_discarded, 1);
        }
    }

    #[test]
    fn missing_prior_history_or_unrepresented_overlap_keeps_the_live_engine() {
        for missing_prior in [true, false] {
            let store = spliceable();
            let generation = {
                let mut s = store.lock().unwrap();
                rq(&mut s, &quote(at(14, 0, 9, 0), 2.00, 2.02));
                rt(&mut s, &trade(at(14, 0, 10, 0), 2.02, 100, &["@"]));
                let generation = begin_in(&mut s, "AAA").unwrap();
                rt(&mut s, &trade(at(14, 0, 20, 0), 2.02, 50, &["@"]));
                generation
            };
            let history = if missing_prior { Vec::new() } else {
                history_events(&[trade(at(14, 0, 10, 0), 2.02, 100, &["@"]), trade(at(14, 0, 20, 0), 2.02, 999, &["@"])], &[])
            };
            finish_in(&store, "AAA", generation, history, Some(FEED));
            let s = store.lock().unwrap();
            assert_eq!(status_in(&s, "AAA"), BackfillStatus::Failed);
            assert_eq!(coverage_of(s.engines["AAA"].partial, status_in(&s, "AAA")), Coverage::Partial);
            assert_eq!(s.engines["AAA"].engine.summary(to_secs(at(14, 0, 21, 0))).profile.unwrap().total_volume, 150);
            assert_eq!(s.health.backfill_discarded, 1);
        }
    }

    #[test]
    fn an_overlapping_arrival_during_replay_is_not_counted_twice() {
        let store = spliceable();
        let generation = {
            let mut s = store.lock().unwrap();
            rq(&mut s, &quote(at(14, 0, 9, 0), 2.00, 2.02));
            rt(&mut s, &trade(at(14, 0, 10, 0), 2.02, 100, &["@"]));
            begin_in(&mut s, "AAA").unwrap()
        };
        let history = history_events(&[trade(at(14, 0, 10, 0), 2.02, 100, &["@"]), trade(at(14, 0, 20, 0), 2.02, 50, &["@"])], &[]);
        finish_with_hook(&store, "AAA", generation, history, Some(FEED), || {
            let mut s = store.lock().unwrap();
            rt(&mut s, &trade(at(14, 0, 20, 0), 2.02, 50, &["@"]));
        });
        let s = store.lock().unwrap();
        assert_eq!(status_in(&s, "AAA"), BackfillStatus::Failed);
        assert_eq!(s.engines["AAA"].engine.summary(to_secs(at(14, 0, 21, 0))).profile.unwrap().total_volume, 150);
        assert_eq!(s.health.backfill_discarded, 1);
    }

    #[test]
    fn duplicate_trade_shapes_do_not_claim_provider_identity() {
        let trade = trade_event(&trade(at(14, 0, 20, 0), 2.02, 50, &["@"]));
        assert!(!splice_overlap_consistent(&[trade], &[trade, trade], trade.timestamp_secs()));
        assert!(!splice_overlap_consistent(&[trade, trade], &[trade], trade.timestamp_secs()));
    }

    #[test]
    fn a_delayed_quote_must_not_drop_trades_observed_before_backfill() {
        let store = spliceable();
        let generation = {
            let mut s = store.lock().unwrap();
            rq(&mut s, &quote(at(14, 0, 9, 0), 2.00, 2.02));
            rt(&mut s, &trade(at(14, 0, 10, 10), 2.02, 100, &["@"]));
            let generation = begin_in(&mut s, "AAA").unwrap();
            // A quote with an earlier event timestamp arrives after the
            // trade and after the HTTP request started. This is plausible
            // inside the receipt window; it must not erase the trade.
            record_quote(&mut s, &quote(at(14, 0, 10, 0), 2.00, 2.02), to_secs(at(14, 0, 10, 30)));
            generation
        };
        let history = history_events(
            &[trade(at(13, 31, 0, 0), 2.02, 1_000, &["@"]), trade(at(14, 0, 10, 10), 2.02, 100, &["@"])],
            &[quote(at(13, 30, 59, 0), 2.00, 2.02), quote(at(14, 0, 10, 0), 2.00, 2.02)],
        );
        finish_in(&store, "AAA", generation, history, Some(FEED));
        let s = store.lock().unwrap();
        let summary = s.engines["AAA"].engine.summary(to_secs(at(14, 0, 11, 0)));
        // The quote's own timestamp is at or before the captured boundary,
        // so the splice is refused and the live engine is kept whole. This
        // is pinned to the branch actually taken rather than accepting
        // either: "no volume is lost either way" would also pass if the
        // refusal silently stopped happening.
        assert_eq!(status_in(&s, "AAA"), BackfillStatus::Failed);
        assert_eq!(summary.profile.unwrap().total_volume, 100, "the trade observed before the backfill must survive the refusal");
        assert_eq!(s.health.backfill_discarded, 1);
    }

    #[test]
    fn backfill_stitches_history_and_live_without_a_gap_or_a_double_count() {
        let store = spliceable();
        let generation = {
            let mut s = store.lock().unwrap();
            rq(&mut s, &quote(at(14, 0, 9, 0), 2.00, 2.02));
            rt(&mut s, &trade(at(14, 0, 10, 0), 2.02, 100, &["@"]));
            let generation = begin_in(&mut s, "AAA").unwrap();
            // Only one backfill per symbol per day.
            assert!(begin_in(&mut s, "AAA").is_none());
            assert_eq!(status_in(&s, "AAA"), BackfillStatus::Running);
            assert_eq!(coverage_of(false, status_in(&s, "AAA")), Coverage::SessionPending);
            // Arrives while the history request is in flight.
            rt(&mut s, &trade(at(14, 0, 20, 0), 2.02, 300, &["@"]));
            generation
        };
        // What the provider returns overlaps both live trades.
        let history = history_events(
            &[
                trade(at(13, 31, 0, 0), 2.00, 1_000, &["@"]),
                trade(at(14, 0, 10, 0), 2.02, 100, &["@"]),
                trade(at(14, 0, 20, 0), 2.02, 300, &["@"]),
            ],
            &[quote(at(13, 30, 59, 0), 1.98, 2.00), quote(at(14, 0, 9, 0), 2.00, 2.02)],
        );
        finish_in(&store, "AAA", generation, history, Some(FEED));

        let s = store.lock().unwrap();
        assert_eq!(status_in(&s, "AAA"), BackfillStatus::Done);
        assert_eq!(coverage_of(s.engines["AAA"].partial, status_in(&s, "AAA")), Coverage::Session);
        let summary = s.engines["AAA"].engine.summary(to_secs(at(14, 0, 21, 0)));
        // 1,000 from before tracking began, 100 seen live before the
        // backfill started (taken from history), 300 seen live during it
        // (taken from the buffer). Each exactly once.
        assert_eq!(summary.cumulative_delta_regular, 1_400);
        assert_eq!(summary.profile.unwrap().total_volume, 1_400);
    }

    #[test]
    fn backfill_is_a_no_op_for_unknown_symbols_and_after_failure() {
        let store = spliceable();
        assert!(begin_in(&mut store.lock().unwrap(), "NOPE").is_none());
        finish_in(&store, "NOPE", 1, Vec::new(), Some(FEED));
        let generation = {
            let mut s = store.lock().unwrap();
            rq(&mut s, &quote(at(14, 0, 9, 0), 2.00, 2.02));
            rt(&mut s, &trade(at(14, 0, 10, 0), 2.02, 100, &["@"]));
            assert_eq!(status_in(&s, "AAA"), BackfillStatus::None);
            let generation = begin_in(&mut s, "AAA").unwrap();
            s.engines.get_mut("AAA").unwrap().backfill = Backfill::Failed;
            generation
        };
        // History arriving after the backfill was abandoned changes nothing.
        finish_in(&store, "AAA", generation, history_events(&[trade(at(13, 31, 0, 0), 2.00, 9_999, &["@"])], &[]), Some(FEED));
        let s = store.lock().unwrap();
        assert_eq!(status_in(&s, "AAA"), BackfillStatus::Failed);
        assert_eq!(coverage_of(false, status_in(&s, "AAA")), Coverage::Partial);
        assert_eq!(s.engines["AAA"].engine.summary(to_secs(at(14, 0, 11, 0))).profile.unwrap().total_volume, 100);
    }

    #[test]
    fn history_for_a_replaced_engine_is_discarded_instead_of_invented() {
        let store = spliceable();
        let generation = {
            let mut s = store.lock().unwrap();
            rq(&mut s, &quote(at(14, 0, 9, 0), 2.00, 2.02));
            rt(&mut s, &trade(at(14, 0, 10, 0), 2.02, 100, &["@"]));
            begin_in(&mut s, "AAA").unwrap()
        };
        // Idle long enough to be swept, then quoted again: same symbol,
        // a different engine, and the history in flight does not describe it.
        let later = at(14, 0, 10, 0) + chrono::Duration::seconds(IDLE_SECS as i64 + 1);
        {
            let mut s = store.lock().unwrap();
            // Another symbol's quote is what runs the sweep; AAA's own
            // would refresh it before the scan and spare it.
            let mut other = quote(later, 3.00, 3.02);
            other.symbol = "BBB".into();
            rq(&mut s, &other);
            assert!(!s.engines.contains_key("AAA"));
            rq(&mut s, &quote(later, 3.00, 3.02));
            rt(&mut s, &trade(later + chrono::Duration::milliseconds(100), 3.02, 50, &["@"]));
            assert_ne!(s.engines["AAA"].generation, generation);
        }
        finish_in(&store, "AAA", generation, history_events(&[trade(at(13, 31, 0, 0), 2.00, 9_999, &["@"])], &[]), Some(FEED));
        let s = store.lock().unwrap();
        // The new engine holds only its own 50 shares, and the recreated
        // entry can never claim a whole session again today.
        assert_eq!(s.engines["AAA"].engine.summary(to_secs(later + chrono::Duration::seconds(1))).profile.unwrap().total_volume, 50);
        assert_eq!(status_in(&s, "AAA"), BackfillStatus::None);
        assert!(s.engines["AAA"].partial);
        assert_eq!(coverage_of(s.engines["AAA"].partial, BackfillStatus::Done), Coverage::Partial);
        // The abandoned attempt is reported once, even though the entry it
        // belonged to is gone.
        assert_eq!(s.health.backfill_discarded, 1);
    }

    #[test]
    fn history_past_the_ceiling_is_refused_rather_than_replayed() {
        let store = spliceable();
        let generation = {
            let mut s = store.lock().unwrap();
            rq(&mut s, &quote(at(14, 0, 9, 0), 2.00, 2.02));
            rt(&mut s, &trade(at(14, 0, 10, 0), 2.02, 100, &["@"]));
            begin_in(&mut s, "AAA").unwrap()
        };
        let oversized = vec![Event::Quote(order_flow::Quote { timestamp_secs: 1.0, bid_price: 1.0, bid_size: 1, ask_price: 1.1, ask_size: 1 }); MAX_HISTORY_EVENTS + 1];
        finish_in(&store, "AAA", generation, oversized, Some(FEED));
        let s = store.lock().unwrap();
        assert_eq!(status_in(&s, "AAA"), BackfillStatus::Failed);
        assert_eq!(s.health.backfill_discarded, 1);
        assert_eq!(s.engines["AAA"].engine.summary(to_secs(at(14, 0, 11, 0))).profile.unwrap().total_volume, 100);
    }

    /// Sets up a symbol with one live trade and returns history that would
    /// splice cleanly, so a test can vary only the thing it is about.
    fn spliceable_history() -> Vec<Event> {
        history_events(
            &[trade(at(13, 31, 0, 0), 2.00, 1_000, &["@"]), trade(at(14, 0, 10, 0), 2.02, 100, &["@"])],
            &[],
        )
    }

    fn begun(store: &Mutex<Store>) -> u64 {
        let mut s = store.lock().unwrap();
        rq(&mut s, &quote(at(14, 0, 9, 0), 2.00, 2.02));
        rt(&mut s, &trade(at(14, 0, 10, 0), 2.02, 100, &["@"]));
        begin_in(&mut s, "AAA").unwrap()
    }

    #[test]
    fn history_from_another_feed_or_an_unnamed_one_is_refused_and_counted_once() {
        // The live segment was observed on `sip`. `iex` is a single venue
        // and would make the pre-cut volume describe a different tape than
        // the post-cut volume; `None` is a caller that cannot say, which
        // is not evidence of a match. Both refuse, and the history that is
        // varied here would otherwise splice cleanly.
        for historical in [Some("iex"), None] {
            let store = spliceable();
            let generation = begun(&store);
            finish_in(&store, "AAA", generation, spliceable_history(), historical);
            let s = store.lock().unwrap();
            assert_eq!(status_in(&s, "AAA"), BackfillStatus::Failed, "{historical:?}");
            assert_eq!(coverage_of(s.engines["AAA"].partial, status_in(&s, "AAA")), Coverage::Partial, "{historical:?}");
            // Only the history is lost; every observed share survives.
            assert_eq!(s.engines["AAA"].engine.summary(to_secs(at(14, 0, 11, 0))).profile.unwrap().total_volume, 100, "{historical:?}");
            assert_eq!(s.health.backfill_discarded, 1, "{historical:?}");
        }
    }

    #[test]
    fn a_live_segment_with_no_recorded_feed_refuses_rather_than_assuming_one() {
        // Nothing called `set_live_feed`, so this process cannot say what
        // tape it is watching -- and must not infer one from the request.
        let store = Mutex::new(Store::default());
        let generation = begun(&store);
        finish_in(&store, "AAA", generation, spliceable_history(), Some(FEED));
        let s = store.lock().unwrap();
        assert_eq!(status_in(&s, "AAA"), BackfillStatus::Failed);
        assert_eq!(s.health.backfill_discarded, 1);
    }

    #[test]
    fn a_fetch_that_never_produced_history_counts_one_discarded_backfill() {
        let store = spliceable();
        let generation = begun(&store);
        fail_in(&mut store.lock().unwrap(), "AAA", generation);
        let s = store.lock().unwrap();
        assert_eq!(status_in(&s, "AAA"), BackfillStatus::Failed);
        assert_eq!(s.health.backfill_discarded, 1);
        // And a second report of the same failure does not count again.
        drop(s);
        fail_in(&mut store.lock().unwrap(), "AAA", generation);
        assert_eq!(store.lock().unwrap().health.backfill_discarded, 1);
    }

    #[test]
    fn a_replaced_engine_counts_exactly_one_discarded_backfill() {
        let store = spliceable();
        let generation = begun(&store);
        {
            let mut s = store.lock().unwrap();
            // Force the entry to be a different engine under the same key.
            s.engines.get_mut("AAA").unwrap().generation = generation + 1;
        }
        finish_in(&store, "AAA", generation, spliceable_history(), Some(FEED));
        let s = store.lock().unwrap();
        assert_eq!(s.health.backfill_discarded, 1);
        // The engine that is actually there was not touched.
        assert_eq!(status_in(&s, "AAA"), BackfillStatus::Running);
    }

    #[test]
    fn completions_with_nothing_in_flight_count_no_discarded_backfill() {
        let store = spliceable();
        // A generation that was never issued: nothing was ever begun, so
        // there is no attempt to abandon and nothing to report.
        finish_in(&store, "NOPE", 1, Vec::new(), Some(FEED));
        fail_in(&mut store.lock().unwrap(), "NOPE", 1);
        assert_eq!(store.lock().unwrap().health.backfill_discarded, 0);

        let generation = {
            let mut s = store.lock().unwrap();
            rq(&mut s, &quote(at(14, 0, 9, 0), 2.00, 2.02));
            rt(&mut s, &trade(at(14, 0, 10, 0), 2.02, 100, &["@"]));
            s.engines["AAA"].generation
        };
        // `NotStarted`: this engine exists but no backfill was begun on it.
        finish_in(&store, "AAA", generation, Vec::new(), Some(FEED));
        assert_eq!(store.lock().unwrap().health.backfill_discarded, 0);
        assert_eq!(status_in(&store.lock().unwrap(), "AAA"), BackfillStatus::None);

        // A splice that succeeded, then a duplicate completion: it must
        // neither count a discard nor downgrade `Done` to `Failed`.
        let generation = begin_in(&mut store.lock().unwrap(), "AAA").unwrap();
        finish_in(&store, "AAA", generation, spliceable_history(), Some(FEED));
        assert_eq!(status_in(&store.lock().unwrap(), "AAA"), BackfillStatus::Done);
        finish_in(&store, "AAA", generation, spliceable_history(), Some(FEED));
        let s = store.lock().unwrap();
        assert_eq!(status_in(&s, "AAA"), BackfillStatus::Done, "a duplicate completion must not fail a finished backfill");
        assert_eq!(s.health.backfill_discarded, 0);
    }

    #[test]
    fn a_provider_trade_id_reaches_the_bounded_large_prints_and_is_never_invented() {
        let store = spliceable();
        let mut s = store.lock().unwrap();
        rq(&mut s, &quote(at(14, 0, 0, 0), 2.00, 2.02));
        // Fifty ordinary prints: the detector has no threshold until it
        // has seen this many, so none of these is large.
        for i in 0..50u32 {
            rt(&mut s, &trade_with_id(at(14, 0, 1, i), 2.02, 100, Some(1_000 + u64::from(i))));
        }
        // One print far above that distribution, carrying its own id, and
        // one that arrived without an id at all.
        rt(&mut s, &trade_with_id(at(14, 0, 2, 0), 2.02, 50_000, Some(777)));
        rt(&mut s, &trade_with_id(at(14, 0, 3, 0), 2.02, 60_000, None));
        let summary = s.engines["AAA"].engine.summary(to_secs(at(14, 0, 4, 0)));
        // Passed straight through, and an absent id stays absent: nothing
        // here fabricates identity for a print that arrived without one.
        assert_eq!(summary.large_prints.iter().map(|print| print.id).collect::<Vec<_>>(), vec![Some(777), None]);
        // The counters describe the whole ingested tape, not just the
        // prints whose ids were kept.
        assert_eq!((s.health.trades_with_provider_id, s.health.trades_without_provider_id), (51, 1));
    }

    #[test]
    fn recorded_rows_carry_no_per_print_ids_while_the_full_read_does() {
        // Pins the limit rather than only documenting it: provider ids
        // reach the full HTTP shape, and are stripped from every recorded
        // row along with the rest of the chart-only arrays. A recording
        // therefore cannot name the prints behind its aggregates, and must
        // not be read as though it could. Putting them in rows is a
        // materially larger recording scope than the byte ceilings allow.
        let store = spliceable();
        let mut s = store.lock().unwrap();
        rq(&mut s, &quote(at(14, 0, 0, 0), 2.00, 2.02));
        for i in 0..50u32 {
            rt(&mut s, &trade_with_id(at(14, 0, 1, i), 2.02, 100, Some(1_000 + u64::from(i))));
        }
        rt(&mut s, &trade_with_id(at(14, 0, 2, 0), 2.02, 50_000, Some(777)));
        // What the full read serves still has the id.
        assert_eq!(
            s.engines["AAA"].engine.summary(to_secs(at(14, 0, 3, 0))).large_prints.iter().map(|print| print.id).collect::<Vec<_>>(),
            vec![Some(777)],
        );
        // The recorded row does not carry the print at all.
        let snapshot = snapshot_in(&s, at(14, 0, 3, 0), f64::INFINITY);
        assert!(snapshot.rows[0].summary.large_prints.is_empty());
        // What it carries instead is how much of the tape was identifiable.
        assert_eq!((snapshot.health.trades_with_provider_id, snapshot.health.trades_without_provider_id), (51, 0));
    }

    #[test]
    fn a_snapshot_names_the_feeds_it_was_configured_with_and_guesses_neither() {
        let store = spliceable();
        {
            let mut s = store.lock().unwrap();
            rq(&mut s, &quote(at(14, 0, 0, 0), 2.00, 2.02));
            rt(&mut s, &trade(at(14, 0, 1, 0), 2.02, 100, &["@"]));
        }
        let snapshot = snapshot_in(&store.lock().unwrap(), at(14, 0, 2, 0), f64::INFINITY);
        assert_eq!(snapshot.source.live_feed.as_deref(), Some(FEED));
        // No history was ever fetched, so there is no historical feed to
        // name -- and `None` must not be filled in with the live one.
        assert_eq!(snapshot.source.historical_feed, None);
    }

    #[test]
    fn a_blank_feed_is_never_told_rather_than_an_empty_feed_name() {
        assert_eq!(non_empty("sip").as_deref(), Some("sip"));
        assert_eq!(non_empty("  sip  ").as_deref(), Some("sip"));
        for blank in ["", "   ", "\t"] {
            assert_eq!(non_empty(blank), None, "{blank:?}");
        }
    }

    #[test]
    fn an_implausible_timestamp_cannot_roll_the_day_or_reach_an_engine() {
        let now = at(14, 0, 0, 0);
        let received = to_secs(now);
        assert!(sane_event_time(received, received));
        // Ordinary socket lag is kept.
        assert!(sane_event_time(received - 60.0, received));
        assert!(sane_event_time(received + MAX_AHEAD_OF_RECEIPT_SECS, received));
        // A feed timestamp from the future, a retro-dated one, and NaN.
        assert!(!sane_event_time(received + MAX_AHEAD_OF_RECEIPT_SECS + 1.0, received));
        assert!(!sane_event_time(received - MAX_BEHIND_RECEIPT_SECS - 1.0, received));
        assert!(!sane_event_time(f64::NAN, received));
        assert!(!sane_event_time(received, f64::INFINITY));

        let mut store = Store::default();
        rq(&mut store, &quote(now, 2.00, 2.02));
        rt(&mut store, &trade(now + chrono::Duration::milliseconds(100), 2.02, 100, &["@"]));
        let day = store.day;
        // Stamped three days out, received now: refused before roll_to.
        let mut future = quote(now + chrono::Duration::days(3), 9.00, 9.02);
        future.symbol = "BBB".into();
        record_quote(&mut store, &future, received);
        // Stamped now, "received" three days ago: the other direction.
        record_trade(&mut store, &trade(now + chrono::Duration::milliseconds(200), 9.99, 9_999, &["@"]), received - 3.0 * 86_400.0);
        assert_eq!(store.health.timestamp_rejected, 2);
        assert_eq!(store.day, day);
        assert!(!store.engines.contains_key("BBB"));
        assert_eq!(store.engines["AAA"].engine.summary(to_secs(now) + 1.0).profile.unwrap().total_volume, 100);
    }

    #[test]
    fn a_lite_read_is_cached_within_its_ttl_and_carries_no_chart_arrays() {
        let mut store = Store::default();
        let now = at(14, 0, 0, 0);
        rq(&mut store, &quote(now, 2.00, 2.02));
        rt(&mut store, &trade(now + chrono::Duration::milliseconds(100), 2.02, 100, &["@"]));
        let first = lite_in(&mut store, "AAA", to_secs(now) + 1.0).unwrap();
        assert!(first.summary.delta_buckets.is_empty());
        assert!(first.summary.large_prints.is_empty());
        assert!(first.summary.profile.as_ref().unwrap().bins.is_empty());
        // The profile's own totals survive stripping: only the bins go.
        assert_eq!(first.summary.profile.as_ref().unwrap().total_volume, 100);

        // A later trade inside the TTL is not reflected: the cached answer
        // is reused, and its own asOfSecs says how old it is.
        rt(&mut store, &trade(now + chrono::Duration::milliseconds(200), 2.02, 900, &["@"]));
        let cached = lite_in(&mut store, "AAA", to_secs(now) + 1.5).unwrap();
        assert_eq!(cached.summary.as_of_secs, first.summary.as_of_secs);
        assert_eq!(cached.summary.profile.unwrap().total_volume, 100);

        // Past the TTL it is rebuilt.
        let rebuilt = lite_in(&mut store, "AAA", to_secs(now) + 2.5).unwrap();
        assert_eq!(rebuilt.summary.profile.unwrap().total_volume, 1_000);
        assert!(lite_in(&mut store, "ZZZ", to_secs(now) + 1.0).is_none());
    }

    #[test]
    fn a_capacity_rejected_symbol_stays_partial_once_it_gets_an_engine() {
        let mut store = Store::default();
        let now = at(14, 0, 0, 0);
        for i in 0..MAX_ENGINES {
            let mut q = quote(now, 1.99, 2.0);
            q.symbol = format!("S{i}");
            rq(&mut store, &q);
        }
        rq(&mut store, &quote(now, 1.99, 2.0));
        assert_eq!(store.health.capacity_rejected, 1);
        // Room appears once the others go idle; AAA's own day already has
        // a hole in it, so no backfill makes it a whole session.
        let later = now + chrono::Duration::seconds(IDLE_SECS as i64 + 1);
        rq(&mut store, &quote(later, 1.99, 2.0));
        assert!(store.engines["AAA"].partial);
        assert_eq!(coverage_of(store.engines["AAA"].partial, BackfillStatus::Done), Coverage::Partial);
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
        rt(&mut store,&trade(at(14, 0, 0, 0), 2.00, 100, &["@"]));
        assert!(store.engines.is_empty());
        rq(&mut store,&quote(at(14, 0, 1, 0), 2.00, 2.02));
        rt(&mut store,&trade(at(14, 0, 1, 100), 2.02, 100, &["@"]));
        let s = store.engines["AAA"].engine.summary(to_secs(at(14, 0, 2, 0)));
        assert_eq!(s.windows.iter().find(|w| w.window_secs == 5.0).unwrap().buy_volume, 100);
    }
}
