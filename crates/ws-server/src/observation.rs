//! Consumer-received observation layer -- `consumer-received-protocol-v1`.
//!
//! This module exists to answer one question the existing research streams
//! structurally cannot: **what did this consumer actually receive, and what
//! state was it in when it finished ranking?** The opportunity-intelligence
//! stream records the engine's *scoring decisions*; the discovery audit
//! records the producer's *emission*. Neither records receipt, and neither
//! records when processing of a received event started or completed. Every
//! attempt to recover those offline failed for the same structural reason:
//! `opportunityId = symbol:session_date:sequence` while discovery ignition
//! rows carry no matching identity, so there is no shared key by
//! construction. The gap is an instrumentation gap, and instrumentation is
//! the only thing that closes it.
//!
//! # What is deliberately NOT claimed here
//!
//! * **No efficacy, profitability or selection-quality claim.** This module
//!   records observation provenance. It scores nothing and decides nothing.
//! * **No byte integrity.** Nothing here hashes a file. `size` and `mtime`
//!   are not integrity evidence and are not used as such; closure is
//!   established from *file content* (a terminal `file_close` record), never
//!   from filesystem metadata.
//! * **No crash durability for rows.** `sync_all` runs when a file is
//!   closed, so a *closed* file's bytes are durable. Rows written into a
//!   still-open file are not, and no certificate ever describes them.
//! * **No end-marker certificate.** A terminal record proves a writer
//!   reached the end; it proves nothing about what came before it. The
//!   research writer's own markers go to a *separate sibling file* through a
//!   queue that may drop them without incrementing any counter, which is
//!   exactly why an end marker can exist while earlier data is missing.
//!   `Certificate::issue` therefore reconciles counted rows against declared
//!   per-window counts and refuses on any shortfall, no matter how tidy the
//!   terminal records look.
//! * **No global counter substitution.** `written`/`dropped`/`write_errors`
//!   are checked for their documented identity, but they can never stand in
//!   for row reconciliation. A capture whose counters balance and whose rows
//!   do not is refused.
//!
//! # Ownership
//!
//! The observation stream is **its own stream**, under its own root, with its
//! own files and its own record shapes. It never writes into an
//! opportunity-intelligence data file -- those are read line-by-line as
//! `OpportunityScoreSnapshot` by the alpha dataset reader and the integrity
//! check, and a line of any other shape counts as malformed, which is
//! blocking. `ResearchWriter` semantics are untouched.
//!
//! # Off by default
//!
//! Gated on `OPPORTUNITY_OBSERVATION`. A new research consumer costs real CPU
//! on a box that also runs the live scan; that cost is opted into, never
//! inherited.

// `ws-server` is a binary crate, so anything the binary itself does not call
// reads as dead code -- and the whole acquisition/authentication/certificate
// half of this module is deliberately *not* called by the binary. It is the
// offline surface: exercised by this module's tests today and the entry point
// step 4 uses to analyse a capture. Without this the crate gains 26 permanent
// warnings, which is how a real one stops being noticed.
//
// The cost, stated so it is not a surprise later: a genuinely unused helper in
// the live observer path is also hidden. If this module is ever split, the
// live half should drop the allow and keep it on the offline half only.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use market_data::ScanEvent;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Protocol constants
// ---------------------------------------------------------------------------

/// The frozen protocol this module implements. Any change to eligibility
/// semantics is a new version string, never a silent redefinition of this one.
pub const PROTOCOL_VERSION: &str = "consumer-received-protocol-v1";

/// Both market age and receipt age must be within this bound at ranking
/// completion.
///
/// **This number is an arbitrary declared hypothesis.** It is not implied by
/// the ranking cadence: ranking runs only when elapsed >= cadence *and the
/// open set is non-empty*, so 30 s is a minimum spacing, not a guaranteed
/// frequency, and a price can be far older than 30 s with no newer ranking
/// opportunity having occurred. It is fixed here, before any outcome is
/// looked at, because a threshold chosen after seeing outcomes is worthless.
pub const FRESHNESS_MAX_AGE_SECS: i64 = 30;

/// Enables the observer. Absent or unset means off.
pub const ENV_FLAG: &str = "OPPORTUNITY_OBSERVATION";
/// Root directory the observer allocates its run directory under.
pub const ENV_ROOT: &str = "OPPORTUNITY_OBSERVATION_ROOT";

/// Per-symbol confirmation-receipt tracking bound.
///
/// Bounded so a long session cannot turn the observer into the thing that
/// grows. Overflow is *counted*, and a symbol that ever overflowed marks its
/// candidates ineligible rather than silently undercounting confirmations.
pub const MAX_TRACKED_CONFIRMATIONS: usize = 64;

/// Bounded retries when allocating a run directory.
const MAX_RUN_ALLOCATION_ATTEMPTS: u32 = 64;

// ---------------------------------------------------------------------------
// Run identity
// ---------------------------------------------------------------------------

/// A run directory allocated exclusively by `create_dir`.
///
/// The guarantee is **allocation uniqueness within this retained root** --
/// not global, not cross-host. `create_dir` fails if the name exists, so two
/// concurrent observers under one root cannot claim the same name. Nothing
/// here proves two *different* roots (a clone, a restored backup, a second
/// host) do not contain colliding names, which is why an export that merges
/// roots must reject duplicate or conflicting namespaces and missing
/// provenance rather than assuming this type settled it.
///
/// Directories are retained. A name is never reused and never deleted to free
/// one.
#[derive(Debug, Clone)]
pub struct ObserverRun {
    id: String,
    dir: PathBuf,
}

#[derive(Debug)]
pub enum RunAllocationError {
    /// `MAX_RUN_ALLOCATION_ATTEMPTS` consecutive names already existed.
    Exhausted { attempts: u32, last: String },
    /// Any other filesystem error. Startup fails rather than continuing with
    /// an unidentified run.
    Io(std::io::Error),
}

impl std::fmt::Display for RunAllocationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exhausted { attempts, last } => {
                write!(
                    f,
                    "run-directory allocation exhausted after {attempts} attempts (last tried {last})"
                )
            }
            Self::Io(e) => write!(f, "run-directory allocation failed: {e}"),
        }
    }
}

impl std::error::Error for RunAllocationError {}

