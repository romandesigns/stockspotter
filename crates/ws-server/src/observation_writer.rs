//! Bounded asynchronous writer for the observation stream.
//!
//! The synchronous sink this replaces for live use had one defect that no
//! amount of testing makes acceptable: it wrote on the consumer thread. Every
//! other research writer in this crate puts a bounded queue and a dedicated
//! thread between the realtime path and the disk, and it does so for a reason
//! that is specific rather than stylistic -- a slow or failing disk must cost
//! **counted dropped records**, never consumer latency. A research subsystem
//! that can stall the market-data consumer is a liability to the thing it is
//! observing.
//!
//! Three properties are load-bearing here, and each has a test:
//!
//! 1. **Enqueue never blocks.** Full queue or exhausted byte budget means the
//!    record is dropped and counted, immediately. There is no path on which
//!    the consumer waits for the disk.
//! 2. **Loss is known, bounded and localised.** A drop is not just a counter
//!    bump: consecutive drops accumulate into a `LossSpan` carrying the
//!    ordinal range, record and byte counts, and the first/last instants. A
//!    capture with any loss cannot be certified, so the spans exist to say
//!    *what* was lost, not to excuse it.
//! 3. **Shutdown is deterministic.** `drain` blocks until the writer thread
//!    has processed everything enqueued before it, or reports a timeout. It
//!    never reports success on a timeout, because that would be exactly the
//!    "absence turned into success" the certificate contract forbids.
//!
//! `written` means *written by the writer thread*, not *accepted by the
//! queue*. That distinction is why `on_finish` drains before reading the
//! counters it records.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{ObservationRecord, ObservationSink, WriterCounters};

/// Default queue depth. Chosen to absorb a ranking window's candidate burst
/// without becoming a second unbounded buffer; it is a starting value, not a
/// measured one, and §13's characterisation is what should move it.
pub const DEFAULT_QUEUE_CAPACITY: usize = 4_096;

/// Default queued-byte budget. A depth bound alone does not bound memory --
/// one window's candidate rows are small, but nothing in the type system says
/// a record cannot be large, so both bounds exist.
pub const DEFAULT_BYTE_CAPACITY: u64 = 32 * 1024 * 1024;

/// Default wait for a close. Short, because a close on the consumer thread
/// (process shutdown) must not hang it. Session rollover closes on its own
/// thread and uses `SESSION_CLOSE_TIMEOUT`.
pub const DEFAULT_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Wait for a session-rollover close, which runs off the consumer thread and
/// can afford to outlast a slow disk rather than condemn the session.
pub const SESSION_CLOSE_TIMEOUT: Duration = Duration::from_secs(120);

/// Retained loss spans. Beyond this the spans are truncated and the flag says
/// so, because a truncated loss report must never look like a complete one.
pub const MAX_LOSS_SPANS: usize = 256;

/// One contiguous run of dropped records.
///
/// Ordinals are the writer's own `attempted` sequence, so a span localises the
/// loss within the stream even though a dropped record left no line behind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LossSpan {
    pub from_ordinal: u64,
    pub to_ordinal: u64,
    pub records: u64,
    pub bytes: u64,
    pub first_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
}

/// Queue and loss telemetry, recorded with the run's terminal record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriterTelemetry {
    pub queue_capacity: u64,
    pub byte_capacity: u64,
    pub queue_peak: u64,
    pub queued_bytes_peak: u64,
    pub dropped_bytes: u64,
    pub loss_spans: Vec<LossSpan>,
    /// More spans occurred than were retained. The span list is then a sample,
    /// and the counters remain the authority on how much was lost.
    pub loss_spans_truncated: bool,
    /// Peak queue use reached the warning level (`QUEUE_WARN_PERMILLE` of
    /// either the record or the byte bound). A warning, not loss: loss is the
    /// first dropped record, which the certificate already refuses.
    #[serde(default)]
    pub queue_warning: bool,
}

/// Queue warning level: 25% of either bound. Step 4A proposal, provisional
/// until the Step 4 preregistration freezes it.
pub const QUEUE_WARN_PERMILLE: u64 = 250;

/// What the writer thread does with a serialized record.
///
/// A trait so overload, slowness and real write failures can be exercised
/// without a filesystem that cooperates on demand.
pub trait RecordWriter: Send {
    fn write_line(&mut self, line: &str) -> std::io::Result<()>;

