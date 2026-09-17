//! Bounded retention for research captures.
//!
//! # Why this has to exist before the repair is deployed
//!
//! The capture repair took Opportunity Intelligence from 282,255 records
//! written per session to a full-fidelity **2,542,526 — about 9.13 GB**, plus
//! roughly 0.32 GB of episodes. `data/research/` has never had a retention
//! policy of any kind. Discovery has both a daily byte budget and a directory
//! ceiling; the two captures in this crate have neither.
//!
//! Deploying a 7.2× volume increase with unbounded growth would trade a
//! capture-capacity defect for a disk-exhaustion one — and disk exhaustion
//! fails *inward*, as write errors on the live capture, which is exactly the
//! evidence loss the repair exists to prevent.
//!
//! # The safety rules, and why each one is there
//!
//! Deletion is irreversible and these files are the only copy of a market
//! session. Every rule below exists to make a wrong deletion impossible rather
//! than unlikely:
//!
//! * **Never the file being written.** Checked against the writer's own
//!   `current_file`, not inferred from the name.
//! * **Never today, never yesterday.** A session must be at least
//!   `min_age_days` (default 2) whole UTC days old. A capture that spans a
//!   midnight boundary can still receive late records for the previous day.
//! * **Never recently touched.** No file in the session may have been modified
//!   within `finalize_grace` (default 6 hours), whatever its date says.
//! * **Never while held.** A `<date>.hold` sentinel, or a global `HOLD` file,
//!   makes a session ineligible — that is what an export, a copy or a
//!   verification run takes before it reads.
//! * **Oldest finalized first**, so the most recent research stays longest.
//! * **Never silently.** Every deletion is logged and counted, and deleting a
//!   session with no export receipt is counted separately and logged at WARN.
//!
//! # What it deliberately does not do
//!
//! * It never touches `data/discovery-audit/`. Discovery owns its own budget
//!   and ceiling, and its semantics are explicitly out of scope.
//! * It never rewrites a record. It deletes whole sessions or nothing.
//! * It never runs on the market path. It runs on its own thread, on a timer.
//!   The writer threads are already decoupled from dispatch by their bounded
//!   queues; putting a multi-gigabyte `remove_file` on one of them would back
//!   that queue up, which is the failure this whole milestone is about.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use chrono::NaiveDate;
use tracing::{info, warn};

/// Default directory ceiling: **64 GiB**.
///
/// Derived, not picked. A full-fidelity regular session is ~9.13 GB of
/// Opportunity Intelligence plus ~0.32 GB of episodes and a few MB of markers,
/// so roughly **9.5 GB per session** — 64 GiB therefore retains about **seven**
/// sessions, which is a research window of a working fortnight.
///
/// Against the VPS as measured (read-only) on 2026-09-16: 387 GB total, 211 GB
/// used, **177 GB free**, of which `discovery-audit` already holds 80 GB under
/// its own separate ceiling. Research growing from its current 3.0 GB to at
/// most 68.7 GB leaves roughly **111 GB free** — substantial operational
/// headroom, which was the requirement.
///
/// Deliberately conservative. `RESEARCH_RETENTION_MAX_BYTES` overrides it.
pub const DEFAULT_CEILING_BYTES: u64 = 64 * 1024 * 1024 * 1024;

/// A session must be this many whole UTC days old before it can be considered.
/// 2 means today and yesterday are never touched.
pub const DEFAULT_MIN_AGE_DAYS: i64 = 2;

/// No file in a session may have been modified this recently.
pub const DEFAULT_FINALIZE_GRACE: Duration = Duration::from_secs(6 * 60 * 60);

/// How often the sweep runs. Retention is not urgent; a session is ~9.5 GB and
/// arrives once a day.
pub const DEFAULT_SWEEP_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Sentinel that makes one session ineligible: `2026-09-16.hold`.
pub const HOLD_SUFFIX: &str = ".hold";
/// Sentinel that makes the whole directory ineligible.
pub const GLOBAL_HOLD: &str = "HOLD";
/// Export receipts live here: `exported/2026-09-16.exported`.
pub const EXPORT_DIR: &str = "exported";
pub const EXPORT_SUFFIX: &str = ".exported";

