//! Writes order-flow summaries to disk so the features can be validated
//! later -- step 4 ("record observations") of the order-flow
//! specification's sequence: instrument, compute, RECORD, validate, and
//! only then integrate into ranking.
//!
//! **Off unless asked for, twice over.** It needs `ORDER_FLOW_ENABLED`
//! (nothing is computed otherwise) and `ORDER_FLOW_RECORD_DIR` (where to
//! write). Neither is set in any deployment today.
//!
//! This is deliberately NOT part of the consumer-received observation
//! layer (`observation.rs`). That layer is frozen under a preregistration
//! with its own capture budget and completeness rules; order flow is a
//! separate, earlier-stage research feed and must not add rows, bytes or
//! failure modes to it. Separate module, separate directory, separate file.
//!
//! One NDJSON line per active symbol every 30 seconds, in
//! `orderflow-<market day>.ndjson`. A line is the lite summary -- the one
//! the store already caches, without the three arrays that exist to draw
//! charts (`deltaBuckets`, `largePrints`, `profile.bins`): they are large,
//! mostly unchanged between samples, and reconstructible. Without them a
//! line measured about 2.8 KB on the first real run (2026-10-06).
//!
//! **A summary alone is not evidence.** The 2026-10-06/07 recordings this
//! module's archived version produced (12,305 and 205,716 rows) hold
//! nothing but `recordedAt`, `symbol` and the summary. Their window
//! volume, delta, ratio and share-velocity arithmetic is internally
//! consistent, and they still cannot answer the questions a later
//! validation run has to ask first: how much of the session is in this
//! row, is this the same engine as the previous sample's, was the tape
//! uncorrected, and which build produced it. Without those, the 265
//! profile-volume decreases and 320 `profileSince` changes observed across
//! the Oct 6 file are uninterpretable -- an evicted-and-recreated engine
//! and a real data problem look identical. So every line now carries, from
//! the same lock acquisition as its summary
//! (`order_flow_store::active_snapshot`):
//!
//! - `coverage` and `backfill` for that symbol, and
//!   `engineGeneration`/`enginePartial`: a generation change between two
//!   samples is exactly "this is a different engine, its totals restart";
//! - `tape`: process-wide correction/cancel evidence, uncorrected either
//!   way -- counters disclose receipt, never reconciliation;
//! - `capture.health`: the process-wide counters (evictions, capacity
//!   rejections, rejected timestamps, discarded backfills) that say what
//!   the sample as a whole lost;
//! - `capture.marketDay`: the market day the **engines** are on, which is
//!   not always the recorder's clock (see below);
//! - `capture.sourceCommit`: the commit this binary was compiled from, or
//!   `null`. Never a placeholder and never a guess -- a build that cannot
//!   prove its identity says so, the same rule `provenance.rs` applies to
//!   the observation campaign.
//!
//! Nothing here is zero-filled. Missing metadata is `null`; a counter at
//! zero is a counter that really is zero.
//!
//! **The file name is a clock, the `marketDay` field is the fact.** The
//! market day rolls at 04:00 ET, and the store rolls on the first frame of
//! the new day. In between, this process's clock says today while every
//! engine still holds the previous session. The file is named for the
//! clock's day (that is what the day's byte budget is counted against);
//! each row is labelled with the day its engine is actually on, and a
//! mismatch is logged. A row is never restamped to the current day to make
//! the file tidy -- that would invent a session for a day that never
//! traded it.
//!
//! **Disk.** This box has had a disk emergency from research output
//! before, so there are three independent limits:
//!
//! - a **byte ceiling per market day**
//!   (`ORDER_FLOW_RECORD_MAX_MB`, default 512). A sample is written up to
//!   exactly what is left of it and no further -- the ceiling is not
//!   allowed to be overshot by a whole sample, which with a full universe
//!   of active symbols is itself a large write. Reaching it stops writing
//!   for the rest of that day and logs once.
//! - a **byte ceiling across everything retained** (`MAX_TOTAL_BYTES`).
//!   The per-day ceiling alone is not a bound on disk: at the defaults it
//!   permits 512 MB x 14 days of retention, and 7 GB is most of the free
//!   space on the box this runs on. The total is what keeps the feed from
//!   filling a disk slowly instead of quickly.
//! - **retention**: files older than
//!   `ORDER_FLOW_RECORD_RETENTION_DAYS` calendar days (default 14, `0`
//!   keeps everything) are deleted at start-up and whenever a new day's
//!   file is begun. Age is read from the date in the file name, never from
//!   the file system.
//!
//! **Protected paths.** Only names this module itself writes
//! (`orderflow-YYYY-MM-DD.ndjson`) are ever deleted, and a directory is
//! refused outright unless it is the recorder's own:
//!
//! - it must not be, contain, or sit inside a path another subsystem owns
//!   -- the observation campaign's capture root above all. Coupling the
//!   two would let a mistake here reach files a preregistration depends
//!   on. `research_retention` is deliberately not used for the same
//!   reason: that module gates deletion on export receipts for sessions
//!   pinned by the campaign, and none of that applies to this feed.
//! - that check is made on **resolved** paths, not on the strings given.
//!   A lexical comparison is not a safety property: `captures/../captures`
//!   does not start with `captures`, and a symlink does not start with
//!   anything. Every existing ancestor of the configured path is required
//!   to be a real directory, `..` is refused outright, and the comparison
//!   is done against the canonical form of both sides.
//! - it must be **claimed**: a directory is only used if it is empty, or
//!   already holds this recorder's marker file, or holds nothing but this
//!   recorder's own recordings. Pointing the variable at a directory that
//!   holds someone else's files is a configuration mistake, and the
//!   recorder refuses it rather than sweeping inside it.
//! - the file it appends to must be a regular file. A symlink in its place
//!   would make "write inside my own directory" write wherever the link
//!   points.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use market_data::order_flow_store::{BackfillStatus, Coverage, ObservationHealth, ObservationSnapshot, RecordedRow, TapeProvenance};
use order_flow::OrderFlowSummary;
use serde::Serialize;
use tracing::{info, warn};