    /// Write the terminal record of the current file. Distinct from
    /// `write_line` because a terminal record must land in the file it closes:
    /// a rotating writer's `write_line` may rotate first, which would put the
    /// terminal record of one file into the next. The default is `write_line`.
    fn write_terminal(&mut self, line: &str) -> std::io::Result<()> {
        self.write_line(line)
    }

    /// Flush and fsync everything written so far, keeping the writer open.
    ///
    /// The close protocol calls this twice: once for the data, before any
    /// terminal record exists, and once for the terminal record. The default
    /// has nothing to make durable.
    fn sync(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    /// Release the file. Called once, after the terminal record is durable.
    fn finish(&mut self) -> std::io::Result<()>;

    /// Cut the last `terminal_bytes` bytes -- a terminal record whose own
    /// durability failed -- back off the file, so it reads as open rather than
    /// as closed. The default cannot, and says so.
    fn retract_terminal(&mut self, terminal_bytes: u64) -> std::io::Result<()> {
        let _ = terminal_bytes;
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "cannot retract a terminal record"))
    }

    /// Leave a `.close-failed` marker beside the file, for when retraction
    /// itself failed. The reader refuses any run carrying one.
    fn mark_close_failed(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "no marker location"))
    }

    /// The terminal record for the file being closed, when the writer knows it
    /// better than the sink does. A rotating writer does: only it knows which
    /// file is current and how many lines that file holds. `None` lets the
    /// sink build the single-file terminal record.
    fn terminal_line(&mut self, run_id: &str, at: DateTime<Utc>) -> Option<String> {
        let _ = (run_id, at);
        None
    }
}

/// The production writer: buffered append, `sync_all` at each durability
/// point of the close protocol.
pub struct FileRecordWriter {
    writer: Option<std::io::BufWriter<std::fs::File>>,
    /// Where a `.close-failed` marker goes; `None` over a bare handle.
    path: Option<std::path::PathBuf>,
    /// Bytes handed to the file, so a terminal record can be cut back off.
    len: u64,
    /// Added before every fsync. Zero in production; tests set it to prove a
    /// slow disk costs the writer thread, never the consumer.
    sync_delay: Duration,
}

impl FileRecordWriter {
    pub fn create(dir: &std::path::Path, file_name: &str) -> std::io::Result<Self> {
        let path = dir.join(file_name);
        let file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
        Ok(Self {
            writer: Some(std::io::BufWriter::new(file)),
            path: Some(path),
            len: 0,
            sync_delay: Duration::ZERO,
        })
    }

    pub fn from_file(file: std::fs::File) -> Self {
        Self { writer: Some(std::io::BufWriter::new(file)), path: None, len: 0, sync_delay: Duration::ZERO }
    }

    /// Test seam: make every fsync at least this slow.
    pub fn with_sync_delay(mut self, delay: Duration) -> Self {
        self.sync_delay = delay;
        self
    }
}

impl RecordWriter for FileRecordWriter {
    fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        use std::io::Write;
        let Some(writer) = self.writer.as_mut() else {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "writer already finished"));
        };
        writer.write_all(line.as_bytes())?;
        writer.write_all(b"\n")?;
        self.len += line.len() as u64 + 1;
        Ok(())
    }

    fn sync(&mut self) -> std::io::Result<()> {
        use std::io::Write;
        let Some(writer) = self.writer.as_mut() else {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "writer already finished"));
        };
        writer.flush()?;
        if !self.sync_delay.is_zero() {
            std::thread::sleep(self.sync_delay);
        }
        writer.get_ref().sync_all()
    }

    fn finish(&mut self) -> std::io::Result<()> {
        // Everything is already durable by the time the protocol calls this;
        // releasing the handle is all that is left.
        match self.writer.take() {
            Some(_) => Ok(()),
            None => Err(std::io::Error::new(std::io::ErrorKind::Other, "writer already finished")),
        }
    }

    fn retract_terminal(&mut self, terminal_bytes: u64) -> std::io::Result<()> {
        let Some(writer) = self.writer.take() else {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "writer already finished"));
        };
        // `into_parts` discards the unflushed buffer instead of writing it:
        // flushing it later would put the terminal record straight back.
        let (file, _unflushed) = writer.into_parts();
        let data_len = self.len.saturating_sub(terminal_bytes);
        file.set_len(data_len)?;
        file.sync_all()
    }

    fn mark_close_failed(&mut self) -> std::io::Result<()> {
        let Some(path) = self.path.as_ref() else {
            return Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "no marker location"));
        };
        std::fs::write(super::close_failed_marker(path), b"terminal record not durable\n")
    }
}

