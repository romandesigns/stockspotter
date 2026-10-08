//! Step-4 offline evaluator: `ws-server step4-eval <command> ...`.
//!
//! Built only with `--features offline-eval`. **Not part of the default build
//! or of any deployed image.** It runs off production against exported,
//! compressed, closed observer runs.
//!
//! It is a CLI *around* the frozen implementations and decides nothing itself:
//!
//! | command              | frozen code it calls                                    |
//! |----------------------|---------------------------------------------------------|
//! | `certify-run`        | `stream::assess_streaming_bound` (the L1 certifier)     |
//! | `extract-oi-session` | `oi_extract::extract` (`d6-oi-extract-v2`)              |
//! | `qualify-session`    | the certifier, `Preregistration::evaluate` (floors),    |
//! |                      | `analysis::extract_session` + `select` (discriminating) |
//! | `replay-campaign`    | `campaign::Campaign::replay`                            |
//! | `record-designation` | `Campaign::apply(Designated)`                           |
//! | `record-skip`        | (operating rule only; the frozen ledger is untouched)   |
//! | `record-capture`     | `Campaign::apply(CaptureRecorded)`                      |
//!
//! What is new here is only evidence intake (receipt-verified decompression),
//! the boundary-run check, the two operating rules (designation deadline
//! 04:00 ET of market day d; every eligible regular session in calendar order,
//! skips recorded with a reason), and append-only ledger I/O.
//!
//! **There is no outcome command.** Nothing here can obtain an `OutcomeAccess`:
//! this module never calls `Campaign::outcome_access`, and a test reads this
//! file to keep it that way. Whether outcomes are locked follows from the
//! `state` that `replay-campaign` prints (the frozen firewall refuses in every
//! state before an explicit authorization event).
//!
//! **Evidence is read, never changed.** No command removes, truncates or
//! replaces a file it was pointed at. The three `record-*` commands append one
//! line to the ledger or the skips file, and that is the only write outside
//! scratch. Archived evidence is decompressed into a scratch directory this
//! process creates fresh (it refuses to reuse an existing one) and afterwards
//! removes file by file -- only the files it wrote there, never recursively.
//!
//! **Names in a receipt are untrusted.** Each must be a bare file name (no
//! separator, no drive or stream qualifier, no `.`/`..`), must name a regular
//! file directly inside the evidence directory, and must not be a symlink or
//! reparse point; the evidence directory itself may hold no links and no
//! subdirectories.
//!
//! **What the path checks assume.** They assume an operator-controlled input
//! directory that nothing is writing to while a command runs. A name is
//! checked and then opened as two separate operations, so this is not hardened
//! against a hostile process changing the directory concurrently. Nothing here
//! is a claim of production readiness: this is offline research tooling.
//!
//! **Reads are bounded.** Compressed artifacts are streamed, hashed while
//! streaming, and capped on both the compressed and the decompressed side
//! (see [`Limits`]); exceeding a cap is a refusal, not a truncation. The OI
//! files the frozen extractor needs in memory are capped per file and in
//! aggregate.
//!
//! **OI evidence is this tool's own extraction, checked.** `qualify-session`
//! accepts only the output of `extract-oi-session`, and does not take its
//! `certifies` flag on trust: the schema, contract, report, tally and row
//! counters must agree with one another (see `oi_zero_loss`).
//!
//! Every command prints exactly one JSON object and fails closed: exit 0 = done
//! (or PASS), 1 = refused / FAIL, 2 = INDETERMINATE, 64 = usage.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::observation::{
    self, analysis, campaign, oi_extract, prereg, stream, BoundIdentity, CaptureVerdict, FloorVerdict, Preregistration,
};

pub const QUALIFICATION_SCHEMA: &str = "step4-eval-qualification-v1";
pub const OI_EXTRACTION_SCHEMA: &str = "step4-eval-oi-extraction-v1";
pub const SKIP_SCHEMA: &str = "step4-operational-skip-v1";
const ARCHIVE_RECEIPT: &str = "archive-receipt.json";
const RECEIPT_SCHEMA: &str = "observation-archive-receipt-v1";
/// Files an archived or exported run directory may legitimately hold besides
/// its listed `.gz` artifacts. Anything else refuses the evidence.
const EVIDENCE_SIDE_FILES: [&str; 5] =
    ["archive-manifest.json", ARCHIVE_RECEIPT, "archive-deletion.json", "export-receipt.json", "certificate.json"];
/// A rollover fires on the first 1 s tick at or after the boundary.
const BOUNDARY_TOLERANCE_SECS: i64 = 5;

const GIB: u64 = 1 << 30;
/// Default cap on the compressed bytes of one run (the sum of its `.gz`
/// artifacts). Gzip never expands its input by more than a fraction of a
/// percent, so the compressed side of a real run stays under the decompressed
/// cap; in practice a session compresses to roughly a fifth of its size.
pub const DEFAULT_MAX_COMPRESSED_BYTES: u64 = 20 * GIB;
/// Default cap on the decompressed bytes of one run (the sum of its sources),
/// which is also the most scratch disk one command can use. Measured sessions
/// are about 4.25 GB (typical) and 9.4 GB (stress) uncompressed and the
/// observer's capture budget is 16 GiB, so 20 GiB admits any run the observer
/// can write while still stopping a decompression bomb.
pub const DEFAULT_MAX_DECOMPRESSED_BYTES: u64 = 20 * GIB;
/// Default cap on ONE `opportunity-intelligence-<date>.ndjson` file (about
/// 10 GB per day). See `extract_oi_session` for why these are held in memory.
pub const DEFAULT_MAX_OI_FILE_BYTES: u64 = 16 * GIB;
/// Default cap on everything `extract-oi-session` holds in memory at once:
/// the session's data files (two UTC dates at about 10 GB each) plus every
/// marker file in the directory. Marker files are small but unbounded in
/// number, so without this the per-file caps bound nothing in total. 24 GiB
/// admits two ordinary days with room for markers.
pub const DEFAULT_MAX_OI_TOTAL_BYTES: u64 = 24 * GIB;
pub const ENV_MAX_COMPRESSED_BYTES: &str = "STEP4_EVAL_MAX_COMPRESSED_BYTES";
pub const ENV_MAX_DECOMPRESSED_BYTES: &str = "STEP4_EVAL_MAX_DECOMPRESSED_BYTES";
pub const ENV_MAX_OI_FILE_BYTES: &str = "STEP4_EVAL_MAX_OI_FILE_BYTES";
pub const ENV_MAX_OI_TOTAL_BYTES: &str = "STEP4_EVAL_MAX_OI_TOTAL_BYTES";
/// Receipts, OI evidence, qualifications, marker files, the ledger and the
/// skips file are small JSON/NDJSON; anything larger is not one of them.
const MAX_SMALL_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// `run_start` and `run_end` are single records; a "line" longer than this is
/// not one.
const MAX_RECORD_LINE_BYTES: u64 = 1024 * 1024;
const STREAM_CHUNK: usize = 1 << 16;

/// Read caps. The defaults are constants above; each can be overridden with a
/// positive integer number of bytes in the named environment variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_compressed_bytes: u64,
    pub max_decompressed_bytes: u64,
    pub max_oi_file_bytes: u64,
    pub max_oi_total_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_compressed_bytes: DEFAULT_MAX_COMPRESSED_BYTES,
            max_decompressed_bytes: DEFAULT_MAX_DECOMPRESSED_BYTES,
            max_oi_file_bytes: DEFAULT_MAX_OI_FILE_BYTES,
            max_oi_total_bytes: DEFAULT_MAX_OI_TOTAL_BYTES,
        }
    }
}