impl ObserverRun {
    /// Allocates a fresh run directory under `root`.
    ///
    /// The name carries the host/root namespace, the PID, the start instant
    /// and a collision counter, so a name is self-describing without a
    /// sidecar. `AlreadyExists` retries with the next counter; any other
    /// error fails immediately.
    pub fn allocate(
        root: &Path,
        namespace: &str,
        started_at: DateTime<Utc>,
        pid: u32,
    ) -> Result<Self, RunAllocationError> {
        std::fs::create_dir_all(root).map_err(RunAllocationError::Io)?;
        let stamp = started_at.format("%Y%m%dT%H%M%S%3fZ");
        let sanitized = sanitize_namespace(namespace);
        let mut last = String::new();
        for collision in 0..MAX_RUN_ALLOCATION_ATTEMPTS {
            let id = format!("{sanitized}-{pid}-{stamp}-{collision}");
            let dir = root.join(&id);
            last = id.clone();
            match std::fs::create_dir(&dir) {
                Ok(()) => return Ok(Self { id, dir }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(RunAllocationError::Io(e)),
            }
        }
        Err(RunAllocationError::Exhausted { attempts: MAX_RUN_ALLOCATION_ATTEMPTS, last })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// Keeps a run name a single safe path component.
fn sanitize_namespace(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(48)
        .collect();
    if cleaned.is_empty() {
        "unknown".to_string()
    } else {
        cleaned
    }
}

/// Host namespace for a run name, from the environment with a stable
/// fallback. Never fails: an unidentified host is recorded as `unknown`
/// rather than blocking startup, and the run directory's own root supplies
/// the rest of the provenance.
pub fn host_namespace() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

// ---------------------------------------------------------------------------
// Record shapes
// ---------------------------------------------------------------------------

/// Whether a price-carrying event moved market time forward for its symbol.
///
/// An out-of-order arrival is a revision of what the consumer believed, and
/// protocol clause 3 requires revision status to be part of price provenance
/// rather than inferred later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceRevision {
    /// Market time is at or after every earlier market time for this symbol.
    Forward,
    /// Market time precedes an earlier observed market time for this symbol.
    OutOfOrder,
}

/// Where the price used at an anchor came from, identified by the received
/// event that supplied it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceProvenance {
    pub source_run_id: String,
    pub source_sequence: u64,
    pub price: f64,
    /// Market time the price refers to. For a finalised bar this is the bar's
    /// timestamp **plus its interval**, matching the engine's own causal
    /// correction -- a finalised bar's close is only knowable one interval
    /// after the bar opened.
    pub market_at: DateTime<Utc>,
    /// Instant this consumer received the event carrying the price.
    pub received_at: DateTime<Utc>,
    pub revision: PriceRevision,
}

/// Why a candidate is not in the strict primary cohort.
///
/// Recorded per candidate rather than collapsed to a boolean, because "not
/// eligible" and "not eligible *for this reason*" are different facts, and the
/// eligibility-rate safeguard needs the breakdown before any outcome is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IneligibilityReason {
    /// No provenance, or the engine's price does not match the last price
    /// this observer saw for the symbol. Clause 3: unknown provenance is
    /// ineligible, never assumed fresh.
    UnknownPriceProvenance,
    MarketAgeExceeded,
    ReceiptAgeExceeded,
    /// Market time is after ranking completion, so the age is not a
    /// non-negative quantity. Reachable in practice: a finalised bar's
    /// corrected market time can sit ahead of its own receipt.
    NegativeMarketAge,
    NegativeReceiptAge,
    /// No confirmation receipt for this lifecycle at the anchor.
    NoConfirmationReceipt,
    /// More than one confirmation receipt. Clause 5: excluded from the strict
    /// primary and counted separately, never resolved by picking one.
    ConfirmationMultiplicity,
    /// Two open lifecycles share this symbol, so a confirmation cannot be
    /// uniquely assigned to one of them.
    AmbiguousLifecycleMapping,
    /// This symbol overflowed `MAX_TRACKED_CONFIRMATIONS`, so its
    /// confirmation count is a floor rather than a count.
    ConfirmationTrackingIncomplete,
    /// The window this candidate belongs to is not a complete actual window.
    /// Applied by validation, not by the live observer.
    WindowIncomplete,
}

/// A candidate's eligibility under the frozen protocol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Eligibility {
    pub eligible: bool,
    pub reasons: Vec<IneligibilityReason>,
}

impl Eligibility {
    fn from_reasons(mut reasons: Vec<IneligibilityReason>) -> Self {
        reasons.sort();
        reasons.dedup();
        Self { eligible: reasons.is_empty(), reasons }
    }
}

/// Writer counters, with the identity the research writer documents:
/// `written + dropped + write_errors == attempted`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriterCounters {
    pub attempted: u64,
    pub written: u64,
    pub dropped: u64,
    pub write_errors: u64,
    /// Set when any counter saturated. A saturated counter is not a count,
    /// and a capture carrying one cannot be certified.
    pub overflowed: bool,
}

impl WriterCounters {
    pub fn identity_holds(&self) -> bool {
        self.written
            .checked_add(self.dropped)
            .and_then(|v| v.checked_add(self.write_errors))
            .map(|v| v == self.attempted)
            .unwrap_or(false)
    }
}

/// One line of the observation stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ObservationRecord {
    #[serde(rename_all = "camelCase")]
    RunStart {
        protocol_version: String,
        run_id: String,
        namespace: String,
        pid: u32,
        started_at: DateTime<Utc>,
        freshness_max_age_secs: i64,
    },
    /// One received event. The sequence is assigned on *successful* receive,
    /// so `(runId, sequence)` is the consumer-received cohort key.
    #[serde(rename_all = "camelCase")]
    Receipt {
        run_id: String,
        sequence: u64,
        received_at: DateTime<Utc>,
        event_type: String,
        symbol: Option<String>,
        market_at: Option<DateTime<Utc>>,
        price: Option<f64>,
        revision: Option<PriceRevision>,
    },
    /// The broadcast channel dropped events before the next receipt. Recorded
    /// so a sequence gap is *explained* evidence rather than unknown loss --
    /// an unexplained gap refuses authentication.
    #[serde(rename_all = "camelCase")]
    Lag { run_id: String, sequence: u64, skipped: u64, at: DateTime<Utc> },
    /// One candidate in the open set at a ranking anchor.
    #[serde(rename_all = "camelCase")]
    Candidate {
        run_id: String,
        window_id: String,
        anchor_at: DateTime<Utc>,
        processing_started_at: DateTime<Utc>,
        opportunity_id: String,
        symbol: String,
        opened_at: DateTime<Utc>,
        /// Whether this window produced a score for this opportunity. Scored
        /// is not the same as open: the engine emits a row per traversed open
        /// opportunity, unscored included, so a scored-only pool would be the
        /// wrong denominator.
        scored: bool,
        provenance: Option<PriceProvenance>,
        market_age_secs: Option<i64>,
        receipt_age_secs: Option<i64>,
        confirmation_receipts: u64,
        eligibility: Eligibility,
    },
    /// Closes a ranking window and **declares** how many candidate records
    /// belong to it. Declared, not trusted: validation counts the rows.
    #[serde(rename_all = "camelCase")]
    WindowClose {
        run_id: String,
        window_id: String,
        anchor_at: DateTime<Utc>,
        processing_started_at: DateTime<Utc>,
        rank_completed_at: DateTime<Utc>,
        entry_count: u64,
        open_set_size: u64,
        /// The engine truncated the ranking cohort for this window, so the
        /// candidate set is not the complete open set.
        cohort_truncated: bool,
    },
    /// Terminal record of one file. A file without this is **open**, and an
    /// open file is never read as evidence.
    #[serde(rename_all = "camelCase")]
    FileClose { run_id: String, file_name: String, records_written: u64, closed_at: DateTime<Utc> },
    #[serde(rename_all = "camelCase")]
    RunEnd { run_id: String, ended_at: DateTime<Utc>, counters: WriterCounters },
}

impl ObservationRecord {
    pub fn run_id(&self) -> &str {
        match self {
            Self::RunStart { run_id, .. }
            | Self::Receipt { run_id, .. }
            | Self::Lag { run_id, .. }
            | Self::Candidate { run_id, .. }
            | Self::WindowClose { run_id, .. }
            | Self::FileClose { run_id, .. }
            | Self::RunEnd { run_id, .. } => run_id,
        }
    }
}

// ---------------------------------------------------------------------------
// Sink
// ---------------------------------------------------------------------------

