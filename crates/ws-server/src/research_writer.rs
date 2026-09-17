//! A bounded, buffered, batch-draining NDJSON writer shared by the two
//! research captures that live in this crate (Opportunity Intelligence and
//! measurement).
//!
//! # Why this exists
//!
//! Both captures previously ran the same three defects, and the September-16
//! instrument-validation session lost the majority of its evidence to them:
//!
//! 1. **A queue sized at roughly 1.6% of a single synchronous emission.** OI
//!    emits its *whole* ranking cohort in one tight loop every 30s -- 3,280
//!    records on average on September 16, up to 4,808 -- into a channel 64
//!    deep. Saturation was not a risk, it was arithmetic.
//! 2. **`OpenOptions::append().open()` per record.** One `open`/`write`/`close`
//!    syscall triple for every single line, so the writer could not drain
//!    anywhere near the rate the producer emitted.
//! 3. **Loss that only the process log could see.** The counter logged at
//!    powers of two, so the artifact itself said nothing, and the true total
//!    was recoverable only as a range.
//!
//! The result was 282,255 snapshots written of 2,558,786 attempted: an 11.0%
//! capture rate, and a session that could not support any ranking or recall
//! claim.
//!
//! # The shape of the repair
//!
//! * **Serialization happens on the producer side**, in `record`. That is what
//!   lets the queue be bounded in *bytes* as well as records, which matters
//!   because a bound in records alone is a bound on an unknown quantity. It
//!   also leaves the writer thread doing nothing but I/O. It does not weaken
//!   the isolation guarantee: both producers are already independent
//!   `broadcast` subscribers, so the cost lands on a research task, never on
//!   market dispatch.
//! * **Persistent handles behind a `BufWriter`**, flushed once per drained
//!   batch rather than once per record. Batching is a storage optimisation
//!   only: records keep their order, their content and their count.
//! * **Exact accounting**, including attempted, and a queue peak -- so
//!   completeness is a reading rather than an inference.
//! * **Loss spans are persisted as markers**, so the artifact self-reports.
//!
//! # What is deliberately unchanged
//!
//! `record` still never blocks. A full queue still drops and counts. A
//! realtime process must not grow memory without bound, and a research
//! subsystem must never be able to stall the path it observes -- that
//! constraint is what made the original queue small, and it is correct; only
//! its size was wrong.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};

use backtest_metrics::completeness::WriterCapture;
use chrono::{DateTime, Utc};
use serde::Serialize;
use tracing::{info, warn};

/// Records drained and written under a single buffer flush.
///
/// Sized well below the queue so a drain always makes progress against a
/// producer that is still emitting, rather than draining the queue exactly as
/// fast as it refills and never flushing.
const BATCH: usize = 512;

/// Open output files retained. Each capture writes one file per UTC day plus
/// one marker file per UTC day, so two is the steady state and this only ever
/// matters across a midnight rollover.
const MAX_OPEN_FILES: usize = 8;

/// Buffer per open file. One ranking window at the observed record size is
/// roughly 13 MB, so this is a batching buffer, not an attempt to hold a
/// window in memory.
const BUFFER_BYTES: usize = 256 * 1024;

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

/// Exact capture accounting for one writer.
///
/// Every field is readable at any moment without a restart, which is the
/// property September 16 did not have: the drop counter existed, but the only
/// way to read it was to stop the process that was still writing.
#[derive(Debug, Default)]
pub struct WriterHealth {
    /// Records offered to `record`. The denominator for everything else.
    pub attempted: AtomicU64,
    /// Records the writer persisted.
    pub written: AtomicU64,
    /// Records discarded because the queue was full.
    pub dropped: AtomicU64,
    /// Records accepted onto the queue but not persisted.
    pub write_errors: AtomicU64,
    /// Distinct loss spans -- a burst that drops 900 records is one span.
    pub loss_spans: AtomicU64,
    /// Queue occupancy now, and its high-water mark.
    pub queue_depth: AtomicU64,
    pub queue_peak: AtomicU64,
    pub queued_bytes: AtomicU64,
    pub queued_bytes_peak: AtomicU64,
    /// Configured bounds, so a depth can be read against them.
    pub queue_capacity: AtomicU64,
    pub queue_capacity_bytes: AtomicU64,
    pub bytes_written: AtomicU64,
    pub batches_written: AtomicU64,
    /// Microseconds since the epoch of the last successful write, 0 if none.
    pub last_write_micros: AtomicI64,
    pub current_file_bytes: AtomicU64,
    /// Path currently being appended to.
    pub current_file: Mutex<String>,
}