impl Limits {
    /// Limits from `lookup` (the environment in production). A value that is
    /// present but not a positive integer is an error, never a silent default.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let get = |name: &str, default: u64| match lookup(name) {
            None => Ok(default),
            Some(raw) => match raw.trim().parse::<u64>() {
                Ok(n) if n > 0 => Ok(n),
                _ => Err(format!("{name} must be a positive integer number of bytes, got {raw:?}")),
            },
        };
        Ok(Limits {
            max_compressed_bytes: get(ENV_MAX_COMPRESSED_BYTES, DEFAULT_MAX_COMPRESSED_BYTES)?,
            max_decompressed_bytes: get(ENV_MAX_DECOMPRESSED_BYTES, DEFAULT_MAX_DECOMPRESSED_BYTES)?,
            max_oi_file_bytes: get(ENV_MAX_OI_FILE_BYTES, DEFAULT_MAX_OI_FILE_BYTES)?,
            max_oi_total_bytes: get(ENV_MAX_OI_TOTAL_BYTES, DEFAULT_MAX_OI_TOTAL_BYTES)?,
        })
    }

    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }
}

const USAGE: &str = "usage: ws-server step4-eval <command>
  certify-run <evidence_dir> <prereg.json>
  extract-oi-session <session> <research_dir> <implementation_sha> <oi_fingerprint>
  qualify-session <session> <evidence_dir> <prereg.json> <oi_evidence.json>
  replay-campaign <ledger.ndjson> <skips.ndjson> <campaign_start>
  record-designation <ledger.ndjson> <skips.ndjson> <campaign_start> <session>
  record-skip <ledger.ndjson> <skips.ndjson> <campaign_start> <session> <reason>
  record-capture <ledger.ndjson> <qualification.json>
environment (bytes; a cap that is exceeded refuses with exit 1):
  STEP4_EVAL_MAX_COMPRESSED_BYTES    per run, sum of .gz artifacts   (default 20 GiB)
  STEP4_EVAL_MAX_DECOMPRESSED_BYTES  per run, sum of sources         (default 20 GiB)
  STEP4_EVAL_MAX_OI_FILE_BYTES       per opportunity-intelligence file (default 16 GiB)
  STEP4_EVAL_MAX_OI_TOTAL_BYTES      all OI data and marker files held in memory (default 24 GiB)
  STEP4_EVAL_SCRATCH                 parent of the scratch directory (default: the temp dir)";

pub fn cli(args: &[String]) -> i32 {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let limits = match Limits::from_env() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{e}");
            return 64;
        }
    };
    let (code, out) = match a.as_slice() {
        ["certify-run", dir, pre] => certify_run(Path::new(dir), Path::new(pre), &limits),
        ["extract-oi-session", s, dir, i, f] => extract_oi_session(s, Path::new(dir), i, f, &limits),
        ["qualify-session", s, dir, pre, oi] => qualify_session(s, Path::new(dir), Path::new(pre), Path::new(oi), &limits),
        ["replay-campaign", l, k, start] => replay_campaign(Path::new(l), Path::new(k), start),
        ["record-designation", l, k, start, s] => record_designation(Path::new(l), Path::new(k), start, s, Utc::now()),
        ["record-skip", l, k, start, s, reason] => record_skip(Path::new(l), Path::new(k), start, s, reason, Utc::now()),
        ["record-capture", l, q] => record_capture(Path::new(l), Path::new(q)),
        _ => {
            eprintln!("{USAGE}");
            return 64;
        }
    };
    println!("{out}");
    code
}