/// Where an observer's records go.
///
/// A trait so tests can inject a real write failure, a flush failure, and a
/// close failure. Faults injected here are *not* injected into the real
/// `ResearchWriter`, so nothing in this module establishes a crash-durability
/// property for that writer.
pub trait ObservationSink {
    fn write(&mut self, record: &ObservationRecord) -> std::io::Result<()>;
    fn counters(&self) -> WriterCounters;
    /// Writes the terminal record and makes the file durable.
    fn close(&mut self, run_id: &str, at: DateTime<Utc>) -> std::io::Result<()>;
}

/// A single-file NDJSON sink that fsyncs on close.
///
/// Writes are **synchronous on the calling thread**. That is a deliberate L2
/// limitation, not a design for production: the existing research writers put
/// a bounded queue and a dedicated thread between the realtime path and the
/// disk precisely so a slow disk drops counted records instead of stalling a
/// consumer. Enabling this observer on a live host needs that queue first.
pub struct FileSink {
    file_name: String,
    writer: Option<BufWriter<File>>,
    counters: WriterCounters,
}

impl FileSink {
    pub fn create(dir: &Path, file_name: &str) -> std::io::Result<Self> {
        let path = dir.join(file_name);
        // `create_new`: a sink never appends to a file it did not create, so
        // two runs cannot interleave lines into one file.
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        Ok(Self {
            file_name: file_name.to_string(),
            writer: Some(BufWriter::new(file)),
            counters: WriterCounters::default(),
        })
    }

    /// Builds a sink over an already-open handle.
    ///
    /// The seam exists so a test can hand this a genuinely unwritable handle
    /// and exercise a **real** filesystem failure -- the buffered write
    /// succeeds, the flush at close does not, and the file ends up with no
    /// terminal record. That is the shape of the failure that matters: a run
    /// that believed it wrote and produced an unclosed file.
    pub fn from_file(file_name: &str, file: File) -> Self {
        Self {
            file_name: file_name.to_string(),
            writer: Some(BufWriter::new(file)),
            counters: WriterCounters::default(),
        }
    }

    fn bump_attempted(&mut self) {
        self.counters.attempted = match self.counters.attempted.checked_add(1) {
            Some(v) => v,
            None => {
                self.counters.overflowed = true;
                u64::MAX
            }
        };
    }
}

impl ObservationSink for FileSink {
    fn write(&mut self, record: &ObservationRecord) -> std::io::Result<()> {
        self.bump_attempted();
        let line = serde_json::to_string(record)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let Some(writer) = self.writer.as_mut() else {
            self.counters.write_errors = self.counters.write_errors.saturating_add(1);
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "sink already closed"));
        };
        match writer.write_all(line.as_bytes()).and_then(|()| writer.write_all(b"\n")) {
            Ok(()) => {
                self.counters.written = self.counters.written.saturating_add(1);
                Ok(())
            }
            Err(e) => {
                self.counters.write_errors = self.counters.write_errors.saturating_add(1);
                Err(e)
            }
        }
    }

    fn counters(&self) -> WriterCounters {
        self.counters
    }

    fn close(&mut self, run_id: &str, at: DateTime<Utc>) -> std::io::Result<()> {
        let records_written = self.counters.written;
        let close = ObservationRecord::FileClose {
            run_id: run_id.to_string(),
            file_name: self.file_name.clone(),
            records_written,
            closed_at: at,
        };
        self.write(&close)?;
        let Some(writer) = self.writer.take() else {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "sink already closed"));
        };
        // Durability for a *closed* file, and only for it. Rows still sitting
        // in a live file's buffer are not covered by this, and no certificate
        // ever describes them.
        let mut file = writer.into_inner().map_err(|e| e.into_error())?;
        file.flush()?;
        file.sync_all()
    }
}

// ---------------------------------------------------------------------------
// The live observer
// ---------------------------------------------------------------------------

/// One open opportunity at a ranking anchor.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenCandidate {
    pub opportunity_id: String,
    pub symbol: String,
    pub opened_at: DateTime<Utc>,
}

/// Everything the observer needs about one ranking window, gathered by the
/// caller around the existing `observe` -> `rank` sequence.
#[derive(Debug, Clone)]
pub struct WindowInput {
    pub window_id: String,
    pub processing_started_at: DateTime<Utc>,
    pub rank_completed_at: DateTime<Utc>,
    /// The complete open set at the anchor, from the engine's read-only
    /// `open_opportunities` iterator.
    pub open: Vec<OpenCandidate>,
    /// Opportunity ids this window actually scored.
    pub scored: BTreeSet<String>,
    /// The price the engine carried for each scored opportunity, used to
    /// decide whether this observer's price provenance describes the *same*
    /// price the engine used.
    pub engine_prices: BTreeMap<String, f64>,
    pub cohort_truncated: bool,
}

/// Hook the shadow driver calls. Kept as a trait so the driver depends on a
/// behaviour rather than on this module's concrete observer, and so a test can
/// observe the hooks without a filesystem.
pub trait ShadowObserver: Send {
    /// Called once per successfully received event, with the receipt instant.
    fn on_receive(&mut self, event: &ScanEvent, received_at: DateTime<Utc>);
    /// Called once per ranking window, after ranking has completed.
    fn on_window(&mut self, input: WindowInput);
    /// Called when the broadcast channel reports dropped events.
    fn on_lag(&mut self, skipped: u64, at: DateTime<Utc>);
    /// Called at shutdown.
    fn on_finish(&mut self, at: DateTime<Utc>);
}

/// Per-symbol state the observer keeps to build price provenance and count
/// confirmation receipts.
#[derive(Debug, Default)]
struct SymbolState {
    last_price: Option<PriceProvenance>,
    max_market_at: Option<DateTime<Utc>>,
    /// Receipt instants of confirmation events, bounded.
    confirmations: Vec<DateTime<Utc>>,
    tracking_incomplete: bool,
}

/// The consumer-received observer.
pub struct Observer {
    run_id: String,
    sequence: u64,
    symbols: HashMap<String, SymbolState>,
    sink: Box<dyn ObservationSink + Send>,
    /// Records the sink refused. Counted, never silently dropped.
    failed_writes: u64,
}

impl Observer {
    /// Starts an observer and writes its `run_start` record.
    pub fn start(
        run: &ObserverRun,
        namespace: &str,
        pid: u32,
        started_at: DateTime<Utc>,
        mut sink: Box<dyn ObservationSink + Send>,
    ) -> std::io::Result<Self> {
        let start = ObservationRecord::RunStart {
            protocol_version: PROTOCOL_VERSION.to_string(),
            run_id: run.id().to_string(),
            namespace: namespace.to_string(),
            pid,
            started_at,
            freshness_max_age_secs: FRESHNESS_MAX_AGE_SECS,
        };
        sink.write(&start)?;
        Ok(Self {
            run_id: run.id().to_string(),
            sequence: 0,
            symbols: HashMap::new(),
            sink,
            failed_writes: 0,
        })
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn failed_writes(&self) -> u64 {
        self.failed_writes
    }

    pub fn counters(&self) -> WriterCounters {
        self.sink.counters()
    }

    fn emit(&mut self, record: &ObservationRecord) {
        if self.sink.write(record).is_err() {
            self.failed_writes = self.failed_writes.saturating_add(1);
        }
    }
}

/// The event-type tag as it appears on the wire, so an observation row joins
/// to a client frame without a second naming convention.
///
/// Deliberately exhaustive with no wildcard arm: a new `ScanEvent` variant
/// must fail to compile here rather than silently record as some other kind.
fn event_type_tag(event: &ScanEvent) -> &'static str {
    match event {
        ScanEvent::FunnelSignal { .. } => "funnel_signal",
        ScanEvent::MomentumUpdate { .. } => "momentum_update",
        ScanEvent::IgnitionEvent { .. } => "ignition_event",
        ScanEvent::ConsolidationEvent { .. } => "consolidation_event",
        ScanEvent::FunnelHealth { .. } => "funnel_health",
        ScanEvent::HaltWarning { .. } => "halt_warning",
        ScanEvent::BarUpdate { .. } => "bar_update",
        ScanEvent::CatalystUpdate { .. } => "catalyst_update",
    }
}

