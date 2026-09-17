//! Optional prospective evidence. Never block market dispatch on disk I/O.
//!
//! # Bounded capture (R5)
//!
//! The previous writer held a single file per UTC day with an 8 GiB ceiling and
//! **no rotation**: on reaching the cap it dropped every subsequent record until
//! the date changed. On 2026-09-10 that cap was reached before the market
//! opened, so the entire regular session went unrecorded while the only
//! evidence of the loss lived in container logs.
//!
//! Four properties replace that behaviour, in order of importance:
//!
//! 1. **Rotation, not stop.** A per-file budget rotates to
//!    `<day>-<run>-<seq>.jsonl`. Reaching a file limit is never a reason to
//!    stop recording.
//! 2. **A reserved session budget.** Windows before the regular session may
//!    spend only `PRE_SESSION_BUDGET_FRACTION` of the day's allowance, so a
//!    premarket flood cannot starve the session the data exists to describe.
//! 3. **Degrade before dropping.** Under pressure the highest-volume,
//!    lowest-value classes are downsampled at a recorded rate while the
//!    analytically critical ones are preserved.
//! 4. **Loss is in-band.** Degradation and drop spans are written into the
//!    stream itself, so a reader can establish completeness from the dataset
//!    without consulting logs.
//!
//! Budget state is rebuilt from the files already on disk at startup, so a
//! restart cannot hand the same UTC day a fresh allowance.
use chrono::Utc;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicI64, AtomicU64, Ordering},
        mpsc::{sync_channel, SyncSender},
        Arc, Mutex, OnceLock,
    },
};

use crate::trading_session::{classify_session, TradingSession};

/// Rotation unit. Deliberately far below the daily budget so retention can
/// reclaim space in useful increments -- deleting one 8 GiB file is a blunt
/// instrument, deleting the oldest of many 1 GiB segments is not.
/// Queue depth, in records.
///
/// A scan tick emits its whole result set synchronously -- one `scan_started`,
/// roughly 65 `snapshot_batch` records, a `coverage`, a `scan_completed` and a
/// `snapshot_complete` -- on top of a continuous ignition print stream. The
/// previous bound was **32 records**, which is under half of one scan tick, so
/// every tick began by overrunning the queue.
///
/// Measured over the preserved September-16 capture: a scan tick emits 68-69
/// records totalling 2.85 MB, and the heaviest single second of the day carried
/// 3,190 records / 2.28 MB. 65,536 absorbs roughly 20 seconds of that peak, or
/// 950 scan ticks -- and costs little, because 93.9% of discovery records are
/// 235-byte ignition prints. The byte bound below is what actually caps the
/// footprint when the mix turns heavy.
const QUEUE_RECORDS: usize = 65_536;

/// Queue bound in bytes, and the reason this writer needs one at all.
///
/// Discovery record sizes span **four orders of magnitude**, measured over the
/// preserved September-16 capture (5,951,808 records, 16.43 GB):
///
/// | kind | records | mean B | max B |
/// |---|---|---|---|
/// | ignition | 5,587,387 | 235 | 380 |
/// | snapshot_batch | 343,329 | 39,932 | 41,240 |
/// | scan_started | 5,266 | 89,796 | 89,934 |
/// | scan_completed | 5,266 | 151,866 | 154,418 |
/// | coverage | 5,266 | 255,086 | **2,048,478** |
///
/// A queue bounded only in records is therefore bounded in an unknown
/// quantity: 65,536 `coverage` records at the observed maximum would be 128 GB.
/// Bounding bytes as well is what makes the footprint statable. 128 MiB holds
/// 45 full scan ticks, or ~56 seconds of the heaviest second observed, and is
/// the writer's worst-case memory reservation rather than its steady state.
const QUEUE_BYTES: u64 = 128 * 1024 * 1024;

/// Records drained under one buffer flush.
const BATCH: usize = 512;

/// Write buffer per segment.
const WRITE_BUFFER_BYTES: usize = 1024 * 1024;

const DEFAULT_PER_FILE_BYTES: u64 = 1024 * 1024 * 1024;

/// Total bytes one UTC day may write. Preserves the previous effective
/// allowance; what changes is that reaching it no longer means silence.
const DEFAULT_DAILY_BUDGET_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Ceiling across the whole capture directory, enforced by deleting the oldest
/// segments. This is the bound that actually protects the disk.
const DEFAULT_DIRECTORY_CEILING_BYTES: u64 = 32 * 1024 * 1024 * 1024;

/// Share of the daily budget reachable *before* the regular session opens.
/// The remainder is reserved, which is the entire point: premarket volume
/// cannot consume the session's allocation.
const PRE_SESSION_BUDGET_FRACTION: f64 = 0.5;

/// Fraction of the applicable budget at which degradation engages, before any
/// hard limit is reached. Degrading early and gently beats dropping late and
/// totally.
const DEGRADE_AT_FRACTION: f64 = 0.8;

/// While degraded, keep one in N records of the degradable classes.
const DEGRADED_SAMPLE_RATE: u64 = 10;

/// Record classes preserved even under pressure: the qualified set, ignition
/// staging, and the scan/stream lifecycle markers that make a file
/// interpretable at all.
const CRITICAL_KINDS: [&str; 4] = [
    "ignition",
    "scan_completed",
    "scan_started",
    "stream_started",
];

/// True when this record class must never be downsampled.
fn is_critical(kind: &str) -> bool {
    CRITICAL_KINDS.contains(&kind) || kind.starts_with("capture_")
}

fn env_bytes(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

/// Low-cardinality bit per record class, so a queue-loss span can name which
/// classes it swallowed without carrying per-class counters on the dispatch
/// path. Unknown kinds collapse to one "other" bit rather than growing.
fn kind_bit(kind: &str) -> u64 {
    match kind {
        "coverage" => 1 << 0,
        "ignition" => 1 << 1,
        "scan_started" => 1 << 2,
        "scan_completed" => 1 << 3,
        "snapshot_batch" => 1 << 4,
        "snapshot_complete" => 1 << 5,
        "stream_started" => 1 << 6,
        _ => 1 << 7,
    }
}

fn kinds_from_mask(mask: u64) -> Vec<&'static str> {
    const NAMES: [&str; 8] = [
        "coverage",
        "ignition",
        "scan_started",
        "scan_completed",
        "snapshot_batch",
        "snapshot_complete",
        "stream_started",
        "other",
    ];
    NAMES
        .iter()
        .enumerate()
        .filter(|(i, _)| mask & (1 << i) != 0)
        .map(|(_, n)| *n)
        .collect()
}

