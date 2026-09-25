//! The retention safety contract shared by every capture that deletes its own
//! history: research retention (`ws-server::research_retention`) and discovery
//! capture (`discovery_audit`).
//!
//! # Why this exists
//!
//! Between 2026-09-17 and 2026-09-25 research retention deleted **nine
//! sessions with no export receipt**, including the preregistered V2
//! development sessions 09-17 and 09-18. Nothing was malfunctioning: the
//! policy was "keep the directory under its ceiling, export not required", it
//! was sized for ~9.5 GB/session when real sessions are ~15.4 GB, and the only
//! thing standing between a designated session and `remove_file` was a human
//! remembering to copy it out first. Discovery capture had the same shape with
//! no export awareness at all.
//!
//! # The invariant
//!
//! > **No file belonging to a protected UTC day is ever deleted unless an export
//! > receipt for that day lists that exact file name, and the file on disk
//! > still has exactly the receipt's byte length and SHA-256.**
//!
//! Everything else is a consequence:
//!
//! * A protected day with no receipt is never deleted — whatever its age,
//!   whatever the ceiling says. Retention reports the pressure instead.
//! * A receipt that does not match the bytes on disk (wrong size, wrong hash,
//!   a file the receipt does not mention, a file appended to after the export)
//!   is no receipt.
//! * A registry file that cannot be parsed **fails safe**: it protects. A
//!   typo can make retention stop; it can never make retention delete.
//! * Days nobody designated are **ordinary** and keep exactly the policy they
//!   had before this module existed. Designation is opt-in so that disk
//!   capacity stays bounded by default.
//!
//! The four classes of data, and where each one lives:
//!
//! | Class | Representation | Deletable? |
//! |---|---|---|
//! | (A) ordinary operational capture | no registry entry | by the capture's own age/ceiling policy, unchanged |
//! | (B) designated research session | `protected/<date>.json`, `"class": "designated"` | only with a verified receipt |
//! | (C) protected forensic evidence | `protected/<date>.json`, `"class": "forensic"` | only with a verified receipt |
//! | (D) exported evidence | (B)/(C) plus `exports/<date>.json` whose per-file size+SHA-256 match disk | yes, by the normal age/ceiling order |
//!
//! (B) and (C) are deliberately enforced identically — the class is recorded
//! for the humans reading the registry and the health report. Anything that
//! must survive even a verified export uses the existing absolute holds
//! (research's `<date>.hold` / `HOLD`), which this module does not replace.
//!
//! # Layout
//!
//! The registry lives **inside each capture directory**, next to the data it
//! governs, so it travels with the data (an `rsync` of the directory carries
//! it) and so there is no second path for a deployment to get wrong:
//!
//! ```text
//! data/research/.retention/protected/2026-09-21.json
//! data/research/.retention/exports/2026-09-21.json
//! data/discovery-audit/.retention/protected/2026-09-21.json
//! data/discovery-audit/.retention/exports/2026-09-21.json
//! ```
//!
//! Research and discovery each have their own registry rather than sharing
//! one, because a receipt is a list of *files*, and the two captures' files
//! for the same day live in different directories and are exported, and
//! verified, separately. Same format, same code, same rules; protecting a day
//! in one capture says nothing about the other. The capture scanners never
//! recurse, so `.retention/` is never mistaken for capture data and its bytes
//! are never counted against a ceiling.
//!
//! # Hashing cost
//!
//! A research session is ~11 GB of Opportunity Intelligence alone. Hashes are
//! computed **only** when a protected day is actually a deletion candidate,
//! and are cached by `(path, length, mtime, inode)`, so a sweep that runs every
//! fifteen minutes hashes a given file once per process lifetime rather than
//! once per sweep. A file that changes in any of those respects is re-hashed.
//! Discovery must never hash on its writer thread; it uses
//! [`BackgroundHasher`] and treats "not hashed yet" as "not deletable yet".

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The registry directory inside a capture directory.
pub const REGISTRY_DIR: &str = ".retention";
/// `<REGISTRY_DIR>/protected/<date>.json` — designation / protection.
pub const PROTECTED_DIR: &str = "protected";
/// `<REGISTRY_DIR>/exports/<date>.json` — export receipts.
pub const EXPORTS_DIR: &str = "exports";
/// The only schema this build understands. Any other version is malformed,
/// which for a protection file means *protected* and for a receipt means *no
/// receipt* — both the safe direction.
pub const SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Protection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProtectionClass {
    /// (B) A session designated for research in advance (a preregistered
    /// development or evaluation session).
    Designated,
    /// (C) Evidence preserved because something about it has to be explained.
    Forensic,
}