impl WriterHealth {
    /// Any non-zero loss of any kind. A degraded capture cannot support a
    /// completeness claim.
    pub fn is_degraded(&self) -> bool {
        self.dropped.load(Ordering::Relaxed) > 0 || self.write_errors.load(Ordering::Relaxed) > 0
    }

    /// A serializable snapshot for the research health surface.
    pub fn snapshot(&self) -> WriterCapture {
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        WriterCapture {
            attempted: g(&self.attempted),
            written: g(&self.written),
            dropped: g(&self.dropped),
            write_errors: g(&self.write_errors),
            loss_spans: g(&self.loss_spans),
            queue_depth: g(&self.queue_depth),
            queue_peak: g(&self.queue_peak),
            queue_capacity: g(&self.queue_capacity),
            queued_bytes: g(&self.queued_bytes),
            queued_bytes_peak: g(&self.queued_bytes_peak),
            queue_capacity_bytes: g(&self.queue_capacity_bytes),
            bytes_written: g(&self.bytes_written),
            batches_written: g(&self.batches_written),
            last_write: match self.last_write_micros.load(Ordering::Relaxed) {
                0 => None,
                micros => DateTime::from_timestamp_micros(micros),
            },
            current_file: self
                .current_file
                .lock()
                .map(|f| f.clone())
                .unwrap_or_default(),
            current_file_bytes: g(&self.current_file_bytes),
            degraded: self.is_degraded(),
        }
    }
}

// ---------------------------------------------------------------------------
// Markers -- how the artifact self-reports
// ---------------------------------------------------------------------------

/// A record written into the capture's marker file rather than its data file.
///
/// # Why a sibling file rather than a field on the record
///
/// Discovery carries its queue-loss span in-band, as a field on the next
/// admitted record, and the brief asks the other two captures to gain
/// comparable semantics. They do -- but in a sibling NDJSON written by the
/// same writer, in the same directory, for the same UTC day, rather than as a
/// new field on `OpportunityScoreSnapshot` or `OpportunityEpisode`.
///
/// Two reasons, and the first is binding:
///
/// 1. The model/rank freeze proof requires that for identical input the
///    persisted records are identical. Adding a field to the snapshot changes
///    every record in the capture and would forfeit exactly the proof this
///    repair exists to preserve.
/// 2. A loss span is not a property of any one record. Attaching it to
///    whichever record happened to be admitted next is a workaround discovery
///    needs because its writer is a global with no second channel; it is not
///    the better representation.
///
/// The property that matters is preserved: reading the capture, and nothing
/// else, is enough to discover that it is incomplete.
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CaptureMarker {
    pub schema_version: u32,
    pub recorded_at: DateTime<Utc>,
    /// `queue_loss`, `writer_started`, `capture_finished`, or a caller-supplied
    /// kind such as `opportunity_capacity_reached`.
    pub kind: String,
    pub capture: String,
    /// Present on `queue_loss`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_loss: Option<QueueLossSpan>,
    /// Free-form payload for caller-supplied markers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// One contiguous span of records lost to queue pressure.
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QueueLossSpan {
    pub lost: u64,
    pub onset: Option<DateTime<Utc>>,
    pub onset_micros: i64,
    /// Cumulative drops at the moment the span was described.
    pub cumulative_dropped: u64,
    pub reason: String,
}

pub const MARKER_SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// The writer
// ---------------------------------------------------------------------------

enum Msg {
    /// Pre-serialized, LF-terminated, with the path it belongs in.
    ///
    /// `data` separates a captured record from a marker. It has to exist:
    /// `written + dropped + write_errors == attempted` is the identity the
    /// completeness verdict is built on, and a marker counted as a written
    /// record breaks it by exactly the number of markers — which would make
    /// every clean session look like it had lost a handful of records.
    Line { path: PathBuf, bytes: Vec<u8>, data: bool },
    Flush(std::sync::mpsc::Sender<()>),
}

impl Msg {
    fn size(&self) -> u64 {
        match self {
            Msg::Line { bytes, .. } => bytes.len() as u64,
            Msg::Flush(_) => 0,
        }
    }
}

/// How a capture names its files. Both callers rotate per UTC day; the marker
/// file is a sibling of the data file.
#[derive(Debug, Clone)]
pub struct Naming {
    pub dir: PathBuf,
    /// e.g. `opportunity-intelligence` -> `opportunity-intelligence-<date>.ndjson`
    /// and `opportunity-intelligence-markers-<date>.ndjson`.
    pub stem: String,
}

impl Naming {
    pub fn data(&self, date: chrono::NaiveDate) -> PathBuf {
        self.dir.join(format!("{}-{date}.ndjson", self.stem))
    }
    pub fn markers(&self, date: chrono::NaiveDate) -> PathBuf {
        self.dir.join(format!("{}-markers-{date}.ndjson", self.stem))
    }
}

/// Queue bounds, derived by the caller from its own measured burst.
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    pub records: usize,
    pub bytes: u64,
}