fn refuse(why: impl std::fmt::Display) -> (i32, Value) {
    (1, json!({ "refused": why.to_string() }))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ===========================================================================
// Frozen preregistration
// ===========================================================================

/// The frozen preregistration, loaded through the production validator.
pub struct Frozen {
    pub jcs_sha256: String,
    pub implementation_sha: String,
    pub oi_fingerprint: String,
    pub floors: Preregistration,
    pub selection: analysis::SelectionConfig,
}

pub fn load_frozen(path: &Path) -> Result<Frozen, String> {
    let bound = prereg::load(path).map_err(|e| format!("preregistration refused: {e}"))?;
    let v = &bound.value;
    if v["freezeStatus"].as_str() != Some("FINAL") {
        return Err("preregistration is not FINAL".into());
    }
    let int = |p: &str| -> Result<u64, String> {
        p.split('.').fold(Some(v), |acc, k| acc.and_then(|x| x.get(k))).and_then(Value::as_u64).ok_or(format!("{p} missing"))
    };
    let text = |p: &str| -> Result<String, String> {
        v.get(p).and_then(Value::as_str).map(str::to_string).ok_or(format!("{p} missing"))
    };
    let floors = Preregistration {
        protocol_version: text("protocolVersion")?,
        gate_sha256: text("gateSha256")?,
        eligibility_floor: int("floorsBasisPoints.eligibility")? as f64 / 10_000.0,
        provenance_establishment_floor: int("floorsBasisPoints.provenanceEstablishment")? as f64 / 10_000.0,
        freshness_max_age_secs: (int("freshness.primaryMaxAgeMs")? / 1000) as i64,
        // The frozen Step-4 preregistration declares no re-freeze ladder:
        // one attempt, no search for a passing threshold.
        refreeze_ladder_secs: Vec::new(),
        max_refreezes: 0,
    };
    floors.validate().map_err(|e| format!("preregistration floors refused: {e}"))?;
    Ok(Frozen {
        jcs_sha256: bound.sha256.clone(),
        implementation_sha: bound.implementation_sha().to_string(),
        oi_fingerprint: text("oiConfigFingerprint")?,
        floors,
        selection: analysis::SelectionConfig {
            budget: int("selection.budget")? as usize,
            min_pool: int("selection.minDiscriminatingPool")? as usize,
        },
    })
}

// ===========================================================================
// Evidence intake
// ===========================================================================

/// A bare file name taken from a receipt or a directory listing: exactly one
/// normal path component. Separators, a drive or stream qualifier (`:`), NUL,
/// `.`, `..`, the empty string and anything absolute are refused before the
/// name is joined to a directory.
pub fn bare_name(name: &str) -> Result<&str, String> {
    let p = Path::new(name);
    let mut parts = p.components();
    let single = matches!((parts.next(), parts.next()), (Some(std::path::Component::Normal(c)), None) if c == std::ffi::OsStr::new(name));
    if name.is_empty() || name.contains(['/', '\\', ':', '\0']) || p.is_absolute() || !single {
        return Err(format!("unsafe file name in evidence: {name:?}"));
    }
    Ok(name)
}

/// A symlink, or on Windows any reparse point (symlink, junction, mount point,
/// cloud placeholder). `meta` must come from `symlink_metadata`.
fn is_link(meta: &std::fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// Resolves `name` inside `root` (which must already be canonical). The name
/// must be bare, must be a regular file that is not a link, and must resolve
/// to a direct child of `root`.
pub fn evidence_file(root: &Path, name: &str, what: &str) -> Result<PathBuf, String> {
    let p = root.join(bare_name(name)?);
    let meta = std::fs::symlink_metadata(&p).map_err(|e| format!("missing {what} {name}: {e}"))?;
    if is_link(&meta) || !meta.is_file() {
        return Err(format!("{what} {name} is not a regular file (symlinks and reparse points are refused)"));
    }
    let real = std::fs::canonicalize(&p).map_err(|e| format!("cannot resolve {what} {name}: {e}"))?;
    if real.parent() != Some(root) {
        return Err(format!("{what} {name} resolves outside the evidence directory"));
    }
    Ok(real)
}

/// The canonical evidence directory and the names in it. Every entry must be a
/// regular file with a UTF-8 name; a link or a subdirectory refuses the whole
/// directory, so nothing reached through it can lie outside it.
fn evidence_root(dir: &Path) -> Result<(PathBuf, BTreeSet<String>), String> {
    let root = std::fs::canonicalize(dir).map_err(|e| format!("cannot read evidence: {e}"))?;
    let mut names = BTreeSet::new();
    for entry in std::fs::read_dir(&root).map_err(|e| format!("cannot read evidence: {e}"))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().into_string().map_err(|n| format!("evidence file name is not UTF-8: {n:?}"))?;
        let meta = std::fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
        if is_link(&meta) || !meta.is_file() {
            return Err(format!("evidence entry {name} is not a regular file (symlinks and reparse points are refused)"));
        }
        names.insert(name);
    }
    Ok((root, names))
}

/// Reads a small file whole, refusing one larger than `MAX_SMALL_FILE_BYTES`.
fn read_small(path: &Path, what: &str) -> Result<Vec<u8>, std::io::Error> {
    read_capped(path, MAX_SMALL_FILE_BYTES, what, "the built-in small-file limit")
}

/// Reads a file whole, but never more than `cap` bytes: a larger file is an
/// error rather than a truncated read.
fn read_capped(path: &Path, cap: u64, what: &str, knob: &str) -> Result<Vec<u8>, std::io::Error> {
    let mut out = Vec::new();
    File::open(path)?.take(cap.saturating_add(1)).read_to_end(&mut out)?;
    if out.len() as u64 > cap {
        return Err(std::io::Error::other(format!("{what} exceeds the size cap of {cap} bytes ({knob})")));
    }
    Ok(out)
}

/// The scratch directory one archived run is decompressed into.
///
/// `root` is created by this process with `create_dir`, which fails if the
/// path already exists, so it never adopts a directory it did not make. Cleanup
/// removes exactly the files recorded in `files` (each created here with
/// `create_new`) and then the two directories with the non-recursive
/// `remove_dir`, which fails on anything that is not empty. There is no
/// recursive removal: a file this process did not create is left where it is,
/// and so is the directory holding it.
struct Scratch {
    root: PathBuf,
    run_dir: PathBuf,
    files: Vec<PathBuf>,
}

impl Scratch {
    fn create(root: PathBuf, run_id: &str) -> Result<Self, String> {
        std::fs::create_dir(&root).map_err(|e| format!("cannot create a fresh scratch directory {}: {e}", root.display()))?;
        let run_dir = root.join(bare_name(run_id)?);
        let scratch = Scratch { root, run_dir, files: Vec::new() };
        std::fs::create_dir(&scratch.run_dir).map_err(|e| format!("cannot create scratch run directory: {e}"))?;
        Ok(scratch)
    }

    fn new_file(&mut self, name: &str) -> Result<File, String> {
        let p = self.run_dir.join(bare_name(name)?);
        let f = std::fs::OpenOptions::new().write(true).create_new(true).open(&p).map_err(|e| format!("cannot create scratch file {name}: {e}"))?;
        self.files.push(p);
        Ok(f)
    }

    fn still_ours(&self) -> bool {
        // A directory that was never created is fine; one that exists must
        // still be a real directory.
        [&self.root, &self.run_dir].iter().all(|d| match std::fs::symlink_metadata(d) {
            Ok(m) => m.is_dir() && !is_link(&m),
            Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // If either directory was replaced by a link, a path under it no
        // longer names what this process created: remove nothing.
        if !self.still_ours() {
            return;
        }
        for f in &self.files {
            let _ = std::fs::remove_file(f);
        }
        let _ = std::fs::remove_dir(&self.run_dir);
        let _ = std::fs::remove_dir(&self.root);
    }
}

/// A name no other evidence in this process (or any other) is using. Unique by
/// construction, and `Scratch::create` refuses if it somehow is not.
fn scratch_path() -> PathBuf {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::env::var_os("STEP4_EVAL_SCRATCH").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join(format!(
        "step4-eval-{}-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}

/// A run's records, materialised as the certifier expects them: a directory
/// named for the run holding `observations-N.ndjson`.
pub struct Evidence {
    pub dir: PathBuf,
    pub run_id: String,
    pub kind: &'static str,
    pub receipt: Option<Value>,
    pub receipt_sha256: Option<String>,
    /// Held only so its `Drop` runs when the evidence is dropped.
    _scratch: Option<Scratch>,
}

fn is_source(name: &str) -> bool {
    name.strip_prefix("observations-")
        .and_then(|r| r.strip_suffix(".ndjson"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Counts and hashes every byte read through it.
struct Hashing<R> {
    inner: R,
    hasher: Sha256,
    bytes: u64,
}

impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }
}

/// What one streamed artifact turned out to be.
struct Streamed {
    compressed_bytes: u64,
    compressed_sha256: String,
    decompressed_bytes: u64,
    decompressed_sha256: String,
}

/// Streams one `.gz` artifact into `out`, hashing both sides as it goes. At
/// most `compressed_cap + 1` bytes are read from the artifact and at most
/// `decompressed_cap` bytes are written; memory is one chunk.
fn stream_gunzip(gz: &Path, name: &str, out: &mut File, compressed_cap: u64, decompressed_cap: u64) -> Result<(Streamed, Option<String>), String> {
    let over_compressed = || {
        format!("compressed-size cap exceeded at {name}: the run's compressed artifacts exceed {ENV_MAX_COMPRESSED_BYTES} (raise it only for evidence you trust)")
    };
    let file = File::open(gz).map_err(|e| format!("missing compressed artifact {name}: {e}"))?;
    if file.metadata().map_err(|e| e.to_string())?.len() > compressed_cap {
        return Err(over_compressed());
    }
    let reader = Hashing { inner: file.take(compressed_cap.saturating_add(1)), hasher: Sha256::new(), bytes: 0 };
    let mut decoder = flate2::read::GzDecoder::new(reader);
    let mut hasher = Sha256::new();
    let mut written: u64 = 0;
    let mut chunk = vec![0u8; STREAM_CHUNK];
    let mut decode_error = None;
    loop {
        match decoder.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                written += n as u64;
                if written > decompressed_cap {
                    return Err(format!(
                        "decompressed-size cap exceeded at {name}: the run decompresses to more than {ENV_MAX_DECOMPRESSED_BYTES} allows (raise it only for evidence you trust)"
                    ));
                }
                hasher.update(&chunk[..n]);
                out.write_all(&chunk[..n]).map_err(|e| format!("cannot write scratch copy of {name}: {e}"))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => {
                decode_error = Some(e);
                break;
            }
        }
    }
    // Hash whatever the decoder did not consume, so the compressed digest is
    // of the whole file even when decoding stopped early.
    let mut reader = decoder.into_inner();
    std::io::copy(&mut reader, &mut std::io::sink()).map_err(|e| e.to_string())?;
    if reader.bytes > compressed_cap {
        return Err(over_compressed());
    }
    out.sync_all().map_err(|e| e.to_string())?;
    let streamed = Streamed {
        compressed_bytes: reader.bytes,
        compressed_sha256: hex(&reader.hasher.finalize()),
        decompressed_bytes: written,
        decompressed_sha256: hex(&hasher.finalize()),
    };
    // A decode failure is returned beside the digests, not instead of them:
    // the caller reports it only if the bytes ARE the receipt's, because a
    // file that is not the receipted artifact is a receipt mismatch first.
    Ok((streamed, decode_error.map(|e| format!("corrupt compressed artifact {name}: {e}"))))
}

/// Opens evidence. An archived run (`archive-receipt.json` present) is
/// verified against its receipt and decompressed to scratch; every compressed
/// artifact must hash to the receipt and decompress to the recorded source
/// bytes. A raw run is accepted only where `allow_raw` (certification of
/// fixtures and local runs), never for qualification.
///
/// Nothing in `dir` is written, renamed or removed.
pub fn open_evidence(dir: &Path, allow_raw: bool, limits: &Limits) -> Result<Evidence, String> {
    open_evidence_in(dir, allow_raw, limits, scratch_path())
}

fn open_evidence_in(dir: &Path, allow_raw: bool, limits: &Limits, scratch_root: PathBuf) -> Result<Evidence, String> {
    let (root, names) = evidence_root(dir)?;
    let run_id = root.file_name().and_then(|n| n.to_str()).map(str::to_string).ok_or("evidence path has no name")?;
    bare_name(&run_id)?;
    if !names.contains(ARCHIVE_RECEIPT) {
        if !allow_raw {
            return Err("no archive receipt: qualification requires archived, receipt-verified evidence".into());
        }
        if names.iter().any(|n| !is_source(n)) || names.is_empty() {
            return Err(format!("raw evidence must hold only observations-N.ndjson files: {names:?}"));
        }
        return Ok(Evidence { dir: root, run_id, kind: "raw", receipt: None, receipt_sha256: None, _scratch: None });
    }
    let rbytes = read_small(&evidence_file(&root, ARCHIVE_RECEIPT, "archive receipt")?, "archive receipt").map_err(|e| e.to_string())?;
    let receipt: Value = serde_json::from_slice(&rbytes).map_err(|e| format!("malformed archive receipt: {e}"))?;
    if receipt["schema"].as_str() != Some(RECEIPT_SCHEMA) {
        return Err("not an observation archive receipt".into());
    }
    if receipt["runId"].as_str() != Some(run_id.as_str()) {
        return Err("archive receipt names a different run".into());
    }
    let sources = receipt["sources"].as_array().ok_or("receipt has no sources")?;
    let compressed = receipt["compressed"].as_array().ok_or("receipt has no compressed artifacts")?;
    if sources.len() != compressed.len() || sources.is_empty() {
        return Err("receipt sources and compressed artifacts do not pair up".into());
    }
    // Validate every receipt-supplied name before any of them touches a path.
    // An artifact is always `<source>.gz`, which leaves a receipt no freedom
    // to point at a file of its choosing.
    let mut pairs: Vec<(&str, &Value, &Value)> = Vec::new();
    let mut declared_decompressed: u64 = 0;
    for s in sources {
        let src = bare_name(s["file"].as_str().ok_or("receipt source without file")?)?;
        if !is_source(src) {
            return Err(format!("receipt source {src:?} is not an observation file"));
        }
        if pairs.iter().any(|(seen, _, _)| *seen == src) {
            return Err(format!("receipt lists source {src} twice"));
        }
        let c = compressed.iter().find(|c| c["source"].as_str() == Some(src)).ok_or(format!("no artifact for {src}"))?;
        let gz_name = bare_name(c["file"].as_str().ok_or("artifact without file")?)?;
        if gz_name != format!("{src}.gz") {
            return Err(format!("receipt artifact {gz_name:?} is not {src}.gz"));
        }
        declared_decompressed = declared_decompressed.saturating_add(s["bytes"].as_u64().ok_or("receipt source without bytes")?);
        pairs.push((src, s, c));
    }
    let listed: BTreeSet<String> = pairs.iter().map(|(src, _, _)| format!("{src}.gz")).collect();
    for n in &names {
        if !(listed.contains(n) || EVIDENCE_SIDE_FILES.contains(&n.as_str()) || is_source(n)) {
            return Err(format!("unknown file in evidence: {n}"));
        }
    }
    if declared_decompressed > limits.max_decompressed_bytes {
        return Err(format!(
            "decompressed-size cap exceeded: the receipt declares {declared_decompressed} bytes, more than {ENV_MAX_DECOMPRESSED_BYTES} allows ({})",
            limits.max_decompressed_bytes
        ));
    }
    let mut scratch = Scratch::create(scratch_root, &run_id)?;
    let (mut compressed_left, mut decompressed_left) = (limits.max_compressed_bytes, limits.max_decompressed_bytes);
    for (src, s, c) in &pairs {
        let gz_name = format!("{src}.gz");
        let gz = evidence_file(&root, &gz_name, "compressed artifact")?;
        let mut out = scratch.new_file(src)?;
        let (got, corrupt) = stream_gunzip(&gz, &gz_name, &mut out, compressed_left, decompressed_left)?;
        if got.compressed_sha256 != c["sha256"].as_str().unwrap_or_default() || Some(got.compressed_bytes) != c["bytes"].as_u64() {
            return Err(format!("compressed artifact {gz_name} does not match its receipt"));
        }
        if let Some(detail) = corrupt {
            return Err(detail);
        }
        if got.decompressed_sha256 != s["sha256"].as_str().unwrap_or_default() || Some(got.decompressed_bytes) != s["bytes"].as_u64() {
            return Err(format!("decompression of {gz_name} does not reproduce the recorded source"));
        }
        compressed_left -= got.compressed_bytes;
        decompressed_left -= got.decompressed_bytes;
    }
    Ok(Evidence {
        dir: scratch.run_dir.clone(),
        run_id,
        kind: "archived",
        receipt: Some(receipt.clone()),
        receipt_sha256: Some(sha256_hex(&rbytes)),
        _scratch: Some(scratch),
    })
}

/// The first line of a file, read through a bounded window.
fn first_line(path: &Path) -> Result<Vec<u8>, String> {
    let mut line = Vec::new();
    BufReader::new(File::open(path).map_err(|e| e.to_string())?.take(MAX_RECORD_LINE_BYTES + 1))
        .read_until(b'\n', &mut line)
        .map_err(|e| e.to_string())?;
    if line.pop() != Some(b'\n') {
        return Err("malformed run_start".into());
    }
    Ok(line)
}

/// The last-but-one non-empty line of a file, read from a tail window that
/// grows only until it holds that line whole.
fn penultimate_line(path: &Path) -> Result<Vec<u8>, String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let size = f.metadata().map_err(|e| e.to_string())?.len();
    let mut span: u64 = 64 * 1024;
    loop {
        let take = span.min(size);
        f.seek(SeekFrom::Start(size - take)).map_err(|e| e.to_string())?;
        let mut tail = Vec::new();
        (&mut f).take(take).read_to_end(&mut tail).map_err(|e| e.to_string())?;
        let lines: Vec<&[u8]> = tail.split(|b| *b == b'\n').filter(|l| !l.is_empty()).collect();
        // With three pieces the first may be cut off, but the last two are whole.
        if lines.len() >= 3 || take == size {
            return lines.len().checked_sub(2).map(|i| lines[i].to_vec()).ok_or_else(|| "missing run_end".to_string());
        }
        if span >= 2 * MAX_RECORD_LINE_BYTES + 2 {
            return Err("missing run_end".into());
        }
        span *= 4;
    }
}

/// Every entry name in `dir`. An entry that cannot be enumerated is an error:
/// skipping it would let a listing that failed halfway pass for a complete one.
fn file_names(dir: &Path) -> Result<Vec<String>, String> {
    std::fs::read_dir(dir)
        .map_err(|e| format!("cannot list {}: {e}", dir.display()))?
        .map(|entry| {
            entry
                .map_err(|e| format!("cannot list {}: {e}", dir.display()))?
                .file_name()
                .into_string()
                .map_err(|n| format!("file name in {} is not UTF-8: {n:?}", dir.display()))
        })
        .collect()
}

/// `run_start.startedAt` and `run_end.endedAt` of materialised evidence.
fn run_bounds(dir: &Path) -> Result<(DateTime<Utc>, DateTime<Utc>), String> {
    let mut files: Vec<String> = file_names(dir)?.into_iter().filter(|n| is_source(n)).collect();
    files.sort_by_key(|n| observation::rotation_index(n));
    let first = files.first().ok_or("no observation files")?;
    let last = files.last().ok_or("no observation files")?;
    let start: Value = serde_json::from_slice(&first_line(&dir.join(first))?).map_err(|_| "malformed run_start")?;
    let end: Value = serde_json::from_slice(&penultimate_line(&dir.join(last))?).map_err(|_| "missing run_end")?;
    if start["kind"] != "run_start" || end["kind"] != "run_end" {
        return Err("run_start/run_end not where a closed run keeps them".into());
    }
    let ts = |v: &Value, k: &str| {
        v[k].as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|t| t.with_timezone(&Utc)).ok_or(format!("{k} missing"))
    };
    Ok((ts(&start, "startedAt")?, ts(&end, "endedAt")?))
}

fn parse_session(s: &str) -> Result<NaiveDate, String> {
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| format!("session {s:?} is not YYYY-MM-DD"))?;
    if d.format("%Y-%m-%d").to_string() != s {
        return Err(format!("session {s:?} is not canonical YYYY-MM-DD"));
    }
    Ok(d)
}

/// The run started at the boundary that opens `session` and ended at the one
/// that closes it -- the only shape a designated session's run may have.
pub fn boundary_run(session: NaiveDate, started: DateTime<Utc>, ended: DateTime<Utc>) -> bool {
    let (start, end) = observation::step4_session_bounds(session);
    let tol = Duration::seconds(BOUNDARY_TOLERANCE_SECS);
    started >= start && started < start + tol && ended >= end && ended < end + tol
}

// ===========================================================================
// certify-run
// ===========================================================================

fn certify_run(dir: &Path, pre: &Path, limits: &Limits) -> (i32, Value) {
    let frozen = match load_frozen(pre) {
        Ok(f) => f,
        Err(e) => return refuse(e),
    };
    let ev = match open_evidence(dir, true, limits) {
        Ok(e) => e,
        Err(e) => return refuse(e),
    };
    let expected = BoundIdentity {
        implementation_sha: frozen.implementation_sha.clone(),
        preregistration_sha256: frozen.jcs_sha256.clone(),
    };
    let (verdict, stats) = stream::assess_streaming_bound(&ev.dir, &expected);
    let (detail, certificate) = match &verdict {
        CaptureVerdict::Pass(c) => (Value::Null, serde_json::to_value(c.as_ref()).unwrap_or_default()),
        CaptureVerdict::Fail(d) => (Value::String(d.clone()), Value::Null),
        CaptureVerdict::Indeterminate(i) => (Value::String(i.to_string()), Value::Null),
    };
    let code = match verdict {
        CaptureVerdict::Pass(_) => 0,
        CaptureVerdict::Fail(_) => 1,
        CaptureVerdict::Indeterminate(_) => 2,
    };
    (code, json!({
        "schema": "observation-certificate-v1",
        "runDir": ev.run_id,
        "evidence": ev.kind,
        "archiveReceiptSha256": ev.receipt_sha256,
        "boundTo": { "implementationSha": expected.implementation_sha, "preregistrationSha256": expected.preregistration_sha256 },
        "verdict": verdict.label(),
        "detail": detail,
        "certificate": certificate,
        "bytesRead": stats.bytes_read,
    }))
}

// ===========================================================================
// extract-oi-session
// ===========================================================================

/// Runs the frozen extractor over one session's OI capture.
///
/// **This command holds the session's data files in memory.** The frozen
/// `oi_extract::extract` takes each file as one contiguous byte slice, and it
/// is called as it stands rather than reimplemented as a streaming reader, so
/// the read cannot be streamed here. It is bounded instead: each data file is
/// read through a cap (`Limits::max_oi_file_bytes`, default 16 GiB against
/// roughly 10 GB per day) and a larger file is a refusal, never a truncated
/// read. A session spans two UTC dates, so budget memory for both files.
/// Each marker file is read through the small-file limit, and because there
/// can be any number of them, everything read here also counts against one
/// aggregate budget (`Limits::max_oi_total_bytes`, default 24 GiB): a file
/// that does not fit in what is left is refused before it is read.
fn extract_oi_session(session: &str, research: &Path, implementation_sha: &str, fingerprint: &str, limits: &Limits) -> (i32, Value) {
    let day = match parse_session(session) {
        Ok(d) => d,
        Err(e) => return refuse(e),
    };
    let (start, end) = observation::step4_session_bounds(day);
    // Data files: the UTC dates the session spans (a session crosses midnight
    // UTC). Marker files: ALL of them. A barrier is filed under the wall-clock
    // UTC date it was written, which a deferred barrier can push past the
    // session; marker files are small, and the frozen extractor itself picks
    // out this session's barrier and refuses duplicates.
    let mut dates = BTreeSet::new();
    let mut d = start.date_naive();
    while d <= end.date_naive() {
        dates.insert(d);
        d += Duration::days(1);
    }
    let root = match std::fs::canonicalize(research) {
        Ok(root) => root,
        Err(e) => return refuse(format!("cannot read research directory: {e}")),
    };
    // A file that is absent is absent (the extractor then reports the gap); a
    // file that is present but a link, unreadable or over its cap is a refusal.
    // `remaining` is what is left of the aggregate budget. A file is refused on
    // the size it reports before a byte of it is read, and the read itself
    // stops at the smaller of its own cap and what is left.
    let mut remaining = limits.max_oi_total_bytes;
    let mut read = |name: String, cap: u64, knob: &str| -> Result<Option<(String, Vec<u8>)>, String> {
        if std::fs::symlink_metadata(root.join(&name)).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
            return Ok(None);
        }
        let path = evidence_file(&root, &name, "research file")?;
        let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
        if size > remaining {
            return Err(format!(
                "aggregate in-memory cap exceeded at {name}: {size} bytes with {remaining} left of {ENV_MAX_OI_TOTAL_BYTES} ({})",
                limits.max_oi_total_bytes
            ));
        }
        let bytes = if cap <= remaining {
            read_capped(&path, cap, &name, knob)
        } else {
            read_capped(&path, remaining, &name, ENV_MAX_OI_TOTAL_BYTES)
        }
        .map_err(|e| e.to_string())?;
        remaining -= bytes.len() as u64;
        Ok(Some((name, bytes)))
    };
    let mut data: Vec<(String, Vec<u8>)> = Vec::new();
    for d in &dates {
        match read(format!("opportunity-intelligence-{d}.ndjson"), limits.max_oi_file_bytes, ENV_MAX_OI_FILE_BYTES) {
            Ok(Some(x)) => data.push(x),
            Ok(None) => {}
            Err(e) => return refuse(e),
        }
    }
    let mut marker_names: Vec<String> = match file_names(&root) {
        Ok(names) => {
            names.into_iter().filter(|n| n.starts_with("opportunity-intelligence-markers-") && n.ends_with(".ndjson")).collect()
        }
        Err(e) => return refuse(format!("cannot read research directory: {e}")),
    };
    marker_names.sort();
    let mut markers: Vec<(String, Vec<u8>)> = Vec::new();
    for name in marker_names {
        match read(name, MAX_SMALL_FILE_BYTES, "the built-in small-file limit") {
            Ok(Some(x)) => markers.push(x),
            Ok(None) => {}
            Err(e) => return refuse(e),
        }
    }
    let dv: Vec<(&str, &[u8])> = data.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    let mv: Vec<(&str, &[u8])> = markers.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    let expected = oi_extract::ExpectedSource { implementation_sha, config_fingerprint: fingerprint };
    match oi_extract::extract(session, &dv, &mv, &expected) {
        Err(e) => refuse(e),
        Ok(x) => {
            let ok = x.report.completeness_established;
            (if ok { 0 } else { 1 }, json!({
                "schema": OI_EXTRACTION_SCHEMA,
                "extractionContract": oi_extract::EXTRACTION_CONTRACT,
                "session": session,
                "implementationSha": implementation_sha,
                "configFingerprint": fingerprint,
                "certifies": ok,
                "report": x.report,
                "normalizedSha256": x.binding.normalized_sha256,
                "normalizedRows": x.binding.normalized_rows,
            }))
        }
    }
}

/// The keys `extract-oi-session` prints, and the keys of the frozen
/// `SessionReport` and `SessionTally` it embeds. A test serialises the frozen
/// types and compares, so these cannot drift from what the extractor emits.
const OI_EXTRACTION_KEYS: [&str; 9] = [
    "schema",
    "extractionContract",
    "session",
    "implementationSha",
    "configFingerprint",
    "certifies",
    "report",
    "normalizedSha256",
    "normalizedRows",
];
const OI_REPORT_COUNTERS: [&str; 12] = [
    "sessionMarkers",
    "lateRecordMarkers",
    "malformedMarkers",
    "foreignMarkers",
    "rowsInRanges",
    "foreignRowsInRanges",
    "sessionRowsOutsideRanges",
    "unparseableOutsideRanges",
    "duplicateIdentical",
    "duplicateConflicting",
    "distinctWindows",
    "knownLoss",
];
const OI_REPORT_OTHER: [&str; 4] = ["processId", "tally", "completenessEstablished", "reasons"];
const OI_TALLY_COUNTERS: [&str; 8] =
    ["attempted", "written", "dropped", "writeErrors", "lossSpans", "bytesWritten", "flushErrors", "lateAfterClose"];

/// `v` as an object holding exactly `keys`: none missing, none extra.
fn exact_object<'a>(v: &'a Value, what: &str, keys: &[&str]) -> Result<&'a serde_json::Map<String, Value>, String> {
    let map = v.as_object().ok_or(format!("malformed OI extraction: {what} is not an object"))?;
    if let Some(missing) = keys.iter().find(|k| !map.contains_key(**k)) {
        return Err(format!("malformed OI extraction: {what} has no {missing}"));
    }
    if let Some(extra) = map.keys().find(|k| !keys.contains(&k.as_str())) {
        return Err(format!("malformed OI extraction: {what} has an unknown field {extra}"));
    }
    Ok(map)
}

