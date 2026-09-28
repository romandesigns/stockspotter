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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use market_data::ScanEvent;
use serde::{Deserialize, Serialize};

#[path = "observation_writer.rs"]
mod writer;
// Re-exported so the observation API is one import for callers and tests.
// Several are used only by tests, which in a binary crate reads as unused.
#[allow(unused_imports)]
pub use writer::{
    AsyncSink, FileRecordWriter, LossSpan, RecordWriter, WriterTelemetry, DEFAULT_BYTE_CAPACITY,
    DEFAULT_QUEUE_CAPACITY, MAX_LOSS_SPANS,
};

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

/// The same bound at the precision ages are actually compared at.
///
/// Frozen clause 4 is `<= 30 s`, and 30.9 s is not `<= 30 s`. An earlier
/// version compared whole seconds (`num_seconds()`, which truncates toward
/// zero), so 30.9 s read as 30 and was admitted, and a market time 0.5 s
/// *after* the anchor read as 0 -- "perfectly fresh" -- instead of the ordering
/// inconsistency it is. Ages are now nanoseconds end to end: 30,000 ms is
/// eligible, 30,000.001 ms is not, and any negative value stays negative.
pub const FRESHNESS_MAX_AGE_NANOS: i64 = FRESHNESS_MAX_AGE_SECS * 1_000_000_000;

/// Enables the observer. Absent or unset means off.
pub const ENV_FLAG: &str = "OPPORTUNITY_OBSERVATION";
/// Root directory the observer allocates its run directory under.
pub const ENV_ROOT: &str = "OPPORTUNITY_OBSERVATION_ROOT";
/// Explicit run namespace. Overrides the host name; must itself be valid.
pub const ENV_NAMESPACE: &str = "OPPORTUNITY_OBSERVATION_NAMESPACE";
/// Capture-level byte budget across every file of a run.
pub const ENV_MAX_BYTES: &str = "OPPORTUNITY_OBSERVATION_MAX_BYTES";

/// Default capture-level budget when `OPPORTUNITY_OBSERVATION_MAX_BYTES` is
/// unset. **A placeholder, not an adopted budget** -- Step 4 has not adopted
/// storage budgets for any host. It exists so an enabled observer is never
/// unbounded; a real capture should set the variable explicitly.
pub const DEFAULT_CAPTURE_MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Longest accepted namespace. Long enough for a host name, short enough that
/// a run directory name stays well inside path limits.
pub const MAX_NAMESPACE_LEN: usize = 48;

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
    /// The namespace is empty, too long, or contains anything but ASCII
    /// letters, digits and `-`. Refused, never rewritten: sanitizing would let
    /// two different requested namespaces (`a.b`, `a_b`) collapse into one
    /// recorded provenance, and would let `../bad` through as `___bad`.
    InvalidNamespace { namespace: String },
    /// `MAX_RUN_ALLOCATION_ATTEMPTS` consecutive names already existed.
    Exhausted { attempts: u32, last: String },
    /// Any other filesystem error. Startup fails rather than continuing with
    /// an unidentified run.
    Io(std::io::Error),
}

impl std::fmt::Display for RunAllocationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidNamespace { namespace } => write!(
                f,
                "invalid observation namespace {namespace:?}: 1-{MAX_NAMESPACE_LEN} ASCII letters, digits or '-' required"
            ),
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
        validate_namespace(namespace)?;
        std::fs::create_dir_all(root).map_err(RunAllocationError::Io)?;
        let stamp = started_at.format("%Y%m%dT%H%M%S%3fZ");
        let mut last = String::new();
        for collision in 0..MAX_RUN_ALLOCATION_ATTEMPTS {
            let id = format!("{namespace}-{pid}-{stamp}-{collision}");
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

/// The frozen L1 namespace rule: non-empty ASCII letters, digits and `-`.
///
/// This keeps a run name a single safe path component *and* keeps the
/// recorded namespace equal to the requested one. A namespace outside the rule
/// is refused rather than rewritten into one.
pub fn validate_namespace(namespace: &str) -> Result<(), RunAllocationError> {
    let ok = !namespace.is_empty()
        && namespace.len() <= MAX_NAMESPACE_LEN
        && namespace.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(RunAllocationError::InvalidNamespace { namespace: namespace.to_string() })
    }
}

/// The run namespace: `OPPORTUNITY_OBSERVATION_NAMESPACE`, else the host name.
///
/// Fails rather than inventing one. Run provenance that cannot be established
/// is missing provenance, and recording it as `unknown` would let two
/// unidentified hosts share a namespace that an export merge then trusts.
pub fn resolve_namespace() -> Result<String, RunAllocationError> {
    let raw = std::env::var(ENV_NAMESPACE)
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .or_else(|_| std::env::var("HOSTNAME"))
        .map_err(|_| RunAllocationError::InvalidNamespace { namespace: String::new() })?;
    validate_namespace(&raw)?;
    Ok(raw)
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
    /// Externally meaningful wall time only -- never used for ordering or age.
    pub received_at: DateTime<Utc>,
    /// The same receipt on the observer's **monotonic** clock, as nanoseconds
    /// since the observer started. Receipt age and the price-receipt <=
    /// processing-start ordering are computed from this, because a wall clock
    /// can step and a monotonic one cannot.
    pub received_mono_nanos: u64,
    pub revision: PriceRevision,
    /// The wire tag of the event that supplied the price.
    ///
    /// Carried so a certificate can tell a bar-sourced price from a
    /// trade-sourced one without re-reading the stream. Without it, a negative
    /// market age is an unattributable anomaly; with it, the two candidate
    /// explanations are separable.
    pub source_event_type: String,
    /// True when `market_at` was **derived** rather than taken from the event.
    ///
    /// Only one derivation exists: a finalised bar's close is dated one
    /// interval after the bar's opening timestamp, because that is when the
    /// close is knowable. Every other event's market time is its own
    /// timestamp.
    pub market_time_derived: bool,
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
    /// `market_at` is after ranking completion, so the age is not a
    /// non-negative quantity.
    ///
    /// **Semantics, decided rather than assumed.** The anchor is never before
    /// the source event's receipt, because ranking completes after the event
    /// that triggered it arrived, and the price's source event arrived no
    /// later than that. So a negative market age means exactly one thing: the
    /// event arrived **before the market time it claims**. Two explanations
    /// remain, and `sourceEventType` + `marketTimeDerived` separate them:
    ///
    /// * **derived, bar-sourced** -- a finalised bar whose close boundary
    ///   (`timestamp + intervalSecs`) lies ahead of its own arrival. A
    ///   producer that emits a finalised bar only after its interval closed
    ///   cannot generate this, so it indicates the bar was emitted early or
    ///   mislabelled `isFinal`.
    /// * **not derived** -- the event's own timestamp is ahead of the local
    ///   clock, i.e. the exchange timestamp domain and this consumer's wall
    ///   clock disagree.
    ///
    /// Either way it is an **ordering or clock inconsistency, not a freshness
    /// measurement**, so the contract is to fail closed: the candidate is
    /// ineligible and the age is recorded as measured. It is deliberately
    /// **not clamped to zero** -- clamping would convert a clock defect into
    /// the freshest possible price, which is the most dangerous direction the
    /// error could take.
    NegativeMarketAge,
    /// The price's receipt is not before processing of this window started,
    /// on the monotonic clock. L1's `price_receipt <= rank_start`: a price the
    /// consumer had not yet received when processing began cannot be the one
    /// the engine ranked on. Not reachable in the live loop, where every
    /// receipt precedes the processing it triggers; kept because a check that
    /// cannot fail is indistinguishable from one that does not work.
    PriceReceivedAfterProcessingStart,
    /// Processing start is after ranking completion on the monotonic clock.
    RankBracketInverted,
    /// No confirmation receipt for this lifecycle at the anchor.
    NoConfirmationReceipt,
    /// More than one confirmation receipt. Clause 5: excluded from the strict
    /// primary and counted separately, never resolved by picking one.
    ConfirmationMultiplicity,
    /// A confirmation for this symbol arrived out of market-time order with a
    /// market time before this lifecycle opened, so whether it belongs to this
    /// lifecycle cannot be decided. Unattributable, therefore ineligible.
    ConfirmationOrderingAmbiguous,
    /// Two open lifecycles share this symbol, so a confirmation cannot be
    /// uniquely assigned to one of them.
    AmbiguousLifecycleMapping,
    /// The window as a whole is ambiguous: some symbol in it has more than one
    /// open lifecycle. Frozen L1 (`mapping_unambiguous == false ⇒
    /// Invalid::Mapping`) invalidates the **whole window**, not only the
    /// ambiguous candidates, so every candidate in it carries this reason.
    WindowMappingAmbiguous,
    /// Broadcast lag dropped events that this window's lifecycles may depend
    /// on. Frozen L1 (`source_lag != 0 ⇒ Invalid::Loss`) invalidates the
    /// window: a lag can hide a confirmation receipt entirely, so no candidate
    /// in it can show it saw the complete receipt stream.
    WindowSourceLag,
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
        /// Monotonic receipt instant, nanoseconds since the observer started.
        received_mono_nanos: u64,
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
    /// Opens a ranking window and declares its **expected** identity set
    /// before any candidate row is written.
    ///
    /// `expected` is the sorted canonical tuple of every candidate
    /// (`opportunity|eligibility decision|price source`, see
    /// `canonical_candidate`). Certification requires the persisted rows to
    /// reproduce it exactly -- not merely in number -- so a row replaced by a
    /// different one of the same count is refused. Counts alone cannot see
    /// that, which is the gap the frozen L1 three-set equality closes.
    #[serde(rename_all = "camelCase")]
    WindowBegin {
        run_id: String,
        window_id: String,
        /// Last receive sequence folded before this window ranked.
        watermark: u64,
        expected: Vec<String>,
    },
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
        /// Anchor wall time minus the price's market time, exact nanoseconds.
        /// Negative values are recorded as measured, never clamped.
        market_age_nanos: Option<i64>,
        /// Anchor minus price receipt on the **monotonic** clock, nanoseconds.
        receipt_age_nanos: Option<i64>,
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
        /// Last receive sequence folded before this window ranked.
        watermark: u64,
        /// Monotonic processing start and ranking completion, nanoseconds
        /// since the observer started. The ages are computed from these.
        processing_started_mono_nanos: u64,
        rank_completed_mono_nanos: u64,
        /// Broadcast lag invalidated this window (`WindowSourceLag`).
        source_lag_invalid: bool,
        /// Ambiguous lifecycle mapping invalidated this window
        /// (`WindowMappingAmbiguous`).
        mapping_ambiguous: bool,
    },
    /// Observation stopped before the run ended. Written once, when the stop
    /// latches; nothing but terminal records follows it. A capture carrying
    /// this is incomplete and never certifies.
    #[serde(rename_all = "camelCase")]
    Stopped {
        run_id: String,
        reason: StopReason,
        at: DateTime<Utc>,
        /// Last receive sequence assigned before the stop.
        sequence: u64,
        capture_bytes: u64,
        capture_max_bytes: u64,
    },
    /// Opens a rotated file. Absent from the first file of a run, whose
    /// `run_start` plays the same role.
    ///
    /// `previous_file` is what makes a rotation chain verifiable in the
    /// *forward* direction as well as the backward one: the old file names its
    /// successor and the new file names its predecessor, so a missing middle
    /// file breaks both links rather than silently shortening the run.
    #[serde(rename_all = "camelCase")]
    FileStart {
        run_id: String,
        file_name: String,
        /// 0 for the run's first file, incrementing by exactly one per
        /// rotation.
        sequence: u32,
        previous_file: String,
    },
    /// Terminal record of one file. A file without this is **open**, and an
    /// open file is never read as evidence.
    #[serde(rename_all = "camelCase")]
    FileClose {
        run_id: String,
        file_name: String,
        records_written: u64,
        closed_at: DateTime<Utc>,
        /// The file this run continued into. `None` means this file ended the
        /// run. Optional so a single-file run's terminal record is unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_file: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    RunEnd {
        run_id: String,
        ended_at: DateTime<Utc>,
        counters: WriterCounters,
        /// Queue and loss telemetry. Absent for a synchronous sink, which has
        /// no queue to report on; `skip_serializing_if` keeps such a run's
        /// terminal record exactly as it was.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        telemetry: Option<WriterTelemetry>,
        /// Why observation stopped early, if it did. Repeats the `stopped`
        /// record so the stop is visible even if that record was lost.
        stopped: Option<StopReason>,
        /// Bytes the observer offered across every file of the run, against
        /// the capture-level budget in force.
        capture_bytes: u64,
        capture_max_bytes: u64,
    },
}