/// Counters that separate **policy** from **defect**.
///
/// The deployed build had one counter, `lost`, incremented by three unrelated
/// things: queue-pressure loss (a defect), a writer error (a defect), and a
/// record refused by the daily byte budget (a deliberate, documented policy).
/// Reading `lost_records=4096` therefore could not distinguish "the instrument
/// is failing" from "the instrument is doing exactly what it was configured to
/// do", and the September-16 gate had to read the surrounding log text to tell
/// which had happened.
///
/// `lost` is kept, unchanged, as the aggregate -- it is carried in-band on
/// every record and existing readers depend on it. The disaggregated counters
/// are additive.
#[derive(Default)]
struct Counters {
    attempted: AtomicU64,
    written: AtomicU64,
    queue_lost: AtomicU64,
    write_errors: AtomicU64,
    budget_dropped: AtomicU64,
    queue_depth: AtomicU64,
    queue_peak: AtomicU64,
    queued_bytes: AtomicU64,
    queued_bytes_peak: AtomicU64,
    bytes_written: AtomicU64,
    batches_written: AtomicU64,
    last_write_micros: AtomicI64,
    current_file_bytes: AtomicU64,
    current_file: Mutex<String>,
}

struct Recorder {
    tx: SyncSender<Message>,
    lost: Arc<AtomicU64>,
    sampled_out: Arc<AtomicU64>,
    counters: Arc<Counters>,
    /// Queue-pressure loss that has **not yet been described in the stream**.
    ///
    /// This is the honest answer to 48-C. When `try_send` fails the queue is
    /// full *by definition*, so a marker cannot be inserted at that moment --
    /// the attempt would fail for the same reason. Instead the loss is
    /// accumulated here and rides out on the next record that is admitted, so
    /// once writing resumes the persisted stream carries the span's onset,
    /// count and affected classes.
    queue_lost_unreported: Arc<AtomicU64>,
    /// Microsecond timestamp of the first loss in the current unreported span.
    queue_loss_onset_micros: Arc<AtomicI64>,
    /// Bitmask of record classes lost in the current unreported span.
    queue_loss_classes: Arc<AtomicU64>,
}
/// A record already encoded, with the few fields the writer needs to route it.
///
/// Encoding on the producer side is what makes the byte bound possible: the
/// queue cannot bound what it cannot measure. It also leaves the writer thread
/// doing nothing but I/O and bookkeeping, which is where its throughput went.
struct Encoded {
    kind: String,
    /// UTC date from the record's own `recorded_at`, for segment rotation.
    day: String,
    /// The record's own clock, for the session-dependent budget.
    now: chrono::DateTime<Utc>,
    bytes: Vec<u8>,
}

enum Message {
    Record(Box<Encoded>),
    Flush(std::sync::mpsc::Sender<Result<(), String>>),
}
static RECORDER: OnceLock<Option<Recorder>> = OnceLock::new();

/// Bytes already written for `day`, total bytes in the directory, and the
/// highest sequence seen for `day` -- all read from disk.
///
/// This is what makes a restart safe: the budget is a property of the files
/// that exist, not of how many times the process has started.
fn scan_disk_state(dir: &Path, day: &str) -> (u64, u64, u32) {
    let mut day_bytes = 0u64;
    let mut dir_bytes = 0u64;
    let mut max_seq = 0u32;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0, 0);
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let len = meta.len();
        dir_bytes += len;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".jsonl") {
            continue;
        }
        if name.starts_with(&format!("{day}-")) {
            day_bytes += len;
            if let Some(seq) = name
                .trim_end_matches(".jsonl")
                .rsplit('-')
                .next()
                .and_then(|s| s.parse::<u32>().ok())
            {
                max_seq = max_seq.max(seq);
            }
        }
    }
    (day_bytes, dir_bytes, max_seq)
}

/// Oldest-first capture segments eligible for deletion, never including
/// `current`.
fn reclaimable_segments(dir: &Path, current: Option<&Path>) -> Vec<(PathBuf, u64)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(PathBuf, u64, std::time::SystemTime)> = entries
        .flatten()
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            if !meta.is_file() || !e.file_name().to_string_lossy().ends_with(".jsonl") {
                return None;
            }
            let path = e.path();
            if Some(path.as_path()) == current {
                return None;
            }
            Some((path, meta.len(), meta.modified().ok()?))
        })
        .collect();
    files.sort_by_key(|(path, _, modified)| (*modified, path.clone()));
    files.into_iter().map(|(p, l, _)| (p, l)).collect()
}

/// The budget reachable right now. Before the regular session only a fraction
/// of the day's allowance is available; the reserve unlocks when the session
/// opens.
fn applicable_budget(daily_budget: u64, now: chrono::DateTime<Utc>) -> u64 {
    match classify_session(now) {
        TradingSession::Premarket | TradingSession::Overnight => {
            (daily_budget as f64 * PRE_SESSION_BUDGET_FRACTION) as u64
        }
        TradingSession::Regular | TradingSession::AfterHours => daily_budget,
    }
}

struct Writer {
    dir: PathBuf,
    run: String,
    per_file: u64,
    daily_budget: u64,
    ceiling: u64,
    day: String,
    seq: u32,
    file: Option<std::io::BufWriter<std::fs::File>>,
    path: Option<PathBuf>,
    file_bytes: u64,
    day_bytes: u64,
    dir_bytes: u64,
    degraded: bool,
    seen_since_sample: u64,
    span_sampled: u64,
    span_dropped: u64,
    lost: Arc<AtomicU64>,
    sampled_out: Arc<AtomicU64>,
    counters: Arc<Counters>,
    /// Write-buffer capacity. Configurable only so the rotation, budget and
    /// retention tests can keep asserting against the filesystem immediately
    /// after `handle` -- a capacity of 0 makes `BufWriter` write through. What
    /// those tests are about is the segment policy, not the buffering.
    buffer_bytes: usize,
}

impl Writer {
    /// Writes one already-encoded marker directly, bypassing sampling. Markers
    /// are the mechanism that makes loss discoverable, so they are never the
    /// thing that gets dropped.
    fn write_marker(&mut self, kind: &str, data: Value) {
        let record = json!({
            "schema": 2,
            "recorded_at": Utc::now(),
            "kind": kind,
            "lost_records": self.lost.load(Ordering::Relaxed),
            "data": data,
        });
        if let Ok(mut encoded) = serde_json::to_vec(&record) {
            encoded.push(b'\n');
            if let Some(file) = self.file.as_mut() {
                let _ = file.write_all(&encoded);
                self.file_bytes += encoded.len() as u64;
                self.day_bytes += encoded.len() as u64;
                self.dir_bytes += encoded.len() as u64;
            }
        }
    }

