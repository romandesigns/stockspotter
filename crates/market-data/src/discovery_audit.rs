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
        Arc, OnceLock,
    },
};

use crate::trading_session::{classify_session, TradingSession};

/// Rotation unit. Deliberately far below the daily budget so retention can
/// reclaim space in useful increments -- deleting one 8 GiB file is a blunt
/// instrument, deleting the oldest of many 1 GiB segments is not.
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

struct Recorder {
    tx: SyncSender<Message>,
    lost: Arc<AtomicU64>,
    sampled_out: Arc<AtomicU64>,
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
enum Message {
    Record(Value),
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
    file: Option<std::fs::File>,
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
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        self.file_bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
        self.file = Some(file);
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

    fn handle(&mut self, record: Value) -> anyhow::Result<()> {
        let stamp = record["recorded_at"].as_str().unwrap_or_default();
        let today = stamp.get(..10).unwrap_or_default().to_string();
        anyhow::ensure!(!today.is_empty(), "record carries no recorded_at date");
        // Session is read from the record's own clock, not the wall clock: the
        // budget is a property of the data being written, which also makes the
        // reservation behaviour deterministically testable.
        let now = stamp
            .parse::<chrono::DateTime<Utc>>()
            .unwrap_or_else(|_| Utc::now());

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
        let kind = record["kind"].as_str().unwrap_or_default().to_string();

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

        let mut encoded = serde_json::to_vec(&record)?;
        encoded.push(b'\n');
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
            let (tx, rx) = sync_channel::<Message>(32);
            let lost = Arc::new(AtomicU64::new(0));
            let sampled_out = Arc::new(AtomicU64::new(0));
            let (writer_lost, writer_sampled) = (lost.clone(), sampled_out.clone());
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
                };
                for message in rx {
                    let record = match message {
                        Message::Record(record) => record,
                        Message::Flush(reply) => {
                            let result = writer
                                .file
                                .as_ref()
                                .map_or(Ok(()), |f| f.sync_all())
                                .map_err(|e| e.to_string());
                            let _ = reply.send(result);
                            continue;
                        }
                    };
                    if let Err(error) = writer.handle(record) {
                        let n = writer_lost.fetch_add(1, Ordering::Relaxed) + 1;
                        if n.is_power_of_two() {
                            tracing::error!(%error, lost_records = n, "discovery audit has gaps");
                        }
                    }
                }
            });
            Some(Recorder {
                tx,
                lost,
                sampled_out,
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
        let record = json!({"schema":2,"recorded_at":Utc::now(),"kind":kind,
            "lost_records":r.lost.load(Ordering::Relaxed),
            "sampled_out":r.sampled_out.load(Ordering::Relaxed),
            "queue_loss":queue_loss,"data":data});
        if r.tx.try_send(Message::Record(record)).is_ok() {
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
        }
    }

    /// A record of `kind` stamped at `hour:minute` UTC on 2026-09-10, padded so
    /// each one costs a predictable number of bytes.
    fn record(kind: &str, h: u32, m: u32, pad: usize) -> Value {
        json!({
            "schema": 2,
            "recorded_at": utc(h, m).to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "kind": kind,
            "data": {"pad": "x".repeat(pad)},
        })
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
        w.handle(next).unwrap();
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