#[derive(Debug, Default)]
struct Metrics {
    attempted: AtomicU64,
    written: AtomicU64,
    dropped: AtomicU64,
    write_errors: AtomicU64,
    queue_depth: AtomicU64,
    queue_peak: AtomicU64,
    queued_bytes: AtomicU64,
    queued_bytes_peak: AtomicU64,
    dropped_bytes: AtomicU64,
    overflowed: AtomicBool,
}

impl Metrics {
    fn counters(&self) -> WriterCounters {
        WriterCounters {
            attempted: self.attempted.load(Ordering::Relaxed),
            written: self.written.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            write_errors: self.write_errors.load(Ordering::Relaxed),
            overflowed: self.overflowed.load(Ordering::Relaxed),
        }
    }

    fn bump(&self, counter: &AtomicU64) {
        if counter.fetch_add(1, Ordering::Relaxed) == u64::MAX {
            self.overflowed.store(true, Ordering::Relaxed);
        }
    }
}

enum Command {
    Line { text: String, bytes: u64 },
    /// FIFO marker: when the writer thread reaches it, everything enqueued
    /// earlier has been handled.
    Drain(SyncSender<()>),
    /// The close protocol, run on the writer thread in order: make the data
    /// durable, then write the terminal line and make that durable, then
    /// release the file. Replies with the first failure.
    Close { run_id: String, at: DateTime<Utc>, fallback: String, ack: SyncSender<std::io::Result<()>> },
}

#[derive(Debug, Default)]
struct LossState {
    open: Option<LossSpan>,
    spans: Vec<LossSpan>,
    truncated: bool,
}

/// A bounded, non-blocking, asynchronous observation sink.
pub struct AsyncSink {
    tx: Option<SyncSender<Command>>,
    handle: Option<JoinHandle<()>>,
    metrics: Arc<Metrics>,
    loss: Arc<Mutex<LossState>>,
    queue_capacity: usize,
    byte_capacity: u64,
    file_name: String,
    next_file: Option<String>,
    finished: bool,
    /// How long `close` waits for the writer to drain and then to finish the
    /// durable close. A timeout is a failed close (never a quiet success).
    close_timeout: Duration,
}

impl AsyncSink {
    pub fn new(file_name: &str, writer: Box<dyn RecordWriter>) -> Self {
        Self::with_capacity(file_name, writer, DEFAULT_QUEUE_CAPACITY, DEFAULT_BYTE_CAPACITY)
    }

    pub fn with_capacity(
        file_name: &str,
        mut writer: Box<dyn RecordWriter>,
        queue_capacity: usize,
        byte_capacity: u64,
    ) -> Self {
        let (tx, rx): (SyncSender<Command>, Receiver<Command>) = sync_channel(queue_capacity);
        let metrics = Arc::new(Metrics::default());
        let thread_metrics = Arc::clone(&metrics);
        let handle = std::thread::Builder::new()
            .name("observation-writer".to_string())
            .spawn(move || {
                while let Ok(command) = rx.recv() {
                    match command {
                        Command::Line { text, bytes } => {
                            thread_metrics.queue_depth.fetch_sub(1, Ordering::Relaxed);
                            thread_metrics.queued_bytes.fetch_sub(bytes, Ordering::Relaxed);
                            match writer.write_line(&text) {
                                Ok(()) => thread_metrics.bump(&thread_metrics.written),
                                Err(_) => thread_metrics.bump(&thread_metrics.write_errors),
                            }
                        }
                        Command::Drain(ack) => {
                            let _ = ack.send(());
                        }
                        Command::Close { run_id, at, fallback, ack } => {
                            // Here, on this thread, so the caller's `close`
                            // learns the real result rather than whether the
                            // message was delivered. A rotating writer builds
                            // its own terminal record: only it knows the
                            // current file and its line count.
                            let terminal = writer.terminal_line(&run_id, at).unwrap_or(fallback);
                            let _ = ack.send(close_durably(writer.as_mut(), &terminal));
                            return;
                        }
                    }
                }
            })
            .expect("spawn observation writer thread");
        Self {
            tx: Some(tx),
            handle: Some(handle),
            metrics,
            loss: Arc::new(Mutex::new(LossState::default())),
            queue_capacity,
            byte_capacity,
            file_name: file_name.to_string(),
            next_file: None,
            finished: false,
            close_timeout: DEFAULT_CLOSE_TIMEOUT,
        }
    }

    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    pub fn set_close_timeout(&mut self, timeout: Duration) {
        self.close_timeout = timeout;
    }