pub struct ResearchWriter {
    tx: SyncSender<Msg>,
    health: Arc<WriterHealth>,
    naming: Naming,
    bounds: Bounds,
    /// Queue-pressure loss not yet described by a marker.
    loss_unreported: AtomicU64,
    loss_onset_micros: AtomicI64,
}

impl ResearchWriter {
    /// Starts the writer thread, or returns `None` when the directory is
    /// unusable -- research capture degrades to *off*, never to a failure of
    /// the service it observes.
    ///
    /// `gate`, when supplied, is waited on before the writer drains anything.
    ///
    /// It exists so the queue bound is *exercisable* rather than merely
    /// asserted: with a live writer thread a queue this size never fills in a
    /// test, and a drop counter no test can reach is indistinguishable from a
    /// drop counter that does not work.
    pub fn start_inner(
        naming: Naming,
        bounds: Bounds,
        gate: Option<Arc<std::sync::Barrier>>,
    ) -> Option<Self> {
        if let Err(error) = std::fs::create_dir_all(&naming.dir) {
            warn!(%error, dir = %naming.dir.display(),
                "research capture disabled: cannot create directory");
            return None;
        }
        let (tx, rx) = sync_channel::<Msg>(bounds.records);
        let health = Arc::new(WriterHealth::default());
        health.queue_capacity.store(bounds.records as u64, Ordering::Relaxed);
        health.queue_capacity_bytes.store(bounds.bytes, Ordering::Relaxed);
        let writer_health = health.clone();
        let stem = naming.stem.clone();

        std::thread::spawn(move || {
            if let Some(gate) = gate {
                gate.wait();
            }
            let mut files: HashMap<PathBuf, std::io::BufWriter<std::fs::File>> = HashMap::new();
            // Blocking receive, then a bounded non-blocking drain. One flush
            // per batch instead of one syscall triple per record is the whole
            // throughput repair.
            while let Ok(first) = rx.recv() {
                let mut batch = Vec::with_capacity(BATCH);
                batch.push(first);
                while batch.len() < BATCH {
                    match rx.try_recv() {
                        Ok(msg) => batch.push(msg),
                        Err(_) => break,
                    }
                }
                let mut replies = Vec::new();
                let mut touched: Vec<PathBuf> = Vec::new();
                let mut wrote = 0u64;
                let mut bytes = 0u64;
                let mut last_data_path: Option<PathBuf> = None;
                for msg in batch {
                    let size = msg.size();
                    match msg {
                        Msg::Flush(reply) => replies.push(reply),
                        Msg::Line { path, bytes: line, data } => {
                            writer_health.queue_depth.fetch_sub(1, Ordering::Relaxed);
                            writer_health.queued_bytes.fetch_sub(size, Ordering::Relaxed);
                            // Order within a path is the order records were
                            // offered: the channel is FIFO and the batch keeps
                            // it. Batching must not reorder anything.
                            let entry = match files.entry(path.clone()) {
                                std::collections::hash_map::Entry::Occupied(e) => Some(e.into_mut()),
                                std::collections::hash_map::Entry::Vacant(v) => {
                                    match std::fs::OpenOptions::new()
                                        .create(true)
                                        .append(true)
                                        .open(&path)
                                    {
                                        Ok(file) => Some(v.insert(std::io::BufWriter::with_capacity(
                                            BUFFER_BYTES,
                                            file,
                                        ))),
                                        Err(error) => {
                                            let n = writer_health
                                                .write_errors
                                                .fetch_add(1, Ordering::Relaxed)
                                                + 1;
                                            if n.is_power_of_two() {
                                                warn!(%error, write_errors = n, capture = %stem,
                                                    "research capture cannot open its target");
                                            }
                                            None
                                        }
                                    }
                                }
                            };
                            let Some(file) = entry else { continue };
                            match file.write_all(&line) {
                                Ok(()) => {
                                    if data {
                                        wrote += 1;
                                        bytes += line.len() as u64;
                                        last_data_path = Some(path.clone());
                                    }
                                    if !touched.iter().any(|p| p == &path) {
                                        touched.push(path);
                                    }
                                }
                                Err(error) => {
                                    let n = writer_health
                                        .write_errors
                                        .fetch_add(1, Ordering::Relaxed)
                                        + 1;
                                    if n.is_power_of_two() {
                                        warn!(%error, write_errors = n, capture = %stem,
                                            "research capture write failed");
                                    }
                                }
                            }
                        }
                    }
                }
                // One flush per batch. Without this the buffer would hold
                // records through a quiet period and `bytes_written` would be
                // a claim about memory rather than about disk.
                for path in &touched {
                    if let Some(file) = files.get_mut(path) {
                        if let Err(error) = file.flush() {
                            let n =
                                writer_health.write_errors.fetch_add(1, Ordering::Relaxed) + 1;
                            if n.is_power_of_two() {
                                warn!(%error, write_errors = n, capture = %stem,
                                    "research capture flush failed");
                            }
                        }
                    }
                }
                if wrote > 0 {
                    writer_health.written.fetch_add(wrote, Ordering::Relaxed);
                    writer_health.bytes_written.fetch_add(bytes, Ordering::Relaxed);
                    writer_health.batches_written.fetch_add(1, Ordering::Relaxed);
                    writer_health
                        .last_write_micros
                        .store(Utc::now().timestamp_micros(), Ordering::Relaxed);
                    // The *data* file, deliberately: an operator reading
                    // `currentFile` wants to know where the evidence is going,
                    // not which stream the writer happened to touch last.
                    if let Some(path) = &last_data_path {
                        if let Ok(mut current) = writer_health.current_file.lock() {
                            *current = path.display().to_string();
                        }
                        writer_health.current_file_bytes.store(
                            std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
                            Ordering::Relaxed,
                        );
                    }
                }
                // Replies go out only after the flush above, so a caller that
                // waited on `flush` knows its records reached the filesystem.
                for reply in replies {
                    let _ = reply.send(());
                }
                // Keep the handle table bounded across day rollovers.
                if files.len() > MAX_OPEN_FILES {
                    let keep: Vec<PathBuf> = touched.clone();
                    files.retain(|path, file| {
                        let keeping = keep.iter().any(|p| p == path);
                        if !keeping {
                            let _ = file.flush();
                        }
                        keeping
                    });
                }
            }
            for (_, mut file) in files {
                let _ = file.flush();
            }
        });

        info!(dir = %naming.dir.display(), stem = %naming.stem,
            queue_records = bounds.records, queue_bytes = bounds.bytes,
            "research capture enabled");
        let writer = Self {
            tx,
            health,
            naming,
            bounds,
            loss_unreported: AtomicU64::new(0),
            loss_onset_micros: AtomicI64::new(0),
        };
        writer.marker("writer_started", None);
        Some(writer)
    }