fn counters<const N: usize>(map: &serde_json::Map<String, Value>, what: &str, keys: [&str; N]) -> Result<[u64; N], String> {
    let mut out = [0u64; N];
    for (slot, key) in out.iter_mut().zip(keys) {
        *slot = map[key].as_u64().ok_or(format!("malformed OI extraction: {what}.{key} is not a non-negative integer"))?;
    }
    Ok(out)
}

/// OI evidence for qualification: the output of this tool's
/// `extract-oi-session`, and nothing else. A document of any other schema or
/// shape is refused by name.
///
/// The `certifies` flag is not taken on trust. The document must have exactly
/// the fields the extractor emits, correctly typed, and its numbers must agree
/// with one another the way the frozen extractor's always do:
///
/// * `certifies` equals `report.completenessEstablished`, which holds exactly
///   when `report.reasons` is empty;
/// * `normalizedRows + duplicateIdentical == rowsInRanges`, and the distinct
///   windows and conflicting duplicates fit inside the normalised rows;
/// * a tally is present exactly when there is exactly one session marker, and
///   `knownLoss` covers at least the tally's dropped and failed rows;
/// * a certifying document additionally has one marker and no late, malformed
///   or foreign ones, a process id, a tally that balances
///   (`attempted == written + dropped + writeErrors`) with every loss counter
///   zero, `rowsInRanges == written`, no foreign row inside the ranges, no
///   session row outside them, and zero known loss.
///
/// A document that breaks any of these is refused as contradicting itself; it
/// is not quietly read as "does not certify". This checks internal
/// consistency only: it does not re-run the extraction, which needs the data.
pub fn oi_zero_loss(v: &Value, session: &str, frozen: &Frozen) -> Result<(bool, &'static str), String> {
    let kind = oi_extract::EXTRACTION_CONTRACT;
    match v.get("schema").and_then(Value::as_str) {
        Some(OI_EXTRACTION_SCHEMA) => {}
        Some(other) => return Err(format!("unrecognised OI evidence schema {other:?}: only {OI_EXTRACTION_SCHEMA} is accepted")),
        None => return Err("unrecognised OI evidence: no schema (only this tool's extract-oi-session output is accepted)".into()),
    }
    let top = exact_object(v, "the document", &OI_EXTRACTION_KEYS)?;
    match top["extractionContract"].as_str() {
        Some(c) if c == kind => {}
        other => return Err(format!("unrecognised OI extraction contract {other:?}: only {kind} is accepted")),
    }
    if top["session"].as_str() != Some(session) {
        return Err("OI evidence is for a different session".into());
    }
    if top["implementationSha"].as_str() != Some(frozen.implementation_sha.as_str()) {
        return Err("OI evidence implementation SHA is not the frozen one".into());
    }
    if top["configFingerprint"].as_str() != Some(frozen.oi_fingerprint.as_str()) {
        return Err("OI evidence configuration fingerprint is not the frozen one".into());
    }
    let certifies = top["certifies"].as_bool().ok_or("malformed OI extraction: certifies is not a boolean")?;
    let normalized_rows = top["normalizedRows"].as_u64().ok_or("malformed OI extraction: normalizedRows is not a non-negative integer")?;
    if !top["normalizedSha256"].as_str().is_some_and(|s| s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))) {
        return Err("malformed OI extraction: normalizedSha256 is not a SHA-256".into());
    }
    let report_keys: Vec<&str> = OI_REPORT_COUNTERS.iter().chain(OI_REPORT_OTHER.iter()).copied().collect();
    let report = exact_object(&top["report"], "report", &report_keys)?;
    let [markers, late, malformed, foreign, in_ranges, foreign_in_ranges, outside_ranges, _unparseable, dup_identical, dup_conflicting, windows, known_loss] =
        counters(report, "report", OI_REPORT_COUNTERS)?;
    let established = report["completenessEstablished"].as_bool().ok_or("malformed OI extraction: report.completenessEstablished is not a boolean")?;
    let reasons = report["reasons"]
        .as_array()
        .filter(|r| r.iter().all(Value::is_string))
        .ok_or("malformed OI extraction: report.reasons is not a list of strings")?;
    let process_id = match &report["processId"] {
        Value::Null => None,
        Value::String(s) => Some(s.as_str()),
        _ => return Err("malformed OI extraction: report.processId is not a string".into()),
    };
    let tally = match &report["tally"] {
        Value::Null => None,
        t => Some(counters(exact_object(t, "report.tally", &OI_TALLY_COUNTERS)?, "report.tally", OI_TALLY_COUNTERS)?),
    };

    let contradiction = |why: String| -> Result<(bool, &'static str), String> { Err(format!("OI extraction contradicts itself: {why}")) };
    if certifies != established {
        return contradiction(format!("certifies is {certifies} but report.completenessEstablished is {established}"));
    }
    if established != reasons.is_empty() {
        return contradiction(format!("completenessEstablished is {established} with {} reasons against it", reasons.len()));
    }
    if normalized_rows.checked_add(dup_identical) != Some(in_ranges) {
        return contradiction(format!("{normalized_rows} normalised rows + {dup_identical} identical duplicates != {in_ranges} rows in ranges"));
    }
    if windows > normalized_rows || (windows == 0) != (normalized_rows == 0) {
        return contradiction(format!("{windows} distinct windows over {normalized_rows} normalised rows"));
    }
    if dup_conflicting > normalized_rows {
        return contradiction(format!("{dup_conflicting} conflicting duplicates among {normalized_rows} normalised rows"));
    }
    if (markers == 1) != tally.is_some() {
        return contradiction(format!("{markers} session markers but the tally is {}", if tally.is_some() { "present" } else { "absent" }));
    }
    if markers != 1 && process_id.is_some() {
        return contradiction(format!("a process id with {markers} session markers"));
    }
    match tally {
        None if known_loss != 0 => return contradiction(format!("known loss {known_loss} with no tally")),
        Some([_, _, dropped, write_errors, ..]) if dropped.checked_add(write_errors).map_or(true, |lost| known_loss < lost) => {
            return contradiction(format!("known loss {known_loss} is less than the tally's {dropped} dropped + {write_errors} write errors"));
        }
        _ => {}
    }
    if certifies {
        let Some([attempted, written, dropped, write_errors, loss_spans, _bytes, flush_errors, late_after_close]) = tally else {
            return contradiction("certifies without a tally".into());
        };
        if markers != 1 || late + malformed + foreign != 0 {
            return contradiction(format!("certifies with {markers} session markers, {late} late, {malformed} malformed, {foreign} foreign"));
        }
        if process_id.is_none() {
            return contradiction("certifies without a process id".into());
        }
        if written.checked_add(dropped).and_then(|x| x.checked_add(write_errors)) != Some(attempted) {
            return contradiction(format!("certifies but the tally does not balance: {attempted} attempted != {written} written + {dropped} dropped + {write_errors} write errors"));
        }
        if [dropped, write_errors, loss_spans, flush_errors, late_after_close].iter().any(|n| *n != 0) {
            return contradiction("certifies with a non-zero loss counter in the tally".into());
        }
        if in_ranges != written {
            return contradiction(format!("certifies but {in_ranges} rows in ranges != {written} written"));
        }
        if foreign_in_ranges != 0 || outside_ranges != 0 {
            return contradiction(format!("certifies with {foreign_in_ranges} foreign rows inside the ranges and {outside_ranges} session rows outside them"));
        }
        if known_loss != 0 {
            return contradiction(format!("certifies with known loss {known_loss}"));
        }
    }
    Ok((certifies, kind))
}