/// `protected/<date>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtectionRecord {
    pub schema_version: u32,
    pub date: NaiveDate,
    pub class: ProtectionClass,
    pub reason: String,
    /// Who designated it. Free text; a name, not a credential.
    pub protected_by: String,
    pub protected_at: DateTime<Utc>,
}

/// What the registry says about one UTC day.
#[derive(Debug, Clone, PartialEq)]
pub enum Protection {
    /// (A) No registry entry. The capture's own policy applies unchanged.
    Ordinary,
    /// (B) or (C).
    Protected {
        class: ProtectionClass,
        reason: String,
    },
    /// A registry entry exists for this day — or the registry as a whole could
    /// not be read — but it did not validate. **Enforced exactly as
    /// `Protected`.** Reported separately only so an operator can fix it.
    Malformed { error: String },
}

impl Protection {
    pub fn is_protected(&self) -> bool {
        !matches!(self, Protection::Ordinary)
    }
}

/// The protection registry of one capture directory, read once per sweep.
#[derive(Debug, Clone, Default)]
pub struct ProtectionIndex {
    days: BTreeMap<NaiveDate, Protection>,
    /// Set when the registry exists but cannot be interpreted as a whole (an
    /// unreadable directory, a stray file whose name is not `<date>.json`).
    /// Every day is then protected: a file that cannot be attributed to a day
    /// might have been meant for any of them.
    global: Option<String>,
    errors: Vec<String>,
}

impl ProtectionIndex {
    /// Reads `<capture_dir>/.retention/protected/`. An absent registry is the
    /// normal, empty case; everything else that is not a well-formed
    /// `<date>.json` protects.
    pub fn load(capture_dir: &Path) -> Self {
        let dir = capture_dir.join(REGISTRY_DIR).join(PROTECTED_DIR);
        let mut index = Self::default();
        match std::fs::metadata(&dir) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // `.retention/` itself may exist as a file by mistake; then the
                // `protected` lookup fails with NotADirectory rather than
                // NotFound on some platforms and NotFound on others. Check the
                // parent explicitly so both platforms fail safe.
                let parent = capture_dir.join(REGISTRY_DIR);
                if parent.exists() && !parent.is_dir() {
                    index.fail_globally(format!("{} is not a directory", parent.display()));
                }
                return index;
            }
            Err(error) => {
                index.fail_globally(format!("cannot stat {}: {error}", dir.display()));
                return index;
            }
            Ok(meta) if !meta.is_dir() => {
                index.fail_globally(format!("{} is not a directory", dir.display()));
                return index;
            }
            Ok(_) => {}
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) => {
                index.fail_globally(format!("cannot read {}: {error}", dir.display()));
                return index;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    index.fail_globally(format!("cannot list {}: {error}", dir.display()));
                    continue;
                }
            };
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(date) = date_of_registry_file(&name) else {
                index.fail_globally(format!(
                    "unrecognised registry entry {name:?} in {}; expected <YYYY-MM-DD>.json",
                    dir.display()
                ));
                continue;
            };
            let protection = match std::fs::read(entry.path()) {
                Err(error) => Protection::Malformed {
                    error: format!("{name}: {error}"),
                },
                Ok(bytes) => match parse_protection(&bytes, date) {
                    Ok(record) => Protection::Protected {
                        class: record.class,
                        reason: record.reason,
                    },
                    Err(error) => Protection::Malformed {
                        error: format!("{name}: {error}"),
                    },
                },
            };
            if let Protection::Malformed { error } = &protection {
                index.errors.push(error.clone());
            }
            index.days.insert(date, protection);
        }
        index
    }

    fn fail_globally(&mut self, error: String) {
        self.errors.push(error.clone());
        self.global.get_or_insert(error);
    }

    /// The protection in force for `date`.
    pub fn of(&self, date: NaiveDate) -> Protection {
        if let Some(p) = self.days.get(&date) {
            return p.clone();
        }
        match &self.global {
            Some(error) => Protection::Malformed {
                error: error.clone(),
            },
            None => Protection::Ordinary,
        }
    }

    /// Whether the registry as a whole failed safe (every day protected).
    pub fn globally_protected(&self) -> bool {
        self.global.is_some()
    }

    /// Every day with a registry entry, in date order.
    pub fn protected_days(&self) -> impl Iterator<Item = (&NaiveDate, &Protection)> {
        self.days.iter()
    }

    /// Human-readable validation failures, for the health report.
    pub fn errors(&self) -> &[String] {
        &self.errors
    }
}