    pub fn health(&self) -> &Arc<WriterHealth> {
        &self.health
    }

    /// Offers one record. **Never blocks.**
    ///
    /// `date` selects the daily file, and is the record's own timestamp rather
    /// than the wall clock so a record always lands in the day it describes.
    pub fn record<T: Serialize>(&self, value: &T, date: chrono::NaiveDate) {
        self.health.attempted.fetch_add(1, Ordering::Relaxed);
        let Ok(mut bytes) = serde_json::to_vec(value) else {
            // A record that cannot be encoded is a defect in the record, not
            // in the queue, and must not be filed as queue pressure.
            let n = self.health.write_errors.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_power_of_two() {
                warn!(write_errors = n, capture = %self.naming.stem,
                    "research record could not be serialized");
            }
            return;
        };
        bytes.push(b'\n');
        self.offer(self.naming.data(date), bytes, true);
    }


    /// Offers one record, **blocking** until the queue accepts it.
    ///
    /// For shutdown only, and the distinction is load-bearing. `record` must
    /// never block because it runs while the market is live and a research
    /// subsystem must not be able to stall the path it observes. At shutdown
    /// there is no such path left: the collector force-settles its entire
    /// pending set in one call, and dropping that tail would discard thousands
    /// of episodes for a latency budget that no longer exists.
    ///
    /// Still bounded: the queue is the same size, so memory does not grow --
    /// the caller waits instead.
    pub fn record_blocking<T: Serialize>(&self, value: &T, date: chrono::NaiveDate) {
        self.health.attempted.fetch_add(1, Ordering::Relaxed);
        let Ok(mut bytes) = serde_json::to_vec(value) else {
            let n = self.health.write_errors.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_power_of_two() {
                warn!(write_errors = n, capture = %self.naming.stem,
                    "research record could not be serialized");
            }
            return;
        };
        bytes.push(b'\n');
        let size = bytes.len() as u64;
        let path = self.naming.data(date);
        // The byte bound is deliberately not enforced here. `send` blocks on
        // the *record* bound, which already caps occupancy; refusing on bytes
        // as well would turn the shutdown tail back into dropped records, which
        // is the thing this method exists to prevent. The reservation is still
        // taken so the counters stay consistent.
        let qb = self.health.queued_bytes.fetch_add(size, Ordering::Relaxed) + size;
        self.health.queued_bytes_peak.fetch_max(qb, Ordering::Relaxed);
        let depth = self.health.queue_depth.fetch_add(1, Ordering::Relaxed) + 1;
        self.health.queue_peak.fetch_max(depth, Ordering::Relaxed);
        if self.tx.send(Msg::Line { path, bytes, data: true }).is_err() {
            // The writer thread is gone; this is a lost record, not a stall.
            self.release(size);
            self.count_drop();
        }
    }