pub const RECORD_DIR_ENV: &str = "ORDER_FLOW_RECORD_DIR";
const INTERVAL: Duration = Duration::from_secs(30);
/// A symbol is recorded only if it traded within one interval.
const MAX_IDLE_SECS: f64 = 30.0;
pub const RETENTION_DAYS_ENV: &str = "ORDER_FLOW_RECORD_RETENTION_DAYS";
const DEFAULT_RETENTION_DAYS: i64 = 14;
pub const MAX_MB_ENV: &str = "ORDER_FLOW_RECORD_MAX_MB";
const DEFAULT_MAX_MB: u64 = 512;
/// Everything this module may hold on disk at once, across every retained
/// day. See the module comment: the per-day ceiling times the retention
/// window is not a number this box can afford.
const MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const FILE_PREFIX: &str = "orderflow-";
const FILE_SUFFIX: &str = ".ndjson";
/// Written into the directory the first time it is used, and required
/// afterwards. This is what makes "the recorder's own directory" a fact on
/// disk rather than an assumption about a configured string.
const MARKER: &str = ".order-flow-recorder";

/// Environment variables naming paths other subsystems own, directories
/// and files alike. The recorder refuses to write inside any of them, or
/// to have any of them inside it: its own sweep deletes files, and a
/// preregistered capture's evidence must not be reachable from here even
/// by a typo.
///
/// These are the names this codebase actually reads, verified against
/// their definitions (`observation::ENV_ROOT`,
/// `observation::ENV_PREREG_PATH`, `auto_trader`'s journal and paper
/// ledger, `market_data::discovery_audit`) rather than assumed. The
/// archived version of this module listed four variables -- none of which
/// exists here -- so its protection was inert.
const PROTECTED_PATH_ENVS: [&str; 5] = [
    crate::observation::ENV_ROOT,
    crate::observation::ENV_PREREG_PATH,
    "AUTO_TRADER_JOURNAL_PATH",
    "AUTO_TRADER_PAPER_LEDGER_PATH",
    "DISCOVERY_AUDIT_DIR",
];

/// Starts the recorder if both switches are on and the directory is its
/// own; otherwise does nothing.
pub fn spawn_if_configured() {
    if !market_data::order_flow_store::enabled() {
        return;
    }
    let Some(dir) = std::env::var_os(RECORD_DIR_ENV).filter(|d| !d.is_empty()).map(PathBuf::from) else {
        return;
    };
    let protected: Vec<PathBuf> = PROTECTED_PATH_ENVS
        .iter()
        .filter_map(|name| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from))
        .collect();
    if let Err(reason) = check_dir(&dir, &protected) {
        warn!(dir = %dir.display(), reason, "order-flow recorder refused this directory; not recording");
        return;
    }
    if let Err(reason) = claim_dir(&dir) {
        warn!(dir = %dir.display(), reason, "order-flow recorder could not claim this directory as its own; not recording");
        return;
    }
    let retention_days = retention_days(std::env::var(RETENTION_DAYS_ENV).ok().as_deref());
    let max_bytes = max_bytes(std::env::var(MAX_MB_ENV).ok().as_deref());
    info!(dir = %dir.display(), retention_days, max_bytes, total_max_bytes = MAX_TOTAL_BYTES, "order-flow recorder on: one line per active symbol every 30s (retention 0 = keep everything)");
    tokio::spawn(run(dir, retention_days, max_bytes));
}

/// Whether this directory is the recorder's alone. A path that is, or
/// contains, or sits inside a path another subsystem owns is refused: the
/// sweep below deletes files, and being wrong there is unrecoverable.
///
/// Comparison is on resolved paths. `..` is refused rather than
/// normalised, because normalising it without resolving symlinks is how a
/// lexical check convinces itself that `captures/../captures` is somewhere
/// else, and every existing ancestor must be a real directory, so a
/// symlink cannot carry the path out from under the comparison.
fn check_dir(dir: &Path, protected: &[PathBuf]) -> Result<(), &'static str> {
    if dir.as_os_str().is_empty() {
        return Err("empty path");
    }
    if dir.components().any(|c| c == std::path::Component::ParentDir) {
        return Err("a path with `..` in it is not checkable; give the directory itself");
    }
    // "Is a root" is "has nothing but a prefix and a root in it", which is
    // the same question on a Unix path and a Windows drive path; counting
    // components is not (`C:\` has two).
    let is_root = |p: &Path| p.components().all(|c| matches!(c, std::path::Component::Prefix(_) | std::path::Component::RootDir));
    if is_root(dir) {
        return Err("a filesystem root is never a research output directory");
    }
    let dir = resolve(dir)?;
    if is_root(&dir) {
        return Err("a filesystem root is never a research output directory");
    }
    if std::fs::metadata(&dir).is_ok_and(|m| !m.is_dir()) {
        return Err("the path exists and is not a directory");
    }
    for other in protected {
        // A protected path that cannot be resolved at all (it does not
        // exist, or sits behind a link that does not) is still compared as
        // given: refusing to compare would be the wrong way to be unsure.
        let other = resolve(other).unwrap_or_else(|_| other.clone());
        if dir == other || dir.starts_with(&other) || other.starts_with(&dir) {
            return Err("overlaps a path another subsystem owns");
        }
    }
    Ok(())
}

/// The absolute, symlink-free form of `path`: its deepest existing
/// ancestor canonicalised, with the not-yet-created remainder appended.
/// Refuses if any existing ancestor is a symlink or is not a directory --
/// the recorder creates its own directory, and a link in its place would
/// put every rule above a level away from what is actually written to.
fn resolve(path: &Path) -> Result<PathBuf, &'static str> {
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    let mut existing = path.to_path_buf();
    loop {
        match std::fs::symlink_metadata(&existing) {
            Ok(meta) if meta.file_type().is_symlink() => return Err("the path, or an ancestor of it, is a symlink"),
            // A leaf that is not a directory still resolves -- a protected
            // path is often a file, and it has to be comparable. Only an
            // *ancestor* that is not a directory is unresolvable.
            Ok(meta) if !meta.is_dir() && !rest.is_empty() => return Err("an ancestor of the path is not a directory"),
            Ok(_) => {
                let canonical = std::fs::canonicalize(&existing).map_err(|_| "the path could not be resolved")?;
                return Ok(rest.iter().rev().fold(canonical, |acc, part| acc.join(part)));
            }
            Err(_) => {
                let Some(name) = existing.file_name().map(|n| n.to_os_string()) else {
                    // Walked off the top without finding anything that
                    // exists: a relative path with no resolvable root.
                    return Err("the path could not be resolved");
                };
                rest.push(name);
                if !existing.pop() {
                    return Err("the path could not be resolved");
                }
            }
        }
    }
}