/// Why an observer stopped observing before its run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The capture-level byte budget would have been exceeded. L1's
    /// `CaptureBudget`: exceeding the cap latches the capture incomplete.
    CaptureBudgetExceeded,
    /// The receive sequence reached `u64::MAX`. L1's checked
    /// `ReceiveSequence::next() == None`: no further identity can be issued
    /// without reusing one, so observation stops instead.
    SequenceExhausted,
}

impl ObservationRecord {
    pub fn run_id(&self) -> &str {
        match self {
            Self::RunStart { run_id, .. }
            | Self::Receipt { run_id, .. }
            | Self::Lag { run_id, .. }
            | Self::WindowBegin { run_id, .. }
            | Self::Candidate { run_id, .. }
            | Self::WindowClose { run_id, .. }
            | Self::Stopped { run_id, .. }
            | Self::FileStart { run_id, .. }
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

    /// Writes a record the caller has already serialized.
    ///
    /// The observer serializes every record once to charge it against the
    /// capture budget; this lets a sink reuse that line instead of serializing
    /// a second time on the consumer thread. `line` must be `record`
    /// serialized. The default ignores it.
    fn write_serialized(&mut self, record: &ObservationRecord, line: &str) -> std::io::Result<()> {
        let _ = line;
        self.write(record)
    }

    fn counters(&self) -> WriterCounters;
    /// Makes the data durable, then writes the terminal record and makes that
    /// durable, in that order. A terminal record must never be left attesting
    /// to data whose own flush or fsync failed; see `FileSink::close`.
    fn close(&mut self, run_id: &str, at: DateTime<Utc>) -> std::io::Result<()>;

    /// Blocks until everything enqueued before the call has been handled.
    ///
    /// A synchronous sink has nothing to wait for. An asynchronous one does,
    /// and the difference matters at shutdown: `written` means *written by the
    /// writer*, not *accepted by the queue*, so a run's terminal counters are
    /// final only after a successful drain. A timeout is an error, never a
    /// quiet success.
    fn drain(&mut self, _timeout: Duration) -> std::io::Result<()> {
        Ok(())
    }

    /// Queue and loss telemetry, where the sink has any.
    fn telemetry(&self) -> Option<WriterTelemetry> {
        None
    }
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
    /// Where a close-failure marker goes. `None` for a sink built over a bare
    /// handle, which then relies on retraction alone.
    path: Option<PathBuf>,
    writer: Option<BufWriter<File>>,
    counters: WriterCounters,
    /// Bytes handed to the file so far, so a terminal record whose own
    /// durability failed can be cut off again.
    len: u64,
}

impl FileSink {
    pub fn create(dir: &Path, file_name: &str) -> std::io::Result<Self> {
        let path = dir.join(file_name);
        // `create_new`: a sink never appends to a file it did not create, so
        // two runs cannot interleave lines into one file.
        let file = OpenOptions::new().write(true).create_new(true).open(&path)?;
        Ok(Self {
            file_name: file_name.to_string(),
            path: Some(path),
            writer: Some(BufWriter::new(file)),
            counters: WriterCounters::default(),
            len: 0,
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
            path: None,
            writer: Some(BufWriter::new(file)),
            counters: WriterCounters::default(),
            len: 0,
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
        let line = serde_json::to_string(record)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        self.write_serialized(record, &line)
    }

    fn write_serialized(&mut self, _record: &ObservationRecord, line: &str) -> std::io::Result<()> {
        self.bump_attempted();
        let Some(writer) = self.writer.as_mut() else {
            self.counters.write_errors = self.counters.write_errors.saturating_add(1);
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "sink already closed"));
        };
        match writer.write_all(line.as_bytes()).and_then(|()| writer.write_all(b"\n")) {
            Ok(()) => {
                self.counters.written = self.counters.written.saturating_add(1);
                self.len = self.len.saturating_add(line.len() as u64 + 1);
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

    /// The frozen L1 close order (`CaptureBudget::close(writer_ok)`): data
    /// durable first, terminal record second.
    ///
    /// 1. flush and fsync the **data**; on failure write no terminal record at
    ///    all, so the file stays open and can only ever read INDETERMINATE;
    /// 2. only then write the terminal record, flush and fsync it;
    /// 3. if any of step 2 fails, cut the terminal record back off
    ///    (`set_len`), and if even that fails leave a `.close-failed` marker
    ///    the reader refuses.
    ///
    /// The earlier order wrote the terminal record *before* the fsync, so a
    /// failed fsync left a file that read as closed and certified -- a terminal
    /// record attesting to durability that never happened.
    fn close(&mut self, run_id: &str, at: DateTime<Utc>) -> std::io::Result<()> {
        let Some(mut writer) = self.writer.take() else {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "sink already closed"));
        };
        writer.flush()?;
        writer.get_ref().sync_all()?;
        let close = ObservationRecord::FileClose {
            run_id: run_id.to_string(),
            file_name: self.file_name.clone(),
            records_written: self.counters.written,
            closed_at: at,
            next_file: None,
        };
        let line = serde_json::to_string(&close)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let data_len = self.len;
        let terminal = writer
            .write_all(line.as_bytes())
            .and_then(|()| writer.write_all(b"\n"))
            .and_then(|()| writer.flush())
            .and_then(|()| writer.get_ref().sync_all());
        if let Err(e) = terminal {
            // Discard anything still buffered: flushing it later would put the
            // terminal record back.
            let (file, _unflushed) = writer.into_parts();
            retract_or_mark(&file, data_len, self.path.as_deref());
            return Err(e);
        }
        Ok(())
    }
}

/// Undoes a terminal record whose own durability failed.
///
/// Truncating to the pre-terminal length leaves an open file (INDETERMINATE,
/// never PASS). If truncation fails too, a sibling `<file>.close-failed`
/// marker makes the reader refuse the run outright. If both fail the
/// filesystem is refusing all writes and nothing further can be recorded; the
/// error still reaches the caller.
pub(crate) fn retract_or_mark(file: &File, data_len: u64, path: Option<&Path>) {
    let retracted = file.set_len(data_len).and_then(|()| file.sync_all());
    if retracted.is_err() {
        if let Some(path) = path {
            let _ = std::fs::write(close_failed_marker(path), b"terminal record not durable\n");
        }
    }
}

/// `<file>.close-failed`, beside the file it condemns.
pub fn close_failed_marker(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(CLOSE_FAILED_SUFFIX);
    path.with_file_name(name)
}

/// Suffix of a close-failure marker. Any such file in a run directory makes
/// acquisition fail.
pub const CLOSE_FAILED_SUFFIX: &str = ".close-failed";

// ---------------------------------------------------------------------------
// The live observer
// ---------------------------------------------------------------------------

/// One open opportunity at a ranking anchor.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenCandidate {
    pub opportunity_id: String,
    pub symbol: String,
    /// The engine's own lifecycle start: the **market time** of the event that
    /// opened it (`opportunity.rs`, `opened_at: at`). Confirmations are
    /// attributed to a lifecycle in that same market-time domain, never by
    /// comparing it against this consumer's wall clock.
    pub opened_at: DateTime<Utc>,
}

/// Everything the observer needs about one ranking window, gathered by the
/// caller around the existing `observe` -> `rank` sequence.
#[derive(Debug, Clone)]
pub struct WindowInput {
    pub window_id: String,
    /// Wall-clock bracket, for externally meaningful timestamps and the market
    /// age (market time is itself wall-clock-domain).
    pub processing_started_at: DateTime<Utc>,
    pub rank_completed_at: DateTime<Utc>,
    /// The same bracket on the monotonic clock. Receipt age and the
    /// receipt-before-processing ordering come from these. `None` samples
    /// `Instant::now()` at `on_window` entry, which is only acceptable for
    /// offline and test callers; the live driver always supplies both.
    pub processing_started_mono: Option<Instant>,
    pub rank_completed_mono: Option<Instant>,
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
    /// One successfully received event, with its wall and monotonic receipt
    /// instants sampled together by the caller.
    fn on_receive_mono(&mut self, event: &ScanEvent, received_at: DateTime<Utc>, received_mono: Instant);
    /// One successfully received event, receipt sampled on the monotonic clock
    /// now. For callers that do not hold a monotonic receipt instant.
    fn on_receive(&mut self, event: &ScanEvent, received_at: DateTime<Utc>) {
        self.on_receive_mono(event, received_at, Instant::now());
    }
    /// One ranking window, after the engine ranked it.
    fn on_window(&mut self, input: WindowInput);
    /// The broadcast channel reported `skipped` events lost upstream.
    fn on_lag(&mut self, skipped: u64, at: DateTime<Utc>);
    /// Shutdown: terminal records, then close.
    fn on_finish(&mut self, at: DateTime<Utc>);
}

/// One confirmation receipt, attributed by receive sequence and market time.
#[derive(Debug, Clone, Copy)]
struct Confirmation {
    /// Receive sequence: the consumer's own order, which is what L1's
    /// `confirmation_sequence <= watermark` requires.
    sequence: u64,
    /// Market time, for lifecycle membership in the engine's own domain.
    market_at: DateTime<Utc>,
    /// Arrived after a later-market-time event for the same symbol.
    out_of_order: bool,
}

/// Per-symbol state the observer keeps to build price provenance and count
/// confirmation receipts.
#[derive(Debug, Default)]
struct SymbolState {
    last_price: Option<PriceProvenance>,
    max_market_at: Option<DateTime<Utc>>,
    /// Confirmation receipts, bounded.
    confirmations: Vec<Confirmation>,
    tracking_incomplete: bool,
}

/// The consumer-received observer.
pub struct Observer {
    run_id: String,
    /// Last receive sequence issued. Checked, never saturating: at `u64::MAX`
    /// observation stops rather than issue a duplicate identity.
    sequence: u64,
    symbols: HashMap<String, SymbolState>,
    sink: Box<dyn ObservationSink + Send>,
    /// Records the sink refused. Counted, never silently dropped.
    failed_writes: u64,
    /// Monotonic origin. Every `*_mono_nanos` field is measured from here.
    epoch: Instant,
    /// Capture-level budget, spanning every file of the run.
    capture_max_bytes: u64,
    capture_bytes: u64,
    /// Latched once observation stops. Nothing but terminal records follows.
    stopped: Option<StopReason>,
    /// A lag was reported and no window has absorbed it yet.
    lag_pending: bool,
    /// Lifecycles that were, or may have been, open across a lag. A window
    /// containing any of them is lag-invalid for as long as they stay open,
    /// because a confirmation lost in the lag stays lost for their lifetime.
    lag_tainted: HashSet<String>,
    /// The open set at the most recent window.
    known_open: HashSet<String>,
}

impl Observer {
    /// Starts an observer and writes its `run_start` record.
    pub fn start(
        run: &ObserverRun,
        namespace: &str,
        pid: u32,
        started_at: DateTime<Utc>,
        sink: Box<dyn ObservationSink + Send>,
    ) -> std::io::Result<Self> {
        let mut observer = Self {
            run_id: run.id().to_string(),
            sequence: 0,
            symbols: HashMap::new(),
            sink,
            failed_writes: 0,
            epoch: Instant::now(),
            capture_max_bytes: DEFAULT_CAPTURE_MAX_BYTES,
            capture_bytes: 0,
            stopped: None,
            lag_pending: false,
            lag_tainted: HashSet::new(),
            known_open: HashSet::new(),
        };
        let start = ObservationRecord::RunStart {
            protocol_version: PROTOCOL_VERSION.to_string(),
            run_id: run.id().to_string(),
            namespace: namespace.to_string(),
            pid,
            started_at,
            freshness_max_age_secs: FRESHNESS_MAX_AGE_SECS,
        };
        let line = serde_json::to_string(&start)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        observer.capture_bytes = line.len() as u64 + 1;
        observer.sink.write_serialized(&start, &line)?;
        Ok(observer)
    }

    /// Sets the capture-level byte budget. Applies to everything written after
    /// `run_start`, across every file of the run.
    pub fn with_capture_max_bytes(mut self, max: u64) -> Self {
        self.capture_max_bytes = max;
        self
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

    pub fn stopped(&self) -> Option<StopReason> {
        self.stopped
    }

    fn mono_nanos(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.epoch).as_nanos().min(u128::from(u64::MAX)) as u64
    }

    /// Writes one record against the capture budget.
    ///
    /// `exempt` records -- the stop record and the terminal `run_end` -- are
    /// written even past the budget, because they are what make the overrun
    /// visible. Everything else that would cross the budget latches the stop
    /// instead of being written, so the budget is a real ceiling rather than
    /// something rotation quietly walks past.
    fn emit(&mut self, record: &ObservationRecord, exempt: bool) {
        let line = match serde_json::to_string(record) {
            Ok(line) => line,
            Err(_) => {
                self.failed_writes = self.failed_writes.saturating_add(1);
                return;
            }
        };
        let bytes = line.len() as u64 + 1;
        if !exempt {
            if self.stopped.is_some() {
                return;
            }
            if self.capture_bytes.saturating_add(bytes) > self.capture_max_bytes {
                self.stop(StopReason::CaptureBudgetExceeded);
                return;
            }
        }
        self.capture_bytes = self.capture_bytes.saturating_add(bytes);
        if self.sink.write_serialized(record, &line).is_err() {
            self.failed_writes = self.failed_writes.saturating_add(1);
        }
    }

    /// Latches the stop and records it once.
    fn stop(&mut self, reason: StopReason) {
        if self.stopped.is_some() {
            return;
        }
        self.stopped = Some(reason);
        let record = ObservationRecord::Stopped {
            run_id: self.run_id.clone(),
            reason,
            at: Utc::now(),
            sequence: self.sequence,
            capture_bytes: self.capture_bytes,
            capture_max_bytes: self.capture_max_bytes,
        };
        self.emit(&record, true);
        tracing::warn!(?reason, run_id = %self.run_id, "consumer-received observation stopped; capture incomplete");
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

/// The canonical identity of one candidate row: opportunity, eligibility
/// decision and price source. The unit of exact set comparison between a
/// window's declared `expected` set and its persisted rows.
pub fn canonical_candidate(
    opportunity_id: &str,
    eligibility: &Eligibility,
    provenance: Option<&PriceProvenance>,
) -> String {
    let decision = if eligibility.eligible {
        "eligible".to_string()
    } else {
        let reasons: Vec<&str> = eligibility.reasons.iter().map(|r| reason_key(*r)).collect();
        format!("ineligible:{}", reasons.join(","))
    };
    let source = match provenance {
        Some(p) => format!("{}#{}", p.source_run_id, p.source_sequence),
        None => "none".to_string(),
    };
    format!("{opportunity_id}|{decision}|{source}")
}

impl ShadowObserver for Observer {
    fn on_receive_mono(&mut self, event: &ScanEvent, received_at: DateTime<Utc>, received_mono: Instant) {
        if self.stopped.is_some() {
            return;
        }
        let Some(sequence) = self.sequence.checked_add(1) else {
            // L1 `ReceiveSequence::next() == None`. Issuing u64::MAX again would
            // be a duplicate receive identity; stop instead.
            self.stop(StopReason::SequenceExhausted);
            return;
        };
        self.sequence = sequence;
        let received_mono_nanos = self.mono_nanos(received_mono);
        // The engine's own extraction, not a second copy of it. Reimplementing
        // this would be a second implementation of the price-incorporation
        // rule, and it would have got the finalised-bar correction wrong --
        // exactly the kind of drift that makes a provenance claim false while
        // looking right.
        let extracted = backtest_metrics::opportunity::event_symbol_time_price(event);
        let event_type = event_type_tag(event);
        // The one derivation the engine performs. Recorded rather than
        // recomputed, so a negative market age can be attributed to the
        // derivation or to a clock disagreement without guessing.
        let market_time_derived = matches!(event, ScanEvent::BarUpdate { is_final: true, .. });
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
                        received_mono_nanos,
                        revision: rev,
                        source_event_type: event_type.to_string(),
                        market_time_derived,
                    });
                }
                if is_confirmation(event) {
                    if state.confirmations.len() >= MAX_TRACKED_CONFIRMATIONS {
                        state.confirmations.remove(0);
                        state.tracking_incomplete = true;
                    }
                    state.confirmations.push(Confirmation {
                        sequence,
                        market_at: *at,
                        out_of_order: rev == PriceRevision::OutOfOrder,
                    });
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
            received_mono_nanos,
            event_type: event_type.to_string(),
            symbol,
            market_at,
            price,
            revision,
        };
        self.emit(&record, false);
    }

    fn on_lag(&mut self, skipped: u64, at: DateTime<Utc>) {
        if self.stopped.is_some() {
            return;
        }
        let record =
            ObservationRecord::Lag { run_id: self.run_id.clone(), sequence: self.sequence, skipped, at };
        self.emit(&record, false);
        // Every lifecycle open at the last window may have lost a receipt.
        // Lifecycles that opened since then are tainted when the next window
        // sees them, because they may have opened before the lag.
        self.lag_pending = true;
        let open: Vec<String> = self.known_open.iter().cloned().collect();
        self.lag_tainted.extend(open);
    }

    fn on_window(&mut self, input: WindowInput) {
        if self.stopped.is_some() {
            return;
        }
        let now = Instant::now();
        let processing_mono = self.mono_nanos(input.processing_started_mono.unwrap_or(now));
        let rank_mono = self.mono_nanos(input.rank_completed_mono.unwrap_or(now));
        let watermark = self.sequence;
        let current: HashSet<String> = input.open.iter().map(|c| c.opportunity_id.clone()).collect();

        // Lag (frozen L1: `source_lag != 0 ⇒ Invalid::Loss`), window-level.
        // A pending lag invalidates this window outright and taints everything
        // in it; a tainted lifecycle keeps invalidating windows while open.
        let source_lag_invalid =
            self.lag_pending || current.iter().any(|id| self.lag_tainted.contains(id));
        if self.lag_pending {
            self.lag_tainted.extend(current.iter().cloned());
            self.lag_pending = false;
        }
        self.lag_tainted.retain(|id| current.contains(id));
        self.known_open = current;

        // Ambiguity (frozen L1: `mapping_unambiguous == false ⇒
        // Invalid::Mapping`), window-level.
        let mut per_symbol: HashMap<&str, usize> = HashMap::new();
        for c in &input.open {
            *per_symbol.entry(c.symbol.as_str()).or_insert(0) += 1;
        }
        let mapping_ambiguous = per_symbol.values().any(|n| *n > 1);
        let bracket_inverted = processing_mono > rank_mono;

        let mut records = Vec::with_capacity(input.open.len());
        let mut expected = Vec::with_capacity(input.open.len());
        for candidate in &input.open {
            let mut reasons = Vec::new();
            let state = self.symbols.get(&candidate.symbol);
            let engine_price = input.engine_prices.get(&candidate.opportunity_id).copied();
            let scored = input.scored.contains(&candidate.opportunity_id);
            // Provenance is known only when this observer's last price for the
            // symbol IS the price the engine used. Anything else is unknown
            // provenance, which clause 3 makes ineligible rather than assuming
            // the two agree. An unscored candidate carries no engine price, so
            // agreement cannot be established for it either way.
            let provenance = match (state.and_then(|s| s.last_price.as_ref()), engine_price) {
                (Some(p), Some(engine)) if p.price == engine => Some(p.clone()),
                _ => None,
            };
            let (market_age, receipt_age) = match &provenance {
                Some(p) => {
                    // Clause 4, exact: nanoseconds, compared against 30,000 ms,
                    // negative kept negative. A `None` here is an age too large
                    // for i64 nanoseconds (~292 years); its sign still decides.
                    let delta = input.rank_completed_at - p.market_at;
                    match delta.num_nanoseconds() {
                        Some(n) if n < 0 => reasons.push(IneligibilityReason::NegativeMarketAge),
                        Some(n) if n > FRESHNESS_MAX_AGE_NANOS => {
                            reasons.push(IneligibilityReason::MarketAgeExceeded)
                        }
                        Some(_) => {}
                        None if delta < chrono::TimeDelta::zero() => {
                            reasons.push(IneligibilityReason::NegativeMarketAge)
                        }
                        None => reasons.push(IneligibilityReason::MarketAgeExceeded),
                    }
                    // Receipt ordering and age on the monotonic clock only.
                    if p.received_mono_nanos > processing_mono {
                        reasons.push(IneligibilityReason::PriceReceivedAfterProcessingStart);
                    }
                    let receipt =
                        (i128::from(rank_mono) - i128::from(p.received_mono_nanos)).clamp(
                            i128::from(i64::MIN),
                            i128::from(i64::MAX),
                        ) as i64;
                    if receipt > FRESHNESS_MAX_AGE_NANOS {
                        reasons.push(IneligibilityReason::ReceiptAgeExceeded);
                    }
                    (delta.num_nanoseconds(), Some(receipt))
                }
                None => {
                    reasons.push(IneligibilityReason::UnknownPriceProvenance);
                    (None, None)
                }
            };
            if bracket_inverted {
                reasons.push(IneligibilityReason::RankBracketInverted);
            }
            if per_symbol.get(candidate.symbol.as_str()).copied().unwrap_or(0) > 1 {
                reasons.push(IneligibilityReason::AmbiguousLifecycleMapping);
            }
            if mapping_ambiguous {
                reasons.push(IneligibilityReason::WindowMappingAmbiguous);
            }
            if source_lag_invalid {
                reasons.push(IneligibilityReason::WindowSourceLag);
            }
            if state.map(|s| s.tracking_incomplete).unwrap_or(false) {
                reasons.push(IneligibilityReason::ConfirmationTrackingIncomplete);
            }
            // Clause 5. A confirmation counts for this lifecycle when the
            // consumer had received it by this window (sequence <= watermark,
            // L1's causal bound) and its market time is within the lifecycle
            // (>= opened_at, the engine's own domain). No consumer wall clock
            // is involved in either test.
            let mut confirmations = 0u64;
            let mut unattributable = false;
            if let Some(s) = state {
                for c in &s.confirmations {
                    if c.sequence > watermark {
                        continue;
                    }
                    if c.market_at >= candidate.opened_at {
                        confirmations += 1;
                    } else if c.out_of_order {
                        unattributable = true;
                    }
                }
            }
            if unattributable {
                reasons.push(IneligibilityReason::ConfirmationOrderingAmbiguous);
            }
            match confirmations {
                0 => reasons.push(IneligibilityReason::NoConfirmationReceipt),
                1 => {}
                _ => reasons.push(IneligibilityReason::ConfirmationMultiplicity),
            }
            let eligibility = Eligibility::from_reasons(reasons);
            expected.push(canonical_candidate(
                &candidate.opportunity_id,
                &eligibility,
                provenance.as_ref(),
            ));
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
                market_age_nanos: market_age,
                receipt_age_nanos: receipt_age,
                confirmation_receipts: confirmations,
                eligibility,
            });
        }
        expected.sort();
        // The expected identity set goes to disk BEFORE any candidate row, so
        // the persisted rows are checked against a declaration that did not
        // come from them.
        let begin = ObservationRecord::WindowBegin {
            run_id: self.run_id.clone(),
            window_id: input.window_id.clone(),
            watermark,
            expected,
        };
        self.emit(&begin, false);
        let entry_count = records.len() as u64;
        for record in &records {
            self.emit(record, false);
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
            watermark,
            processing_started_mono_nanos: processing_mono,
            rank_completed_mono_nanos: rank_mono,
            source_lag_invalid,
            mapping_ambiguous,
        };
        self.emit(&close, false);
    }

    fn on_finish(&mut self, at: DateTime<Utc>) {
        // Drain before reading the counters. With an asynchronous sink,
        // `written` lags acceptance, so terminal counters read without a
        // successful drain would understate what reached the disk -- and a
        // drain that timed out means they are not final at all, which is
        // itself a failed write rather than something to paper over.
        if self.sink.drain(Duration::from_secs(5)).is_err() {
            self.failed_writes = self.failed_writes.saturating_add(1);
        }
        let counters = self.sink.counters();
        let telemetry = self.sink.telemetry();
        let end = ObservationRecord::RunEnd {
            run_id: self.run_id.clone(),
            ended_at: at,
            counters,
            telemetry,
            stopped: self.stopped,
            capture_bytes: self.capture_bytes,
            capture_max_bytes: self.capture_max_bytes,
        };
        self.emit(&end, true);
        let run_id = self.run_id.clone();
        if self.sink.close(&run_id, at).is_err() {
            self.failed_writes = self.failed_writes.saturating_add(1);
        }
    }
}


