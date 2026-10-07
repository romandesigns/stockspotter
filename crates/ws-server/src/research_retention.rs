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
//! * **Never a protected session without a verified export receipt.** See
//!   below; this is the rule the first six weeks of production did not have.
//! * **Oldest finalized first**, so the most recent research stays longest.
//! * **Never silently.** Every deletion is logged and counted, and deleting a
//!   session with no export receipt is counted separately and logged at WARN.
//!
//! # Protected sessions (added 2026-09-25)
//!
//! The rules above made a *wrong* deletion impossible and said nothing about a
//! *valuable* one. Between 09-17 and 09-25 this sweep deleted nine sessions
//! with no export receipt — the preregistered V2 development sessions 09-17
//! and 09-18 among them — entirely within policy: the ceiling was sized for
//! ~9.5 GB/session, real sessions are ~15.4 GB, and "export first" was a thing
//! a human had to remember.
//!
//! The contract now lives in `market_data::retention_registry` and is shared
//! with discovery capture. Stated for this directory:
//!
//! > **A session whose date has `.retention/protected/<date>.json` is deleted
//! > only if `.retention/exports/<date>.json` lists every one of the session's
//! > files by name, with the exact byte length and SHA-256 of the file on
//! > disk.** A registry file that does not parse protects.
//!
//! When the ceiling cannot be met without deleting a protected session, the
//! sweep deletes nothing protected and reports `blockedByProtection` and
//! `bytesOverCeiling` instead. That is a deliberate choice of disk pressure
//! over evidence loss, and it is loud rather than quiet. Unprotected sessions
//! are unaffected: their policy, including `RESEARCH_RETENTION_REQUIRE_EXPORT`,
//! is exactly what it was.
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
use market_data::retention_registry::{
    self as registry, FileVerdict, HashCache, HashMode, Protection, ProtectionIndex, ReceiptState,
};
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
///
/// **Correction, measured 2026-09-25:** a regular session is ~15.4 GB, not
/// ~9.5 — Opportunity Intelligence ~11.3 GB, opportunity outcomes ~3.7 GB
/// (a stream that did not exist when this was sized), episodes ~0.33 GB. 64 GiB
/// (68.7 GB) therefore retains about **four** sessions, not seven. The default
/// is left unchanged here because changing it changes what an unprotected
/// deployment deletes; the sizing is an operator decision, made against the
/// disk, and protection (not the ceiling) is what keeps designated sessions.
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
    ///
    /// Protected sessions do not depend on this flag: they always require a
    /// *verified* receipt, whatever it says.
    pub require_export: bool,
    /// SHA-256 results for receipt verification, shared across sweeps so an
    /// 11 GB file is hashed once per process rather than every fifteen
    /// minutes. Keyed by length, mtime and inode, so a changed file re-hashes.
    pub hash_cache: Arc<HashCache>,
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
            hash_cache: Arc::default(),
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
    /// Over the ceiling **because** a protected session without a verified
    /// export receipt was the next candidate. Nothing protected was deleted.
    pub blocked_by_protection: AtomicBool,
    pub bytes_over_ceiling: AtomicU64,
    pub protected_sessions: AtomicU64,
    pub protected_bytes: AtomicU64,
    /// Protected sessions deleted under a verified receipt, cumulative.
    pub deleted_protected_with_receipt: AtomicU64,
    /// Last sweep's detail: protected dates with no usable receipt file, dates
    /// retained for protection, and registry files that did not validate.
    pub protection_detail: Mutex<ProtectionDetail>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProtectionDetail {
    pub protected_without_receipt: Vec<String>,
    pub protected_retained: Vec<String>,
    pub registry_errors: Vec<String>,
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
    // --- protection (additive; `default` so an older document still reads) ---
    /// Over the ceiling, and the next candidate was protected with no verified
    /// export receipt. Nothing protected was deleted; the directory is over
    /// its ceiling by `bytes_over_ceiling` instead.
    #[serde(default)]
    pub blocked_by_protection: bool,
    #[serde(default)]
    pub bytes_over_ceiling: u64,
    #[serde(default)]
    pub protected_sessions: u64,
    #[serde(default)]
    pub protected_bytes: u64,
    #[serde(default)]
    pub deleted_protected_with_receipt: u64,
    /// Protected dates with no receipt file at all, or one that does not parse.
    /// Cheap to compute, so reported on every sweep — a hash mismatch is found
    /// only when the session becomes a deletion candidate.
    #[serde(default)]
    pub protected_without_receipt: Vec<String>,
    /// Dates the last sweep wanted to reclaim and could not, for protection.
    #[serde(default)]
    pub protected_retained: Vec<String>,
    /// Registry files that did not validate. Each one is enforced as protected.
    #[serde(default)]
    pub registry_errors: Vec<String>,
}