    /// Writes a marker into the capture's marker file.
    pub fn marker(&self, kind: &str, data: Option<serde_json::Value>) {
        let now = Utc::now();
        let marker = CaptureMarker {
            schema_version: MARKER_SCHEMA_VERSION,
            recorded_at: now,
            kind: kind.to_string(),
            capture: self.naming.stem.clone(),
            queue_loss: None,
            data,
        };
        if let Ok(mut bytes) = serde_json::to_vec(&marker) {
            bytes.push(b'\n');
            // Markers are how loss becomes discoverable, so they are never
            // themselves filed as data loss when the queue rejects them.
            self.offer(self.naming.markers(now.date_naive()), bytes, false);
        }
    }

    /// Pushes onto the bounded queue, or counts the drop.
    ///
    /// `countable` distinguishes a data record (whose loss is evidence loss)
    /// from a marker (whose loss is only a loss of description, and which must
    /// never inflate the data-loss figure).
    fn offer(&self, path: PathBuf, bytes: Vec<u8>, countable: bool) {
        // `countable` and `data` are the same distinction seen from the two
        // ends: a data record's loss is evidence loss and its write is a
        // written record; a marker's loss is only a loss of description.
        let data = countable;
        let size = bytes.len() as u64;
        // Bounded in bytes as well as records. A record bound alone bounds an
        // unknown quantity, which is how discovery ended up able to queue
        // 2 MB records against a 32-slot channel.
        if !self.reserve(size) {
            if countable {
                self.count_drop();
            }
            return;
        }
        match self.tx.try_send(Msg::Line { path, bytes, data }) {
            Ok(()) => {
                if countable {
                    self.describe_any_loss_span();
                }
            }
            Err(_) => {
                self.release(size);
                if countable {
                    self.count_drop();
                }
            }
        }
    }