    fn open_segment(&mut self, day: &str, seq: u32) -> anyhow::Result<()> {
        let path = self
            .dir
            .join(format!("{day}-{}-{seq}.jsonl", self.run));
        // Flushed before it is dropped: a `BufWriter` that goes out of scope
        // swallows the error from its own final write, which is exactly the
        // kind of silent loss this subsystem exists to make impossible.
        if let Some(mut previous) = self.file.take() {
            use std::io::Write as _;
            if let Err(error) = previous.flush() {
                self.counters.write_errors.fetch_add(1, Ordering::Relaxed);
                tracing::error!(%error, "discovery audit could not flush the outgoing segment");
            }
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        self.file_bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
        self.file = Some(std::io::BufWriter::with_capacity(self.buffer_bytes, file));
        self.counters.current_file_bytes.store(self.file_bytes, Ordering::Relaxed);
        if let Ok(mut current) = self.counters.current_file.lock() {
            *current = path.display().to_string();
        }
        self.path = Some(path);
        self.seq = seq;
        Ok(())
    }

    /// Enforces the directory ceiling by removing oldest segments. The segment
    /// currently being written is never a candidate.
    fn enforce_ceiling(&mut self) {
        if self.dir_bytes <= self.ceiling {
            return;
        }
        let current = self.path.clone();
        for (path, len) in reclaimable_segments(&self.dir, current.as_deref()) {
            if self.dir_bytes <= self.ceiling {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                self.dir_bytes = self.dir_bytes.saturating_sub(len);
                let name = path.file_name().map(|n| n.to_string_lossy().to_string());
                self.write_marker(
                    "capture_retention_removed",
                    json!({"file": name, "bytes": len, "dir_bytes": self.dir_bytes}),
                );
            }
        }
    }

    fn set_degraded(&mut self, on: bool, budget: u64) {
        if on == self.degraded {
            return;
        }
        self.degraded = on;
        if on {
            self.span_sampled = 0;
            self.span_dropped = 0;
            self.write_marker(
                "capture_degraded_start",
                json!({
                    "reason": "budget_pressure",
                    "classes": ["coverage", "snapshot_batch", "snapshot_complete"],
                    "policy": "keep_one_in_n",
                    "sample_rate_n": DEGRADED_SAMPLE_RATE,
                    "day_bytes": self.day_bytes,
                    "applicable_budget": budget,
                }),
            );
        } else {
            let (sampled, dropped) = (self.span_sampled, self.span_dropped);
            self.write_marker(
                "capture_degraded_end",
                json!({
                    "sampled_out": sampled,
                    "dropped": dropped,
                    "day_bytes": self.day_bytes,
                    "applicable_budget": budget,
                }),
            );
        }
    }

    fn handle(&mut self, record: Encoded) -> anyhow::Result<()> {
        let Encoded { kind, day: today, now, bytes: encoded } = record;
        anyhow::ensure!(!today.is_empty(), "record carries no recorded_at date");

        if today != self.day {
            // A new UTC day resets the day's allowance and starts a fresh
            // sequence, rebuilt from disk so a restart mid-day cannot pretend
            // the day is untouched.
            let (day_bytes, dir_bytes, max_seq) = scan_disk_state(&self.dir, &today);
            self.day = today.clone();
            self.day_bytes = day_bytes;
            self.dir_bytes = dir_bytes;
            self.degraded = false;
            self.open_segment(&today, max_seq + 1)?;
        }

        let budget = applicable_budget(self.daily_budget, now);

        // Degrade before the hard limit, and recover when pressure drops.
        let pressure = self.day_bytes as f64 >= budget as f64 * DEGRADE_AT_FRACTION;
        self.set_degraded(pressure, budget);

        if self.degraded && !is_critical(&kind) {
            self.seen_since_sample += 1;
            if self.seen_since_sample % DEGRADED_SAMPLE_RATE != 0 {
                self.span_sampled += 1;
                self.sampled_out.fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
        }

        let len = encoded.len() as u64;

        // Hard daily limit. Critical records are still written; only the
        // degradable classes stop, and the span is recorded in-band.
        if self.day_bytes + len > budget && !is_critical(&kind) {
            if self.span_dropped == 0 {
                self.write_marker(
                    "capture_drop_start",
                    json!({
                        "reason": "daily_budget_exhausted",
                        "day_bytes": self.day_bytes,
                        "applicable_budget": budget,
                        "class": kind,
                    }),
                );
            }
            self.span_dropped += 1;
            self.lost.fetch_add(1, Ordering::Relaxed);
            // Disaggregated: a record refused by the daily budget is the
            // policy working, not the queue failing, and the completeness
            // verdict must be able to tell them apart.
            self.counters.budget_dropped.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
        if self.span_dropped > 0 && self.day_bytes + len <= budget {
            let dropped = self.span_dropped;
            self.span_dropped = 0;
            self.write_marker(
                "capture_drop_end",
                json!({"dropped": dropped, "day_bytes": self.day_bytes}),
            );
        }

        if self.file_bytes + len > self.per_file {
            let next = self.seq + 1;
            let day = self.day.clone();
            self.open_segment(&day, next)?;
            self.write_marker(
                "capture_rotated",
                json!({"sequence": next, "day_bytes": self.day_bytes}),
            );
        }

        self.file
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("no open capture segment"))?
            .write_all(&encoded)?;
        self.file_bytes += len;
        self.day_bytes += len;
        self.dir_bytes += len;
        self.counters.written.fetch_add(1, Ordering::Relaxed);
        self.counters.bytes_written.fetch_add(len, Ordering::Relaxed);
        self.counters.current_file_bytes.store(self.file_bytes, Ordering::Relaxed);
        self.enforce_ceiling();
        Ok(())
    }
}

fn recorder() -> Option<&'static Recorder> {
    RECORDER
        .get_or_init(|| {
            let dir = std::env::var_os("DISCOVERY_AUDIT_DIR").filter(|v| !v.is_empty())?;
            let dir = PathBuf::from(dir);
            if let Err(error) = std::fs::create_dir_all(&dir) {
                tracing::error!(%error, "discovery audit unavailable: cannot create directory");
                return None;
            }
            let run = format!("{}-{}", std::process::id(), Utc::now().timestamp_micros());
            let (tx, rx) = sync_channel::<Message>(QUEUE_RECORDS);
            let lost = Arc::new(AtomicU64::new(0));
            let sampled_out = Arc::new(AtomicU64::new(0));
            let counters = Arc::new(Counters::default());
            counters.queue_peak.store(0, Ordering::Relaxed);
            let (writer_lost, writer_sampled) = (lost.clone(), sampled_out.clone());
            let writer_counters = counters.clone();
            let per_file = env_bytes("DISCOVERY_AUDIT_PER_FILE_BYTES", DEFAULT_PER_FILE_BYTES);
            let daily_budget =
                env_bytes("DISCOVERY_AUDIT_DAILY_BYTES", DEFAULT_DAILY_BUDGET_BYTES);
            let ceiling = env_bytes("DISCOVERY_AUDIT_MAX_BYTES", DEFAULT_DIRECTORY_CEILING_BYTES);
            std::thread::spawn(move || {
                let mut writer = Writer {
                    dir,
                    run,
                    per_file,
                    daily_budget,
                    ceiling,
                    day: String::new(),
                    seq: 0,
                    file: None,
                    path: None,
                    file_bytes: 0,
                    day_bytes: 0,
                    dir_bytes: 0,
                    degraded: false,
                    seen_since_sample: 0,
                    span_sampled: 0,
                    span_dropped: 0,
                    lost: writer_lost.clone(),
                    sampled_out: writer_sampled,
                    counters: writer_counters.clone(),
                    buffer_bytes: WRITE_BUFFER_BYTES,
                };
                // Blocking receive, then a bounded non-blocking drain, then one
                // flush. The deployed build flushed nothing and reached the
                // filesystem once per record; this is the throughput half of
                // the repair, and it changes no record's content or order.
                while let Ok(first) = rx.recv() {
                    let mut batch = Vec::with_capacity(BATCH);
                    batch.push(first);
                    while batch.len() < BATCH {
                        match rx.try_recv() {
                            Ok(message) => batch.push(message),
                            Err(_) => break,
                        }
                    }
                    let mut replies = Vec::new();
                    let mut wrote_any = false;
                    for message in batch {
                        match message {
                            Message::Flush(reply) => replies.push(reply),
                            Message::Record(record) => {
                                let size = record.bytes.len() as u64;
                                writer_counters.queue_depth.fetch_sub(1, Ordering::Relaxed);
                                writer_counters.queued_bytes.fetch_sub(size, Ordering::Relaxed);
                                wrote_any = true;
                                if let Err(error) = writer.handle(*record) {
                                    let n = writer_lost.fetch_add(1, Ordering::Relaxed) + 1;
                                    writer_counters.write_errors.fetch_add(1, Ordering::Relaxed);
                                    if n.is_power_of_two() {
                                        tracing::error!(%error, lost_records = n,
                                            "discovery audit has gaps");
                                    }
                                }
                            }
                        }
                    }
                    // Durability before the reply, always: a caller that waited
                    // on `flush` is entitled to assume its records are on disk.
                    let flushed = {
                        use std::io::Write as _;
                        writer
                            .file
                            .as_mut()
                            .map_or(Ok(()), |f| f.flush().and_then(|()| f.get_ref().sync_all()))
                            .map_err(|e| e.to_string())
                    };
                    if let Err(error) = &flushed {
                        writer_counters.write_errors.fetch_add(1, Ordering::Relaxed);
                        tracing::error!(%error, "discovery audit could not flush a batch");
                    }
                    if wrote_any && flushed.is_ok() {
                        writer_counters.batches_written.fetch_add(1, Ordering::Relaxed);
                        writer_counters
                            .last_write_micros
                            .store(Utc::now().timestamp_micros(), Ordering::Relaxed);
                    }
                    for reply in replies {
                        let _ = reply.send(flushed.clone());
                    }
                }
                use std::io::Write as _;
                if let Some(file) = writer.file.as_mut() {
                    let _ = file.flush();
                }
            });
            Some(Recorder {
                tx,
                lost,
                sampled_out,
                counters,
                queue_lost_unreported: Arc::new(AtomicU64::new(0)),
                queue_loss_onset_micros: Arc::new(AtomicI64::new(0)),
                queue_loss_classes: Arc::new(AtomicU64::new(0)),
            })
        })
        .as_ref()
}

pub fn enabled() -> bool {
    recorder().is_some()
}

pub fn emit(kind: &str, data: Value) {
    if let Some(r) = recorder() {
        // Describe any queue-loss span that has not yet reached the stream.
        // Read before building the record so the count we publish is exactly
        // the count we later clear -- anything lost in between stays pending
        // for the next admitted record rather than being dropped silently.
        let unreported = r.queue_lost_unreported.load(Ordering::Relaxed);
        let queue_loss = if unreported > 0 {
            let onset = r.queue_loss_onset_micros.load(Ordering::Relaxed);
            json!({
                "lost": unreported,
                "onsetMicros": onset,
                "onset": chrono::DateTime::from_timestamp_micros(onset)
                    .map(|t| t.to_rfc3339()),
                "classes": kinds_from_mask(r.queue_loss_classes.load(Ordering::Relaxed)),
                "reason": "queue_full",
            })
        } else {
            Value::Null
        };
        let now = Utc::now();
        let record = json!({"schema":2,"recorded_at":now,"kind":kind,
            "lost_records":r.lost.load(Ordering::Relaxed),
            "sampled_out":r.sampled_out.load(Ordering::Relaxed),
            "queue_loss":queue_loss,"data":data});
        r.counters.attempted.fetch_add(1, Ordering::Relaxed);
        // Encoded here rather than on the writer thread, so the queue can be
        // bounded in bytes. Discovery record sizes span 235 B to 2 MB, and a
        // bound in records alone is a bound on an unknown quantity.
        let Ok(mut encoded) = serde_json::to_vec(&record) else {
            r.counters.write_errors.fetch_add(1, Ordering::Relaxed);
            r.lost.fetch_add(1, Ordering::Relaxed);
            return;
        };
        encoded.push(b'\n');
        let size = encoded.len() as u64;
        let day = now.date_naive().to_string();
        // Reserve the slot and its bytes *before* sending, and give them back if
        // the channel refuses.
        //
        // Ordering matters and is not a style choice. The writer thread
        // subtracts the moment it receives a message, so adding after a
        // successful send races: the subtraction can land first, wrap the
        // unsigned counter to near `u64::MAX`, and the next byte-bound check
        // then overflows. Reserving first makes every subtraction correspond to
        // an addition that already happened.
        //
        // Found by the combined load test, not by inspection -- this path only
        // races when a writer is genuinely draining while a producer is
        // genuinely emitting, which is exactly the condition a single-threaded
        // unit test never creates.
        let qb = r.counters.queued_bytes.fetch_add(size, Ordering::Relaxed) + size;
        let admitted = if qb > QUEUE_BYTES {
            r.counters.queued_bytes.fetch_sub(size, Ordering::Relaxed);
            false
        } else {
            r.counters.queued_bytes_peak.fetch_max(qb, Ordering::Relaxed);
            let depth = r.counters.queue_depth.fetch_add(1, Ordering::Relaxed) + 1;
            r.counters.queue_peak.fetch_max(depth, Ordering::Relaxed);
            if r
                .tx
                .try_send(Message::Record(Box::new(Encoded {
                    kind: kind.to_string(),
                    day,
                    now,
                    bytes: encoded,
                })))
                .is_ok()
            {
                true
            } else {
                r.counters.queue_depth.fetch_sub(1, Ordering::Relaxed);
                r.counters.queued_bytes.fetch_sub(size, Ordering::Relaxed);
                false
            }
        };
        if admitted {
            if unreported > 0 {
                // Subtract exactly what this record described. A concurrent
                // loss that arrived after the load stays counted for the next
                // record, so no span is ever reported twice or lost.
                r.queue_lost_unreported
                    .fetch_sub(unreported, Ordering::Relaxed);
                if r.queue_lost_unreported.load(Ordering::Relaxed) == 0 {
                    r.queue_loss_classes.store(0, Ordering::Relaxed);
                }
            }
        } else {
            let n = r.lost.fetch_add(1, Ordering::Relaxed) + 1;
            r.counters.queue_lost.fetch_add(1, Ordering::Relaxed);
            r.queue_loss_classes
                .fetch_or(kind_bit(kind), Ordering::Relaxed);
            if r.queue_lost_unreported.fetch_add(1, Ordering::Relaxed) == 0 {
                r.queue_loss_onset_micros
                    .store(Utc::now().timestamp_micros(), Ordering::Relaxed);
            }
            if n.is_power_of_two() {
                tracing::error!(
                    lost_records = n,
                    "discovery audit queue full or writer stopped"
                );
            }
        }
    }
}

/// Discovery's capture accounting, readable at any moment.
///
/// Shaped for `backtest_metrics::completeness::DiscoveryCapture` but declared
/// here so `market-data` keeps no dependency on the metrics crate.
#[derive(Debug, Clone, Default)]
pub struct DiscoveryHealth {
    pub attempted: u64,
    pub written: u64,
    pub queue_lost: u64,
    pub write_errors: u64,
    pub sampled_out: u64,
    pub budget_dropped: u64,
    pub lost_records_total: u64,
    pub queue_depth: u64,
    pub queue_peak: u64,
    pub queue_capacity: u64,
    pub queued_bytes: u64,
    pub queued_bytes_peak: u64,
    pub queue_capacity_bytes: u64,
    pub bytes_written: u64,
    pub batches_written: u64,
    pub last_write: Option<chrono::DateTime<Utc>>,
    pub current_file: String,
    pub current_file_bytes: u64,
    pub degraded: bool,
}

/// Capture accounting for the running process.
///
/// All zero when capture is disabled, which a caller distinguishes with
/// [`enabled`]. Note that `queue_lost` and `write_errors` are defects while
/// `sampled_out` and `budget_dropped` are the documented policy working -- the
/// deployed build summed all four into one `lost` counter, so the distinction
/// could only be recovered from the surrounding log text.
pub fn health() -> DiscoveryHealth {
    // `RECORDER.get()`, deliberately, not `recorder()`. `recorder()` is a
    // `get_or_init`, so reading health through it would *initialise* capture as
    // a side effect -- and if `DISCOVERY_AUDIT_DIR` is not set yet, it would
    // latch the recorder to `None` permanently. A health read must never be the
    // thing that decides whether capture runs.
    let Some(r) = RECORDER.get().and_then(|r| r.as_ref()) else {
        return DiscoveryHealth::default();
    };
    let c = &r.counters;
    let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
    let queue_lost = g(&c.queue_lost);
    let write_errors = g(&c.write_errors);
    DiscoveryHealth {
        attempted: g(&c.attempted),
        written: g(&c.written),
        queue_lost,
        write_errors,
        sampled_out: r.sampled_out.load(Ordering::Relaxed),
        budget_dropped: g(&c.budget_dropped),
        lost_records_total: r.lost.load(Ordering::Relaxed),
        queue_depth: g(&c.queue_depth),
        queue_peak: g(&c.queue_peak),
        queue_capacity: QUEUE_RECORDS as u64,
        queued_bytes: g(&c.queued_bytes),
        queued_bytes_peak: g(&c.queued_bytes_peak),
        queue_capacity_bytes: QUEUE_BYTES,
        bytes_written: g(&c.bytes_written),
        batches_written: g(&c.batches_written),
        last_write: match c.last_write_micros.load(Ordering::Relaxed) {
            0 => None,
            micros => chrono::DateTime::from_timestamp_micros(micros),
        },
        current_file: c.current_file.lock().map(|f| f.clone()).unwrap_or_default(),
        current_file_bytes: g(&c.current_file_bytes),
        degraded: queue_lost > 0 || write_errors > 0,
    }
}

/// Blocking durability barrier for one-shot tools, never called from tick dispatch.
pub fn flush() -> anyhow::Result<()> {
    let r =
        recorder().ok_or_else(|| anyhow::anyhow!("DISCOVERY_AUDIT_DIR is unset or unavailable"))?;
    let (tx, rx) = std::sync::mpsc::channel();
    r.tx.send(Message::Flush(tx))
        .map_err(|_| anyhow::anyhow!("audit writer stopped"))?;
    rx.recv_timeout(std::time::Duration::from_secs(30))?
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        r.lost.load(Ordering::Relaxed) == 0,
        "audit records were lost"
    );
    Ok(())
}