impl RetentionHealth {
    pub fn snapshot(&self) -> RetentionSnapshot {
        let detail = self.protection_detail.lock().map(|d| d.clone()).unwrap_or_default();
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
            blocked_by_protection: self.blocked_by_protection.load(Ordering::Relaxed),
            bytes_over_ceiling: self.bytes_over_ceiling.load(Ordering::Relaxed),
            protected_sessions: self.protected_sessions.load(Ordering::Relaxed),
            protected_bytes: self.protected_bytes.load(Ordering::Relaxed),
            deleted_protected_with_receipt: self
                .deleted_protected_with_receipt
                .load(Ordering::Relaxed),
            protected_without_receipt: detail.protected_without_receipt,
            protected_retained: detail.protected_retained,
            registry_errors: detail.registry_errors,
        }
    }
}

//// One capture session on disk: every file whose name ends `-<date>.ndjson`.
#[derive(Debug, Clone)]
pub struct Session {
    pub date: NaiveDate,
    pub files: Vec<PathBuf>,
    pub bytes: u64,
    /// Most recent modification across the session's files.
    pub modified: Option<SystemTime>,
    /// A legacy `exported/<date>.exported` marker **or** a well-formed
    /// `.retention/exports/<date>.json` receipt. Used only for the
    /// `deleted_without_export` accounting and `require_export`; neither is
    /// hash-verified, and neither is enough for a protected session.
    pub exported: bool,
    pub held: bool,
    /// What `.retention/protected/` says about this date.
    pub protection: Protection,
    /// `.retention/exports/<date>.json`, as read (not yet verified).
    pub receipt: ReceiptState,
}

/// Why a session cannot be deleted right now. `None` means it can.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ineligible {
    TooRecent { age_days: i64 },
    BeingWritten,
    RecentlyModified,
    Held,
    AwaitingExport,
    /// Protected, and `file` is not covered by a verified export receipt.
    ProtectedAwaitingExport { file: String, verdict: FileVerdict },
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
    /// Over the ceiling, and at least one candidate was retained only because
    /// it is protected without a verified receipt.
    pub blocked_by_protection: bool,
    pub bytes_over_ceiling: u64,
    pub protected_sessions: usize,
    pub protected_bytes: u64,
    pub protected_without_receipt: Vec<NaiveDate>,
    pub protected_retained: Vec<NaiveDate>,
    pub deleted_protected_with_receipt: usize,
    pub registry_errors: Vec<String>,
}

// Reads the trailing `-YYYY-MM-DD` from a research capture filename.
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

//// Groups the directory into sessions. Never recurses: `exported/` receipts,
/// the `.retention/` registry and any other subdirectory are deliberately not
/// capture data.
pub fn scan(dir: &Path) -> BTreeMap<NaiveDate, Session> {
    scan_with_registry(dir).0
}