/// True for the one event kind that confirms a lifecycle.
fn is_confirmation(event: &ScanEvent) -> bool {
    matches!(
        event,
        ScanEvent::IgnitionEvent {
            kind: market_data::IgnitionEventKind::FollowThroughConfirmed,
            ..
        }
    )
}

impl ShadowObserver for Observer {
    fn on_receive(&mut self, event: &ScanEvent, received_at: DateTime<Utc>) {
        self.sequence = self.sequence.saturating_add(1);
        let sequence = self.sequence;
        // The engine's own extraction, not a second copy of it. Reimplementing
        // this would be a second implementation of the price-incorporation
        // rule, and it would have got the finalised-bar correction wrong --
        // exactly the kind of drift that makes a provenance claim false while
        // looking right.
        let extracted = backtest_metrics::opportunity::event_symbol_time_price(event);
        let mut revision = None;
        let (symbol, market_at, price) = match &extracted {
            Some((symbol, at, price)) => {
                let state = self.symbols.entry(symbol.clone()).or_default();
                let rev = match state.max_market_at {
                    Some(prev) if *at < prev => PriceRevision::OutOfOrder,
                    _ => PriceRevision::Forward,
                };
                state.max_market_at = Some(match state.max_market_at {
                    Some(prev) if prev > *at => prev,
                    _ => *at,
                });
                if let Some(p) = price {
                    state.last_price = Some(PriceProvenance {
                        source_run_id: self.run_id.clone(),
                        source_sequence: sequence,
                        price: *p,
                        market_at: *at,
                        received_at,
                        revision: rev,
                    });
                }
                if is_confirmation(event) {
                    if state.confirmations.len() >= MAX_TRACKED_CONFIRMATIONS {
                        state.confirmations.remove(0);
                        state.tracking_incomplete = true;
                    }
                    state.confirmations.push(received_at);
                }
                revision = Some(rev);
                (Some(symbol.clone()), Some(*at), *price)
            }
            None => (None, None, None),
        };
        let record = ObservationRecord::Receipt {
            run_id: self.run_id.clone(),
            sequence,
            received_at,
            event_type: event_type_tag(event).to_string(),
            symbol,
            market_at,
            price,
            revision,
        };
        self.emit(&record);
    }

    fn on_lag(&mut self, skipped: u64, at: DateTime<Utc>) {
        let record =
            ObservationRecord::Lag { run_id: self.run_id.clone(), sequence: self.sequence, skipped, at };
        self.emit(&record);
    }

    fn on_window(&mut self, input: WindowInput) {
        // Two open lifecycles on one symbol make a confirmation unassignable.
        // Detected, never resolved by convenience.
        let mut per_symbol: HashMap<String, usize> = HashMap::new();
        for c in &input.open {
            *per_symbol.entry(c.symbol.clone()).or_insert(0) += 1;
        }
        let mut records = Vec::with_capacity(input.open.len());
        for candidate in &input.open {
            let mut reasons = Vec::new();
            let state = self.symbols.get(&candidate.symbol);
            let engine_price = input.engine_prices.get(&candidate.opportunity_id).copied();
            let scored = input.scored.contains(&candidate.opportunity_id);
            // Provenance is known only when this observer's last price for the
            // symbol IS the price the engine used. Anything else is unknown
            // provenance, which clause 3 makes ineligible rather than assuming
            // the two agree.
            let provenance = match (state.and_then(|s| s.last_price.as_ref()), engine_price) {
                (Some(p), Some(engine)) if p.price == engine => Some(p.clone()),
                // An unscored candidate carries no engine price to agree with,
                // so agreement cannot be established for it either way. It
                // stays in the pool (the denominator is the open set, not the
                // scored set) with provenance unknown.
                (Some(_), None) if !scored => None,
                _ => None,
            };
            let (market_age, receipt_age) = match &provenance {
                Some(p) => (
                    Some((input.rank_completed_at - p.market_at).num_seconds()),
                    Some((input.rank_completed_at - p.received_at).num_seconds()),
                ),
                None => (None, None),
            };
            match (&provenance, market_age, receipt_age) {
                (Some(_), Some(m), Some(r)) => {
                    if m < 0 {
                        reasons.push(IneligibilityReason::NegativeMarketAge);
                    } else if m > FRESHNESS_MAX_AGE_SECS {
                        reasons.push(IneligibilityReason::MarketAgeExceeded);
                    }
                    if r < 0 {
                        reasons.push(IneligibilityReason::NegativeReceiptAge);
                    } else if r > FRESHNESS_MAX_AGE_SECS {
                        reasons.push(IneligibilityReason::ReceiptAgeExceeded);
                    }
                }
                _ => reasons.push(IneligibilityReason::UnknownPriceProvenance),
            }
            if per_symbol.get(&candidate.symbol).copied().unwrap_or(0) > 1 {
                reasons.push(IneligibilityReason::AmbiguousLifecycleMapping);
            }
            if state.map(|s| s.tracking_incomplete).unwrap_or(false) {
                reasons.push(IneligibilityReason::ConfirmationTrackingIncomplete);
            }
            // "At the anchor" is scoped to the lifecycle: a confirmation
            // received before this opportunity opened belongs to a previous
            // one and must not be counted for this one.
            let confirmations = state
                .map(|s| {
                    s.confirmations
                        .iter()
                        .filter(|at| **at >= candidate.opened_at && **at <= input.rank_completed_at)
                        .count() as u64
                })
                .unwrap_or(0);
            match confirmations {
                0 => reasons.push(IneligibilityReason::NoConfirmationReceipt),
                1 => {}
                _ => reasons.push(IneligibilityReason::ConfirmationMultiplicity),
            }
            records.push(ObservationRecord::Candidate {
                run_id: self.run_id.clone(),
                window_id: input.window_id.clone(),
                anchor_at: input.rank_completed_at,
                processing_started_at: input.processing_started_at,
                opportunity_id: candidate.opportunity_id.clone(),
                symbol: candidate.symbol.clone(),
                opened_at: candidate.opened_at,
                scored,
                provenance,
                market_age_secs: market_age,
                receipt_age_secs: receipt_age,
                confirmation_receipts: confirmations,
                eligibility: Eligibility::from_reasons(reasons),
            });
        }
        let entry_count = records.len() as u64;
        for record in &records {
            self.emit(record);
        }
        let close = ObservationRecord::WindowClose {
            run_id: self.run_id.clone(),
            window_id: input.window_id,
            anchor_at: input.rank_completed_at,
            processing_started_at: input.processing_started_at,
            rank_completed_at: input.rank_completed_at,
            entry_count,
            open_set_size: input.open.len() as u64,
            cohort_truncated: input.cohort_truncated,
        };
        self.emit(&close);
    }