#[derive(Debug, Clone)]
pub struct RetentionConfig {
    pub dir: PathBuf,
    pub ceiling_bytes: u64,
    pub min_age_days: i64,
    pub finalize_grace: Duration,
    pub sweep_interval: Duration,
    /// When true, a session with no export receipt is **ineligible** rather
    /// than merely logged.
    ///
    /// Off by default, and the trade-off is deliberate: on, an operator who
    /// never exports would see the ceiling silently stop being enforced, which
    /// re-creates the unbounded growth this module exists to prevent. Off, the
    /// ceiling always holds and an unexported deletion is counted and logged at
    /// WARN — loud, never silent. An operator who would rather run out of disk
    /// than lose an unexported session sets `RESEARCH_RETENTION_REQUIRE_EXPORT=1`.
    pub require_export: bool,
}

impl RetentionConfig {
    pub fn from_env(dir: PathBuf) -> Self {
        fn bytes(key: &str, default: u64) -> u64 {
            std::env::var(key).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default)
        }
        Self {
            dir,
            ceiling_bytes: bytes("RESEARCH_RETENTION_MAX_BYTES", DEFAULT_CEILING_BYTES),
            min_age_days: std::env::var("RESEARCH_RETENTION_MIN_AGE_DAYS")
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(DEFAULT_MIN_AGE_DAYS),
            finalize_grace: std::env::var("RESEARCH_RETENTION_GRACE_SECS")
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .map(Duration::from_secs)
                .unwrap_or(DEFAULT_FINALIZE_GRACE),
            sweep_interval: DEFAULT_SWEEP_INTERVAL,
            require_export: std::env::var("RESEARCH_RETENTION_REQUIRE_EXPORT")
                .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
                .unwrap_or(false),
        }
    }
}

/// Retention accounting, readable live alongside every other capture counter.
#[derive(Debug, Default)]
pub struct RetentionHealth {
    pub ceiling_bytes: AtomicU64,
    pub dir_bytes: AtomicU64,
    pub sessions_present: AtomicU64,
    pub sessions_deleted: AtomicU64,
    pub bytes_reclaimed: AtomicU64,
    /// Sessions deleted that carried no export receipt. Non-zero is not an
    /// error, but it is something an operator should know without reading logs.
    pub deleted_without_export: AtomicU64,
    /// Over the ceiling with nothing eligible to delete. The explicit degraded
    /// state: the policy is not silently doing nothing, it is blocked.
    pub retention_pending: AtomicBool,
    pub last_sweep_micros: AtomicI64,
    pub last_deleted: Mutex<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionSnapshot {
    pub ceiling_bytes: u64,
    pub dir_bytes: u64,
    pub sessions_present: u64,
    pub sessions_deleted: u64,
    pub bytes_reclaimed: u64,
    pub deleted_without_export: u64,
    pub retention_pending: bool,
    pub last_sweep: Option<chrono::DateTime<chrono::Utc>>,
    pub last_deleted: String,
}

impl RetentionHealth {
    pub fn snapshot(&self) -> RetentionSnapshot {
        RetentionSnapshot {
            ceiling_bytes: self.ceiling_bytes.load(Ordering::Relaxed),
            dir_bytes: self.dir_bytes.load(Ordering::Relaxed),
            sessions_present: self.sessions_present.load(Ordering::Relaxed),
            sessions_deleted: self.sessions_deleted.load(Ordering::Relaxed),
            bytes_reclaimed: self.bytes_reclaimed.load(Ordering::Relaxed),
            deleted_without_export: self.deleted_without_export.load(Ordering::Relaxed),
            retention_pending: self.retention_pending.load(Ordering::Relaxed),
            last_sweep: match self.last_sweep_micros.load(Ordering::Relaxed) {
                0 => None,
                micros => chrono::DateTime::from_timestamp_micros(micros),
            },
            last_deleted: self.last_deleted.lock().map(|s| s.clone()).unwrap_or_default(),
        }
    }
}

/// One capture session on disk: every file whose name ends `-<date>.ndjson`.
#[derive(Debug, Clone)]
pub struct Session {
    pub date: NaiveDate,
    pub files: Vec<PathBuf>,
    pub bytes: u64,
    /// Most recent modification across the session's files.
    pub modified: Option<SystemTime>,
    pub exported: bool,
    pub held: bool,
}

/// Why a session cannot be deleted right now. `None` means it can.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ineligible {
    TooRecent { age_days: i64 },
    BeingWritten,
    RecentlyModified,
    Held,
    AwaitingExport,
}