fn date_of_registry_file(name: &str) -> Option<NaiveDate> {
    let stem = name.strip_suffix(".json")?;
    // Exactly `YYYY-MM-DD`: `2026-9-21.json` or `2026-09-21.old.json` is not a
    // registry file, and is therefore a global fail-safe, not a silent skip.
    if stem.len() != 10 {
        return None;
    }
    NaiveDate::parse_from_str(stem, "%Y-%m-%d").ok()
}

fn parse_protection(bytes: &[u8], date: NaiveDate) -> Result<ProtectionRecord, String> {
    let record: ProtectionRecord = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if record.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "schemaVersion {} is not {SCHEMA_VERSION}",
            record.schema_version
        ));
    }
    if record.date != date {
        return Err(format!(
            "date {} does not match the file name {date}",
            record.date
        ));
    }
    if record.reason.trim().is_empty() {
        return Err("reason is empty".into());
    }
    if record.protected_by.trim().is_empty() {
        return Err("protectedBy is empty".into());
    }
    Ok(record)
}

// ---------------------------------------------------------------------------
// Export receipts
// ---------------------------------------------------------------------------

/// `exports/<date>.json`: proof that a verified copy of these exact bytes
/// exists somewhere this machine's retention cannot reach.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportReceipt {
    pub schema_version: u32,
    pub date: NaiveDate,
    /// Where the copy is. Free text, for a human — the service cannot check a
    /// remote destination, which is exactly why the per-file hashes below are
    /// checked against the *local* bytes instead: a receipt whose hashes match
    /// what is on disk describes a copy of what is on disk.
    pub destination: String,
    /// When the copy was verified against its source.
    pub verified_at: DateTime<Utc>,
    pub verified_by: String,
    pub files: Vec<ReceiptFile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptFile {
    /// Bare file name inside the capture directory. Never a path.
    pub name: String,
    pub bytes: u64,
    /// Lowercase hex SHA-256 of the whole file.
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReceiptState {
    Missing,
    /// Present but invalid — enforced exactly as `Missing`.
    Malformed(String),
    Present(ExportReceipt),
}

impl ReceiptState {
    pub fn receipt(&self) -> Option<&ExportReceipt> {
        match self {
            ReceiptState::Present(r) => Some(r),
            _ => None,
        }
    }
}

/// Reads `<capture_dir>/.retention/exports/<date>.json`.
pub fn load_receipt(capture_dir: &Path, date: NaiveDate) -> ReceiptState {
    let path = capture_dir
        .join(REGISTRY_DIR)
        .join(EXPORTS_DIR)
        .join(format!("{date}.json"));
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return ReceiptState::Missing,
        Err(error) => return ReceiptState::Malformed(format!("{}: {error}", path.display())),
    };
    match parse_receipt(&bytes, date) {
        Ok(receipt) => ReceiptState::Present(receipt),
        Err(error) => ReceiptState::Malformed(format!("{date}.json: {error}")),
    }
}