    fn on_finish(&mut self, at: DateTime<Utc>) {
        let counters = self.sink.counters();
        let end = ObservationRecord::RunEnd { run_id: self.run_id.clone(), ended_at: at, counters };
        self.emit(&end);
        let run_id = self.run_id.clone();
        if self.sink.close(&run_id, at).is_err() {
            self.failed_writes = self.failed_writes.saturating_add(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Independent acquisition: the closed-file reader
// ---------------------------------------------------------------------------
//
// This is the piece the typed validation layer deliberately did without.
// Validation took rows, loss and closure as *caller-supplied* inputs, which
// means it could be handed anything and would happily validate it -- so no
// certificate it produced described a real capture. Everything below acquires
// the evidence itself, from files on disk, and only the type this reader
// constructs can reach `Certificate::issue`.

/// One file's acquired evidence.
#[derive(Debug, Clone)]
pub struct FileEvidence {
    pub name: String,
    /// Records parsed from a **closed** file. Empty for an open file: an open
    /// file is never used as evidence, only reported.
    pub records: Vec<ObservationRecord>,
    /// A file is closed iff its content ends with a `file_close` record.
    /// Established from content, never from size or mtime -- neither of which
    /// is byte-integrity or closure evidence.
    pub closed: bool,
    /// `records_written` as declared by the terminal record, if present.
    pub declared_records: Option<u64>,
    /// Records this reader actually counted, excluding the terminal record.
    pub counted_records: u64,
    /// The file's last line had no terminating newline.
    pub trailing_partial_line: bool,
    /// Reported for provenance only. **Not** integrity evidence.
    pub byte_len: u64,
}

/// Evidence acquired from disk by `acquire`.
///
/// Fields are private and there is no public constructor: a `Certificate` can
/// only be issued from evidence this reader produced, so "certify from
/// counters I was handed" is not an expressible program.
#[derive(Debug, Clone)]
pub struct AcquiredCapture {
    run_dir: PathBuf,
    files: Vec<FileEvidence>,
}

impl AcquiredCapture {
    pub fn run_dir(&self) -> &Path {
        &self.run_dir
    }

    pub fn files(&self) -> &[FileEvidence] {
        &self.files
    }

    /// Records from closed files, in file-name order.
    pub fn records(&self) -> impl Iterator<Item = &ObservationRecord> {
        self.files.iter().flat_map(|f| f.records.iter())
    }

    pub fn open_files(&self) -> Vec<&str> {
        self.files.iter().filter(|f| !f.closed).map(|f| f.name.as_str()).collect()
    }
}

#[derive(Debug)]
pub enum AcquisitionError {
    Io(std::io::Error),
    /// The run directory held no observation files at all.
    NoFiles { run_dir: PathBuf },
    /// A line of a closed file did not parse as an `ObservationRecord`.
    MalformedLine { file: String, line: usize },
    /// A closed file's last line had no terminating newline, so its final
    /// record may be truncated. Refused rather than censored: the terminal
    /// record is what makes a file closed, and a truncated one cannot be
    /// trusted to be it.
    TrailingPartialLine { file: String },
    /// Records appear after the terminal record.
    RecordsAfterFileClose { file: String, line: usize },
    /// More than one terminal record in one file.
    DuplicateFileClose { file: String },
}

impl std::fmt::Display for AcquisitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "acquisition io error: {e}"),
            Self::NoFiles { run_dir } => {
                write!(f, "no observation files under {}", run_dir.display())
            }
            Self::MalformedLine { file, line } => {
                write!(f, "{file}: line {line} is not a valid observation record")
            }
            Self::TrailingPartialLine { file } => {
                write!(f, "{file}: closed file ends with an unterminated line")
            }
            Self::RecordsAfterFileClose { file, line } => {
                write!(f, "{file}: line {line} appears after the terminal record")
            }
            Self::DuplicateFileClose { file } => write!(f, "{file}: more than one terminal record"),
        }
    }
}

impl std::error::Error for AcquisitionError {}

/// The extension the observer writes and this reader accepts.
pub const OBSERVATION_FILE_SUFFIX: &str = ".ndjson";

/// Reads every observation file in `run_dir` and returns what is actually
/// there.
///
/// Open files are reported, never used. Sorting by name gives a deterministic
/// order without consulting mtime, which is not evidence.
pub fn acquire(run_dir: &Path) -> Result<AcquiredCapture, AcquisitionError> {
    let mut names: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(run_dir).map_err(AcquisitionError::Io)? {
        let entry = entry.map_err(AcquisitionError::Io)?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(OBSERVATION_FILE_SUFFIX)
            && entry.file_type().map_err(AcquisitionError::Io)?.is_file()
        {
            names.push(name);
        }
    }
    names.sort();
    if names.is_empty() {
        return Err(AcquisitionError::NoFiles { run_dir: run_dir.to_path_buf() });
    }
    let mut files = Vec::with_capacity(names.len());
    for name in names {
        files.push(read_file(run_dir, &name)?);
    }
    Ok(AcquiredCapture { run_dir: run_dir.to_path_buf(), files })
}

fn read_file(dir: &Path, name: &str) -> Result<FileEvidence, AcquisitionError> {
    let path = dir.join(name);
    let bytes = std::fs::metadata(&path).map_err(AcquisitionError::Io)?.len();
    let file = File::open(&path).map_err(AcquisitionError::Io)?;
    let mut reader = BufReader::new(file);
    let mut raw = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    let mut unterminated = false;
    loop {
        raw.clear();
        let read = reader.read_until(b'\n', &mut raw).map_err(AcquisitionError::Io)?;
        if read == 0 {
            break;
        }
        let terminated = raw.last() == Some(&b'\n');
        let text = String::from_utf8_lossy(&raw).trim_end_matches('\n').to_string();
        if !terminated {
            // Only the final line can be unterminated.
            unterminated = true;
            if !text.is_empty() {
                lines.push(text);
            }
            break;
        }
        lines.push(text);
    }

    // First pass: is this file closed? Parse only enough to answer that, so
    // an open file's possibly-torn content is never turned into evidence.
    let mut closed = false;
    let mut declared = None;
    let mut close_index = None;
    let mut duplicate_close = false;
    for (i, line) in lines.iter().enumerate() {
        if let Ok(ObservationRecord::FileClose { records_written, .. }) =
            serde_json::from_str::<ObservationRecord>(line)
        {
            if closed {
                duplicate_close = true;
            } else {
                closed = true;
                declared = Some(records_written);
                close_index = Some(i);
            }
        }
    }
    if !closed {
        return Ok(FileEvidence {
            name: name.to_string(),
            records: Vec::new(),
            closed: false,
            declared_records: None,
            counted_records: lines.len() as u64,
            trailing_partial_line: unterminated,
            byte_len: bytes,
        });
    }
    if duplicate_close {
        return Err(AcquisitionError::DuplicateFileClose { file: name.to_string() });
    }
    if unterminated {
        return Err(AcquisitionError::TrailingPartialLine { file: name.to_string() });
    }
    let close_index = close_index.unwrap_or(0);
    if close_index + 1 != lines.len() {
        return Err(AcquisitionError::RecordsAfterFileClose {
            file: name.to_string(),
            line: close_index + 2,
        });
    }
    let mut records = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        match serde_json::from_str::<ObservationRecord>(line) {
            Ok(record) => records.push(record),
            Err(_) => {
                return Err(AcquisitionError::MalformedLine {
                    file: name.to_string(),
                    line: i + 1,
                })
            }
        }
    }
    Ok(FileEvidence {
        name: name.to_string(),
        records,
        closed: true,
        declared_records: declared,
        counted_records: close_index as u64,
        trailing_partial_line: false,
        byte_len: bytes,
    })
}