// ---------------------------------------------------------------------------
// Rotation
// ---------------------------------------------------------------------------

/// Default rotation size. A session-length capture in one file makes the
/// reader hold the whole thing to authenticate it; rotation bounds that, and
/// bounds how much a single unclosed file can cost at a crash.
pub const DEFAULT_ROTATE_BYTES: u64 = 256 * 1024 * 1024;

/// File name for a run's `index`-th file.
pub fn rotation_file_name(index: u32) -> String {
    format!("observations-{index}.ndjson")
}

/// The numeric suffix of a rotation file name, for ordering.
///
/// Files that do not match the pattern sort last rather than being rejected
/// here: acquisition reports what is present and the chain check decides.
pub fn rotation_index(name: &str) -> u64 {
    name.strip_suffix(OBSERVATION_FILE_SUFFIX)
        .and_then(|stem| stem.rsplit('-').next())
        .and_then(|digits| digits.parse::<u64>().ok())
        .unwrap_or(u64::MAX)
}

/// A sink that rolls to a new file on a byte threshold.
///
/// The invariant that makes rotation safe for evidence: **a file is only ever
/// left in one of two states.** Either it carries a terminal record naming its
/// successor -- closed, fsynced, immutable, and provably not the end of the
/// run -- or it carries no terminal record at all and the reader refuses it.
/// There is no state in which a rotated-away file looks complete but has lost
/// its tail, because the terminal record is written and flushed before the
/// next file exists.
///
/// The chain is doubly linked on purpose. The old file names its successor and
/// the new file names its predecessor, so a missing middle file breaks two
/// links rather than silently shortening the run into something that still
/// looks well-formed.
pub struct RotatingSink {
    dir: PathBuf,
    run_id: String,
    index: u32,
    rotate_bytes: u64,
    bytes_in_file: u64,
    queue_capacity: usize,
    byte_capacity: u64,
    current: Option<AsyncSink>,
    /// Counters accumulated across files already closed, so the run's totals
    /// survive rotation.
    retired: WriterCounters,
    retired_dropped_bytes: u64,
    retired_spans: Vec<LossSpan>,
    retired_spans_truncated: bool,
    files: Vec<String>,
}