fn parse_receipt(bytes: &[u8], date: NaiveDate) -> Result<ExportReceipt, String> {
    let mut receipt: ExportReceipt = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if receipt.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "schemaVersion {} is not {SCHEMA_VERSION}",
            receipt.schema_version
        ));
    }
    if receipt.date != date {
        return Err(format!(
            "date {} does not match the file name {date}",
            receipt.date
        ));
    }
    if receipt.destination.trim().is_empty() {
        return Err("destination is empty".into());
    }
    if receipt.verified_by.trim().is_empty() {
        return Err("verifiedBy is empty".into());
    }
    if receipt.files.is_empty() {
        return Err("files is empty".into());
    }
    let mut seen = HashSet::new();
    for file in &mut receipt.files {
        let name = &file.name;
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains('/')
            || name.contains('\\')
        {
            return Err(format!("file name {name:?} is not a bare file name"));
        }
        if !seen.insert(name.clone()) {
            return Err(format!("file {name:?} is listed twice"));
        }
        file.sha256 = file.sha256.to_ascii_lowercase();
        if file.sha256.len() != 64 || !file.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("sha256 for {name:?} is not 64 hex characters"));
        }
    }
    Ok(receipt)
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

/// Why a file on disk is, or is not, covered by a receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileVerdict {
    /// Name, length and SHA-256 all match the receipt. The only verdict that
    /// permits deletion of a protected file.
    Verified,
    NoReceipt,
    MalformedReceipt(String),
    /// The receipt does not mention this file — typically data written after
    /// the export (a late record, a marker file), which the copy therefore
    /// does not contain.
    NotInReceipt,
    SizeMismatch {
        receipt: u64,
        disk: u64,
    },
    HashMismatch,
    /// The hash is not known yet and the caller may not compute it inline.
    Pending,
    Unreadable(String),
}

/// A file's identity for cache purposes: if any of these change, the bytes may
/// have, and the hash is recomputed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileKey {
    len: u64,
    modified: Option<SystemTime>,
    inode: u64,
}

impl FileKey {
    fn of(meta: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        let inode = {
            use std::os::unix::fs::MetadataExt;
            meta.ino()
        };
        // Windows has no stable std accessor for the file index; length and
        // mtime still catch every realistic change on a development machine.
        #[cfg(not(unix))]
        let inode = 0;
        Self {
            len: meta.len(),
            modified: meta.modified().ok(),
            inode,
        }
    }
}

/// SHA-256 results keyed by path and [`FileKey`].
#[derive(Debug, Default)]
pub struct HashCache {
    entries: Mutex<HashMap<PathBuf, (FileKey, String)>>,
}

impl HashCache {
    /// The cached hash, only if the file is unchanged since it was computed.
    pub fn cached(&self, path: &Path) -> Option<String> {
        let meta = std::fs::metadata(path).ok()?;
        let key = FileKey::of(&meta);
        let entries = self.entries.lock().ok()?;
        entries
            .get(path)
            .filter(|(k, _)| *k == key)
            .map(|(_, h)| h.clone())
    }

    /// Returns the hash, computing and caching it if needed.
    ///
    /// The file's identity is read before *and* after hashing; a file that
    /// changed while it was being read produced a hash of nothing in
    /// particular, so it is refused rather than cached.
    pub fn hash(&self, path: &Path) -> std::io::Result<String> {
        if let Some(hash) = self.cached(path) {
            return Ok(hash);
        }
        let before = FileKey::of(&std::fs::metadata(path)?);
        let hash = sha256_file(path)?;
        let after = FileKey::of(&std::fs::metadata(path)?);
        if before != after {
            return Err(std::io::Error::other(
                "file changed while it was being hashed",
            ));
        }
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(path.to_path_buf(), (after, hash.clone()));
        }
        Ok(hash)
    }

    /// Drops a path's entry, after the file is deleted.
    pub fn forget(&self, path: &Path) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.remove(path);
        }
    }
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// How [`verify_file`] may obtain a hash it does not already have.
pub enum HashMode<'a> {
    /// Compute it now, on this thread. Research retention's own thread.
    Inline,
    /// Never block: hand the path to a background hasher and report
    /// [`FileVerdict::Pending`]. Discovery's writer thread.
    Background(&'a BackgroundHasher),
}