    /// Closes naming the file this run continues into.
    ///
    /// Separate from `close` so a rotation cannot be mistaken for the end of a
    /// run: a terminal record with `next_file: None` asserts the run stopped
    /// here, and one naming a successor asserts it did not.
    pub fn close_rotating(
        &mut self,
        run_id: &str,
        at: DateTime<Utc>,
        next_file: &str,
    ) -> std::io::Result<()> {
        self.next_file = Some(next_file.to_string());
        self.close(run_id, at)
    }

    pub fn queue_depth(&self) -> u64 {
        self.metrics.queue_depth.load(Ordering::Relaxed)
    }

    pub fn queue_peak(&self) -> u64 {
        self.metrics.queue_peak.load(Ordering::Relaxed)
    }

    pub fn queued_bytes(&self) -> u64 {
        self.metrics.queued_bytes.load(Ordering::Relaxed)
    }

    pub fn queued_bytes_peak(&self) -> u64 {
        self.metrics.queued_bytes_peak.load(Ordering::Relaxed)
    }

    /// Telemetry for the run's terminal record.
    ///
    /// Named apart from the trait method so the two never shadow each other:
    /// the trait returns `Option` because most sinks have no queue, this
    /// always has one.
    pub fn telemetry_snapshot(&self) -> WriterTelemetry {
        let mut loss = self.loss.lock().unwrap_or_else(|e| e.into_inner());
        // A span still open at the end is real loss and must be reported.
        let mut spans = loss.spans.clone();
        if let Some(open) = loss.open.take() {
            if spans.len() < MAX_LOSS_SPANS {
                spans.push(open);
            } else {
                loss.truncated = true;
            }
        }
        WriterTelemetry {
            queue_capacity: self.queue_capacity as u64,
            byte_capacity: self.byte_capacity,
            queue_peak: self.metrics.queue_peak.load(Ordering::Relaxed),
            queued_bytes_peak: self.metrics.queued_bytes_peak.load(Ordering::Relaxed),
            dropped_bytes: self.metrics.dropped_bytes.load(Ordering::Relaxed),
            loss_spans: spans,
            loss_spans_truncated: loss.truncated,
            queue_warning: self.queue_warning(),
        }
    }

    /// Peak use reached `QUEUE_WARN_PERMILLE` of either bound.
    pub fn queue_warning(&self) -> bool {
        let peak = self.metrics.queue_peak.load(Ordering::Relaxed);
        let bytes = self.metrics.queued_bytes_peak.load(Ordering::Relaxed);
        peak.saturating_mul(1000) >= (self.queue_capacity as u64).saturating_mul(QUEUE_WARN_PERMILLE)
            || bytes.saturating_mul(1000) >= self.byte_capacity.saturating_mul(QUEUE_WARN_PERMILLE)
    }

    fn record_drop(&self, ordinal: u64, bytes: u64) {
        self.metrics.bump(&self.metrics.dropped);
        self.metrics.dropped_bytes.fetch_add(bytes, Ordering::Relaxed);
        let now = Utc::now();
        let mut loss = self.loss.lock().unwrap_or_else(|e| e.into_inner());
        match loss.open.as_mut() {
            // Contiguous with the span already open: extend it.
            Some(span) if span.to_ordinal + 1 == ordinal => {
                span.to_ordinal = ordinal;
                span.records += 1;
                span.bytes += bytes;
                span.last_at = now;
            }
            _ => {
                if let Some(closed) = loss.open.take() {
                    if loss.spans.len() < MAX_LOSS_SPANS {
                        loss.spans.push(closed);
                    } else {
                        loss.truncated = true;
                    }
                }
                loss.open = Some(LossSpan {
                    from_ordinal: ordinal,
                    to_ordinal: ordinal,
                    records: 1,
                    bytes,
                    first_at: now,
                    last_at: now,
                });
            }
        }
    }