/// [`scan`], plus the protection registry it consulted, so a sweep can report
/// registry errors without reading the registry twice.
pub fn scan_with_registry(dir: &Path) -> (BTreeMap<NaiveDate, Session>, ProtectionIndex) {
    let mut sessions: BTreeMap<NaiveDate, Session> = BTreeMap::new();
    let index = ProtectionIndex::load(dir);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (sessions, index);
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
        let session = sessions.entry(date).or_insert_with(|| {
            let receipt = registry::load_receipt(dir, date);
            Session {
                date,
                files: Vec::new(),
                bytes: 0,
                modified: None,
                exported: dir.join(EXPORT_DIR).join(format!("{date}{EXPORT_SUFFIX}")).exists()
                    || receipt.receipt().is_some(),
                held: global_hold || dir.join(format!("{date}{HOLD_SUFFIX}")).exists(),
                protection: index.of(date),
                receipt,
            }
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
    (sessions, index)
}

// Whether one session may be deleted, and if not, why not.
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

//// For a protected session: the first file not covered by a verified export
/// receipt, or `None` when every file is. Hashes inline -- this runs on the
/// retention thread, never a writer's -- through the config's shared cache.
///
/// Only called once a session has passed every cheap rule, so a protected
/// session that is too recent, held or being written never costs a hash.
pub fn unverified_file(session: &Session, config: &RetentionConfig) -> Option<(String, FileVerdict)> {
    session.files.iter().find_map(|file| {
        let verdict =
            registry::verify_file(&session.receipt, file, &config.hash_cache, HashMode::Inline);
        (verdict != FileVerdict::Verified).then(|| {
            let name = file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            (name, verdict)
        })
    })
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
    let (sessions, index) = scan_with_registry(&config.dir);
    let mut outcome = SweepOutcome {
        dir_bytes_before: sessions.values().map(|s| s.bytes).sum(),
        sessions_present: sessions.len(),
        registry_errors: index.errors().to_vec(),
        ..SweepOutcome::default()
    };
    // Protection accounting is reported on every sweep, including the ones that
    // delete nothing: an operator should see "four sessions protected, two with
    // no receipt" long before the ceiling makes it matter.
    for session in sessions.values().filter(|s| s.protection.is_protected()) {
        outcome.protected_sessions += 1;
        outcome.protected_bytes += session.bytes;
        if session.receipt.receipt().is_none() {
            outcome.protected_without_receipt.push(session.date);
        }
    }
    if !outcome.registry_errors.is_empty() {
        warn!(errors = ?outcome.registry_errors,
            "research retention: registry files did not validate; each is enforced as protected");
    }
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
        let mut reason = eligibility(session, config, today, now, current_files);
        // The protection gate runs last, after every cheap rule has passed,
        // because it may have to hash ~15 GB.
        if reason.is_none() && session.protection.is_protected() {
            if let Some((file, verdict)) = unverified_file(session, config) {
                reason = Some(Ineligible::ProtectedAwaitingExport { file, verdict });
            }
        }
        if let Some(reason) = reason {
            if matches!(reason, Ineligible::ProtectedAwaitingExport { .. }) {
                outcome.protected_retained.push(session.date);
                warn!(
                    date = %session.date, ?reason, bytes = session.bytes,
                    protection = ?session.protection,
                    "research retention: protected session retained -- no verified export receipt"
                );
            } else {
                info!(
                    date = %session.date, ?reason, bytes = session.bytes,
                    "research retention: session retained"
                );
            }
            continue;
        }
        let mut removed = 0u64;
        let mut failed = false;
        for file in &session.files {
            match std::fs::metadata(file).map(|m| m.len()) {
                Ok(len) => match std::fs::remove_file(file) {
                    Ok(()) => {
                        removed += len;
                        config.hash_cache.forget(file);
                    }
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
        if session.protection.is_protected() {
            // Reaching here means every file was verified against the receipt.
            outcome.deleted_protected_with_receipt += 1;
            info!(
                date = %session.date, bytes = removed,
                destination = session.receipt.receipt().map(|r| r.destination.as_str()).unwrap_or(""),
                "research retention: protected session reclaimed under a verified export receipt"
            );
        } else if !session.exported {
            outcome.deleted_without_export += 1;
            // Loud, deliberately. This is the only copy of a market session and
            // nothing has recorded that it was exported first.
            warn!(
                date = %session.date, bytes = removed,
                "research retention: deleted a session with no export receipt; \
                 protect it (.retention/protected/<date>.json) or set \
                 RESEARCH_RETENTION_REQUIRE_EXPORT=1 to refuse instead"
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
    outcome.bytes_over_ceiling = dir_bytes.saturating_sub(config.ceiling_bytes);
    outcome.blocked_by_protection = outcome.pending && !outcome.protected_retained.is_empty();
    if outcome.blocked_by_protection {
        warn!(
            dir_bytes,
            ceiling = config.ceiling_bytes,
            bytes_over_ceiling = outcome.bytes_over_ceiling,
            protected = ?outcome.protected_retained,
            "research retention BLOCKED BY PROTECTION: the ceiling cannot be met without \
             deleting protected sessions that have no verified export receipt; nothing \
             protected was deleted"
        );
    } else if outcome.pending {
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
    health
        .blocked_by_protection
        .store(outcome.blocked_by_protection, Ordering::Relaxed);
    health.bytes_over_ceiling.store(outcome.bytes_over_ceiling, Ordering::Relaxed);
    health
        .protected_sessions
        .store(outcome.protected_sessions as u64, Ordering::Relaxed);
    health.protected_bytes.store(outcome.protected_bytes, Ordering::Relaxed);
    health
        .deleted_protected_with_receipt
        .fetch_add(outcome.deleted_protected_with_receipt as u64, Ordering::Relaxed);
    if let Ok(mut slot) = health.protection_detail.lock() {
        let dates = |v: &[NaiveDate]| v.iter().map(|d| d.to_string()).collect();
        *slot = ProtectionDetail {
            protected_without_receipt: dates(&outcome.protected_without_receipt),
            protected_retained: dates(&outcome.protected_retained),
            registry_errors: outcome.registry_errors.clone(),
        };
    }
}

#[cfg(test)]
#[path = "research_retention_tests.rs"]
mod tests;