/// Whether `path` is covered by `receipt`. The cheap checks (presence, name,
/// length) run first, so a receipt that is obviously wrong never costs a hash.
pub fn verify_file(
    receipt: &ReceiptState,
    path: &Path,
    cache: &HashCache,
    mode: HashMode<'_>,
) -> FileVerdict {
    let receipt = match receipt {
        ReceiptState::Missing => return FileVerdict::NoReceipt,
        ReceiptState::Malformed(error) => return FileVerdict::MalformedReceipt(error.clone()),
        ReceiptState::Present(receipt) => receipt,
    };
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
        return FileVerdict::NotInReceipt;
    };
    let Some(entry) = receipt.files.iter().find(|f| f.name == name) else {
        return FileVerdict::NotInReceipt;
    };
    let disk = match std::fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(error) => return FileVerdict::Unreadable(error.to_string()),
    };
    if disk != entry.bytes {
        return FileVerdict::SizeMismatch {
            receipt: entry.bytes,
            disk,
        };
    }
    let hash = match cache.cached(path) {
        Some(hash) => hash,
        None => match mode {
            HashMode::Inline => match cache.hash(path) {
                Ok(hash) => hash,
                Err(error) => return FileVerdict::Unreadable(error.to_string()),
            },
            HashMode::Background(hasher) => {
                hasher.request(path);
                return FileVerdict::Pending;
            }
        },
    };
    if hash == entry.sha256 {
        FileVerdict::Verified
    } else {
        FileVerdict::HashMismatch
    }
}

/// One worker thread that hashes files for a caller that must never block.
///
/// Requests are de-duplicated while in flight, and results land in the shared
/// [`HashCache`], where the next `verify_file` finds them.
#[derive(Debug)]
pub struct BackgroundHasher {
    cache: Arc<HashCache>,
    inflight: Arc<Mutex<HashSet<PathBuf>>>,
    tx: Mutex<Option<Sender<PathBuf>>>,
}

impl BackgroundHasher {
    pub fn new(cache: Arc<HashCache>) -> Self {
        Self {
            cache,
            inflight: Arc::default(),
            tx: Mutex::new(None),
        }
    }

    pub fn cache(&self) -> &HashCache {
        &self.cache
    }

    /// Queues `path` for hashing unless it is already queued. Never blocks on
    /// I/O; the worker thread is started on first use.
    pub fn request(&self, path: &Path) {
        {
            let Ok(mut inflight) = self.inflight.lock() else {
                return;
            };
            if !inflight.insert(path.to_path_buf()) {
                return;
            }
        }
        let Ok(mut tx) = self.tx.lock() else { return };
        if tx.is_none() {
            let (sender, receiver) = channel::<PathBuf>();
            let cache = self.cache.clone();
            let inflight = self.inflight.clone();
            let spawned = std::thread::Builder::new()
                .name("retention-hasher".into())
                .spawn(move || {
                    while let Ok(path) = receiver.recv() {
                        if let Err(error) = cache.hash(&path) {
                            tracing::warn!(%error, path = %path.display(),
                                "retention: could not hash a file for receipt verification");
                        }
                        if let Ok(mut set) = inflight.lock() {
                            set.remove(&path);
                        }
                    }
                });
            if spawned.is_err() {
                if let Ok(mut set) = self.inflight.lock() {
                    set.remove(path);
                }
                return;
            }
            *tx = Some(sender);
        }
        if let Some(sender) = tx.as_ref() {
            if sender.send(path.to_path_buf()).is_err() {
                *tx = None;
                if let Ok(mut set) = self.inflight.lock() {
                    set.remove(path);
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "retention_registry_tests.rs"]
mod tests;