// ===========================================================================
// qualify-session
// ===========================================================================

fn qualify_session(session: &str, dir: &Path, pre: &Path, oi: &Path, limits: &Limits) -> (i32, Value) {
    let day = match parse_session(session) {
        Ok(d) => d,
        Err(e) => return refuse(e),
    };
    if market_data::trading_session::regular_session_open(day).is_none() {
        return refuse(format!("{session} has no regular trading session"));
    }
    let frozen = match load_frozen(pre) {
        Ok(f) => f,
        Err(e) => return refuse(e),
    };
    let oi_value: Value = match read_small(oi, "OI evidence").map_err(|e| e.to_string()).and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string())) {
        Ok(v) => v,
        Err(e) => return refuse(format!("OI evidence unreadable: {e}")),
    };
    let (oi_ok, oi_kind) = match oi_zero_loss(&oi_value, session, &frozen) {
        Ok(x) => x,
        Err(e) => return refuse(e),
    };
    let ev = match open_evidence(dir, false, limits) {
        Ok(e) => e,
        Err(e) => return refuse(e),
    };
    let receipt = ev.receipt.as_ref().expect("archived evidence has a receipt");
    if receipt["implementationSha"].as_str() != Some(frozen.implementation_sha.as_str())
        || receipt["preregistrationSha256"].as_str() != Some(frozen.jcs_sha256.as_str())
    {
        return refuse("archive receipt identity is not the frozen implementation/preregistration");
    }
    let boundary = match run_bounds(&ev.dir) {
        Ok((s, e)) => boundary_run(day, s, e),
        Err(e) => return refuse(e),
    };
    let expected = BoundIdentity {
        implementation_sha: frozen.implementation_sha.clone(),
        preregistration_sha256: frozen.jcs_sha256.clone(),
    };
    let (verdict, _) = stream::assess_streaming_bound(&ev.dir, &expected);
    let verdict_label = verdict.label();
    let (floor_label, floors_satisfied) = match &verdict {
        CaptureVerdict::Pass(cert) => {
            let f = frozen.floors.evaluate(cert);
            (f.label(), matches!(f, FloorVerdict::Met { .. }))
        }
        _ => ("NOT_EVALUATED", false),
    };
    let (discriminating, in_scope, invalid) = if matches!(verdict, CaptureVerdict::Pass(_)) {
        match analysis::extract_session(&ev.dir) {
            Ok(x) => {
                // Pool membership does not depend on OI ranks; no rank is
                // supplied and no arm is read here. The >= 20 gate counts ALL
                // valid discriminating windows in the session, as the frozen
                // preregistration words it; the primary-scope count is
                // reported alongside and never gates.
                let oi = analysis::OiRanks { zero_loss_established: oi_ok, rows: Default::default() };
                let sel = analysis::select(&x, &oi, frozen.selection);
                let disc = sel.windows.iter().filter(|w| w.discriminating).count() as u64;
                let scope =
                    sel.windows.iter().filter(|w| w.discriminating && analysis::in_primary_scope(w.anchor_at)).count() as u64;
                (disc, scope, sel.invalid_windows)
            }
            Err(e) => return refuse(e),
        }
    } else {
        (0, 0, 0)
    };
    let q = campaign::CaptureQualification {
        session: session.to_string(),
        run_id: ev.run_id.clone(),
        // A certificate counts only for the run that spans the session from
        // boundary to boundary.
        certificate_pass: matches!(verdict, CaptureVerdict::Pass(_)) && boundary,
        floors_satisfied,
        discriminating_windows: discriminating,
        oi_zero_loss: oi_ok,
    };
    let qualifies = q.qualifies(&campaign::CampaignRules::default());
    (0, json!({
        "schema": QUALIFICATION_SCHEMA,
        "qualification": q,
        "qualifies": qualifies,
        "detail": {
            "certificateVerdict": verdict_label,
            "boundaryRun": boundary,
            "floorVerdict": floor_label,
            "discriminatingWindows": discriminating,
            "discriminatingWindowsInPrimaryScope": in_scope,
            "invalidWindows": invalid,
            "oiEvidence": oi_kind,
            "archiveReceiptSha256": ev.receipt_sha256,
            "preregistrationSha256": frozen.jcs_sha256,
            "implementationSha": frozen.implementation_sha,
        },
    }))
}