    /// Claims a queue slot and its bytes *before* the message is sent.
    ///
    /// Ordering matters and is not a style choice. The writer thread subtracts
    /// the moment it receives a message, so adding after a successful send
    /// races: under a live writer the subtraction can land first and wrap the
    /// unsigned counter to near `u64::MAX`, after which the byte-bound check
    /// overflows. Reserving first makes every subtraction correspond to an
    /// addition that already happened.
    ///
    /// Returns `false` when the byte bound is already committed, in which case
    /// nothing was reserved.
    fn reserve(&self, size: u64) -> bool {
        let qb = self.health.queued_bytes.fetch_add(size, Ordering::Relaxed) + size;
        if qb > self.bounds.bytes {
            self.health.queued_bytes.fetch_sub(size, Ordering::Relaxed);
            return false;
        }
        self.health.queued_bytes_peak.fetch_max(qb, Ordering::Relaxed);
        let depth = self.health.queue_depth.fetch_add(1, Ordering::Relaxed) + 1;
        self.health.queue_peak.fetch_max(depth, Ordering::Relaxed);
        true
    }

    /// Returns a reservation the channel refused.
    fn release(&self, size: u64) {
        self.health.queue_depth.fetch_sub(1, Ordering::Relaxed);
        self.health.queued_bytes.fetch_sub(size, Ordering::Relaxed);
    }

    fn count_drop(&self) {
        let n = self.health.dropped.fetch_add(1, Ordering::Relaxed) + 1;
        if self.loss_unreported.fetch_add(1, Ordering::Relaxed) == 0 {
            self.health.loss_spans.fetch_add(1, Ordering::Relaxed);
            self.loss_onset_micros
                .store(Utc::now().timestamp_micros(), Ordering::Relaxed);
        }
        if n.is_power_of_two() {
            warn!(dropped = n, capture = %self.naming.stem,
                "research capture queue full; records dropped");
        }
    }

    /// Emits a marker for any loss span that has not yet reached the capture.
    ///
    /// Read-then-subtract exactly as discovery does: a drop that lands between
    /// the load and the send stays pending for the next marker rather than
    /// being reported twice or not at all.
    fn describe_any_loss_span(&self) {
        let unreported = self.loss_unreported.load(Ordering::Relaxed);
        if unreported == 0 {
            return;
        }
        let onset_micros = self.loss_onset_micros.load(Ordering::Relaxed);
        let now = Utc::now();
        let marker = CaptureMarker {
            schema_version: MARKER_SCHEMA_VERSION,
            recorded_at: now,
            kind: "queue_loss".to_string(),
            capture: self.naming.stem.clone(),
            queue_loss: Some(QueueLossSpan {
                lost: unreported,
                onset: DateTime::from_timestamp_micros(onset_micros),
                onset_micros,
                cumulative_dropped: self.health.dropped.load(Ordering::Relaxed),
                reason: "queue_full".to_string(),
            }),
            data: None,
        };
        let Ok(mut bytes) = serde_json::to_vec(&marker) else { return };
        bytes.push(b'\n');
        let size = bytes.len() as u64;
        if !self.reserve(size) {
            // The span stays pending and rides out on a later record.
            return;
        }
        if self
            .tx
            .try_send(Msg::Line {
                path: self.naming.markers(now.date_naive()),
                bytes,
                data: false,
            })
            .is_ok()
        {
            self.loss_unreported.fetch_sub(unreported, Ordering::Relaxed);
        } else {
            self.release(size);
        }
    }

    /// Bounded drain. Shutdown must not hang on research bookkeeping.
    pub fn flush(&self, timeout: std::time::Duration) {
        // Describe any span still outstanding before the file is closed, so a
        // capture that ended mid-loss still says so.
        self.describe_any_loss_span();
        let (tx, rx) = std::sync::mpsc::channel();
        if self.tx.send(Msg::Flush(tx)).is_ok() {
            let _ = rx.recv_timeout(timeout);
        }
    }
}

#[cfg(test)]
#[path = "research_writer_tests.rs"]
mod tests;