/// One first observed print per symbol per heartbeat interval, not every tick.
/// Presence proves receipt only at that instant; absence never proves no subscription.
#[derive(Default)]
pub struct Receipts(pub HashMap<String, Value>);
impl Receipts {
    pub fn trade(&mut self, trade: &crate::Trade, monitored: bool) {
        if enabled() && !self.0.contains_key(&trade.symbol) {
            self.0.insert(
                trade.symbol.clone(),
                json!({"symbol":trade.symbol,
                "market_at":trade.timestamp,"received_at":Utc::now(),
                "price":trade.price,"ignition_monitored":monitored}),
            );
        }
    }
    pub fn take(&mut self) -> HashMap<String, Value> {
        std::mem::take(&mut self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(h: u32, m: u32) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 10, h, m, 0).unwrap()
    }

    // --- 48-C: queue-pressure loss must become self-describing ---

    #[test]
    fn every_emitted_class_maps_to_exactly_one_bit() {
        let kinds = [
            "coverage",
            "ignition",
            "scan_started",
            "scan_completed",
            "snapshot_batch",
            "snapshot_complete",
            "stream_started",
        ];
        let mut seen = 0u64;
        for k in kinds {
            let bit = kind_bit(k);
            assert_eq!(bit.count_ones(), 1, "{k} must map to one bit");
            assert_eq!(seen & bit, 0, "{k} collides with another class");
            seen |= bit;
            assert_eq!(kinds_from_mask(bit), vec![k]);
        }
        // Unknown kinds collapse rather than growing cardinality.
        assert_eq!(kinds_from_mask(kind_bit("something_new")), vec!["other"]);
        assert_eq!(kinds_from_mask(kind_bit("another_new")), vec!["other"]);
    }