impl RotatingSink {
    pub fn create(
        dir: &Path,
        run_id: &str,
        rotate_bytes: u64,
        queue_capacity: usize,
        byte_capacity: u64,
    ) -> std::io::Result<Self> {
        let name = rotation_file_name(0);
        let writer = FileRecordWriter::create(dir, &name)?;
        let current = AsyncSink::with_capacity(
            &name,
            Box::new(writer),
            queue_capacity,
            byte_capacity,
        );
        Ok(Self {
            dir: dir.to_path_buf(),
            run_id: run_id.to_string(),
            index: 0,
            rotate_bytes,
            bytes_in_file: 0,
            queue_capacity,
            byte_capacity,
            current: Some(current),
            retired: WriterCounters::default(),
            retired_dropped_bytes: 0,
            retired_spans: Vec::new(),
            retired_spans_truncated: false,
            files: vec![name],
        })
    }

    pub fn files(&self) -> &[String] {
        &self.files
    }

    pub fn current_file(&self) -> &str {
        self.files.last().map(|s| s.as_str()).unwrap_or("")
    }

    fn accumulate(&mut self, sink: &AsyncSink) {
        let c = sink.counters();
        self.retired.attempted = self.retired.attempted.saturating_add(c.attempted);
        self.retired.written = self.retired.written.saturating_add(c.written);
        self.retired.dropped = self.retired.dropped.saturating_add(c.dropped);
        self.retired.write_errors = self.retired.write_errors.saturating_add(c.write_errors);
        self.retired.overflowed |= c.overflowed;
        let t = sink.telemetry_snapshot();
        self.retired_dropped_bytes = self.retired_dropped_bytes.saturating_add(t.dropped_bytes);
        self.retired_spans_truncated |= t.loss_spans_truncated;
        for span in t.loss_spans {
            if self.retired_spans.len() < MAX_LOSS_SPANS {
                self.retired_spans.push(span);
            } else {
                self.retired_spans_truncated = true;
            }
        }
    }