// ===========================================================================
// Campaign ledger and operating rules
// ===========================================================================

/// The ledger's canonical line for an event. Replay refuses any line that is
/// not exactly this, so the file has one spelling and no hidden fields.
pub fn event_line(e: &campaign::CampaignEvent) -> String {
    serde_json::to_string(e).expect("campaign events serialize")
}

pub fn parse_ledger(text: &str) -> Result<Vec<campaign::CampaignEvent>, String> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.is_empty() {
            return Err(format!("ledger line {} is empty", i + 1));
        }
        let e: campaign::CampaignEvent = serde_json::from_str(line).map_err(|e| format!("ledger line {}: {e}", i + 1))?;
        if event_line(&e) != line {
            return Err(format!("ledger line {} is not in canonical form", i + 1));
        }
        out.push(e);
    }
    if !text.is_empty() && !text.ends_with('\n') {
        return Err("ledger does not end with a newline".into());
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Skip {
    pub session: String,
    pub reason: String,
}

pub fn parse_skips(text: &str) -> Result<Vec<Skip>, String> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let v: Value = serde_json::from_str(line).map_err(|e| format!("skips line {}: {e}", i + 1))?;
        let session = v["session"].as_str().ok_or(format!("skips line {}: no session", i + 1))?;
        let reason = v["reason"].as_str().filter(|r| !r.trim().is_empty()).ok_or(format!("skips line {}: no reason", i + 1))?;
        if v["schema"].as_str() != Some(SKIP_SCHEMA) {
            return Err(format!("skips line {}: wrong schema", i + 1));
        }
        parse_session(session)?;
        out.push(Skip { session: session.into(), reason: reason.into() });
    }
    if !text.is_empty() && !text.ends_with('\n') {
        return Err("skips file does not end with a newline".into());
    }
    Ok(out)
}