    /// Blocks until the writer thread has handled everything enqueued before
    /// this call, or the deadline passes.
    ///
    /// A timeout is reported as an error and never as success: a drain that
    /// gave up is exactly the condition under which the counters are not yet
    /// final, and recording them as final would be a false completeness claim.
    pub fn drain_for(&self, timeout: Duration) -> std::io::Result<()> {
        let Some(tx) = self.tx.as_ref() else {
            return Ok(());
        };
        let (ack_tx, ack_rx) = sync_channel::<()>(1);
        let deadline = Instant::now() + timeout;
        let mut command = Command::Drain(ack_tx);
        loop {
            match tx.try_send(command) {
                Ok(()) => break,
                Err(TrySendError::Full(returned)) => {
                    if Instant::now() >= deadline {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "observation writer queue stayed full; drain not established",
                        ));
                    }
                    command = returned;
                    std::thread::yield_now();
                }
                Err(TrySendError::Disconnected(_)) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "observation writer thread is gone",
                    ))
                }
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        ack_rx.recv_timeout(remaining.max(Duration::from_millis(1))).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "observation writer did not acknowledge drain",
            )
        })
    }
}

/// A `RecordWriter` that rotates files **on the writer thread**.
///
/// Rotation used to run in the sink, on the caller's thread: drain, fsync the
/// old file, wait for the acknowledgement, open the next. Measured on NVMe
/// that added 13-24 ms to the ranking window that triggered it, and on a busy
/// disk an fsync is unbounded -- a research writer stalling the market-data
/// consumer, which is the one thing the async writer exists to prevent. Here
/// the consumer only ever enqueues; the whole rotation happens behind the
/// queue, in stream order.
///
/// The order is unchanged and still load-bearing: the old file runs the full
/// close protocol (data fsync, terminal record naming its successor, terminal
/// fsync) **before** the successor exists, so a crash between the two leaves a
/// complete closed file and no successor -- a truncated run, never a complete
/// one. A rotation that fails latches the writer failed: every later line is a
/// counted write error, so the capture is refused rather than continued into a
/// broken chain.
pub struct RotatingFileWriter {
    dir: std::path::PathBuf,
    run_id: String,
    rotate_bytes: u64,
    index: u32,
    current: FileRecordWriter,
    current_name: String,
    bytes_in_file: u64,
    /// Lines in the current file, the file's own opening record included --
    /// exactly what the reader counts before the terminal record.
    lines_in_file: u64,
    files: Arc<Mutex<Vec<String>>>,
    failed: bool,
    sync_delay: Duration,
}

impl RotatingFileWriter {
    pub fn create(dir: &std::path::Path, run_id: &str, rotate_bytes: u64) -> std::io::Result<Self> {
        let name = super::rotation_file_name(0);
        let current = FileRecordWriter::create(dir, &name)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            run_id: run_id.to_string(),
            rotate_bytes,
            index: 0,
            current,
            current_name: name.clone(),
            bytes_in_file: 0,
            lines_in_file: 0,
            files: Arc::new(Mutex::new(vec![name])),
            failed: false,
            sync_delay: Duration::ZERO,
        })
    }

    /// Test seam: every fsync, in every file of the run, at least this slow.
    pub fn with_sync_delay(mut self, delay: Duration) -> Self {
        self.sync_delay = delay;
        self.current = std::mem::replace(
            &mut self.current,
            FileRecordWriter { writer: None, path: None, len: 0, sync_delay: Duration::ZERO },
        )
        .with_sync_delay(delay);
        self
    }

    /// The files this run has opened, in order. Shared with the sink so the
    /// caller can see rotations without a round trip through the queue.
    pub fn files_handle(&self) -> Arc<Mutex<Vec<String>>> {
        Arc::clone(&self.files)
    }

    pub fn first_file_name(&self) -> String {
        self.current_name.clone()
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        let next_index = self.index + 1;
        let next_name = super::rotation_file_name(next_index);
        let terminal = serde_json::to_string(&ObservationRecord::FileClose {
            run_id: self.run_id.clone(),
            file_name: self.current_name.clone(),
            records_written: self.lines_in_file,
            closed_at: Utc::now(),
            next_file: Some(next_name.clone()),
        })
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        close_durably(&mut self.current, &terminal)?;
        let mut next = FileRecordWriter::create(&self.dir, &next_name)?.with_sync_delay(self.sync_delay);
        let start = serde_json::to_string(&ObservationRecord::FileStart {
            run_id: self.run_id.clone(),
            file_name: next_name.clone(),
            sequence: next_index,
            previous_file: self.current_name.clone(),
        })
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        next.write_line(&start)?;
        self.current = next;
        self.current_name = next_name.clone();
        self.index = next_index;
        self.bytes_in_file = start.len() as u64 + 1;
        self.lines_in_file = 1;
        self.files.lock().unwrap_or_else(|e| e.into_inner()).push(next_name);
        Ok(())
    }
}