    /// Closes the current file naming its successor, then opens that successor
    /// and writes its opening record.
    ///
    /// Order matters and is not arbitrary: the old file is fully written,
    /// flushed and fsynced **before** the new one is created. A crash between
    /// the two leaves a complete closed file plus no successor, which the
    /// chain check reports as a truncated run -- not as a complete one.
    fn rotate(&mut self, at: DateTime<Utc>) -> std::io::Result<()> {
        let next_index = self.index + 1;
        let next_name = rotation_file_name(next_index);
        let previous_name = self.current_file().to_string();
        if let Some(mut sink) = self.current.take() {
            sink.close_rotating(&self.run_id, at, &next_name)?;
            self.accumulate(&sink);
        }
        let writer = FileRecordWriter::create(&self.dir, &next_name)?;
        let mut sink = AsyncSink::with_capacity(
            &next_name,
            Box::new(writer),
            self.queue_capacity,
            self.byte_capacity,
        );
        let start = ObservationRecord::FileStart {
            run_id: self.run_id.clone(),
            file_name: next_name.clone(),
            sequence: next_index,
            previous_file: previous_name,
        };
        sink.write(&start)?;
        self.current = Some(sink);
        self.index = next_index;
        self.bytes_in_file = 0;
        self.files.push(next_name);
        Ok(())
    }
}

impl ObservationSink for RotatingSink {
    fn write(&mut self, record: &ObservationRecord) -> std::io::Result<()> {
        let line = serde_json::to_string(record)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        self.write_serialized(record, &line)
    }