/// Takes the directory as this recorder's own, creating it if needed.
/// Refused if it already holds files that are not this module's: the sweep
/// deletes by name, and a directory holding someone else's work is a
/// configuration mistake, not something to start writing into.
fn claim_dir(dir: &Path) -> Result<(), &'static str> {
    std::fs::create_dir_all(dir).map_err(|_| "the directory could not be created")?;
    if dir.join(MARKER).exists() {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir).map_err(|_| "the directory could not be read")?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let ours = name.to_str().is_some_and(|n| recorded_day(n).is_some());
        if !ours {
            return Err("the directory already holds files this recorder did not write");
        }
    }
    std::fs::write(dir.join(MARKER), "order-flow recorder output; see crates/ws-server/src/order_flow_recorder.rs\n").map_err(|_| "the marker file could not be written")
}

/// Unset or unparseable means the default, not "keep forever": a typo must
/// not be able to turn cleanup off.
fn retention_days(raw: Option<&str>) -> i64 {
    raw.and_then(|v| v.trim().parse::<i64>().ok()).filter(|d| *d >= 0).unwrap_or(DEFAULT_RETENTION_DAYS)
}

/// Same rule for the byte ceiling, and zero is not "unlimited" either --
/// there is no way to ask for unlimited, deliberately.
fn max_bytes(raw: Option<&str>) -> u64 {
    let mb = raw.and_then(|v| v.trim().parse::<u64>().ok()).filter(|mb| *mb > 0).unwrap_or(DEFAULT_MAX_MB);
    mb.saturating_mul(1024 * 1024)
}

/// The market day a recording's name says it holds, if it is one of ours.
fn recorded_day(file_name: &str) -> Option<NaiveDate> {
    let stem = file_name.strip_prefix(FILE_PREFIX)?.strip_suffix(FILE_SUFFIX)?;
    NaiveDate::parse_from_str(stem, "%Y-%m-%d").ok()
}

/// Which of `file_names` are recordings more than `retention_days` older
/// than `today`. Zero keeps everything.
fn expired<'a>(file_names: impl IntoIterator<Item = &'a str>, today: NaiveDate, retention_days: i64) -> Vec<&'a str> {
    if retention_days <= 0 {
        return Vec::new();
    }
    file_names.into_iter().filter(|name| recorded_day(name).is_some_and(|day| (today - day).num_days() > retention_days)).collect()
}

/// Deletes expired recordings. Best-effort throughout: a directory that
/// cannot be read or a file that cannot be removed is logged and skipped.
fn sweep(dir: &Path, today: NaiveDate, retention_days: i64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let names: Vec<String> = entries.flatten().filter_map(|e| e.file_name().into_string().ok()).collect();
    for name in expired(names.iter().map(String::as_str), today, retention_days) {
        match std::fs::remove_file(dir.join(name)) {
            Ok(()) => info!(file = name, retention_days, "order-flow recorder removed an expired recording"),
            Err(error) => warn!(%error, file = name, "order-flow recorder could not remove an expired recording"),
        }
    }
}

async fn run(dir: PathBuf, retention_days: i64, max_bytes: u64) {
    let mut swept_for: Option<NaiveDate> = None;
    // Which engine market day has already been reported as not being this
    // process's, so the 04:00 ET window is logged once rather than per
    // sample.
    let mut stale_day_reported: Option<NaiveDate> = None;
    // Bytes written for `swept_for`'s day, bytes this module holds in
    // every OTHER day's recording, and whether a ceiling has already been
    // reported, so it is logged once rather than per sample.
    let mut written: u64 = 0;
    let mut other_days: u64 = 0;
    let mut ceiling_reported = false;
    let mut ticker = tokio::time::interval(INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let now = Utc::now();
        let today = market_data::market_day(now);
        // Once at start-up and once per new market day, whether or not
        // anything is being recorded right now.
        if swept_for != Some(today) {
            swept_for = Some(today);
            ceiling_reported = false;
            // Sweep, re-count and re-claim together on a blocking thread:
            // all three touch the file system, and none of them is work
            // for an async worker.
            let roll = dir.clone();
            let rolled = tokio::task::spawn_blocking(move || {
                sweep(&roll, today, retention_days);
                // Counted after the sweep, so what retention just
                // released is available again, and after a restart, so the
                // day's budget is inherited rather than reset.
                let day = existing_bytes(&roll, today);
                let total = total_bytes(&roll);
                // The directory must still be the recorder's own: a day
                // roll is the one moment it is cheap to check again, and a
                // directory that has been replaced underneath this process
                // is not one to keep appending to.
                (day, total.saturating_sub(day), claim_dir(&roll))
            })
            .await;
            let Ok((day_bytes, other_bytes, claimed)) = rolled else {
                warn!(dir = %dir.display(), "order-flow recorder could not roll to a new market day; stopping");
                return;
            };
            if let Err(reason) = claimed {
                warn!(dir = %dir.display(), reason, "order-flow recorder no longer owns its directory; stopping");
                return;
            }
            written = day_bytes;
            other_days = other_bytes;
        }
        let remaining = remaining_budget(written, other_days, max_bytes);
        if remaining == 0 {
            if !ceiling_reported {
                ceiling_reported = true;
                warn!(dir = %dir.display(), written, other_days, max_bytes, total_max_bytes = MAX_TOTAL_BYTES, "order-flow recorder reached a byte ceiling; not recording further until the next market day");
            }
            continue;
        }
        // One lock acquisition for the summaries and for every piece of
        // context they are recorded with.
        let Some(snapshot) = market_data::order_flow_store::active_snapshot(now, MAX_IDLE_SECS) else {
            continue;
        };
        if snapshot.rows.is_empty() {
            continue;
        }
        // The engines are on a market day of their own; today is this
        // process's. They differ across the 04:00 ET roll until the first
        // frame of the new day arrives, and the rows carry the engines'
        // day rather than being restamped with this one.
        if snapshot.market_day.is_some_and(|day| day != today) && stale_day_reported != snapshot.market_day {
            stale_day_reported = snapshot.market_day;
            warn!(
                engine_day = ?snapshot.market_day,
                file_day = %today,
                "order-flow recorder is sampling a session from an earlier market day; rows carry their own marketDay, which is the authoritative one"
            );
        }
        // Serialized here, with the store lock released: the snapshot is
        // owned data, and the live scan loop takes that lock per trade.
        let lines = sample_lines(now, &snapshot, crate::provenance::build_commit());
        let path = dir.join(format!("{FILE_PREFIX}{today}{FILE_SUFFIX}"));
        // A failed write loses this sample and nothing else: research
        // output must never be able to take the server down.
        match append(&path, &lines, remaining) {
            Ok(bytes) => written = written.saturating_add(bytes),
            Err(error) => warn!(%error, path = %path.display(), "order-flow recorder could not write; sample dropped"),
        }
    }
}

