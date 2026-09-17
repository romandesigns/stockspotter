//! Retention policy tests.
//!
//! Deletion is irreversible and these files are the only copy of a market
//! session, so every safety rule gets a test that would fail if the rule were
//! removed — not a test that merely passes while the rule happens to hold.

use super::*;

use std::sync::atomic::AtomicUsize;

fn temp_dir(tag: &str) -> PathBuf {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "research-retention-{tag}-{}-{}-{n}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

/// Writes one session's worth of files, `bytes` each.
fn session(dir: &Path, day: &str, bytes: usize) {
    let payload = vec![b'x'; bytes];
    for name in [
        format!("opportunity-intelligence-{day}.ndjson"),
        format!("opportunity-intelligence-markers-{day}.ndjson"),
        format!("episodes-{day}.ndjson"),
        format!("episodes-markers-{day}.ndjson"),
    ] {
        std::fs::write(dir.join(name), &payload).unwrap();
    }
}

fn config(dir: &Path, ceiling: u64) -> RetentionConfig {
    RetentionConfig {
        dir: dir.to_path_buf(),
        ceiling_bytes: ceiling,
        min_age_days: DEFAULT_MIN_AGE_DAYS,
        // Zero, so freshly-written fixture files are not held back by the grace
        // window. The grace rule gets its own test, where it is the subject.
        finalize_grace: Duration::from_secs(0),
        sweep_interval: Duration::from_secs(900),
        require_export: false,
    }
}

fn present(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| e.metadata().map(|m| m.is_file()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".ndjson"))
        .collect();
    v.sort();
    v
}

const TODAY: &str = "2026-09-20";
fn now() -> SystemTime {
    SystemTime::now()
}

// ---------------------------------------------------------------------------
// Filename grouping
// ---------------------------------------------------------------------------