impl RecordWriter for RotatingFileWriter {
    fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        if self.failed {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "rotation failed earlier; run refused"));
        }
        let size = line.len() as u64 + 1;
        // Rotate before writing, never mid-record, so no row straddles files.
        if self.bytes_in_file > 0 && self.bytes_in_file + size > self.rotate_bytes {
            if let Err(e) = self.rotate() {
                self.failed = true;
                return Err(e);
            }
        }
        self.current.write_line(line)?;
        self.bytes_in_file += size;
        self.lines_in_file += 1;
        Ok(())
    }

    fn write_terminal(&mut self, line: &str) -> std::io::Result<()> {
        self.current.write_line(line)
    }

    fn sync(&mut self) -> std::io::Result<()> {
        self.current.sync()
    }

    fn finish(&mut self) -> std::io::Result<()> {
        self.current.finish()
    }

    fn retract_terminal(&mut self, terminal_bytes: u64) -> std::io::Result<()> {
        self.current.retract_terminal(terminal_bytes)
    }

    fn mark_close_failed(&mut self) -> std::io::Result<()> {
        self.current.mark_close_failed()
    }

    fn terminal_line(&mut self, run_id: &str, at: DateTime<Utc>) -> Option<String> {
        serde_json::to_string(&ObservationRecord::FileClose {
            run_id: run_id.to_string(),
            file_name: self.current_name.clone(),
            records_written: self.lines_in_file,
            closed_at: at,
            next_file: None,
        })
        .ok()
    }
}

/// The frozen L1 close order, for any `RecordWriter`.
///
/// 1. flush + fsync the **data**. On failure no terminal record is written:
///    the file stays open and can only read INDETERMINATE.
/// 2. write the terminal line, then flush + fsync it.
/// 3. release the file.
///
/// If step 2 or 3 fails, the terminal line is retracted; if retraction fails,
/// a `.close-failed` marker is left for the reader to refuse. Either way the
/// capture cannot read PASS, and the first error is returned.
fn close_durably(writer: &mut dyn RecordWriter, terminal: &str) -> std::io::Result<()> {
    writer.sync()?;
    let terminal_bytes = terminal.len() as u64 + 1;
    let result =
        writer.write_terminal(terminal).and_then(|()| writer.sync()).and_then(|()| writer.finish());
    if let Err(e) = result {
        if writer.retract_terminal(terminal_bytes).is_err() {
            let _ = writer.mark_close_failed();
        }
        return Err(e);
    }
    Ok(())
}

impl ObservationSink for AsyncSink {
    fn write(&mut self, record: &ObservationRecord) -> std::io::Result<()> {
        let text = serde_json::to_string(record)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        self.write_serialized(record, &text)
    }

    fn write_serialized(&mut self, record: &ObservationRecord, line: &str) -> std::io::Result<()> {
        self.write_serialized_owned(record, line.to_string())
    }