// ---------------------------------------------------------------------------
// Evidence authentication
// ---------------------------------------------------------------------------

/// What authentication established, reported whether or not a certificate is
/// later issued.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticationReport {
    pub run_id: String,
    pub protocol_version: String,
    pub files: u64,
    pub receipts: u64,
    pub candidates: u64,
    pub windows: u64,
    /// Events the broadcast channel dropped before this consumer saw them.
    /// Upstream loss, which the consumer-received cohort is *defined* as not
    /// containing -- reported so a reader knows the cohort is not the
    /// producer's output.
    pub upstream_skipped_events: u64,
    pub declared_counters: WriterCounters,
    /// Always false. Nothing here hashes a file, and size/mtime are not
    /// integrity evidence.
    pub byte_integrity_established: bool,
    /// Always false. Faults are injected at the sink boundary, not into the
    /// real research writer, and the research writer performs no fsync.
    pub crash_durability_established: bool,
}

/// Authenticated evidence. Only `authenticate` constructs this.
#[derive(Debug, Clone)]
pub struct AuthenticatedCapture {
    acquired: AcquiredCapture,
    report: AuthenticationReport,
}

impl AuthenticatedCapture {
    pub fn report(&self) -> &AuthenticationReport {
        &self.report
    }

    pub fn acquired(&self) -> &AcquiredCapture {
        &self.acquired
    }

    pub fn candidates(&self) -> impl Iterator<Item = &ObservationRecord> {
        self.acquired.records().filter(|r| matches!(r, ObservationRecord::Candidate { .. }))
    }
}

#[derive(Debug, PartialEq)]
pub enum AuthenticationFailure {
    MissingRunStart,
    MultipleRunStart { count: usize },
    ProtocolMismatch { found: String, expected: &'static str },
    MixedRunIds { found: Vec<String> },
    /// At least one file has no terminal record. The file set is therefore
    /// incomplete and nothing may be certified from it.
    OpenFilePresent { names: Vec<String> },
    MissingRunEnd,
    MultipleRunEnd { count: usize },
    /// A receipt sequence is missing. Sequences are assigned contiguously on
    /// successful receive, so a gap is a *lost row*, not upstream lag -- lag
    /// is recorded separately and consumes no sequence.
    SequenceGap { expected: u64, found: u64 },
    /// A file's declared record count does not match what the reader counted.
    FileRecordCountMismatch { file: String, declared: u64, counted: u64 },
}

impl std::fmt::Display for AuthenticationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingRunStart => write!(f, "no run_start record"),
            Self::MultipleRunStart { count } => write!(f, "{count} run_start records"),
            Self::ProtocolMismatch { found, expected } => {
                write!(f, "protocol {found} is not {expected}")
            }
            Self::MixedRunIds { found } => write!(f, "records from several runs: {found:?}"),
            Self::OpenFilePresent { names } => {
                write!(f, "file set incomplete, still-open files: {names:?}")
            }
            Self::MissingRunEnd => write!(f, "no run_end record"),
            Self::MultipleRunEnd { count } => write!(f, "{count} run_end records"),
            Self::SequenceGap { expected, found } => {
                write!(f, "receipt sequence gap: expected {expected}, found {found}")
            }
            Self::FileRecordCountMismatch { file, declared, counted } => {
                write!(f, "{file}: declared {declared} records, counted {counted}")
            }
        }
    }
}

impl std::error::Error for AuthenticationFailure {}

/// Establishes that acquired evidence is internally consistent and complete
/// enough to reason about. It does **not** decide that the capture is good --
/// that is `Certificate::issue`, and it is deliberately a separate gate.
pub fn authenticate(acquired: AcquiredCapture) -> Result<AuthenticatedCapture, AuthenticationFailure> {
    let open = acquired.open_files();
    if !open.is_empty() {
        return Err(AuthenticationFailure::OpenFilePresent {
            names: open.into_iter().map(|s| s.to_string()).collect(),
        });
    }
    for file in acquired.files() {
        if let Some(declared) = file.declared_records {
            if declared != file.counted_records {
                return Err(AuthenticationFailure::FileRecordCountMismatch {
                    file: file.name.clone(),
                    declared,
                    counted: file.counted_records,
                });
            }
        }
    }

    let mut run_starts = Vec::new();
    let mut run_ends = 0usize;
    let mut ids: BTreeSet<String> = BTreeSet::new();
    let mut receipts = 0u64;
    let mut candidates = 0u64;
    let mut windows = 0u64;
    let mut upstream_skipped = 0u64;
    let mut counters = WriterCounters::default();
    let mut sequences: Vec<u64> = Vec::new();
    for record in acquired.records() {
        ids.insert(record.run_id().to_string());
        match record {
            ObservationRecord::RunStart { protocol_version, .. } => {
                run_starts.push(protocol_version.clone())
            }
            ObservationRecord::Receipt { sequence, .. } => {
                receipts += 1;
                sequences.push(*sequence);
            }
            ObservationRecord::Lag { skipped, .. } => {
                upstream_skipped = upstream_skipped.saturating_add(*skipped)
            }
            ObservationRecord::Candidate { .. } => candidates += 1,
            ObservationRecord::WindowClose { .. } => windows += 1,
            ObservationRecord::FileClose { .. } => {}
            ObservationRecord::RunEnd { counters: c, .. } => {
                run_ends += 1;
                counters = *c;
            }
        }
    }
    if run_starts.is_empty() {
        return Err(AuthenticationFailure::MissingRunStart);
    }
    if run_starts.len() > 1 {
        return Err(AuthenticationFailure::MultipleRunStart { count: run_starts.len() });
    }
    if run_starts[0] != PROTOCOL_VERSION {
        return Err(AuthenticationFailure::ProtocolMismatch {
            found: run_starts[0].clone(),
            expected: PROTOCOL_VERSION,
        });
    }
    if ids.len() > 1 {
        return Err(AuthenticationFailure::MixedRunIds { found: ids.into_iter().collect() });
    }
    match run_ends {
        0 => return Err(AuthenticationFailure::MissingRunEnd),
        1 => {}
        n => return Err(AuthenticationFailure::MultipleRunEnd { count: n }),
    }
    sequences.sort_unstable();
    for (i, seq) in sequences.iter().enumerate() {
        let expected = i as u64 + 1;
        if *seq != expected {
            return Err(AuthenticationFailure::SequenceGap { expected, found: *seq });
        }
    }
    let run_id = ids.into_iter().next().unwrap_or_default();
    let report = AuthenticationReport {
        run_id,
        protocol_version: PROTOCOL_VERSION.to_string(),
        files: acquired.files().len() as u64,
        receipts,
        candidates,
        windows,
        upstream_skipped_events: upstream_skipped,
        declared_counters: counters,
        byte_integrity_established: false,
        crash_durability_established: false,
    };
    Ok(AuthenticatedCapture { acquired, report })
}

// ---------------------------------------------------------------------------
// Real-capture certificate
// ---------------------------------------------------------------------------

/// A window's reconciliation: what the terminal record declared against what
/// the reader counted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowReconciliation {
    pub window_id: String,
    pub declared_entries: Option<u64>,
    pub counted_entries: u64,
    pub declared_open_set: Option<u64>,
    pub cohort_truncated: bool,
    pub complete: bool,
}

