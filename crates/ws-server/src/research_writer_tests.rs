//! Tests for the shared bounded research writer.
//!
//! The load-shaped tests here are deliberately about the *writer*, not about
//! either capture that uses it: if the queue bound, the batching or the
//! accounting is wrong, it is wrong for both, and proving it once against a
//! synthetic record is stronger than proving it twice against two production
//! record types that share no code.

use super::*;

use std::sync::atomic::AtomicUsize;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct Row {
    n: u64,
    payload: String,
}

fn row(n: u64) -> Row {
    Row { n, payload: "x".repeat(200) }
}

fn date() -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(2026, 9, 16).unwrap()
}

fn temp_dir(tag: &str) -> PathBuf {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "research-writer-{tag}-{}-{}-{n}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn naming(dir: &std::path::Path) -> Naming {
    Naming { dir: dir.to_path_buf(), stem: "cap".to_string() }
}

fn wide() -> Bounds {
    Bounds { records: 4_096, bytes: 64 * 1024 * 1024 }
}

/// Reads back every data line, in file order.
fn data_lines(dir: &std::path::Path) -> Vec<Row> {
    let path = naming(dir).data(date());
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn markers(dir: &std::path::Path) -> Vec<CaptureMarker> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.contains("-markers-") {
            continue;
        }
        let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
        for line in text.lines() {
            out.push(serde_json::from_str(line).unwrap());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Ordering and content -- section 6: batching is storage, never semantics
// ---------------------------------------------------------------------------

/// Batching must not reorder, merge, drop or rewrite anything. A batch may hold
/// N serialized records, but the file must still be exactly those N records in
/// exactly that order.
#[test]
fn batching_preserves_order_content_and_count() {
    let dir = temp_dir("order");
    let writer = ResearchWriter::start_inner(naming(&dir), wide(), None).unwrap();
    // Deliberately more than one BATCH, so several drains are involved and a
    // per-batch reordering would show.
    let expected: Vec<Row> = (0..(BATCH as u64 * 3 + 7)).map(row).collect();
    for r in &expected {
        writer.record(r, date());
    }
    writer.flush(std::time::Duration::from_secs(10));

    let got = data_lines(&dir);
    assert_eq!(got.len(), expected.len(), "every accepted record must reach disk exactly once");
    assert_eq!(got, expected, "batching must preserve order and content exactly");

    let h = writer.health();
    assert_eq!(h.attempted.load(Ordering::Relaxed), expected.len() as u64);
    assert_eq!(h.written.load(Ordering::Relaxed), expected.len() as u64);
    assert_eq!(h.dropped.load(Ordering::Relaxed), 0);
    assert_eq!(h.write_errors.load(Ordering::Relaxed), 0);
    assert!(
        h.batches_written.load(Ordering::Relaxed) >= 1
            && h.batches_written.load(Ordering::Relaxed) < expected.len() as u64,
        "records must actually have been batched, not written one batch each"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// What reaches disk is one valid NDJSON object per line, LF-terminated.
#[test]
fn records_are_one_parseable_lf_terminated_line_each() {
    let dir = temp_dir("ndjson");
    let writer = ResearchWriter::start_inner(naming(&dir), wide(), None).unwrap();
    for n in 0..64 {
        writer.record(&row(n), date());
    }
    writer.flush(std::time::Duration::from_secs(10));

    let text = std::fs::read_to_string(naming(&dir).data(date())).unwrap();
    // A CRLF here would make the file unverifiable by `sha256sum -c`
    // downstream, which has already cost one artifact.
    assert!(!text.contains('\r'), "records must be LF-terminated");
    assert!(text.ends_with('\n'), "the final record must be terminated too");
    assert_eq!(text.lines().count(), 64);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Queue bounds
// ---------------------------------------------------------------------------

/// A full queue drops and counts. It never blocks the caller.
///
/// The writer is held at a barrier so the bound is actually reachable -- see
/// `start_inner`'s comment on why a drop counter no test can reach is worth
/// nothing.
#[test]
fn a_full_queue_drops_and_counts_without_blocking() {
    let dir = temp_dir("full");
    let gate = Arc::new(std::sync::Barrier::new(2));
    let writer = ResearchWriter::start_inner(
        naming(&dir),
        Bounds { records: 8, bytes: u64::MAX / 2 },
        Some(gate.clone()),
    )
    .unwrap();

    let started = std::time::Instant::now();
    for n in 0..512 {
        writer.record(&row(n), date());
    }
    let elapsed = started.elapsed();

    let h = writer.health();
    assert_eq!(h.attempted.load(Ordering::Relaxed), 512);
    assert!(h.dropped.load(Ordering::Relaxed) > 0, "a saturated queue must drop and count");
    assert!(h.is_degraded(), "drops must mark the capture degraded");
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "record() must never block on the writer (took {elapsed:?})"
    );
    assert!(
        h.queue_peak.load(Ordering::Relaxed) <= 8 + 1,
        "queue occupancy must stay inside the declared bound"
    );

    gate.wait();
    writer.flush(std::time::Duration::from_secs(10));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The byte bound binds independently of the record bound.
///
/// This is the discovery lesson generalised: a queue bounded only in records is
/// bounded in an unknown quantity, because record size is not a constant. Here
/// the record bound is far out of reach and the byte bound is what must hold.
#[test]
fn the_byte_bound_binds_independently_of_the_record_bound() {
    let dir = temp_dir("bytes");
    let gate = Arc::new(std::sync::Barrier::new(2));
    let writer = ResearchWriter::start_inner(
        naming(&dir),
        Bounds { records: 100_000, bytes: 4_096 },
        Some(gate.clone()),
    )
    .unwrap();

    for n in 0..512 {
        writer.record(&row(n), date());
    }
    let h = writer.health();
    assert!(
        h.dropped.load(Ordering::Relaxed) > 0,
        "the byte bound must bind even with records to spare"
    );
    assert!(
        h.queued_bytes_peak.load(Ordering::Relaxed) <= 4_096 + 300,
        "queued bytes must stay inside the declared byte bound (peak {})",
        h.queued_bytes_peak.load(Ordering::Relaxed)
    );
    assert!(
        h.queue_depth.load(Ordering::Relaxed) < 100_000,
        "the record bound must not have been what stopped it"
    );

    gate.wait();
    writer.flush(std::time::Duration::from_secs(10));
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// In-band loss semantics -- section 5 / section 17
// ---------------------------------------------------------------------------

/// A loss span is described *in the capture*, not only in the process log.
///
/// This is the property September 16 lacked: `dropped` existed, but the only
/// record of it was a power-of-two log line, so the artifact could not report
/// its own incompleteness and the true total was recoverable only as a range.
#[test]
fn a_loss_span_is_described_in_the_capture_itself() {
    let dir = temp_dir("lossspan");
    let gate = Arc::new(std::sync::Barrier::new(2));
    let writer = ResearchWriter::start_inner(
        naming(&dir),
        Bounds { records: 4, bytes: u64::MAX / 2 },
        Some(gate.clone()),
    )
    .unwrap();

    for n in 0..256 {
        writer.record(&row(n), date());
    }
    let dropped = writer.health().dropped.load(Ordering::Relaxed);
    assert!(dropped > 0);

    // Let the writer drain, then offer one more record: the pending span rides
    // out with it, exactly as discovery's does.
    gate.wait();
    writer.flush(std::time::Duration::from_secs(10));
    writer.record(&row(9_999), date());
    writer.flush(std::time::Duration::from_secs(10));

    let spans: Vec<CaptureMarker> =
        markers(&dir).into_iter().filter(|m| m.kind == "queue_loss").collect();
    assert!(!spans.is_empty(), "the capture must carry at least one queue_loss marker");
    let total: u64 = spans.iter().filter_map(|m| m.queue_loss.as_ref()).map(|s| s.lost).sum();
    assert_eq!(
        total, dropped,
        "the markers must account for every dropped record, not a sample of them"
    );
    let first = spans[0].queue_loss.as_ref().unwrap();
    assert!(first.onset.is_some(), "a span must name when it started");
    assert_eq!(first.reason, "queue_full");
    assert_eq!(
        writer.health().loss_spans.load(Ordering::Relaxed),
        1,
        "one contiguous burst is one span, not one span per record"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Markers never inflate the data-loss figure. Losing a description is not
/// losing evidence, and conflating the two would make `dropped` unusable as a
/// completeness gate.
#[test]
fn marker_pressure_is_never_counted_as_data_loss() {
    let dir = temp_dir("markerloss");
    let gate = Arc::new(std::sync::Barrier::new(2));
    let writer = ResearchWriter::start_inner(
        naming(&dir),
        Bounds { records: 1, bytes: u64::MAX / 2 },
        Some(gate.clone()),
    )
    .unwrap();

    let before = writer.health().dropped.load(Ordering::Relaxed);
    for _ in 0..64 {
        writer.marker("synthetic", None);
    }
    assert_eq!(
        writer.health().dropped.load(Ordering::Relaxed),
        before,
        "rejected markers must not be counted as dropped records"
    );
    assert_eq!(writer.health().attempted.load(Ordering::Relaxed), 0, "markers are not records");

    gate.wait();
    writer.flush(std::time::Duration::from_secs(10));
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Failure behaviour
// ---------------------------------------------------------------------------

/// An unusable directory disables capture. It must not panic, and must not be
/// reported as anything other than off.
#[test]
fn an_unusable_directory_disables_capture_rather_than_failing() {
    let dir = temp_dir("blocked");
    let occupied = dir.join("not-a-directory");
    std::fs::write(&occupied, b"x").unwrap();
    let naming = Naming { dir: occupied.join("inner"), stem: "cap".to_string() };
    assert!(
        ResearchWriter::start_inner(naming, wide(), None).is_none(),
        "capture must degrade to off, not to a panic"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A write failure is counted, never propagated, and never reported as a
/// successful write.
#[test]
fn write_failures_are_counted_and_never_propagate() {
    let dir = temp_dir("writefail");
    // Occupying the exact filename with a directory is a target no
    // `OpenOptions::append` can open on any platform.
    std::fs::create_dir_all(naming(&dir).data(date())).unwrap();
    let writer = ResearchWriter::start_inner(naming(&dir), wide(), None).unwrap();
    for n in 0..32 {
        writer.record(&row(n), date());
    }
    writer.flush(std::time::Duration::from_secs(10));

    let h = writer.health();
    assert!(h.write_errors.load(Ordering::Relaxed) > 0, "an unwritable target must be counted");
    assert_eq!(h.written.load(Ordering::Relaxed), 0, "nothing must be reported as written");
    assert_eq!(h.attempted.load(Ordering::Relaxed), 32, "attempts are still attempts");
    assert!(h.is_degraded());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Health reports enough to decide completeness without reading the files:
/// attempted, written, queue peak, bytes, batches, last write, current file.
#[test]
fn health_reports_the_full_writer_surface() {
    let dir = temp_dir("surface");
    let writer = ResearchWriter::start_inner(naming(&dir), wide(), None).unwrap();
    for n in 0..100 {
        writer.record(&row(n), date());
    }
    writer.flush(std::time::Duration::from_secs(10));

    let s = writer.health().snapshot();
    assert_eq!(s.attempted, 100);
    assert_eq!(s.written, 100);
    assert_eq!(s.dropped, 0);
    assert_eq!(s.write_errors, 0);
    assert!(s.bytes_written > 0);
    assert!(s.batches_written > 0);
    assert!(s.last_write.is_some(), "a successful write must stamp a time");
    assert!(s.current_file.ends_with(".ndjson"), "current file: {}", s.current_file);
    assert!(s.current_file_bytes > 0);
    assert_eq!(s.queue_capacity, 4_096);
    assert_eq!(s.queue_capacity_bytes, 64 * 1024 * 1024);
    assert_eq!(s.queue_depth, 0, "a drained queue must read as empty");
    assert!(!s.degraded);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Recovery: once pressure falls the writer resumes cleanly and loses nothing
/// further.
#[test]
fn the_writer_recovers_after_pressure_falls() {
    let dir = temp_dir("recover");
    let gate = Arc::new(std::sync::Barrier::new(2));
    let writer = ResearchWriter::start_inner(
        naming(&dir),
        Bounds { records: 8, bytes: u64::MAX / 2 },
        Some(gate.clone()),
    )
    .unwrap();

    for n in 0..512 {
        writer.record(&row(n), date());
    }
    let dropped_under_pressure = writer.health().dropped.load(Ordering::Relaxed);
    assert!(dropped_under_pressure > 0);

    gate.wait();
    writer.flush(std::time::Duration::from_secs(10));

    // Post-recovery traffic, well inside the bound.
    for n in 10_000..10_004 {
        writer.record(&row(n), date());
        writer.flush(std::time::Duration::from_secs(10));
    }
    assert_eq!(
        writer.health().dropped.load(Ordering::Relaxed),
        dropped_under_pressure,
        "nothing further may be lost once pressure has fallen"
    );
    let tail = data_lines(&dir);
    assert!(
        tail.iter().any(|r| r.n == 10_003),
        "post-recovery records must reach disk"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