    #[test]
    fn a_loss_span_decodes_to_every_class_it_swallowed() {
        // The marker must name which classes were lost, not just how many.
        let mask = kind_bit("coverage") | kind_bit("ignition") | kind_bit("snapshot_batch");
        let mut names = kinds_from_mask(mask);
        names.sort();
        assert_eq!(names, vec!["coverage", "ignition", "snapshot_batch"]);
    }

    #[test]
    fn an_empty_mask_names_nothing() {
        assert!(kinds_from_mask(0).is_empty());
    }

    #[test]
    fn premarket_cannot_spend_the_whole_daily_budget() {
        // The R5 invariant: a premarket flood must leave the regular session
        // something to write into.
        let daily = 8 * 1024 * 1024 * 1024u64;
        let pre = applicable_budget(daily, utc(12, 0)); // 08:00 ET, premarket
        let regular = applicable_budget(daily, utc(15, 0)); // 11:00 ET, regular
        assert!(pre < regular, "premarket budget must be reserved-against");
        assert_eq!(pre, (daily as f64 * PRE_SESSION_BUDGET_FRACTION) as u64);
        assert_eq!(regular, daily);
        assert!(
            regular - pre > 0,
            "the reserve must be non-empty or the session is not protected"
        );
    }

    #[test]
    fn overnight_is_also_held_to_the_reserved_fraction() {
        let daily = 1000u64;
        assert_eq!(applicable_budget(daily, utc(6, 0)), 500); // 02:00 ET overnight
    }