#[test]
fn a_session_is_every_file_carrying_its_date() {
    let dir = temp_dir("group");
    session(&dir, "2026-09-16", 10);
    session(&dir, "2026-09-17", 10);
    let sessions = scan(&dir);
    assert_eq!(sessions.len(), 2);
    let s = &sessions[&date("2026-09-16")];
    assert_eq!(s.files.len(), 4, "data and marker streams belong to one session");
    assert_eq!(s.bytes, 40);
    // Both stems parse, including the one with an extra hyphenated component.
    assert_eq!(session_date_of("episodes-2026-09-16.ndjson"), Some(date("2026-09-16")));
    assert_eq!(
        session_date_of("opportunity-intelligence-markers-2026-09-16.ndjson"),
        Some(date("2026-09-16"))
    );
    assert_eq!(session_date_of("not-a-capture.txt"), None);
    assert_eq!(session_date_of("opportunity-intelligence.ndjson"), None);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// The safety rules
// ---------------------------------------------------------------------------

/// The file the writer currently holds is never removed, even when it is old
/// enough and the ceiling is exceeded.
#[test]
fn the_current_capture_file_is_never_removed() {
    let dir = temp_dir("current");
    session(&dir, "2026-09-10", 100);
    session(&dir, "2026-09-11", 100);
    let cfg = config(&dir, 100); // far below the 800 bytes present

    let current = vec![dir
        .join("opportunity-intelligence-2026-09-10.ndjson")
        .to_string_lossy()
        .to_string()];
    let outcome = sweep(&cfg, date(TODAY), now(), &current);

    assert!(
        !outcome.deleted.contains(&date("2026-09-10")),
        "the session holding the writer's current file must survive"
    );
    assert!(present(&dir).iter().any(|n| n.contains("2026-09-10")));
    assert!(outcome.deleted.contains(&date("2026-09-11")), "but a finalized one is reclaimed");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Today and yesterday are never eligible, whatever the ceiling says.
#[test]
fn the_current_and_previous_day_are_never_removed() {
    let dir = temp_dir("recent");
    session(&dir, "2026-09-20", 100); // today
    session(&dir, "2026-09-19", 100); // yesterday
    session(&dir, "2026-09-18", 100); // eligible
    let cfg = config(&dir, 1);

    let outcome = sweep(&cfg, date(TODAY), now(), &[]);
    assert_eq!(
        outcome.deleted,
        vec![date("2026-09-18")],
        "only the session at least {} whole days old may go",
        DEFAULT_MIN_AGE_DAYS
    );
    assert!(outcome.pending, "still over the ceiling, and that must be explicit");

    let files = present(&dir);
    assert!(files.iter().any(|n| n.contains("2026-09-20")));
    assert!(files.iter().any(|n| n.contains("2026-09-19")));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A session touched inside the grace window is not finalized, whatever its
/// date claims.
#[test]
fn a_recently_modified_session_is_not_finalized() {
    let dir = temp_dir("grace");
    session(&dir, "2026-09-10", 100);
    let mut cfg = config(&dir, 1);
    cfg.finalize_grace = Duration::from_secs(3600);

    let sessions = scan(&dir);
    let s = &sessions[&date("2026-09-10")];
    assert_eq!(
        eligibility(s, &cfg, date(TODAY), now(), &[]),
        Some(Ineligible::RecentlyModified),
        "a file written moments ago is not a finalized session"
    );

    let outcome = sweep(&cfg, date(TODAY), now(), &[]);
    assert!(outcome.deleted.is_empty());
    assert!(outcome.pending);
    assert_eq!(present(&dir).len(), 4, "nothing removed");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A hold sentinel is what an export, copy or verification run takes. It must
/// be absolute.
#[test]
fn a_held_session_is_never_removed() {
    let dir = temp_dir("hold");
    session(&dir, "2026-09-10", 100);
    session(&dir, "2026-09-11", 100);
    std::fs::write(dir.join(format!("2026-09-10{HOLD_SUFFIX}")), b"exporting").unwrap();
    let cfg = config(&dir, 1);

    let outcome = sweep(&cfg, date(TODAY), now(), &[]);
    assert_eq!(outcome.deleted, vec![date("2026-09-11")]);
    assert!(present(&dir).iter().any(|n| n.contains("2026-09-10")), "the held session survives");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The global hold stops the policy entirely — the switch to throw before a
/// bulk export.
#[test]
fn a_global_hold_stops_retention_completely() {
    let dir = temp_dir("globalhold");
    session(&dir, "2026-09-10", 100);
    session(&dir, "2026-09-11", 100);
    std::fs::write(dir.join(GLOBAL_HOLD), b"bulk export in progress").unwrap();
    let cfg = config(&dir, 1);

    let outcome = sweep(&cfg, date(TODAY), now(), &[]);
    assert!(outcome.deleted.is_empty(), "nothing may be reclaimed while the directory is held");
    assert!(outcome.pending, "and being blocked must be explicit");
    assert_eq!(present(&dir).len(), 8);
    let _ = std::fs::remove_dir_all(&dir);
}

/// With `require_export`, an unexported session is refused rather than deleted.
#[test]
fn require_export_refuses_to_delete_an_unexported_session() {
    let dir = temp_dir("reqexport");
    session(&dir, "2026-09-10", 100);
    session(&dir, "2026-09-11", 100);
    std::fs::create_dir_all(dir.join(EXPORT_DIR)).unwrap();
    std::fs::write(dir.join(EXPORT_DIR).join(format!("2026-09-11{EXPORT_SUFFIX}")), b"ok").unwrap();

    let mut cfg = config(&dir, 1);
    cfg.require_export = true;
    let outcome = sweep(&cfg, date(TODAY), now(), &[]);

    assert_eq!(
        outcome.deleted,
        vec![date("2026-09-11")],
        "only the session with an export receipt may be reclaimed"
    );
    assert_eq!(outcome.deleted_without_export, 0);
    assert!(outcome.pending, "and the block must be explicit rather than silent");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Without `require_export`, the ceiling still holds — but an unexported
/// deletion is counted, so it is never silent.
#[test]
fn an_unexported_deletion_is_counted_not_silent() {
    let dir = temp_dir("unexported");
    session(&dir, "2026-09-10", 100);
    let cfg = config(&dir, 1);

    let outcome = sweep(&cfg, date(TODAY), now(), &[]);
    assert_eq!(outcome.deleted, vec![date("2026-09-10")]);
    assert_eq!(
        outcome.deleted_without_export, 1,
        "deleting the only copy of a session must be recorded, not merely logged"
    );

    let health = RetentionHealth::default();
    apply(&health, &outcome);
    assert_eq!(health.deleted_without_export.load(Ordering::Relaxed), 1);
    assert_eq!(health.snapshot().last_deleted, "2026-09-10");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// The policy itself
// ---------------------------------------------------------------------------

/// Oldest finalized first, so the most recent research survives longest.
#[test]
fn the_oldest_finalized_session_goes_first() {
    let dir = temp_dir("oldest");
    for day in ["2026-09-14", "2026-09-12", "2026-09-10", "2026-09-16"] {
        session(&dir, day, 100);
    }
    // 1,600 bytes present; a 900-byte ceiling needs two sessions gone.
    let cfg = config(&dir, 900);
    let outcome = sweep(&cfg, date(TODAY), now(), &[]);

    assert_eq!(
        outcome.deleted,
        vec![date("2026-09-10"), date("2026-09-12")],
        "oldest first, and only as many as the ceiling requires"
    );
    assert!(!outcome.pending);
    let files = present(&dir);
    assert!(files.iter().any(|n| n.contains("2026-09-14")));
    assert!(files.iter().any(|n| n.contains("2026-09-16")));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The ceiling is respected, and no more than necessary is deleted.
#[test]
fn the_ceiling_is_respected_and_nothing_extra_is_deleted() {
    let dir = temp_dir("ceiling");
    for day in ["2026-09-10", "2026-09-11", "2026-09-12", "2026-09-13"] {
        session(&dir, day, 100); // 400 bytes each, 1,600 total
    }
    let cfg = config(&dir, 1_200);
    let outcome = sweep(&cfg, date(TODAY), now(), &[]);

    assert_eq!(outcome.dir_bytes_before, 1_600);
    assert!(outcome.dir_bytes_after <= 1_200, "must end inside the ceiling");
    assert_eq!(outcome.deleted.len(), 1, "one session is enough; a second would be wasteful");
    assert_eq!(outcome.bytes_reclaimed, 400);
    assert!(!outcome.pending);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Under the ceiling, retention does nothing at all.
#[test]
fn nothing_is_deleted_while_inside_the_ceiling() {
    let dir = temp_dir("under");
    session(&dir, "2026-09-10", 100);
    let cfg = config(&dir, 10_000);
    let outcome = sweep(&cfg, date(TODAY), now(), &[]);
    assert!(outcome.deleted.is_empty());
    assert!(!outcome.pending);
    assert_eq!(outcome.dir_bytes_after, 400);
    assert_eq!(present(&dir).len(), 4);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Over the ceiling with nothing eligible is an explicit degraded state, not a
/// quiet no-op.
#[test]
fn no_eligible_victim_is_an_explicit_pending_state() {
    let dir = temp_dir("pending");
    session(&dir, "2026-09-20", 100); // today only
    let cfg = config(&dir, 1);

    let outcome = sweep(&cfg, date(TODAY), now(), &[]);
    assert!(outcome.deleted.is_empty());
    assert!(outcome.pending, "being blocked is a state, not silence");

    let health = RetentionHealth::default();
    apply(&health, &outcome);
    let snap = health.snapshot();
    assert!(snap.retention_pending, "and it must be readable from health");
    assert_eq!(snap.sessions_present, 1);
    assert!(snap.last_sweep.is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Byte state is rebuilt from disk on every sweep, so a restart cannot grant a
/// fresh allowance — the same property discovery's `scan_disk_state` provides.
#[test]
fn a_restart_reconstructs_the_retained_byte_state_from_disk() {
    let dir = temp_dir("restart");
    for day in ["2026-09-10", "2026-09-11", "2026-09-12"] {
        session(&dir, day, 100);
    }
    let cfg = config(&dir, 900);

    // "First process": one sweep, one session reclaimed.
    let first = sweep(&cfg, date(TODAY), now(), &[]);
    assert_eq!(first.deleted.len(), 1);

    // "After a restart": fresh health, no memory of the first sweep at all.
    let health = RetentionHealth::default();
    let second = sweep(&cfg, date(TODAY), now(), &[]);
    apply(&health, &second);

    assert_eq!(
        second.dir_bytes_before, 800,
        "the sweep must see what is actually on disk, not a remembered total"
    );
    assert!(second.deleted.is_empty(), "already inside the ceiling");
    assert_eq!(health.snapshot().dir_bytes, 800);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Retention deletes whole sessions or nothing. It never rewrites a record.
#[test]
fn retention_never_alters_a_retained_records_contents() {
    let dir = temp_dir("contents");
    session(&dir, "2026-09-10", 100);
    let keep = dir.join("episodes-2026-09-16.ndjson");
    let content = b"{\"schemaVersion\":1,\"id\":{\"symbol\":\"AAA\"}}\n";
    std::fs::write(&keep, content).unwrap();

    let cfg = config(&dir, 200);
    let outcome = sweep(&cfg, date(TODAY), now(), &[]);

    assert_eq!(outcome.deleted, vec![date("2026-09-10")]);
    assert_eq!(
        std::fs::read(&keep).unwrap(),
        content,
        "a retained record must be byte-identical afterwards"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Discovery's directory is never in scope.
#[test]
fn retention_only_ever_touches_its_own_directory() {
    let root = temp_dir("scope");
    let research = root.join("research");
    let discovery = root.join("discovery-audit");
    std::fs::create_dir_all(&research).unwrap();
    std::fs::create_dir_all(&discovery).unwrap();
    session(&research, "2026-09-10", 100);
    // Discovery's own naming, which is not `.ndjson` and not date-suffixed the
    // same way — but the real guarantee is that it is a different directory.
    std::fs::write(discovery.join("2026-09-10-1-1-1.jsonl"), vec![b'x'; 10_000]).unwrap();

    let cfg = config(&research, 1);
    let outcome = sweep(&cfg, date(TODAY), now(), &[]);

    assert_eq!(outcome.deleted, vec![date("2026-09-10")]);
    assert!(
        discovery.join("2026-09-10-1-1-1.jsonl").exists(),
        "discovery capture must be untouched by research retention"
    );
    assert_eq!(
        outcome.dir_bytes_before, 400,
        "and discovery's bytes must not even be counted"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The default ceiling is the documented one, and it implies the documented
/// number of sessions.
#[test]
fn the_default_ceiling_retains_the_documented_number_of_sessions() {
    assert_eq!(DEFAULT_CEILING_BYTES, 64 * 1024 * 1024 * 1024);
    // ~9.13 GB of OI plus ~0.32 GB of episodes per regular session.
    let per_session = 9_800_000_000u64;
    let sessions = DEFAULT_CEILING_BYTES / per_session;
    assert_eq!(sessions, 7, "64 GiB retains seven full-fidelity sessions");
    assert_eq!(DEFAULT_MIN_AGE_DAYS, 2, "today and yesterday are always safe");
}