    /// Rotation bounds a *file*; it never bounds the capture. The capture-level
    /// budget is enforced by the observer across every file of the run, so
    /// rotating cannot be used to walk past it.
    fn write_serialized(&mut self, record: &ObservationRecord, line: &str) -> std::io::Result<()> {
        let size = line.len() as u64 + 1;
        // Rotate before writing, never mid-record, so no row can straddle two
        // files and no row can be written into a file that is about to be
        // closed behind it.
        if self.bytes_in_file > 0 && self.bytes_in_file + size > self.rotate_bytes {
            self.rotate(Utc::now())?;
        }
        let Some(sink) = self.current.as_mut() else {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "sink already closed"));
        };
        let result = sink.write_serialized(record, line);
        // Counted whether or not the write succeeded: a dropped record still
        // consumed its place in the stream's accounting, and rotating on
        // accepted bytes alone would make the threshold depend on loss.
        self.bytes_in_file += size;
        result
    }

    fn counters(&self) -> WriterCounters {
        let mut total = self.retired;
        if let Some(sink) = self.current.as_ref() {
            let c = sink.counters();
            total.attempted = total.attempted.saturating_add(c.attempted);
            total.written = total.written.saturating_add(c.written);
            total.dropped = total.dropped.saturating_add(c.dropped);
            total.write_errors = total.write_errors.saturating_add(c.write_errors);
            total.overflowed |= c.overflowed;
        }
        total
    }

    fn drain(&mut self, timeout: Duration) -> std::io::Result<()> {
        match self.current.as_mut() {
            Some(sink) => sink.drain(timeout),
            None => Ok(()),
        }
    }

    fn telemetry(&self) -> Option<WriterTelemetry> {
        let current = self.current.as_ref().map(|s| s.telemetry_snapshot());
        let mut spans = self.retired_spans.clone();
        let mut truncated = self.retired_spans_truncated;
        let mut dropped_bytes = self.retired_dropped_bytes;
        let (mut queue_peak, mut bytes_peak) = (0, 0);
        if let Some(t) = current {
            dropped_bytes = dropped_bytes.saturating_add(t.dropped_bytes);
            truncated |= t.loss_spans_truncated;
            queue_peak = t.queue_peak;
            bytes_peak = t.queued_bytes_peak;
            for span in t.loss_spans {
                if spans.len() < MAX_LOSS_SPANS {
                    spans.push(span);
                } else {
                    truncated = true;
                }
            }
        }
        Some(WriterTelemetry {
            queue_capacity: self.queue_capacity as u64,
            byte_capacity: self.byte_capacity,
            queue_peak,
            queued_bytes_peak: bytes_peak,
            dropped_bytes,
            loss_spans: spans,
            loss_spans_truncated: truncated,
        })
    }

    fn close(&mut self, run_id: &str, at: DateTime<Utc>) -> std::io::Result<()> {
        match self.current.take() {
            // `next_file` stays `None`, which is what marks this the end of the
            // run rather than another rotation.
            Some(mut sink) => {
                let result = sink.close(run_id, at);
                self.accumulate(&sink);
                result
            }
            None => Err(std::io::Error::new(std::io::ErrorKind::Other, "sink already closed")),
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
    /// A `.close-failed` marker is present: a terminal record's own flush or
    /// fsync failed and could not be retracted. The run is refused outright.
    CloseFailed { marker: String },
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
            Self::CloseFailed { marker } => {
                write!(f, "{marker}: a terminal record's durability failed")
            }
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
        if name.ends_with(CLOSE_FAILED_SUFFIX) {
            return Err(AcquisitionError::CloseFailed { marker: name });
        }
        if name.ends_with(OBSERVATION_FILE_SUFFIX)
            && entry.file_type().map_err(AcquisitionError::Io)?.is_file()
        {
            names.push(name);
        }
    }
    // Numeric order, not lexicographic. `observations-10.ndjson` sorts before
    // `observations-2.ndjson` as text, which would present a rotated run's
    // files out of order once it exceeds ten -- reachable on a multi-day
    // capture. The chain check below would catch it, but as a confusing
    // "broken chain" rather than the ordering problem it is.
    names.sort_by_key(|name| (rotation_index(name), name.clone()));
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

    // One parse pass, not two. A malformed line is *remembered* rather than
    // returned immediately, because whether it is an error depends on
    // something only the whole file can answer: an unclosed file is allowed to
    // end in torn content, a closed one is not.
    let mut closed = false;
    let mut declared = None;
    let mut close_index = None;
    let mut duplicate_close = false;
    let mut first_malformed: Option<usize> = None;
    let mut parsed: Vec<ObservationRecord> = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        match serde_json::from_str::<ObservationRecord>(line) {
            Ok(record) => {
                if let ObservationRecord::FileClose { records_written, .. } = &record {
                    if closed {
                        duplicate_close = true;
                    } else {
                        closed = true;
                        declared = Some(*records_written);
                        close_index = Some(i);
                    }
                }
                parsed.push(record);
            }
            Err(_) => {
                if first_malformed.is_none() {
                    first_malformed = Some(i + 1);
                }
                // Keeps indices aligned with line numbers for the checks below.
                parsed.push(ObservationRecord::Lag {
                    run_id: String::new(),
                    sequence: 0,
                    skipped: 0,
                    at: DateTime::<Utc>::MIN_UTC,
                });
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
    if let Some(line) = first_malformed {
        return Err(AcquisitionError::MalformedLine { file: name.to_string(), line });
    }
    Ok(FileEvidence {
        name: name.to_string(),
        records: parsed,
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
    /// The rotation chain does not form a single unbroken sequence.
    ///
    /// A missing middle file, a file that names the wrong predecessor or
    /// successor, a second run-start, or a run that ended without a terminal
    /// `next_file: None`. Each would otherwise present a truncated run as a
    /// complete one.
    BrokenRotationChain { detail: String },
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
            Self::BrokenRotationChain { detail } => write!(f, "rotation chain broken: {detail}"),
        }
    }
}

impl std::error::Error for AuthenticationFailure {}


/// Checks that the acquired files form one unbroken rotation chain.
///
/// Both directions are checked because either alone can be satisfied by an
/// incomplete run. Backward-only (`previousFile`) accepts a chain whose tail
/// was deleted; forward-only (`nextFile`) accepts one whose head was. Together
/// they pin every file between the run's start and its end.
fn verify_rotation_chain(acquired: &AcquiredCapture) -> Result<(), AuthenticationFailure> {
    let broken = |detail: String| AuthenticationFailure::BrokenRotationChain { detail };
    let files = acquired.files();

    // Per file: its opening record, its terminal successor claim, and whether
    // it carries the run's start.
    let mut view: Vec<(&str, Option<(u32, &str)>, Option<&str>, bool)> = Vec::new();
    for file in files {
        let mut start: Option<(u32, &str)> = None;
        let mut next: Option<&str> = None;
        let mut has_run_start = false;
        for record in &file.records {
            match record {
                ObservationRecord::FileStart { file_name, sequence, previous_file, .. } => {
                    if start.is_some() {
                        return Err(broken(format!("{}: more than one opening record", file.name)));
                    }
                    if *file_name != file.name {
                        return Err(broken(format!(
                            "{}: opening record names {file_name}",
                            file.name
                        )));
                    }
                    start = Some((*sequence, previous_file.as_str()));
                }
                ObservationRecord::FileClose { next_file, .. } => {
                    next = next_file.as_deref();
                }
                ObservationRecord::RunStart { .. } => has_run_start = true,
                _ => {}
            }
        }
        view.push((file.name.as_str(), start, next, has_run_start));
    }

    // Exactly one head: the file with the run's start and no opening record.
    let heads: Vec<&str> =
        view.iter().filter(|(_, start, _, run)| *run && start.is_none()).map(|v| v.0).collect();
    if heads.len() != 1 {
        return Err(broken(format!("expected exactly one head file, found {}", heads.len())));
    }
    if view[0].0 != heads[0] {
        return Err(broken(format!("head {} is not the first acquired file", heads[0])));
    }

    // Walk it. Every link is checked from both ends.
    for i in 0..view.len() {
        let (name, start, next, _) = view[i];
        if i > 0 {
            let Some((sequence, previous)) = start else {
                return Err(broken(format!("{name}: rotated file has no opening record")));
            };
            if sequence as usize != i {
                return Err(broken(format!("{name}: opening sequence {sequence}, expected {i}")));
            }
            if previous != view[i - 1].0 {
                return Err(broken(format!(
                    "{name}: names predecessor {previous}, acquired predecessor is {}",
                    view[i - 1].0
                )));
            }
        }
        match (next, view.get(i + 1)) {
            (Some(named), Some((actual, _, _, _))) if named == *actual => {}
            (Some(named), Some((actual, _, _, _))) => {
                return Err(broken(format!("{name}: names successor {named}, acquired {actual}")))
            }
            // A successor was named and is not here: the run continued into a
            // file that was not acquired. Refused rather than treated as the
            // end, which is the whole point of naming it.
            (Some(named), None) => {
                return Err(broken(format!("{name}: names successor {named}, which is missing")))
            }
            (None, Some((actual, _, _, _))) => {
                return Err(broken(format!(
                    "{name}: ends the run, but {actual} was acquired after it"
                )))
            }
            (None, None) => {}
        }
    }
    Ok(())
}

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

    verify_rotation_chain(&acquired)?;

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
            ObservationRecord::WindowBegin { .. }
            | ObservationRecord::Stopped { .. }
            | ObservationRecord::FileStart { .. }
            | ObservationRecord::FileClose { .. } => {}
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
    /// Candidates carrying at least one confirmation receipt at the anchor.
    ///
    /// The population every rate below is defined over: consumer-received
    /// **detector-confirmed** candidates. A candidate with no confirmation is
    /// outside the question entirely, not a negative observation within it.
    pub detector_confirmed: u64,
    /// Detector-confirmed candidates for which price provenance could be
    /// established, i.e. the engine carried a price to agree with.
    pub provenance_establishable: u64,
    /// Detector-confirmed, provenance-establishable candidates within both age
    /// bounds -- freshness alone, ignoring confirmation multiplicity and
    /// lifecycle ambiguity.
    pub fresh: u64,
    /// **PRIMARY safeguard statistic.** Eligible / provenance-establishable.
    ///
    /// The denominator is provenance-establishable rather than all candidates
    /// because a candidate whose provenance cannot be established cannot
    /// validly be classified as fresh or stale at all. Counting it as a
    /// failure would let missing measurement coverage masquerade as a negative
    /// eligibility observation, which is the one substitution this rate exists
    /// to prevent. `NaN` on a zero denominator: an absent rate is not a rate
    /// of zero.
    pub eligibility_rate: f64,
    /// Provenance-establishable / detector-confirmed.
    ///
    /// The companion that stops the primary rate hiding poor coverage. A high
    /// `eligibility_rate` over a tiny establishable subset is not a good
    /// result, and reporting the two together is what makes that visible.
    /// Neither substitutes for the other.
    pub provenance_establishment_rate: f64,
    /// Fresh / provenance-establishable.
    ///
    /// Distinct from `eligibility_rate`: eligibility requires every clause,
    /// freshness only the age bounds. A gap between the two is confirmation
    /// multiplicity or lifecycle ambiguity rather than staleness, and
    /// collapsing them would hide which of those is happening.
    pub freshness_eligibility_rate: f64,
    /// Negative market ages by `sourceEventType`, suffixed `+derived` where the
    /// finalised-bar interval correction produced the market time.
    ///
    /// Present so the two explanations for a negative age stay separable in the
    /// certificate itself rather than requiring a re-read of the stream.
    pub negative_market_age_sources: BTreeMap<String, u64>,
    pub upstream_skipped_events: u64,
    /// Windows invalidated as a whole (frozen L1 per-window `reconcile`
    /// failures): by lag, by ambiguous mapping, or both. None of their
    /// candidates is eligible -- the certificate refuses otherwise -- and none
    /// can serve as a first-eligible window.
    pub invalid_windows: u64,
    pub invalid_windows_by_reason: BTreeMap<String, u64>,
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
    /// Observation stopped before the run ended (capture budget exceeded or
    /// receive sequence exhausted). The capture is incomplete by definition.
    CaptureStopped { reason: StopReason },
    /// A window has candidate rows or a terminal record but no `window_begin`,
    /// so there is no expected identity set to reconcile against.
    MissingWindowBegin { window_id: String },
    /// More than one `window_begin` for one window.
    DuplicateWindowBegin { window_id: String },
    /// The persisted rows are not exactly the declared expected set. Equal
    /// counts do not help: a substituted row is refused here.
    WindowSetMismatch { window_id: String, expected: u64, persisted: u64, differing: u64 },
    /// A candidate in a window invalidated by lag or ambiguity claims to be
    /// eligible.
    EligibleInInvalidWindow { window_id: String, opportunity_id: String },
    /// A lag is recorded but the next window was not marked lag-invalid.
    LagNotApplied { window_id: String },
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
            Self::CaptureStopped { reason } => write!(f, "observation stopped early: {reason:?}"),
            Self::MissingWindowBegin { window_id } => {
                write!(f, "window {window_id} has no expected identity set")
            }
            Self::DuplicateWindowBegin { window_id } => {
                write!(f, "window {window_id} declared its expected set more than once")
            }
            Self::WindowSetMismatch { window_id, expected, persisted, differing } => write!(
                f,
                "window {window_id} persisted set differs from expected ({persisted} persisted, {expected} expected, {differing} differing)"
            ),
            Self::EligibleInInvalidWindow { window_id, opportunity_id } => write!(
                f,
                "window {window_id} is invalid but {opportunity_id} is marked eligible"
            ),
            Self::LagNotApplied { window_id } => {
                write!(f, "a lag preceded window {window_id} but it was not invalidated")
            }
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
        // A stopped capture is incomplete however tidy the rest looks. Both
        // the stop record and the terminal record's copy are checked, so the
        // stop is caught even if one of them was lost.
        for record in capture.acquired().records() {
            match record {
                ObservationRecord::Stopped { reason, .. }
                | ObservationRecord::RunEnd { stopped: Some(reason), .. } => {
                    return Err(CertificateRefusal::CaptureStopped { reason: *reason })
                }
                _ => {}
            }
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
        let mut establishable = 0u64;
        let mut detector_confirmed = 0u64;
        let mut fresh = 0u64;
        let mut by_reason: BTreeMap<String, u64> = BTreeMap::new();
        let mut negative_sources: BTreeMap<String, u64> = BTreeMap::new();
        for record in capture.candidates() {
            let ObservationRecord::Candidate {
                window_id,
                opportunity_id,
                eligibility,
                provenance,
                confirmation_receipts,
                ..
            } = record
            else {
                continue;
            };
            // The population is detector-confirmed candidates. A candidate
            // with no confirmation receipt is outside the question, not a
            // negative answer to it.
            let confirmed = *confirmation_receipts >= 1;
            if confirmed {
                detector_confirmed += 1;
            }
            if let Some(p) = provenance {
                if confirmed {
                    establishable += 1;
                    // Freshness alone: the age clauses, independent of
                    // multiplicity or ambiguity.
                    let stale = eligibility.reasons.iter().any(|r| {
                        matches!(
                            r,
                            IneligibilityReason::MarketAgeExceeded
                                | IneligibilityReason::ReceiptAgeExceeded
                                | IneligibilityReason::NegativeMarketAge
                                | IneligibilityReason::PriceReceivedAfterProcessingStart
                        )
                    });
                    if !stale {
                        fresh += 1;
                    }
                }
                if eligibility.reasons.contains(&IneligibilityReason::NegativeMarketAge) {
                    let key = if p.market_time_derived {
                        format!("{}+derived", p.source_event_type)
                    } else {
                        p.source_event_type.clone()
                    };
                    *negative_sources.entry(key).or_insert(0) += 1;
                }
            }
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

        // Clause 6, exact: EXPECTED == PERSISTED as sorted canonical tuples, per
        // window. EMITTED is the observer's own row count (`entry_count`),
        // already reconciled above against both; with zero writer loss, exact
        // equality of the persisted rows to the declaration is what makes
        // emitted == persisted an identity rather than a count coincidence.
        //
        // Lag and ambiguity (frozen L1 per-window `reconcile`): the window is
        // invalid as a whole. Checked from the records, not trusted from the
        // observer's verdicts: a lag must be followed by a lag-invalid window,
        // and nothing in an invalid window may be eligible.
        let mut expected_sets: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut persisted_sets: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut invalid: BTreeMap<String, (bool, bool)> = BTreeMap::new();
        let mut eligible_by_window: Vec<(String, String)> = Vec::new();
        let mut lag_pending = false;
        for record in capture.acquired().records() {
            match record {
                ObservationRecord::WindowBegin { window_id, expected, .. } => {
                    let mut sorted = expected.clone();
                    sorted.sort();
                    if expected_sets.insert(window_id.clone(), sorted).is_some() {
                        return Err(CertificateRefusal::DuplicateWindowBegin {
                            window_id: window_id.clone(),
                        });
                    }
                }
                ObservationRecord::Candidate {
                    window_id, opportunity_id, eligibility, provenance, ..
                } => {
                    persisted_sets.entry(window_id.clone()).or_default().push(canonical_candidate(
                        opportunity_id,
                        eligibility,
                        provenance.as_ref(),
                    ));
                    if eligibility.eligible {
                        eligible_by_window.push((window_id.clone(), opportunity_id.clone()));
                    }
                }
                ObservationRecord::Lag { .. } => lag_pending = true,
                ObservationRecord::WindowClose {
                    window_id, source_lag_invalid, mapping_ambiguous, ..
                } => {
                    if lag_pending && !source_lag_invalid {
                        return Err(CertificateRefusal::LagNotApplied {
                            window_id: window_id.clone(),
                        });
                    }
                    lag_pending = false;
                    if *source_lag_invalid || *mapping_ambiguous {
                        invalid.insert(window_id.clone(), (*source_lag_invalid, *mapping_ambiguous));
                    }
                }
                _ => {}
            }
        }
        for w in &windows {
            let Some(expected) = expected_sets.get(&w.window_id) else {
                return Err(CertificateRefusal::MissingWindowBegin { window_id: w.window_id.clone() });
            };
            let mut persisted = persisted_sets.remove(&w.window_id).unwrap_or_default();
            persisted.sort();
            if persisted != *expected {
                let e: BTreeSet<&String> = expected.iter().collect();
                let p: BTreeSet<&String> = persisted.iter().collect();
                return Err(CertificateRefusal::WindowSetMismatch {
                    window_id: w.window_id.clone(),
                    expected: expected.len() as u64,
                    persisted: persisted.len() as u64,
                    differing: e.symmetric_difference(&p).count() as u64,
                });
            }
        }
        for (window_id, opportunity_id) in &eligible_by_window {
            if invalid.contains_key(window_id) {
                return Err(CertificateRefusal::EligibleInInvalidWindow {
                    window_id: window_id.clone(),
                    opportunity_id: opportunity_id.clone(),
                });
            }
        }
        let mut invalid_by_reason: BTreeMap<String, u64> = BTreeMap::new();
        for (lag, ambiguous) in invalid.values() {
            if *lag {
                *invalid_by_reason.entry("source_lag".to_string()).or_insert(0) += 1;
            }
            if *ambiguous {
                *invalid_by_reason.entry("mapping_ambiguous".to_string()).or_insert(0) += 1;
            }
        }

        // An absent denominator yields NaN, not zero. A rate of zero says
        // "nothing was fresh"; NaN says "there was nothing to ask about", and
        // collapsing the two would let absence read as a failed safeguard --
        // or worse, a passed one.
        let rate = |num: u64, den: u64| {
            if den == 0 {
                f64::NAN
            } else {
                num as f64 / den as f64
            }
        };
        let eligibility_rate = rate(eligible, establishable);
        let provenance_establishment_rate = rate(establishable, detector_confirmed);
        let freshness_eligibility_rate = rate(fresh, establishable);
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
            detector_confirmed,
            provenance_establishable: establishable,
            fresh,
            eligibility_rate,
            provenance_establishment_rate,
            freshness_eligibility_rate,
            negative_market_age_sources: negative_sources,
            upstream_skipped_events: capture.report().upstream_skipped_events,
            invalid_windows: invalid.len() as u64,
            invalid_windows_by_reason: invalid_by_reason,
            byte_integrity_established: false,
            crash_durability_established: false,
        })
    }
}