    /// Takes the observer's own serialized line: the one copy the consumer
    /// used to make here (`line.to_string()`) is gone, the bytes are identical.
    fn write_serialized_owned(&mut self, _record: &ObservationRecord, text: String) -> std::io::Result<()> {
        let bytes = text.len() as u64 + 1;
        // `attempted` is bumped first and its value is the record's ordinal, so
        // a dropped record still occupies a position in the stream's accounting
        // even though it leaves no line.
        let ordinal = self.metrics.attempted.fetch_add(1, Ordering::Relaxed) + 1;
        if ordinal == u64::MAX {
            self.metrics.overflowed.store(true, Ordering::Relaxed);
        }
        let Some(tx) = self.tx.as_ref() else {
            self.record_drop(ordinal, bytes);
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "sink already closed"));
        };
        // Byte budget before depth: a small number of large records can exhaust
        // memory long before the depth bound notices.
        let queued = self.metrics.queued_bytes.load(Ordering::Relaxed);
        if queued + bytes > self.byte_capacity {
            self.record_drop(ordinal, bytes);
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "observation queue byte budget exhausted",
            ));
        }
        // Account for the record BEFORE handing it to the channel. The writer
        // thread decrements as soon as it pops, and it can pop before this
        // thread resumes -- so incrementing afterwards races, and the
        // decrement underflows a zero depth into `u64::MAX`. The rollback on a
        // refused send is what keeps the pre-accounting honest.
        let depth = self.metrics.queue_depth.fetch_add(1, Ordering::Relaxed) + 1;
        let total = self.metrics.queued_bytes.fetch_add(bytes, Ordering::Relaxed) + bytes;
        let rollback = || {
            self.metrics.queue_depth.fetch_sub(1, Ordering::Relaxed);
            self.metrics.queued_bytes.fetch_sub(bytes, Ordering::Relaxed);
        };
        match tx.try_send(Command::Line { text, bytes }) {
            Ok(()) => {
                // Peaks move only on a record that was really queued, so a
                // refused enqueue cannot inflate a high-water mark.
                self.metrics.queue_peak.fetch_max(depth, Ordering::Relaxed);
                self.metrics.queued_bytes_peak.fetch_max(total, Ordering::Relaxed);
                Ok(())
            }
            // The whole point: a full queue costs a counted record, not a
            // stalled consumer.
            Err(TrySendError::Full(_)) => {
                rollback();
                self.record_drop(ordinal, bytes);
                Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, "observation queue full"))
            }
            Err(TrySendError::Disconnected(_)) => {
                rollback();
                self.record_drop(ordinal, bytes);
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "observation writer thread is gone",
                ))
            }
        }
    }

    fn counters(&self) -> WriterCounters {
        self.metrics.counters()
    }

    fn drain(&mut self, timeout: Duration) -> std::io::Result<()> {
        self.drain_for(timeout)
    }

    fn telemetry(&self) -> Option<WriterTelemetry> {
        Some(self.telemetry_snapshot())
    }

    fn set_close_timeout(&mut self, timeout: Duration) {
        self.close_timeout = timeout;
    }

    fn close(&mut self, run_id: &str, at: DateTime<Utc>) -> std::io::Result<()> {
        if self.finished {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "sink already closed"));
        }
        // Drain first so `records_written` describes what is actually on disk
        // rather than what was accepted by the queue.
        self.drain_for(self.close_timeout)?;
        let close = ObservationRecord::FileClose {
            run_id: run_id.to_string(),
            file_name: self.file_name.clone(),
            records_written: self.metrics.written.load(Ordering::Relaxed),
            closed_at: at,
            next_file: self.next_file.clone(),
        };
        let fallback = serde_json::to_string(&close)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        self.finished = true;
        let tx = self.tx.take().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::Other, "sink already closed")
        })?;
        let (ack_tx, ack_rx) = sync_channel::<std::io::Result<()>>(1);
        let mut command =
            Command::Close { run_id: run_id.to_string(), at, fallback, ack: ack_tx };
        let deadline = Instant::now() + self.close_timeout;
        loop {
            match tx.try_send(command) {
                Ok(()) => break,
                Err(TrySendError::Full(returned)) if Instant::now() < deadline => {
                    command = returned;
                    std::thread::yield_now();
                }
                Err(TrySendError::Full(_)) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "observation writer queue stayed full; close not established",
                    ))
                }
                Err(TrySendError::Disconnected(_)) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "observation writer thread is gone",
                    ))
                }
            }
        }
        drop(tx);
        let result = ack_rx
            .recv_timeout(self.close_timeout)
            .unwrap_or_else(|_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "observation writer did not acknowledge close",
                ))
            });
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        result
    }
}

impl Drop for AsyncSink {
    fn drop(&mut self) {
        // Dropping without `close` leaves an unclosed file, which the reader
        // refuses as evidence. That is the correct outcome, so the only job
        // here is to let the thread exit rather than leak it.
        self.tx.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