/// A capture that certifies. Issued only from authenticated evidence this
/// module's reader acquired.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Certificate {
    pub run_id: String,
    pub protocol_version: String,
    /// Fixed by the protocol, recorded so a consumer of the certificate never
    /// has to infer which instant the ages were measured at.
    pub anchor: String,
    pub freshness_max_age_secs: i64,
    pub windows: u64,
    pub candidates: u64,
    pub eligible: u64,
    /// Clause 5 requires multiplicity to be counted separately rather than
    /// resolved, so it is a field of the certificate, not a footnote.
    pub excluded_multiplicity: u64,
    pub ineligible_by_reason: BTreeMap<String, u64>,
    /// The outcome-free safeguard statistic. Reported here so it is available
    /// *before* any outcome is read.
    pub eligibility_rate: f64,
    pub upstream_skipped_events: u64,
    pub byte_integrity_established: bool,
    pub crash_durability_established: bool,
}

#[derive(Debug, PartialEq)]
pub enum CertificateRefusal {
    /// `written + dropped + write_errors != attempted`.
    CountersIdentityViolated { counters: WriterCounters },
    /// A counter saturated, so it is not a count.
    CounterOverflow { counters: WriterCounters },
    /// Records were lost. A capture with loss is not a complete capture, and
    /// no completeness claim may be made from it.
    RecordsLost { dropped: u64, write_errors: u64 },
    /// A window declared a different number of candidate records than the
    /// reader counted. **This is the check an end marker cannot replace**: the
    /// terminal records can all be present and tidy while rows are missing.
    WindowEntryCountMismatch { window_id: String, declared: u64, counted: u64 },
    /// Candidate records exist for a window with no terminal record, so the
    /// window is partial. Clause 6: partial windows are ineligible.
    WindowWithoutClose { window_id: String },
    DuplicateCandidate { window_id: String, opportunity_id: String },
    /// The engine truncated the cohort, so the candidate set is not the
    /// complete open set and the common-pool requirement fails.
    CohortTruncated { window_id: String },
    /// The declared open-set size disagrees with the candidate rows.
    OpenSetMismatch { window_id: String, declared: u64, counted: u64 },
    /// Nothing to certify.
    NoWindows,
}

impl std::fmt::Display for CertificateRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CountersIdentityViolated { counters } => {
                write!(f, "counter identity violated: {counters:?}")
            }
            Self::CounterOverflow { counters } => write!(f, "counter overflowed: {counters:?}"),
            Self::RecordsLost { dropped, write_errors } => {
                write!(f, "records lost: {dropped} dropped, {write_errors} write errors")
            }
            Self::WindowEntryCountMismatch { window_id, declared, counted } => write!(
                f,
                "window {window_id} declared {declared} candidates, counted {counted}"
            ),
            Self::WindowWithoutClose { window_id } => {
                write!(f, "window {window_id} has candidates but no terminal record")
            }
            Self::DuplicateCandidate { window_id, opportunity_id } => {
                write!(f, "window {window_id} repeats opportunity {opportunity_id}")
            }
            Self::CohortTruncated { window_id } => {
                write!(f, "window {window_id} truncated its cohort")
            }
            Self::OpenSetMismatch { window_id, declared, counted } => write!(
                f,
                "window {window_id} declared an open set of {declared}, counted {counted} candidates"
            ),
            Self::NoWindows => write!(f, "no ranking windows to certify"),
        }
    }
}

impl std::error::Error for CertificateRefusal {}

/// Per-window reconciliation over authenticated evidence.
pub fn reconcile_windows(capture: &AuthenticatedCapture) -> Vec<WindowReconciliation> {
    let mut counted: BTreeMap<String, u64> = BTreeMap::new();
    let mut declared: BTreeMap<String, (u64, u64, bool)> = BTreeMap::new();
    for record in capture.acquired().records() {
        match record {
            ObservationRecord::Candidate { window_id, .. } => {
                *counted.entry(window_id.clone()).or_insert(0) += 1;
            }
            ObservationRecord::WindowClose {
                window_id, entry_count, open_set_size, cohort_truncated, ..
            } => {
                declared.insert(window_id.clone(), (*entry_count, *open_set_size, *cohort_truncated));
            }
            _ => {}
        }
    }
    let mut ids: BTreeSet<&String> = BTreeSet::new();
    ids.extend(counted.keys());
    ids.extend(declared.keys());
    ids.into_iter()
        .map(|id| {
            let counted_entries = counted.get(id).copied().unwrap_or(0);
            let d = declared.get(id);
            WindowReconciliation {
                window_id: id.clone(),
                declared_entries: d.map(|(e, _, _)| *e),
                counted_entries,
                declared_open_set: d.map(|(_, o, _)| *o),
                cohort_truncated: d.map(|(_, _, t)| *t).unwrap_or(false),
                complete: d.is_some_and(|(e, o, t)| {
                    *e == counted_entries && *o == counted_entries && !*t
                }),
            }
        })
        .collect()
}

impl Certificate {
    /// Issues a certificate, or refuses with the exact reason.
    ///
    /// Takes `&AuthenticatedCapture` by design: that type has private fields
    /// and only this module's reader constructs it, so a certificate cannot be
    /// issued from counters, a summary, or anything else a caller assembled
    /// itself. The counters are checked, but they are never the evidence --
    /// every window is reconciled row by row, because the failure this guards
    /// against is precisely a capture whose counters balance while its rows do
    /// not.
    pub fn issue(capture: &AuthenticatedCapture) -> Result<Self, CertificateRefusal> {
        let counters = capture.report().declared_counters;
        if counters.overflowed {
            return Err(CertificateRefusal::CounterOverflow { counters });
        }
        if !counters.identity_holds() {
            return Err(CertificateRefusal::CountersIdentityViolated { counters });
        }
        if counters.dropped > 0 || counters.write_errors > 0 {
            return Err(CertificateRefusal::RecordsLost {
                dropped: counters.dropped,
                write_errors: counters.write_errors,
            });
        }

        let windows = reconcile_windows(capture);
        if windows.is_empty() {
            return Err(CertificateRefusal::NoWindows);
        }
        for w in &windows {
            match w.declared_entries {
                None => {
                    return Err(CertificateRefusal::WindowWithoutClose {
                        window_id: w.window_id.clone(),
                    })
                }
                Some(declared) if declared != w.counted_entries => {
                    return Err(CertificateRefusal::WindowEntryCountMismatch {
                        window_id: w.window_id.clone(),
                        declared,
                        counted: w.counted_entries,
                    })
                }
                Some(_) => {}
            }
            if w.cohort_truncated {
                return Err(CertificateRefusal::CohortTruncated { window_id: w.window_id.clone() });
            }
            if let Some(open) = w.declared_open_set {
                if open != w.counted_entries {
                    return Err(CertificateRefusal::OpenSetMismatch {
                        window_id: w.window_id.clone(),
                        declared: open,
                        counted: w.counted_entries,
                    });
                }
            }
        }

        let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
        let mut eligible = 0u64;
        let mut candidates = 0u64;
        let mut multiplicity = 0u64;
        let mut by_reason: BTreeMap<String, u64> = BTreeMap::new();
        for record in capture.candidates() {
            let ObservationRecord::Candidate { window_id, opportunity_id, eligibility, .. } = record
            else {
                continue;
            };
            if !seen.insert((window_id.clone(), opportunity_id.clone())) {
                return Err(CertificateRefusal::DuplicateCandidate {
                    window_id: window_id.clone(),
                    opportunity_id: opportunity_id.clone(),
                });
            }
            candidates += 1;
            if eligibility.eligible {
                eligible += 1;
            }
            for reason in &eligibility.reasons {
                if *reason == IneligibilityReason::ConfirmationMultiplicity {
                    multiplicity += 1;
                }
                *by_reason.entry(reason_key(*reason).to_string()).or_insert(0) += 1;
            }
        }

        let eligibility_rate =
            if candidates == 0 { 0.0 } else { eligible as f64 / candidates as f64 };
        Ok(Self {
            run_id: capture.report().run_id.clone(),
            protocol_version: PROTOCOL_VERSION.to_string(),
            anchor: "ranking_completion".to_string(),
            freshness_max_age_secs: FRESHNESS_MAX_AGE_SECS,
            windows: windows.len() as u64,
            candidates,
            eligible,
            excluded_multiplicity: multiplicity,
            ineligible_by_reason: by_reason,
            eligibility_rate,
            upstream_skipped_events: capture.report().upstream_skipped_events,
            byte_integrity_established: false,
            crash_durability_established: false,
        })
    }
}