// ---------------------------------------------------------------------------
// Three-valued assessment
// ---------------------------------------------------------------------------

/// Why a capture could not be decided either way.
///
/// Separate from refusal on purpose. "The evidence contradicts the contract"
/// and "there is no evidence to apply the contract to" are different findings,
/// and a system that reports them identically will eventually report the second
/// as the first -- or, far worse, let absence pass as success.
#[derive(Debug, PartialEq)]
pub enum Indeterminate {
    /// No observation files at all.
    NoEvidence { detail: String },
    /// Files exist but the set is incomplete: something is still open, or the
    /// run never recorded its end.
    EvidenceIncomplete { detail: String },
    /// A complete, consistent capture that contains no ranking window. Nothing
    /// is wrong with it; there is simply nothing to certify.
    NoWindows,
}

impl std::fmt::Display for Indeterminate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoEvidence { detail } => write!(f, "no evidence: {detail}"),
            Self::EvidenceIncomplete { detail } => write!(f, "evidence incomplete: {detail}"),
            Self::NoWindows => write!(f, "no ranking windows to certify"),
        }
    }
}

/// The verdict on a capture directory.
#[derive(Debug)]
pub enum CaptureVerdict {
    Pass(Box<Certificate>),
    /// The evidence exists and contradicts the contract.
    Fail(String),
    /// The evidence needed to decide is absent or incomplete.
    Indeterminate(Indeterminate),
}

impl CaptureVerdict {
    /// The only accessor that should gate anything downstream.
    ///
    /// `Indeterminate` deliberately answers `false`: fail closed where the
    /// protocol requires evidence that is absent.
    pub fn is_pass(&self) -> bool {
        matches!(self, Self::Pass(_))
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Pass(_) => "PASS",
            Self::Fail(_) => "FAIL",
            Self::Indeterminate(_) => "INDETERMINATE",
        }
    }
}