/// What one sweep did.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct SweepOutcome {
    pub dir_bytes_before: u64,
    pub dir_bytes_after: u64,
    pub sessions_present: usize,
    pub deleted: Vec<NaiveDate>,
    pub bytes_reclaimed: u64,
    pub deleted_without_export: usize,
    /// Over the ceiling, and nothing was eligible.
    pub pending: bool,
}

/// Reads the trailing `-YYYY-MM-DD` from a research capture filename.
///
/// Matches both `episodes-2026-09-16.ndjson` and
/// `opportunity-intelligence-markers-2026-09-16.ndjson`, because the date is
/// always the last hyphen-delimited component before the extension.
fn session_date_of(name: &str) -> Option<NaiveDate> {
    let stem = name.strip_suffix(".ndjson")?;
    let date = stem.rsplit('-').take(3).collect::<Vec<_>>();
    if date.len() != 3 {
        return None;
    }
    let text = format!("{}-{}-{}", date[2], date[1], date[0]);
    NaiveDate::parse_from_str(&text, "%Y-%m-%d").ok()
}

/// Groups the directory into sessions. Never recurses: `exported/` receipts and
/// any subdirectory are deliberately not capture data.
pub fn scan(dir: &Path) -> BTreeMap<NaiveDate, Session> {
    let mut sessions: BTreeMap<NaiveDate, Session> = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return sessions;
    };
    let global_hold = dir.join(GLOBAL_HOLD).exists();
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(date) = session_date_of(&name) else { continue };
        let modified = meta.modified().ok();
        let session = sessions.entry(date).or_insert_with(|| Session {
            date,
            files: Vec::new(),
            bytes: 0,
            modified: None,
            exported: dir.join(EXPORT_DIR).join(format!("{date}{EXPORT_SUFFIX}")).exists(),
            held: global_hold || dir.join(format!("{date}{HOLD_SUFFIX}")).exists(),
        });
        session.files.push(entry.path());
        session.bytes += meta.len();
        session.modified = match (session.modified, modified) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
    }
    for session in sessions.values_mut() {
        // Deterministic order, so a deletion log reads the same way twice.
        session.files.sort();
    }
    sessions
}

/// Whether one session may be deleted, and if not, why not.
pub fn eligibility(
    session: &Session,
    config: &RetentionConfig,
    today: NaiveDate,
    now: SystemTime,
    current_files: &[String],
) -> Option<Ineligible> {
    let age_days = (today - session.date).num_days();
    if age_days < config.min_age_days {
        return Some(Ineligible::TooRecent { age_days });
    }
    // Checked against what the writer says it is holding, not inferred from the
    // date — a writer that is behind, or replaying, may legitimately still be
    // appending to an older file.
    if session.files.iter().any(|f| {
        let path = f.to_string_lossy().to_string();
        current_files.iter().any(|c| !c.is_empty() && (c == &path || path.ends_with(c.as_str())))
    }) {
        return Some(Ineligible::BeingWritten);
    }
    if session.held {
        return Some(Ineligible::Held);
    }
    if let Some(modified) = session.modified {
        if now.duration_since(modified).map(|d| d < config.finalize_grace).unwrap_or(true) {
            return Some(Ineligible::RecentlyModified);
        }
    }
    if config.require_export && !session.exported {
        return Some(Ineligible::AwaitingExport);
    }
    None
}