/// Stable string for a reason, so a certificate's breakdown is diffable
/// without depending on `Debug` formatting.
pub fn reason_key(reason: IneligibilityReason) -> &'static str {
    match reason {
        IneligibilityReason::UnknownPriceProvenance => "unknown_price_provenance",
        IneligibilityReason::MarketAgeExceeded => "market_age_exceeded",
        IneligibilityReason::ReceiptAgeExceeded => "receipt_age_exceeded",
        IneligibilityReason::NegativeMarketAge => "negative_market_age",
        IneligibilityReason::NegativeReceiptAge => "negative_receipt_age",
        IneligibilityReason::NoConfirmationReceipt => "no_confirmation_receipt",
        IneligibilityReason::ConfirmationMultiplicity => "confirmation_multiplicity",
        IneligibilityReason::AmbiguousLifecycleMapping => "ambiguous_lifecycle_mapping",
        IneligibilityReason::ConfirmationTrackingIncomplete => "confirmation_tracking_incomplete",
        IneligibilityReason::WindowIncomplete => "window_incomplete",
    }
}

// ---------------------------------------------------------------------------
// Canonical comparison
// ---------------------------------------------------------------------------

/// One canonical tuple: `(run, window, opportunity, eligibility provenance)`.
///
/// Exact sorted-tuple equality rather than a home-grown digest. A digest would
/// have been an unreviewed primitive with no dependency available, and at
/// fixture scale it buys nothing; exact equality is authoritative and catches
/// the case a count-only comparison misses -- identical counts with changed
/// content.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CanonicalTuple {
    pub run_id: String,
    pub window_id: String,
    pub opportunity_id: String,
    pub eligibility_provenance: String,
}

/// Canonical sorted tuples for two independently acquired evidence sets to be
/// compared exactly.
pub fn canonical_tuples(capture: &AuthenticatedCapture) -> Vec<CanonicalTuple> {
    let mut out: Vec<CanonicalTuple> = capture
        .candidates()
        .filter_map(|record| {
            let ObservationRecord::Candidate {
                run_id,
                window_id,
                opportunity_id,
                eligibility,
                provenance,
                ..
            } = record
            else {
                return None;
            };
            let decision = if eligibility.eligible {
                "eligible".to_string()
            } else {
                let reasons: Vec<&str> =
                    eligibility.reasons.iter().map(|r| reason_key(*r)).collect();
                format!("ineligible:{}", reasons.join(","))
            };
            let source = match provenance {
                Some(p) => format!("{}#{}", p.source_run_id, p.source_sequence),
                None => "none".to_string(),
            };
            Some(CanonicalTuple {
                run_id: run_id.clone(),
                window_id: window_id.clone(),
                opportunity_id: opportunity_id.clone(),
                eligibility_provenance: format!("{decision}|{source}"),
            })
        })
        .collect();
    out.sort();
    out
}

/// A candidate's eligibility after window completeness is applied.
///
/// Separate from `Certificate::issue` on purpose. Issue *refuses* an
/// incomplete capture; this reports what an incomplete capture nonetheless
/// says, with clause 6 applied -- a candidate in a partial window becomes
/// ineligible rather than silently retaining the live observer's verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct FinalCandidate {
    pub window_id: String,
    pub opportunity_id: String,
    pub eligibility: Eligibility,
}

pub fn finalize_eligibility(capture: &AuthenticatedCapture) -> Vec<FinalCandidate> {
    let complete: BTreeSet<String> = reconcile_windows(capture)
        .into_iter()
        .filter(|w| w.complete)
        .map(|w| w.window_id)
        .collect();
    capture
        .candidates()
        .filter_map(|record| {
            let ObservationRecord::Candidate { window_id, opportunity_id, eligibility, .. } = record
            else {
                return None;
            };
            let mut reasons = eligibility.reasons.clone();
            if !complete.contains(window_id) {
                reasons.push(IneligibilityReason::WindowIncomplete);
            }
            Some(FinalCandidate {
                window_id: window_id.clone(),
                opportunity_id: opportunity_id.clone(),
                eligibility: Eligibility::from_reasons(reasons),
            })
        })
        .collect()
}


// ---------------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------------

/// Where run directories go when `OPPORTUNITY_OBSERVATION_ROOT` is unset.
pub const DEFAULT_ROOT: &str = "research/observation";

/// One observation file per run. Rotation is not implemented: a run writes one
/// file, and the reader's file-set logic is general enough to accept several so
/// rotation can be added without changing the acquisition contract.
pub const RUN_FILE_NAME: &str = "observations-0.ndjson";

/// True when the flag is set to an affirmative value. Anything else, including
/// absence, is off.
pub fn enabled() -> bool {
    std::env::var(ENV_FLAG).map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on")).unwrap_or(false)
}

/// Builds an observer from the environment, or `None`.
///
/// A failure here logs and returns `None` rather than aborting: this is
/// research instrumentation, and it must not be able to take the realtime
/// server down. What it must never do is fail *silently* -- every path logs,
/// because a research subsystem that is quietly absent is indistinguishable
/// from one that is working and finding nothing.
pub fn start_from_env() -> Option<Observer> {
    if !enabled() {
        tracing::info!(
            "consumer-received observation disabled (set {}=1)",
            ENV_FLAG
        );
        return None;
    }
    let root = std::env::var(ENV_ROOT).unwrap_or_else(|_| DEFAULT_ROOT.to_string());
    let root = PathBuf::from(root);
    let namespace = host_namespace();
    let pid = std::process::id();
    let started_at = Utc::now();
    let run = match ObserverRun::allocate(&root, &namespace, started_at, pid) {
        Ok(run) => run,
        Err(e) => {
            tracing::warn!(error = %e, root = %root.display(), "observation run allocation failed; observation off for this process");
            return None;
        }
    };
    let sink = match FileSink::create(run.dir(), RUN_FILE_NAME) {
        Ok(sink) => sink,
        Err(e) => {
            tracing::warn!(error = %e, dir = %run.dir().display(), "observation file creation failed; observation off for this process");
            return None;
        }
    };
    match Observer::start(&run, &namespace, pid, started_at, Box::new(sink)) {
        Ok(observer) => {
            tracing::info!(
                run_id = %run.id(),
                dir = %run.dir().display(),
                protocol = PROTOCOL_VERSION,
                freshness_max_age_secs = FRESHNESS_MAX_AGE_SECS,
                "consumer-received observation started"
            );
            Some(observer)
        }
        Err(e) => {
            tracing::warn!(error = %e, "observation run_start write failed; observation off for this process");
            None
        }
    }
}

#[cfg(test)]
#[path = "observation_tests.rs"]
mod observation_tests;