/// How many bytes this sample may write: whatever is left of the day's
/// ceiling, and of what this module may hold on disk at all. Whichever is
/// smaller, because both are limits and neither is a target.
fn remaining_budget(written: u64, other_days: u64, max_bytes: u64) -> u64 {
    let day = max_bytes.saturating_sub(written);
    let total = MAX_TOTAL_BYTES.saturating_sub(other_days.saturating_add(written));
    day.min(total)
}

/// What today's recording already holds, so a restart does not reset the
/// day's byte budget.
fn existing_bytes(dir: &Path, today: NaiveDate) -> u64 {
    std::fs::metadata(dir.join(format!("{FILE_PREFIX}{today}{FILE_SUFFIX}"))).map(|m| m.len()).unwrap_or(0)
}

/// What this module holds in the directory across every day, by the same
/// name rule the sweep deletes by. Nothing else in the directory is
/// counted: the total ceiling bounds this feed, not the disk.
fn total_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| e.file_name().to_str().is_some_and(|n| recorded_day(n).is_some()))
        .filter_map(|e| e.metadata().ok().map(|m| m.len()))
        .sum()
}

/// Appends as much of one sample as `remaining` bytes allows and returns
/// how many bytes it wrote. Whole lines only: a truncated JSON line is not
/// a record, and the next sample would be appended onto it. A sample is
/// one line per active symbol, which at a full universe is a large write,
/// so the ceiling is applied inside the sample rather than after it.
///
/// The one place this module writes: every rule above (the symlink
/// refusal, the budget, whole lines) is applied here rather than once per
/// caller.
fn append(path: &Path, lines: &[String], remaining: u64) -> std::io::Result<u64> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // "Inside my own directory" means nothing if the name is a link: it
    // would append wherever the link points, under this process's rights.
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "recording path is a symlink"));
    }
    let mut out = String::new();
    for line in lines {
        if (out.len() + line.len() + 1) as u64 > remaining {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    if out.is_empty() {
        return Ok(0);
    }
    // One write per sample, which keeps the lines of a sample together.
    // It is NOT a crash guarantee: `write_all` can be interrupted
    // part-way, and a kernel's append is not atomic for a write this size.
    // A reader of these files must tolerate a final partial line.
    std::fs::OpenOptions::new().create(true).append(true).open(path)?.write_all(out.as_bytes())?;
    Ok(out.len() as u64)
}

/// Process-wide correction evidence. Counters disclose that correction and
/// cancel frames were *received*; nothing in this layer applies them, so
/// this is never a claim that a row's volume or profile is reconciled.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
struct Tape {
    provenance: TapeProvenance,
    corrections_seen: u64,
    cancels_seen: u64,
}

/// The half of a record that is the same for every row of one sample: what
/// this process is, what it has lost, and which day its engines are on.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
struct Capture<'a> {
    /// The day the engines are on, from the store itself. `null` before any
    /// event has arrived -- not this process's date, which across the 04:00
    /// ET roll is a different day from the one the rows describe.
    market_day: Option<NaiveDate>,
    /// The commit this binary was compiled from, or `null` for a build that
    /// cannot prove its identity. Never a placeholder: a wrong-looking
    /// value passed through would read as *a different build having run*.
    source_commit: Option<&'a str>,
    health: ObservationHealth,
}

impl<'a> Capture<'a> {
    fn of(snapshot: &ObservationSnapshot, source_commit: Option<&'a str>) -> Self {
        Capture { market_day: snapshot.market_day, source_commit, health: snapshot.health }
    }
}

/// One NDJSON line. `recordedAt`, `symbol` and `summary` are the shape the
/// existing recordings already have, unchanged and in that order;
/// everything else is new beside them.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Record<'a> {
    recorded_at: DateTime<Utc>,
    symbol: &'a str,
    /// The chart-only arrays are already absent: `active_snapshot` strips
    /// them at the source rather than serializing them to throw them away
    /// here.
    summary: &'a OrderFlowSummary,
    coverage: Coverage,
    backfill: BackfillStatus,
    /// This symbol has already lost an engine today, so no backfill makes
    /// its day whole.
    engine_partial: bool,
    /// Which engine produced the summary. A change between samples means
    /// the entry was replaced and the cumulative totals restarted -- which
    /// is what makes a decrease in a profile volume readable.
    engine_generation: u64,
    tape: Tape,
    capture: Capture<'a>,
}

/// The lines for one sample, in the snapshot's order.
///
/// Serialization is fallible, so a row that cannot be written is dropped
/// and the loss is reported rather than being papered over with a null
/// summary (which would read as "this symbol had nothing to say"). Note
/// that a non-finite number is *not* such a case: `serde_json` writes
/// `null` for it and the row is kept, which is why a reader must take a
/// null number as "not representable" and never as zero.
fn sample_lines(recorded_at: DateTime<Utc>, snapshot: &ObservationSnapshot, source_commit: Option<&str>) -> Vec<String> {
    let capture = Capture::of(snapshot, source_commit);
    let lines: Vec<String> = snapshot.rows.iter().filter_map(|row| record_line(recorded_at, snapshot, &capture, row)).collect();
    if lines.len() < snapshot.rows.len() {
        warn!(dropped = snapshot.rows.len() - lines.len(), rows = snapshot.rows.len(), "order-flow recorder could not serialize every row of a sample");
    }
    lines
}