/// Every regular NYSE session from `from` on, in calendar order.
fn next_regular_session(from: NaiveDate) -> NaiveDate {
    let mut d = from;
    while market_data::trading_session::regular_session_open(d).is_none() {
        d += Duration::days(1);
    }
    d
}

/// The calendar-order rule: designations and skips together must be exactly
/// the regular sessions from `campaign_start`, in order, each once. Returns
/// the next session the rule permits.
pub fn calendar_cursor(events: &[campaign::CampaignEvent], skips: &[Skip], campaign_start: NaiveDate) -> Result<NaiveDate, String> {
    let designated: Vec<NaiveDate> = events
        .iter()
        .filter_map(|e| match e {
            campaign::CampaignEvent::Designated { session } => Some(parse_session(session)),
            _ => None,
        })
        .collect::<Result<_, _>>()?;
    let skipped: Vec<NaiveDate> = skips.iter().map(|s| parse_session(&s.session)).collect::<Result<_, _>>()?;
    let mut all: Vec<NaiveDate> = designated.iter().chain(skipped.iter()).copied().collect();
    all.sort();
    let unique: BTreeSet<NaiveDate> = all.iter().copied().collect();
    if unique.len() != all.len() {
        return Err("a session is both designated and skipped, or listed twice".into());
    }
    if designated.windows(2).any(|w| w[0] >= w[1]) {
        return Err("designations are not in calendar order".into());
    }
    let mut expect = next_regular_session(campaign_start);
    for d in &all {
        if *d != expect {
            return Err(format!("calendar order broken: expected {expect}, found {d} (a skipped session must be recorded with its reason)"));
        }
        expect = next_regular_session(*d + Duration::days(1));
    }
    Ok(expect)
}