/// Acquires, authenticates and certifies a run directory in one pass,
/// classifying the result three ways.
///
/// The split between `Fail` and `Indeterminate` follows one rule: evidence
/// that is *present and wrong* fails; evidence that is *missing or unfinished*
/// is indeterminate. An unclosed file is indeterminate because a run that is
/// still going has not yet failed anything; a malformed line in a closed file
/// is a failure because the file claimed to be complete and is not.
pub fn assess(run_dir: &Path) -> CaptureVerdict {
    let acquired = match acquire(run_dir) {
        Ok(a) => a,
        Err(e @ AcquisitionError::NoFiles { .. }) => {
            return CaptureVerdict::Indeterminate(Indeterminate::NoEvidence {
                detail: e.to_string(),
            })
        }
        Err(e @ AcquisitionError::Io(_)) => {
            return CaptureVerdict::Indeterminate(Indeterminate::NoEvidence {
                detail: e.to_string(),
            })
        }
        Err(other) => return CaptureVerdict::Fail(other.to_string()),
    };
    let authed = match authenticate(acquired) {
        Ok(a) => a,
        Err(
            e @ (AuthenticationFailure::OpenFilePresent { .. }
            | AuthenticationFailure::MissingRunEnd
            | AuthenticationFailure::MissingRunStart),
        ) => {
            return CaptureVerdict::Indeterminate(Indeterminate::EvidenceIncomplete {
                detail: e.to_string(),
            })
        }
        Err(other) => return CaptureVerdict::Fail(other.to_string()),
    };
    match Certificate::issue(&authed) {
        Ok(certificate) => CaptureVerdict::Pass(Box::new(certificate)),
        Err(CertificateRefusal::NoWindows) => {
            CaptureVerdict::Indeterminate(Indeterminate::NoWindows)
        }
        Err(other) => CaptureVerdict::Fail(other.to_string()),
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
        IneligibilityReason::PriceReceivedAfterProcessingStart => {
            "price_received_after_processing_start"
        }
        IneligibilityReason::RankBracketInverted => "rank_bracket_inverted",
        IneligibilityReason::NoConfirmationReceipt => "no_confirmation_receipt",
        IneligibilityReason::ConfirmationMultiplicity => "confirmation_multiplicity",
        IneligibilityReason::ConfirmationOrderingAmbiguous => "confirmation_ordering_ambiguous",
        IneligibilityReason::AmbiguousLifecycleMapping => "ambiguous_lifecycle_mapping",
        IneligibilityReason::WindowMappingAmbiguous => "window_mapping_ambiguous",
        IneligibilityReason::WindowSourceLag => "window_source_lag",
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
    // Provenance that cannot be established stops the observer rather than
    // being invented: no `unknown` namespace, no rewritten one.
    let namespace = match resolve_namespace() {
        Ok(ns) => ns,
        Err(e) => {
            tracing::warn!(error = %e, "observation namespace unavailable or invalid; observation off for this process");
            return None;
        }
    };
    let capture_max_bytes = match std::env::var(ENV_MAX_BYTES) {
        Err(_) => DEFAULT_CAPTURE_MAX_BYTES,
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(n) if n > 0 => n,
            _ => {
                tracing::warn!(value = %raw, "{} is not a positive byte count; observation off for this process", ENV_MAX_BYTES);
                return None;
            }
        },
    };
    let pid = std::process::id();
    let started_at = Utc::now();
    let run = match ObserverRun::allocate(&root, &namespace, started_at, pid) {
        Ok(run) => run,
        Err(e) => {
            tracing::warn!(error = %e, root = %root.display(), "observation run allocation failed; observation off for this process");
            return None;
        }
    };
    // Asynchronous, bounded, non-blocking. `FileSink` remains for offline and
    // test use, but nothing that shares a thread with the market-data consumer
    // may write to a disk inline: a slow or failing disk has to cost counted
    // dropped records, never consumer latency.
    let file_writer = match FileRecordWriter::create(run.dir(), RUN_FILE_NAME) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!(error = %e, dir = %run.dir().display(), "observation file creation failed; observation off for this process");
            return None;
        }
    };
    let sink = AsyncSink::new(RUN_FILE_NAME, Box::new(file_writer));
    match Observer::start(&run, &namespace, pid, started_at, Box::new(sink)) {
        Ok(observer) => {
            let observer = observer.with_capture_max_bytes(capture_max_bytes);
            tracing::info!(
                run_id = %run.id(),
                dir = %run.dir().display(),
                protocol = PROTOCOL_VERSION,
                freshness_max_age_secs = FRESHNESS_MAX_AGE_SECS,
                capture_max_bytes,
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


// ---------------------------------------------------------------------------
// Step 4 preregistration
// ---------------------------------------------------------------------------
//
// The point of a preregistration record is that it is fixed *before* the
// evidence exists and cannot be quietly revised afterwards. That is a property
// of process, not of a type -- but a type can make the revision visible, and
// that is what this does: the record is content-addressed, a capture names the
// preregistration it was run under, and a certificate that cannot match the two
// says so instead of assuming.

/// A frozen Step-4 preregistration.
///
/// Every field here is a decision that could otherwise be made after seeing
/// the data. `NaN` is not permitted in the floors: an unset floor must be
/// absent by construction, not a value that silently compares false.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preregistration {
    /// The protocol this preregistration is written against.
    pub protocol_version: String,
    /// Free-form identity of the frozen design gate, e.g. the Step-3 verdict's
    /// SHA-256. Carried so a preregistration cannot be read as applying to a
    /// contract it was not written against.
    pub gate_sha256: String,
    /// Minimum acceptable `eligibilityRate`, over provenance-establishable
    /// detector-confirmed candidates.
    pub eligibility_floor: f64,
    /// Minimum acceptable `provenanceEstablishmentRate`. A separate quantity
    /// with a separate floor: coverage and eligibility fail for different
    /// reasons and must not share a threshold.
    pub provenance_establishment_floor: f64,
    /// Freshness bound in force, which must equal the implementation's.
    pub freshness_max_age_secs: i64,
    /// Successive freshness bounds to re-freeze to if the eligibility floor is
    /// breached, in order, and the maximum number of re-freezes permitted.
    ///
    /// Declared in advance so a breach cannot become an open-ended search for
    /// a threshold that passes. An empty ladder means one attempt only.
    pub refreeze_ladder_secs: Vec<i64>,
    pub max_refreezes: u32,
}

#[derive(Debug, PartialEq)]
pub enum PreregistrationError {
    ProtocolMismatch { found: String, expected: &'static str },
    /// A floor is NaN, negative, or above 1.
    FloorOutOfRange { field: &'static str, value: f64 },
    /// The preregistered freshness bound is not the one the code enforces, so
    /// the record describes a different experiment from the one that would run.
    FreshnessMismatch { preregistered: i64, implemented: i64 },
    /// A re-freeze ladder that does not strictly loosen, or loosens without a
    /// bound on how many times.
    LadderNotMonotonic { previous: i64, next: i64 },
    LadderWithoutBudget,
    EmptyGate,
}

impl std::fmt::Display for PreregistrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProtocolMismatch { found, expected } => {
                write!(f, "preregistered protocol {found} is not {expected}")
            }
            Self::FloorOutOfRange { field, value } => {
                write!(f, "{field} = {value} is not a proportion in [0, 1]")
            }
            Self::FreshnessMismatch { preregistered, implemented } => write!(
                f,
                "preregistered freshness {preregistered}s does not match the implemented {implemented}s"
            ),
            Self::LadderNotMonotonic { previous, next } => {
                write!(f, "re-freeze ladder is not strictly increasing: {previous} then {next}")
            }
            Self::LadderWithoutBudget => {
                write!(f, "a re-freeze ladder needs a non-zero maxRefreezes")
            }
            Self::EmptyGate => write!(f, "gateSha256 is empty"),
        }
    }
}

impl std::error::Error for PreregistrationError {}

impl Preregistration {
    /// Checks a preregistration is internally coherent and describes the
    /// experiment this build would actually run.
    ///
    /// It cannot check the thing that matters most -- that the record predates
    /// the evidence. Only the process can establish that, which is why the
    /// record is content-addressed and a capture names it.
    pub fn validate(&self) -> Result<(), PreregistrationError> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(PreregistrationError::ProtocolMismatch {
                found: self.protocol_version.clone(),
                expected: PROTOCOL_VERSION,
            });
        }
        if self.gate_sha256.trim().is_empty() {
            return Err(PreregistrationError::EmptyGate);
        }
        for (field, value) in [
            ("eligibilityFloor", self.eligibility_floor),
            ("provenanceEstablishmentFloor", self.provenance_establishment_floor),
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(PreregistrationError::FloorOutOfRange { field, value });
            }
        }
        if self.freshness_max_age_secs != FRESHNESS_MAX_AGE_SECS {
            return Err(PreregistrationError::FreshnessMismatch {
                preregistered: self.freshness_max_age_secs,
                implemented: FRESHNESS_MAX_AGE_SECS,
            });
        }
        if !self.refreeze_ladder_secs.is_empty() && self.max_refreezes == 0 {
            return Err(PreregistrationError::LadderWithoutBudget);
        }
        let mut previous = self.freshness_max_age_secs;
        for next in &self.refreeze_ladder_secs {
            if *next <= previous {
                return Err(PreregistrationError::LadderNotMonotonic { previous, next: *next });
            }
            previous = *next;
        }
        Ok(())
    }

    /// Applies the floors to a certificate.
    ///
    /// Deliberately three-valued for the same reason `assess` is: a rate that
    /// could not be computed has not failed a floor, and reporting it as a
    /// breach would turn missing coverage into a finding about freshness.
    pub fn evaluate(&self, certificate: &Certificate) -> FloorVerdict {
        let coverage = certificate.provenance_establishment_rate;
        let eligibility = certificate.eligibility_rate;
        if coverage.is_nan() {
            return FloorVerdict::Indeterminate {
                detail: "no detector-confirmed candidates; coverage is undefined".to_string(),
            };
        }
        if coverage < self.provenance_establishment_floor {
            return FloorVerdict::CoverageBreach {
                observed: coverage,
                floor: self.provenance_establishment_floor,
            };
        }
        if eligibility.is_nan() {
            return FloorVerdict::Indeterminate {
                detail: "no provenance-establishable candidates; eligibility is undefined"
                    .to_string(),
            };
        }
        if eligibility < self.eligibility_floor {
            return FloorVerdict::EligibilityBreach {
                observed: eligibility,
                floor: self.eligibility_floor,
                next_freshness_secs: self.refreeze_ladder_secs.first().copied(),
            };
        }
        FloorVerdict::Met { eligibility, coverage }
    }
}

#[derive(Debug, PartialEq)]
pub enum FloorVerdict {
    Met { eligibility: f64, coverage: f64 },
    /// Coverage failed first. Checked before eligibility on purpose: with poor
    /// coverage the eligibility rate describes a subsample, so reporting an
    /// eligibility breach would attribute a measurement failure to the
    /// freshness bound.
    CoverageBreach { observed: f64, floor: f64 },
    EligibilityBreach { observed: f64, floor: f64, next_freshness_secs: Option<i64> },
    Indeterminate { detail: String },
}

impl FloorVerdict {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Met { .. } => "MET",
            Self::CoverageBreach { .. } => "COVERAGE_BREACH",
            Self::EligibilityBreach { .. } => "ELIGIBILITY_BREACH",
            Self::Indeterminate { .. } => "INDETERMINATE",
        }
    }
}

#[cfg(test)]
#[path = "observation_tests.rs"]
mod observation_tests;

#[cfg(test)]
#[path = "observation_contract_tests.rs"]
mod observation_contract_tests;

#[cfg(test)]
#[path = "observation_step4a_bench.rs"]
mod observation_step4a_bench;