fn record_line(recorded_at: DateTime<Utc>, snapshot: &ObservationSnapshot, capture: &Capture, row: &RecordedRow) -> Option<String> {
    let record = Record {
        recorded_at,
        symbol: &row.symbol,
        summary: &row.summary,
        coverage: row.coverage,
        backfill: row.backfill,
        engine_partial: row.partial,
        engine_generation: row.generation,
        tape: Tape {
            provenance: snapshot.provenance,
            corrections_seen: snapshot.health.corrections_seen,
            cancels_seen: snapshot.health.cancels_seen,
        },
        capture: *capture,
    };
    serde_json::to_string(&record).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const A_COMMIT: &str = "af986b84cd3745f077b48fef912610990b7db725";

    /// A real engine's summary, stripped the way the store hands it to the
    /// recorder. Not a hand-written JSON stand-in: what is being tested is
    /// what actually reaches disk.
    fn summary(trades: u32) -> OrderFlowSummary {
        let mut engine = order_flow::OrderFlowEngine::default();
        engine.on_quote(order_flow::Quote { timestamp_secs: 0.0, bid_price: 2.00, bid_size: 5, ask_price: 2.02, ask_size: 5 });
        for i in 0..trades {
            engine.on_trade(
                order_flow::Trade { timestamp_secs: f64::from(i) * 40.0, price: 2.02, size: 100 },
                order_flow::Session::Regular,
                order_flow::TradeFlags::default(),
            );
        }
        let mut summary = engine.summary(f64::from(trades) * 40.0 + 1.0);
        summary.delta_buckets = Vec::new();
        summary.large_prints = Vec::new();
        if let Some(profile) = summary.profile.as_mut() {
            profile.bins = Vec::new();
        }
        summary
    }

    fn row(symbol: &str) -> RecordedRow {
        RecordedRow {
            symbol: symbol.to_string(),
            summary: summary(3),
            coverage: Coverage::Partial,
            backfill: BackfillStatus::None,
            generation: 1,
            partial: false,
        }
    }

    /// A snapshot as the store would hand one over, with real counters.
    fn snapshot(rows: Vec<RecordedRow>) -> ObservationSnapshot {
        ObservationSnapshot {
            market_day: NaiveDate::from_ymd_opt(2026, 10, 6),
            health: ObservationHealth {
                engines: 4,
                evicted: 2,
                capacity_rejected: 1,
                late_discarded: 0,
                corrections_seen: 7,
                cancels_seen: 3,
                timestamp_rejected: 5,
                backfill_discarded: 1,
            },
            provenance: TapeProvenance::CorrectionsPending,
            rows,
        }
    }

    fn one_line(rows: Vec<RecordedRow>, commit: Option<&str>) -> serde_json::Value {
        let at = Utc.with_ymd_and_hms(2026, 10, 6, 14, 0, 30).unwrap();
        let lines = sample_lines(at, &snapshot(rows), commit);
        assert_eq!(lines.len(), 1);
        serde_json::from_str(&lines[0]).unwrap()
    }

    /// The lines of a three-symbol sample, for the byte-budget tests.
    fn three_lines() -> Vec<String> {
        let at = Utc.with_ymd_and_hms(2026, 10, 6, 14, 0, 30).unwrap();
        sample_lines(at, &snapshot(vec![row("AAA"), row("BBB"), row("CCC")]), None)
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("orderflow-{tag}-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_record_keeps_the_features_and_carries_no_chart_arrays() {
        let at = Utc.with_ymd_and_hms(2026, 10, 6, 14, 0, 30).unwrap();
        let lines = sample_lines(at, &snapshot(vec![row("AAA")]), None);
        let v: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        // The shape the existing recordings have, unchanged.
        assert_eq!(v["symbol"], "AAA");
        assert_eq!(v["recordedAt"], "2026-10-06T14:00:30Z");
        assert_eq!(v["summary"]["profile"]["totalVolume"], 300);
        assert_eq!(v["summary"]["windows"][0]["windowSecs"], 1.0);
        assert_eq!(v["summary"]["windows"].as_array().unwrap().len(), order_flow::WINDOWS_SECS.len());
        // The chart arrays are gone, and the profile's own totals are not.
        assert!(v["summary"]["profile"]["bins"].as_array().unwrap().is_empty());
        assert!(v["summary"]["deltaBuckets"].as_array().unwrap().is_empty());
        assert!(v["summary"]["largePrints"].as_array().unwrap().is_empty());
        assert!(!lines[0].contains('\n'));
    }

    #[test]
    fn a_symbol_with_no_profile_yet_still_records() {
        // A quote and no trade: there is no profile to write, and `null` is
        // the honest answer rather than an empty one.
        let mut quiet = row("AAA");
        quiet.summary = summary(0);
        let v = one_line(vec![quiet], None);
        assert!(v["summary"]["profile"].is_null());
        assert_eq!(v["symbol"], "AAA");
    }

    #[test]
    fn a_record_carries_the_coverage_and_tape_context_its_summary_was_read_with() {
        let v = one_line(
            vec![RecordedRow {
                symbol: "AAA".into(),
                summary: summary(3),
                coverage: Coverage::SessionPending,
                backfill: BackfillStatus::Running,
                generation: 9,
                partial: true,
            }],
            Some(A_COMMIT),
        );
        // How much of the day this row covers, and why.
        assert_eq!(v["coverage"], "session_pending");
        assert_eq!(v["backfill"], "running");
        assert_eq!(v["enginePartial"], true);
        assert_eq!(v["engineGeneration"], 9);
        // Correction evidence: received, never applied.
        assert_eq!(v["tape"]["provenance"], "corrections_pending");
        assert_eq!(v["tape"]["correctionsSeen"], 7);
        assert_eq!(v["tape"]["cancelsSeen"], 3);
        // What the process as a whole lost while this sample was taken.
        assert_eq!(v["capture"]["health"]["engines"], 4);
        assert_eq!(v["capture"]["health"]["evicted"], 2);
        assert_eq!(v["capture"]["health"]["capacityRejected"], 1);
        assert_eq!(v["capture"]["health"]["timestampRejected"], 5);
        assert_eq!(v["capture"]["health"]["backfillDiscarded"], 1);
        // A counter that really is zero is written as zero; nothing else is.
        assert_eq!(v["capture"]["health"]["lateDiscarded"], 0);
        assert_eq!(v["capture"]["sourceCommit"], A_COMMIT);
        assert_eq!(v["capture"]["marketDay"], "2026-10-06");
    }

    #[test]
    fn an_unstamped_build_records_no_commit_rather_than_a_placeholder() {
        // `provenance::build_commit()` is `None` for an ordinary build, and
        // for one whose stamp is malformed. Either way the record says so.
        let v = one_line(vec![row("AAA")], None);
        assert!(v["capture"].get("sourceCommit").is_some(), "the field is present either way");
        assert!(v["capture"]["sourceCommit"].is_null());
        // Specifically not any of the things that would read as an identity.
        for faked in ["unknown", "", "HEAD", "0000000000000000000000000000000000000000"] {
            assert_ne!(v["capture"]["sourceCommit"], faked);
        }
        // And what the running recorder actually passes is that same
        // function, not a value of its own.
        assert_eq!(crate::provenance::build_commit(), option_env!("STOCKSPOTTER_COMMIT").map(str::trim).filter(|c| c.len() == 40));
    }

    #[test]
    fn a_store_with_no_market_day_records_null_rather_than_todays_date() {
        let at = Utc.with_ymd_and_hms(2026, 10, 6, 14, 0, 30).unwrap();
        let mut snap = snapshot(vec![row("AAA")]);
        snap.market_day = None;
        let lines = sample_lines(at, &snap, None);
        let v: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert!(v["capture"]["marketDay"].is_null());
        assert_eq!(v["recordedAt"], "2026-10-06T14:00:30Z");
    }

    /// The 04:00 ET window: the market day has rolled, the store has not
    /// (no frame of the new day has arrived yet), and the engines still
    /// hold the previous session. The row must say which day it is from.
    #[test]
    fn a_session_from_an_earlier_market_day_is_labelled_with_the_day_it_is_from() {
        let yesterday = NaiveDate::from_ymd_opt(2026, 10, 6).unwrap();
        // 04:10 ET on the 7th is already the 7th's market day.
        let after_roll = Utc.with_ymd_and_hms(2026, 10, 7, 8, 10, 0).unwrap();
        assert_eq!(market_data::market_day(after_roll), NaiveDate::from_ymd_opt(2026, 10, 7).unwrap());
        let mut snap = snapshot(vec![row("AAA")]);
        snap.market_day = Some(yesterday);
        let lines = sample_lines(after_roll, &snap, None);
        let v: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        // Taken now, from yesterday's engines, and not restamped as today.
        assert_eq!(v["recordedAt"], "2026-10-07T08:10:00Z");
        assert_eq!(v["capture"]["marketDay"], "2026-10-06");
        assert_ne!(v["capture"]["marketDay"], "2026-10-07");
    }

    #[test]
    fn a_sample_is_one_line_per_row_in_the_snapshots_order() {
        let at = Utc.with_ymd_and_hms(2026, 10, 6, 14, 0, 30).unwrap();
        let lines = sample_lines(at, &snapshot(vec![row("AAA"), row("BBB")]), None);
        let symbols: Vec<String> =
            lines.iter().map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["symbol"].as_str().unwrap().to_string()).collect();
        assert_eq!(symbols, vec!["AAA", "BBB"]);
        assert!(sample_lines(at, &snapshot(Vec::new()), None).is_empty());
    }

    /// A non-finite float is not representable in JSON, and `serde_json`
    /// writes `null` for it rather than failing. That is worth pinning
    /// down, because it is the one way a numeric field in a recording can
    /// be null: a reader must take a null number as "this value was not
    /// representable", never as zero. The row is still written -- every
    /// other field of it is sound, and dropping the symbol silently would
    /// be a worse answer than a visible hole.
    #[test]
    fn a_non_finite_number_is_recorded_as_null_and_never_as_zero() {
        let at = Utc.with_ymd_and_hms(2026, 10, 6, 14, 0, 30).unwrap();
        let mut broken = row("BAD");
        broken.summary.as_of_secs = f64::NAN;
        broken.summary.net_impact_efficiency = Some(f64::INFINITY);
        let lines = sample_lines(at, &snapshot(vec![row("AAA"), broken, row("CCC")]), None);
        let symbols: Vec<String> =
            lines.iter().map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["symbol"].as_str().unwrap().to_string()).collect();
        assert_eq!(symbols, vec!["AAA", "BAD", "CCC"]);
        let bad: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
        assert!(bad["summary"]["asOfSecs"].is_null());
        assert!(bad["summary"]["netImpactEfficiency"].is_null());
        assert_ne!(bad["summary"]["asOfSecs"], 0.0);
        // The metadata beside it is unaffected, which is the point of
        // keeping the row.
        assert_eq!(bad["capture"]["marketDay"], "2026-10-06");
        assert_eq!(bad["tape"]["correctionsSeen"], 7);
    }

    #[test]
    fn retention_defaults_when_unset_or_nonsense_and_zero_means_keep() {
        assert_eq!(retention_days(None), DEFAULT_RETENTION_DAYS);
        assert_eq!(retention_days(Some("abc")), DEFAULT_RETENTION_DAYS);
        assert_eq!(retention_days(Some("-3")), DEFAULT_RETENTION_DAYS);
        assert_eq!(retention_days(Some(" 30 ")), 30);
        assert_eq!(retention_days(Some("0")), 0);
    }

    #[test]
    fn the_byte_ceiling_has_no_way_to_be_switched_off() {
        assert_eq!(max_bytes(None), DEFAULT_MAX_MB * 1024 * 1024);
        assert_eq!(max_bytes(Some("abc")), DEFAULT_MAX_MB * 1024 * 1024);
        // Zero and negatives read as "not a number I will honour".
        assert_eq!(max_bytes(Some("0")), DEFAULT_MAX_MB * 1024 * 1024);
        assert_eq!(max_bytes(Some("-1")), DEFAULT_MAX_MB * 1024 * 1024);
        assert_eq!(max_bytes(Some(" 8 ")), 8 * 1024 * 1024);
        // A value large enough to overflow bytes saturates instead.
        assert_eq!(max_bytes(Some(&u64::MAX.to_string())), u64::MAX);
    }

    #[test]
    fn a_directory_another_subsystem_owns_is_refused_in_either_direction() {
        let root = temp_dir("protected-test");
        let observation = root.join("observation");
        std::fs::create_dir_all(&observation).unwrap();
        let protected = [observation.clone()];
        assert!(check_dir(&root.join("orderflow"), &protected).is_ok());
        // Inside it, equal to it, and containing it.
        assert!(check_dir(&observation.join("orderflow"), &protected).is_err());
        assert!(check_dir(&observation, &protected).is_err());
        assert!(check_dir(&root, &protected).is_err());
        // A root is never an output directory, protected list or not.
        assert!(check_dir(Path::new("/"), &[]).is_err());
        assert!(check_dir(Path::new(""), &[]).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_dot_dot_path_cannot_walk_into_a_protected_directory() {
        // The lexical check this replaces compared the strings it was
        // given: `<root>/orderflow/../observation` does not start with
        // `<root>/observation`, so it was allowed, and it is that
        // directory.
        let root = temp_dir("dotdot-test");
        let observation = root.join("observation");
        std::fs::create_dir_all(&observation).unwrap();
        std::fs::create_dir_all(root.join("orderflow")).unwrap();
        let protected = [observation];
        let sneaky = root.join("orderflow").join("..").join("observation");
        assert!(check_dir(&sneaky, &protected).is_err());
        // And `..` is refused on its own merits, not only when it lands
        // somewhere protected: an unresolvable path is not a checkable one.
        assert!(check_dir(&root.join("orderflow").join("..").join("elsewhere"), &[]).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Creating a symlink needs a privilege this process may not have on
    /// Windows. A test that cannot create one proves nothing and says so,
    /// rather than passing quietly.
    fn try_symlink_dir(target: &Path, link: &Path) -> bool {
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(target, link);
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(target, link);
        if made.is_err() {
            eprintln!("skipped: this platform/process cannot create a directory symlink ({})", link.display());
        }
        made.is_ok()
    }

    fn try_symlink_file(target: &Path, link: &Path) -> bool {
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(target, link);
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(target, link);
        if made.is_err() {
            eprintln!("skipped: this platform/process cannot create a file symlink ({})", link.display());
        }
        made.is_ok()
    }

    #[test]
    fn a_symlinked_directory_is_refused_rather_than_followed() {
        let root = temp_dir("symlink-dir-test");
        let observation = root.join("observation");
        std::fs::create_dir_all(&observation).unwrap();
        let link = root.join("orderflow");
        if !try_symlink_dir(&observation, &link) {
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        // Lexically this is `<root>/orderflow`, which overlaps nothing.
        // It is the capture directory.
        assert!(check_dir(&link, &[observation.clone()]).is_err());
        // Refused even with nothing protected: the recorder creates and
        // sweeps its own directory, and a link is not it.
        assert!(check_dir(&link, &[]).is_err());
        // A link as an *ancestor* is refused too.
        assert!(check_dir(&link.join("inner"), &[]).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_symlinked_recording_file_is_not_written_through() {
        let root = temp_dir("symlink-file-test");
        std::fs::create_dir_all(&root).unwrap();
        let protected = root.join("prereg.json");
        std::fs::write(&protected, "{}").unwrap();
        let path = root.join("orderflow-2026-10-06.ndjson");
        if !try_symlink_file(&protected, &path) {
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        assert!(append(&path, &three_lines(), u64::MAX).is_err());
        // The file the link points at is untouched.
        assert_eq!(std::fs::read_to_string(&protected).unwrap(), "{}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_directory_holding_someone_elses_files_is_not_claimed() {
        let root = temp_dir("claim-test");
        std::fs::create_dir_all(&root).unwrap();
        // Empty: claimed, and marked so that a later restart recognises it.
        assert!(claim_dir(&root).is_ok());
        assert!(root.join(MARKER).exists());
        // Marked: claimed again, whatever else is in it now.
        std::fs::write(root.join("orderflow-2026-10-06.ndjson"), "x").unwrap();
        assert!(claim_dir(&root).is_ok());

        // Someone else's directory, with no marker: refused.
        let theirs = temp_dir("claim-theirs");
        std::fs::create_dir_all(&theirs).unwrap();
        std::fs::write(theirs.join("capture-0001.ndjson"), "evidence").unwrap();
        assert!(claim_dir(&theirs).is_err());
        assert!(!theirs.join(MARKER).exists());
        // Nothing of theirs was touched, which is the whole point.
        assert_eq!(std::fs::read_to_string(theirs.join("capture-0001.ndjson")).unwrap(), "evidence");

        // A directory holding only this recorder's own recordings is
        // claimable without a marker: that is a restart after an upgrade.
        let ours = temp_dir("claim-ours");
        std::fs::create_dir_all(&ours).unwrap();
        std::fs::write(ours.join("orderflow-2026-10-06.ndjson"), "x").unwrap();
        assert!(claim_dir(&ours).is_ok());

        for dir in [root, theirs, ours] {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn the_protected_list_names_variables_this_codebase_actually_reads() {
        // The archived version of this module protected four variables
        // that do not exist here, so it protected nothing. These are
        // checked against their definitions rather than spelled twice.
        assert!(PROTECTED_PATH_ENVS.contains(&crate::observation::ENV_ROOT));
        assert!(PROTECTED_PATH_ENVS.contains(&crate::observation::ENV_PREREG_PATH));
        assert!(PROTECTED_PATH_ENVS.contains(&"AUTO_TRADER_JOURNAL_PATH"));
        assert!(PROTECTED_PATH_ENVS.contains(&"DISCOVERY_AUDIT_DIR"));
        // And a file-valued one is comparable: its parent is what the
        // recorder must not be handed.
        let root = temp_dir("protected-file-test");
        std::fs::create_dir_all(&root).unwrap();
        let journal = root.join("auto_trader_journal.jsonl");
        std::fs::write(&journal, "{}").unwrap();
        assert!(check_dir(&root, &[journal.clone()]).is_err());
        assert!(check_dir(&root.join("orderflow"), &[journal]).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_our_own_old_recordings_expire() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        let names = [
            "orderflow-2026-10-05.ndjson", // 15 days: expired
            "orderflow-2026-10-06.ndjson", // 14 days: kept, the limit is inclusive
            "orderflow-2026-10-20.ndjson",
            "orderflow-notadate.ndjson",
            "orderflow-2026-09-01.ndjson.bak",
            "2026-09-01.ndjson",
            "observation-2026-09-01.ndjson",
        ];
        assert_eq!(expired(names, today, 14), vec!["orderflow-2026-10-05.ndjson"]);
        assert!(expired(names, today, 0).is_empty());
    }

    #[test]
    fn sweep_deletes_expired_files_and_nothing_else() {
        let dir = temp_dir("sweep-test");
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["orderflow-2026-09-01.ndjson", "orderflow-2026-10-19.ndjson", "keep-me.txt"] {
            std::fs::write(dir.join(name), "x").unwrap();
        }
        sweep(&dir, NaiveDate::from_ymd_opt(2026, 10, 20).unwrap(), 14);
        let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().into_string().unwrap()).collect();
        left.sort();
        assert_eq!(left, vec!["keep-me.txt", "orderflow-2026-10-19.ndjson"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_sample_is_written_up_to_exactly_what_is_left_and_no_further() {
        let dir = temp_dir("ceiling-test");
        let path = dir.join("orderflow-2026-10-06.ndjson");
        let lines = three_lines();
        // Every symbol's record is the same size here, so the budget
        // arithmetic below is exact.
        assert!(lines.iter().all(|l| l.len() == lines[0].len()));
        let one_line = lines[0].len() as u64 + 1;

        // Room for two of the three lines, by one byte.
        let written = append(&path, &lines, one_line * 2 + 1).unwrap();
        assert_eq!(written, one_line * 2);
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
        // Every line is whole: a truncated JSON record is not a record,
        // and the next sample would be appended onto it.
        for line in std::fs::read_to_string(&path).unwrap().lines() {
            serde_json::from_str::<serde_json::Value>(line).unwrap();
        }
        // Exactly one line's worth left: exactly one line.
        assert_eq!(append(&path, &lines, one_line).unwrap(), one_line);
        // One byte short of a line: nothing at all, and no empty write.
        assert_eq!(append(&path, &lines, one_line - 1).unwrap(), 0);
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_day_and_the_total_are_both_ceilings_and_the_smaller_one_wins() {
        // Mid-day, with room on both.
        assert_eq!(remaining_budget(100, 0, 500), 400);
        // The day's ceiling reached exactly.
        assert_eq!(remaining_budget(500, 0, 500), 0);
        // The day has room but the retained total does not: 512 MB a day
        // across a 14-day retention is 7 GB, which is why this limit
        // exists at all.
        assert_eq!(remaining_budget(0, MAX_TOTAL_BYTES, 500), 0);
        assert_eq!(remaining_budget(0, MAX_TOTAL_BYTES - 100, 500), 100);
        // Today's own bytes count against the total as well as the day.
        assert_eq!(remaining_budget(60, MAX_TOTAL_BYTES - 100, 500), 40);
        // Nothing underflows into a very large allowance.
        assert_eq!(remaining_budget(u64::MAX, u64::MAX, 500), 0);
    }

    #[test]
    fn the_retained_total_counts_this_modules_own_files_only() {
        let dir = temp_dir("total-test");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(total_bytes(&dir), 0);
        std::fs::write(dir.join("orderflow-2026-10-06.ndjson"), "0123456789").unwrap();
        std::fs::write(dir.join("orderflow-2026-10-07.ndjson"), "01234").unwrap();
        // Not ours: not this feed's budget to spend or to be blamed for.
        std::fs::write(dir.join("notes.txt"), "0123456789012345678901234567890123456789").unwrap();
        std::fs::write(dir.join(MARKER), "x").unwrap();
        assert_eq!(total_bytes(&dir), 15);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_writes_one_line_per_symbol_reports_its_bytes_and_adds_to_an_existing_file() {
        let dir = temp_dir("recorder-test");
        let path = dir.join("nested").join("orderflow-2026-10-06.ndjson");
        let at = Utc.with_ymd_and_hms(2026, 10, 6, 14, 0, 30).unwrap();
        let first = append(&path, &sample_lines(at, &snapshot(vec![row("AAA"), row("BBB")]), None), u64::MAX).unwrap();
        let second = append(&path, &sample_lines(at, &snapshot(vec![row("CCC")]), None), u64::MAX).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let symbols: Vec<String> = text.lines().map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["symbol"].as_str().unwrap().to_string()).collect();
        assert_eq!(symbols, vec!["AAA", "BBB", "CCC"]);
        // Byte counts are what the day's ceiling is charged against, so
        // they have to be the bytes actually on disk.
        assert_eq!(first + second, text.len() as u64);
        assert_eq!(existing_bytes(&dir.join("nested"), NaiveDate::from_ymd_opt(2026, 10, 6).unwrap()), text.len() as u64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_restart_inherits_the_days_bytes_rather_than_starting_over() {
        let dir = temp_dir("resume-test");
        std::fs::create_dir_all(&dir).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 10, 6).unwrap();
        assert_eq!(existing_bytes(&dir, today), 0);
        std::fs::write(dir.join("orderflow-2026-10-06.ndjson"), "0123456789").unwrap();
        assert_eq!(existing_bytes(&dir, today), 10);
        // Another day's file does not count against today.
        assert_eq!(existing_bytes(&dir, NaiveDate::from_ymd_opt(2026, 10, 7).unwrap()), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_recorder_does_not_start_with_its_switches_unset() {
        // ORDER_FLOW_ENABLED is unset under test; this must return before
        // it looks at a directory or spawns anything.
        assert!(!market_data::order_flow_store::enabled());
        spawn_if_configured();
        // And the snapshot the loop reads is `None` with the feature off,
        // so the loop writes nothing. That it also leaves the global store
        // uninitialized is asserted in `order_flow_store`'s own
        // fresh-process gate test.
        assert!(market_data::order_flow_store::active_snapshot(Utc::now(), MAX_IDLE_SECS).is_none());
    }
}