fn read_text(p: &Path) -> Result<String, String> {
    match read_small(p, "ledger file") {
        Ok(b) => String::from_utf8(b).map_err(|_| format!("{} is not UTF-8", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(format!("{} does not exist (create it empty)", p.display())),
        Err(e) => Err(e.to_string()),
    }
}

/// Appends one line. `append(true)` without `create` or `truncate`: the file
/// must already exist and its existing bytes are never rewritten.
fn append_line(p: &Path, line: &str) -> Result<(), String> {
    let mut f = std::fs::OpenOptions::new().append(true).open(p).map_err(|e| e.to_string())?;
    f.write_all(format!("{line}\n").as_bytes()).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())
}

/// Loads and validates ledger + skips under both the frozen campaign rules and
/// the operating rules.
pub fn load_campaign(
    ledger: &str,
    skips: &str,
    campaign_start: NaiveDate,
) -> Result<(campaign::Campaign, Vec<campaign::CampaignEvent>, Vec<Skip>, NaiveDate), String> {
    let events = parse_ledger(ledger)?;
    let skips = parse_skips(skips)?;
    let c = campaign::Campaign::replay(campaign::CampaignRules::default(), &events).map_err(|e| format!("ledger replay refused: {e:?}"))?;
    let next = calendar_cursor(&events, &skips, campaign_start)?;
    Ok((c, events, skips, next))
}

fn replay_campaign(ledger: &Path, skips: &Path, start: &str) -> (i32, Value) {
    let r = (|| {
        let start = parse_session(start)?;
        let (c, events, skips, next) = load_campaign(&read_text(ledger)?, &read_text(skips)?, start)?;
        let pending: Vec<&String> =
            c.designated().iter().filter(|s| !events.iter().any(|e| matches!(e, campaign::CampaignEvent::CaptureRecorded { qualification } if &qualification.session == *s))).collect();
        Ok::<Value, String>(json!({
            "state": c.state(),
            "events": events.len(),
            "designated": c.designated(),
            "pendingCapture": pending,
            "qualifyingSessions": c.qualifying_sessions(),
            "skipped": skips.iter().map(|s| json!({"session": s.session, "reason": s.reason})).collect::<Vec<_>>(),
            "nextSessionByCalendarRule": next.to_string(),
        }))
    })();
    match r {
        Ok(v) => (0, v),
        Err(e) => refuse(e),
    }
}

/// The designation window for session `d`: from the boundary that opens it
/// (its run must already exist) to 04:00 ET of market day `d`.
pub fn designation_window(d: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    (observation::step4_session_bounds(d).0, market_data::trading_session::market_day_open(d))
}

pub fn designate(ledger: &str, skips: &str, campaign_start: NaiveDate, session: &str, now: DateTime<Utc>) -> Result<String, String> {
    let d = parse_session(session)?;
    if market_data::trading_session::regular_session_open(d).is_none() {
        return Err(format!("{session} has no regular trading session; it is never designated"));
    }
    let (mut c, _, _, next) = load_campaign(ledger, skips, campaign_start)?;
    if d != next {
        return Err(format!("calendar order: the next session is {next}, not {session}"));
    }
    let (open, deadline) = designation_window(d);
    if now < open {
        return Err(format!("too early: {session}'s run starts at {open}"));
    }
    if now >= deadline {
        return Err(format!("deadline passed: designation for {session} closed at {deadline} (04:00 ET); record a skip"));
    }
    let e = campaign::CampaignEvent::Designated { session: session.to_string() };
    c.apply(e.clone()).map_err(|e| format!("campaign refused: {e:?}"))?;
    Ok(event_line(&e))
}

fn record_designation(ledger: &Path, skips: &Path, start: &str, session: &str, now: DateTime<Utc>) -> (i32, Value) {
    let r = (|| {
        let line = designate(&read_text(ledger)?, &read_text(skips)?, parse_session(start)?, session, now)?;
        append_line(ledger, &line)?;
        Ok::<_, String>(line)
    })();
    match r {
        Ok(line) => (0, json!({ "appended": serde_json::from_str::<Value>(&line).unwrap_or_default(), "recordedAt": now })),
        Err(e) => refuse(e),
    }
}

pub fn skip_line(ledger: &str, skips: &str, campaign_start: NaiveDate, session: &str, reason: &str, now: DateTime<Utc>) -> Result<String, String> {
    let d = parse_session(session)?;
    if reason.trim().is_empty() {
        return Err("a skip needs a reason".into());
    }
    let (_, _, _, next) = load_campaign(ledger, skips, campaign_start)?;
    if d != next {
        return Err(format!("calendar order: the next session is {next}, not {session}"));
    }
    Ok(json!({ "schema": SKIP_SCHEMA, "session": session, "reason": reason, "recordedAt": now }).to_string())
}

fn record_skip(ledger: &Path, skips: &Path, start: &str, session: &str, reason: &str, now: DateTime<Utc>) -> (i32, Value) {
    let r = (|| {
        let line = skip_line(&read_text(ledger)?, &read_text(skips)?, parse_session(start)?, session, reason, now)?;
        append_line(skips, &line)?;
        Ok::<_, String>(line)
    })();
    match r {
        Ok(line) => (0, json!({ "appended": serde_json::from_str::<Value>(&line).unwrap_or_default() })),
        Err(e) => refuse(e),
    }
}

pub fn capture_line(ledger: &str, qualification: &Value) -> Result<String, String> {
    if qualification["schema"].as_str() != Some(QUALIFICATION_SCHEMA) {
        return Err("not a qualify-session result".into());
    }
    let q: campaign::CaptureQualification =
        serde_json::from_value(qualification["qualification"].clone()).map_err(|e| format!("malformed qualification: {e}"))?;
    let mut c = campaign::Campaign::replay(campaign::CampaignRules::default(), &parse_ledger(ledger)?)
        .map_err(|e| format!("ledger replay refused: {e:?}"))?;
    let e = campaign::CampaignEvent::CaptureRecorded { qualification: q };
    c.apply(e.clone()).map_err(|e| format!("campaign refused: {e:?}"))?;
    Ok(event_line(&e))
}

fn record_capture(ledger: &Path, q: &Path) -> (i32, Value) {
    let r = (|| {
        let qv: Value = serde_json::from_slice(&read_small(q, "qualification").map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let line = capture_line(&read_text(ledger)?, &qv)?;
        append_line(ledger, &line)?;
        Ok::<_, String>(line)
    })();
    match r {
        Ok(line) => (0, json!({ "appended": serde_json::from_str::<Value>(&line).unwrap_or_default() })),
        Err(e) => refuse(e),
    }
}

#[cfg(test)]
#[path = "offline_eval_tests.rs"]
mod offline_eval_tests;