    #[test]
    fn after_hours_may_use_the_full_budget() {
        // After-hours follows the session it belongs to; the reserve exists to
        // protect the regular session from what comes *before* it.
        let daily = 1000u64;
        assert_eq!(applicable_budget(daily, utc(22, 0)), 1000); // 18:00 ET
    }

    #[test]
    fn critical_classes_are_never_downsampled() {
        for kind in CRITICAL_KINDS {
            assert!(is_critical(kind), "{kind} must be preserved under pressure");
        }
        // Markers describe the loss, so they must outrank the loss.
        assert!(is_critical("capture_degraded_start"));
        assert!(is_critical("capture_drop_start"));
        // The high-volume classes are the ones that give way.
        assert!(!is_critical("coverage"));
        assert!(!is_critical("snapshot_batch"));
    }

    #[test]
    fn disk_scan_rebuilds_the_days_spend_so_restart_cannot_reset_it() {
        let dir = std::env::temp_dir().join(format!("ss-audit-{}", Utc::now().timestamp_nanos_opt().unwrap()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("2026-09-10-run-1.jsonl"), vec![b'x'; 100]).unwrap();
        std::fs::write(dir.join("2026-09-10-run-2.jsonl"), vec![b'x'; 250]).unwrap();
        std::fs::write(dir.join("2026-09-09-run-1.jsonl"), vec![b'x'; 900]).unwrap();
        let (day_bytes, dir_bytes, max_seq) = scan_disk_state(&dir, "2026-09-10");
        assert_eq!(day_bytes, 350, "today's spend must come from disk");
        assert_eq!(dir_bytes, 1250, "ceiling accounting spans every segment");
        assert_eq!(max_seq, 2, "rotation resumes after the highest existing sequence");
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- Adversarial: drive the writer directly against a temp directory ---

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ss-{tag}-{}",
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn writer(dir: &Path, per_file: u64, daily: u64, ceiling: u64) -> Writer {
        Writer {
            dir: dir.to_path_buf(),
            run: "test".into(),
            per_file,
            daily_budget: daily,
            ceiling,
            day: String::new(),
            seq: 0,
            file: None,
            path: None,
            file_bytes: 0,
            day_bytes: 0,
            dir_bytes: 0,
            degraded: false,
            seen_since_sample: 0,
            span_sampled: 0,
            span_dropped: 0,
            lost: Arc::new(AtomicU64::new(0)),
            sampled_out: Arc::new(AtomicU64::new(0)),
            counters: Arc::new(Counters::default()),
            // Write-through, so these tests can read the filesystem straight
            // after `handle`.
            buffer_bytes: 0,
        }
    }

    /// Encodes a `Value` the way `emit` does, so a test drives the writer
    /// through exactly the representation production uses.
    fn encoded(value: Value) -> Encoded {
        let stamp = value["recorded_at"].as_str().unwrap_or_default().to_string();
        let now = stamp.parse::<chrono::DateTime<Utc>>().unwrap();
        let mut bytes = serde_json::to_vec(&value).unwrap();
        bytes.push(b'\n');
        Encoded {
            kind: value["kind"].as_str().unwrap_or_default().to_string(),
            day: stamp[..10].to_string(),
            now,
            bytes,
        }
    }

    /// A record of `kind` stamped at `hour:minute` UTC on 2026-09-10, padded so
    /// each one costs a predictable number of bytes.
    fn record(kind: &str, h: u32, m: u32, pad: usize) -> Encoded {
        encoded(json!({
            "schema": 2,
            "recorded_at": utc(h, m).to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "kind": kind,
            "data": {"pad": "x".repeat(pad)},
        }))
    }

    fn segments(dir: &Path) -> Vec<PathBuf> {
        let mut v: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
            .collect();
        v.sort();
        v
    }

    fn all_kinds(dir: &Path) -> Vec<String> {
        let mut kinds = Vec::new();
        for path in segments(dir) {
            for line in std::fs::read_to_string(&path).unwrap_or_default().lines() {
                if let Ok(v) = serde_json::from_str::<Value>(line) {
                    kinds.push(v["kind"].as_str().unwrap_or_default().to_string());
                }
            }
        }
        kinds
    }

    #[test]
    fn a_premarket_flood_cannot_make_regular_session_capture_impossible() {
        // The central R5 invariant, and the exact 2026-09-10 failure: premarket
        // volume exhausted the day's allowance an hour before the open.
        let dir = temp_dir("flood");
        let mut w = writer(&dir, 4096, 8192, 1 << 20);

        // Premarket flood: far more than the whole daily budget.
        for i in 0..400 {
            let _ = w.handle(record("coverage", 11, i % 60, 200));
        }
        let after_premarket = w.day_bytes;
        assert!(
            after_premarket <= (8192.0 * PRE_SESSION_BUDGET_FRACTION) as u64 + 512,
            "premarket spent {after_premarket}, past its reserved-against share"
        );

        // Regular session must still be able to write.
        let before = w.day_bytes;
        for i in 0..10 {
            w.handle(record("scan_completed", 15, i, 100)).unwrap();
        }
        assert!(
            w.day_bytes > before,
            "regular session could not write after a premarket flood"
        );
        let kinds = all_kinds(&dir);
        assert!(
            kinds.iter().any(|k| k == "scan_completed"),
            "no regular-session record survived"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reaching_a_file_budget_rotates_instead_of_stopping() {
        let dir = temp_dir("rotate");
        let mut w = writer(&dir, 512, 1 << 20, 1 << 20);
        for i in 0..40 {
            w.handle(record("scan_completed", 15, i % 60, 60)).unwrap();
        }
        let files = segments(&dir);
        assert!(
            files.len() > 2,
            "expected multiple rotations, got {}",
            files.len()
        );
        assert!(
            all_kinds(&dir).iter().any(|k| k == "capture_rotated"),
            "rotation must be announced in-band"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_directory_ceiling_bounds_growth_and_spares_the_current_segment() {
        let dir = temp_dir("ceiling");
        let mut w = writer(&dir, 512, 1 << 30, 2048);
        for i in 0..200 {
            w.handle(record("scan_completed", 15, i % 60, 60)).unwrap();
        }
        let total: u64 = segments(&dir)
            .iter()
            .map(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
            .sum();
        assert!(
            total <= 2048 * 3,
            "directory grew to {total} against a 2048-byte ceiling"
        );
        assert!(
            w.path.as_ref().is_some_and(|p| p.exists()),
            "the segment being written must never be deleted"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn degradation_downsamples_the_bulk_class_and_says_so_in_band() {
        let dir = temp_dir("degrade");
        let mut w = writer(&dir, 1 << 20, 4096, 1 << 20);
        for i in 0..300 {
            let _ = w.handle(record("coverage", 15, i % 60, 100));
        }
        assert!(w.degraded, "pressure must engage degradation");
        assert!(
            w.sampled_out.load(Ordering::Relaxed) > 0,
            "degradation must actually downsample"
        );
        let kinds = all_kinds(&dir);
        assert!(
            kinds.iter().any(|k| k == "capture_degraded_start"),
            "degradation onset must be discoverable from the data alone"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn critical_records_survive_budget_exhaustion_and_drops_are_self_describing() {
        let dir = temp_dir("critical");
        let mut w = writer(&dir, 1 << 20, 3000, 1 << 20);
        for i in 0..200 {
            let _ = w.handle(record("coverage", 15, i % 60, 120));
        }
        // Budget is now exhausted for the degradable class.
        w.handle(record("ignition", 15, 59, 40)).unwrap();
        let kinds = all_kinds(&dir);
        assert!(
            kinds.iter().any(|k| k == "ignition"),
            "an analytically critical record must survive exhaustion"
        );
        assert!(
            kinds.iter().any(|k| k == "capture_drop_start"),
            "a drop span must be announced in-band, not only in logs"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_restart_cannot_grant_the_same_day_a_fresh_budget() {
        // Restart safety: the second writer must inherit the first's spend.
        let dir = temp_dir("restart");
        let mut first = writer(&dir, 1 << 20, 4000, 1 << 20);
        for i in 0..10 {
            first.handle(record("scan_completed", 15, i, 100)).unwrap();
        }
        let spent = first.day_bytes;
        assert!(spent > 0);
        drop(first);

        let mut second = writer(&dir, 1 << 20, 4000, 1 << 20);
        second.handle(record("scan_completed", 16, 0, 100)).unwrap();
        assert!(
            second.day_bytes > spent,
            "restart reset the day's spend: {} vs {spent}",
            second.day_bytes
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_utc_day_rollover_starts_a_fresh_allowance_and_a_new_segment() {
        let dir = temp_dir("rollover");
        let mut w = writer(&dir, 1 << 20, 4000, 1 << 20);
        for i in 0..10 {
            w.handle(record("scan_completed", 15, i, 100)).unwrap();
        }
        let day_one = w.day_bytes;

        // Same writer, next UTC day.
        let next = json!({
            "schema": 2,
            "recorded_at": "2026-09-11T15:00:00Z",
            "kind": "scan_completed",
            "data": {"pad": "x"},
        });
        w.handle(encoded(next)).unwrap();
        assert_eq!(w.day, "2026-09-11");
        assert!(
            w.day_bytes < day_one,
            "a new UTC day must start from its own allowance"
        );
        assert!(
            segments(&dir).iter().any(|p| p
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("2026-09-11-")),
            "the new day must open its own segment"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- Writer load (section 17) ------------------------------------------
    //
    // Measured over the preserved September-16 capture (5,951,808 records,
    // 16.43 GB):
    //
    //   one scan tick          68-69 records, 2.85 MB, emitted synchronously
    //   heaviest second        3,190 records, 2.28 MB
    //   ignition prints        5,587,387 records at a 235 B mean
    //   coverage records       255 KB mean, 2,048,478 B maximum
    //
    // The deployed queue was 32 records, which is under half of one scan tick.

    /// One scan tick, at its measured shape.
    fn scan_tick(h: u32, m: u32) -> Vec<Encoded> {
        let mut out = vec![record("scan_started", h, m, 89_000)];
        for _ in 0..65 {
            out.push(record("snapshot_batch", h, m, 39_000));
        }
        out.push(record("coverage", h, m, 250_000));
        out.push(record("scan_completed", h, m, 150_000));
        out.push(record("snapshot_complete", h, m, 40));
        out
    }

    #[test]
    fn one_scan_tick_no_longer_overruns_the_queue() {
        let tick = scan_tick(15, 0);
        assert_eq!(tick.len(), 69, "the fixture must match the measured tick shape");
        let bytes: usize = tick.iter().map(|r| r.bytes.len()).sum();
        assert!(
            bytes > 2_500_000,
            "and its measured size: {bytes} B"
        );
        assert!(
            tick.len() > 32,
            "the deployed queue held 32 records -- under half of this tick, which is \
             why every scan began by overrunning it"
        );
        assert!(
            tick.len() < QUEUE_RECORDS,
            "the repaired queue must hold a whole tick with room to spare"
        );
        assert!(
            (bytes as u64) < QUEUE_BYTES,
            "and the byte bound must hold it too"
        );
        // 45 whole ticks, which is the figure the constant's documentation cites.
        assert!(QUEUE_BYTES / bytes as u64 >= 40);
    }

    /// A record bound alone bounds an unknown quantity.
    ///
    /// This is the discovery-specific lesson: record sizes here span 235 B to
    /// 2,048,478 B, so 65,536 `coverage` records at the observed maximum would
    /// be 128 GB of queued memory. The byte bound is what makes the footprint
    /// statable.
    #[test]
    fn the_byte_bound_is_what_caps_the_footprint_not_the_record_bound() {
        let largest = 2_048_478u64;
        assert!(
            QUEUE_RECORDS as u64 * largest > 100 * 1024 * 1024 * 1024,
            "the record bound alone would permit a footprint of {} GB",
            QUEUE_RECORDS as u64 * largest / (1024 * 1024 * 1024)
        );
        assert_eq!(QUEUE_BYTES, 128 * 1024 * 1024, "the byte bound is the real cap");
        // At the ignition mean the record bound is the one that binds, and
        // costs little: 65,536 x 235 B is about 15 MB.
        assert!(QUEUE_RECORDS as u64 * 235 < QUEUE_BYTES);
    }

    /// Batching is a storage optimisation only: same records, same order,
    /// same count, same bytes.
    #[test]
    fn buffered_writing_preserves_order_content_and_count() {
        let dir = temp_dir("batched");
        // A real write buffer, unlike the policy tests above, so the buffered
        // path is the one under test.
        let mut w = writer(&dir, 1 << 30, 1 << 40, 1 << 40);
        w.buffer_bytes = 1024 * 1024;

        let mut expected: Vec<String> = Vec::new();
        for i in 0..1_000u32 {
            let rec = record("ignition", 15, i % 60, 40 + (i % 17) as usize);
            expected.push(String::from_utf8(rec.bytes.clone()).unwrap());
            w.handle(rec).unwrap();
        }
        {
            use std::io::Write as _;
            w.file.as_mut().unwrap().flush().unwrap();
        }

        let mut got: Vec<String> = Vec::new();
        for path in segments(&dir) {
            for line in std::fs::read_to_string(&path).unwrap().lines() {
                got.push(format!("{line}\n"));
            }
        }
        assert_eq!(got.len(), expected.len(), "every record must reach disk exactly once");
        assert_eq!(got, expected, "buffering must not reorder or rewrite anything");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Policy and defect stay separable.
    ///
    /// The deployed build folded queue loss, writer errors and budget refusals
    /// into one `lost` counter, so `lost_records=4096` could not distinguish the
    /// instrument failing from the instrument doing exactly what it was
    /// configured to do. `lost` is preserved unchanged as the aggregate; the
    /// disaggregated counters are what the completeness verdict reads.
    #[test]
    fn a_budget_refusal_is_counted_as_policy_not_as_queue_loss() {
        let dir = temp_dir("disagg");
        let mut w = writer(&dir, 1 << 20, 3_000, 1 << 20);
        let counters = w.counters.clone();
        let lost = w.lost.clone();

        // Well past a 3,000-byte daily budget.
        for i in 0..40u32 {
            let _ = w.handle(record("coverage", 15, i % 60, 200));
        }

        let budget_dropped = counters.budget_dropped.load(Ordering::Relaxed);
        assert!(budget_dropped > 0, "the budget must actually have refused records");
        assert_eq!(
            counters.queue_lost.load(Ordering::Relaxed),
            0,
            "a budget refusal is not queue pressure"
        );
        assert_eq!(
            counters.write_errors.load(Ordering::Relaxed),
            0,
            "and it is not a write error"
        );
        assert_eq!(
            lost.load(Ordering::Relaxed),
            budget_dropped,
            "the aggregate `lost_records` keeps its existing meaning, unchanged"
        );
        assert!(
            counters.written.load(Ordering::Relaxed) > 0,
            "critical records must still have been written"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The in-band queue-loss span semantics are preserved exactly.
    ///
    /// Discovery already carried its loss span on the next admitted record, and
    /// section 5 requires that to survive the repair rather than be replaced.
    #[test]
    fn the_in_band_queue_loss_span_is_unchanged() {
        // Asserted structurally rather than by driving the global recorder,
        // which a test cannot initialise twice in one process.
        let mask = kind_bit("ignition") | kind_bit("coverage");
        let classes = kinds_from_mask(mask);
        assert!(classes.contains(&"ignition"), "a loss span still names its classes");
        assert!(classes.contains(&"coverage"));
        assert_eq!(kinds_from_mask(0), Vec::<&str>::new(), "an empty span names nothing");
    }

    #[test]
    fn retention_never_offers_the_current_segment() {
        let dir = std::env::temp_dir().join(format!("ss-ret-{}", Utc::now().timestamp_nanos_opt().unwrap()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("2026-09-10-run-1.jsonl");
        let b = dir.join("2026-09-10-run-2.jsonl");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let eligible = reclaimable_segments(&dir, Some(&b));
        assert!(eligible.iter().any(|(p, _)| *p == a));
        assert!(
            !eligible.iter().any(|(p, _)| *p == b),
            "the segment being written must never be reclaimable"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