/// Deletes oldest-finalized-first until the directory is inside its ceiling.
///
/// Pure with respect to time and the writer's state — both are arguments — so
/// the whole policy is testable without a clock or a live writer.
pub fn sweep(
    config: &RetentionConfig,
    today: NaiveDate,
    now: SystemTime,
    current_files: &[String],
) -> SweepOutcome {
    let sessions = scan(&config.dir);
    let mut outcome = SweepOutcome {
        dir_bytes_before: sessions.values().map(|s| s.bytes).sum(),
        sessions_present: sessions.len(),
        ..SweepOutcome::default()
    };
    let mut dir_bytes = outcome.dir_bytes_before;

    if dir_bytes <= config.ceiling_bytes {
        outcome.dir_bytes_after = dir_bytes;
        return outcome;
    }

    // `BTreeMap` iterates in key order, so oldest first — which is the policy,
    // not an accident of `read_dir`.
    for session in sessions.values() {
        if dir_bytes <= config.ceiling_bytes {
            break;
        }
        if let Some(reason) = eligibility(session, config, today, now, current_files) {
            info!(
                date = %session.date, ?reason, bytes = session.bytes,
                "research retention: session retained"
            );
            continue;
        }
        let mut removed = 0u64;
        let mut failed = false;
        for file in &session.files {
            match std::fs::metadata(file).map(|m| m.len()) {
                Ok(len) => match std::fs::remove_file(file) {
                    Ok(()) => removed += len,
                    Err(error) => {
                        warn!(%error, path = %file.display(),
                            "research retention: could not remove a file");
                        failed = true;
                    }
                },
                Err(error) => {
                    warn!(%error, path = %file.display(),
                        "research retention: could not stat a file");
                    failed = true;
                }
            }
        }
        dir_bytes = dir_bytes.saturating_sub(removed);
        outcome.bytes_reclaimed += removed;
        outcome.deleted.push(session.date);
        if !session.exported {
            outcome.deleted_without_export += 1;
            // Loud, deliberately. This is the only copy of a market session and
            // nothing has recorded that it was exported first.
            warn!(
                date = %session.date, bytes = removed,
                "research retention: deleted a session with no export receipt; \
                 set RESEARCH_RETENTION_REQUIRE_EXPORT=1 to refuse instead"
            );
        } else {
            info!(date = %session.date, bytes = removed, "research retention: session reclaimed");
        }
        if failed {
            warn!(date = %session.date, "research retention: session only partially removed");
        }
    }

    outcome.dir_bytes_after = dir_bytes;
    outcome.pending = dir_bytes > config.ceiling_bytes;
    if outcome.pending {
        warn!(
            dir_bytes,
            ceiling = config.ceiling_bytes,
            sessions = outcome.sessions_present,
            "research retention pending: over the ceiling with no eligible session to reclaim"
        );
    }
    outcome
}

/// Runs `sweep` on a timer, on its own thread.
///
/// Its own thread because retention must not block market dispatch, and must
/// not block the *writers* either: a multi-gigabyte `remove_file` on a writer
/// thread would back its bounded queue up, which is precisely the failure mode
/// this milestone exists to remove.
pub fn start(
    config: RetentionConfig,
    health: Arc<RetentionHealth>,
    current_files: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
) {
    health.ceiling_bytes.store(config.ceiling_bytes, Ordering::Relaxed);
    if let Err(error) = std::fs::create_dir_all(&config.dir) {
        warn!(%error, dir = %config.dir.display(),
            "research retention disabled: cannot read the capture directory");
        return;
    }
    info!(
        dir = %config.dir.display(),
        ceiling_bytes = config.ceiling_bytes,
        min_age_days = config.min_age_days,
        grace_secs = config.finalize_grace.as_secs(),
        require_export = config.require_export,
        "research retention enabled"
    );
    std::thread::spawn(move || loop {
        let today = chrono::Utc::now().date_naive();
        let outcome = sweep(&config, today, SystemTime::now(), &current_files());
        apply(&health, &outcome);
        std::thread::sleep(config.sweep_interval);
    });
}

/// Folds one sweep into the shared counters.
pub fn apply(health: &RetentionHealth, outcome: &SweepOutcome) {
    health.dir_bytes.store(outcome.dir_bytes_after, Ordering::Relaxed);
    health.sessions_present.store(outcome.sessions_present as u64, Ordering::Relaxed);
    health.sessions_deleted.fetch_add(outcome.deleted.len() as u64, Ordering::Relaxed);
    health.bytes_reclaimed.fetch_add(outcome.bytes_reclaimed, Ordering::Relaxed);
    health
        .deleted_without_export
        .fetch_add(outcome.deleted_without_export as u64, Ordering::Relaxed);
    health.retention_pending.store(outcome.pending, Ordering::Relaxed);
    health
        .last_sweep_micros
        .store(chrono::Utc::now().timestamp_micros(), Ordering::Relaxed);
    if let Some(last) = outcome.deleted.last() {
        if let Ok(mut slot) = health.last_deleted.lock() {
            *slot = last.to_string();
        }
    }
}

#[cfg(test)]
#[path = "research_retention_tests.rs"]
mod tests;
