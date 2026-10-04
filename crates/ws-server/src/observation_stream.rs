//! Bounded-memory streaming acquire / authenticate / reconcile / certify.
//!
//! `acquire` materialises every record of every file, which cannot scale to a
//! session: a design-point capture is ~13 GB of NDJSON. This module reaches the
//! same verdict reading each line once per pass and holding only:
//!
//! * per-file summaries (name, closure, counts, chain links) -- O(files);
//! * per-window summaries (declared/counted, flags) -- O(windows), about
//!   1,920 per session;
//! * the expected and persisted identity sets of the **currently open**
//!   window(s) only -- O(largest window), about 16,375 tuples at capacity;
//! * the distinct run ids seen (normally one).
//!
//! Receipt contiguity is checked incrementally. The writer is a single FIFO
//! thread, so receipts are persisted in sequence order; the whole-capture
//! reader sorted them first. Here an out-of-order receipt is a failure -- a
//! strictly stronger check, and one no genuine capture can trip.
//!
//! **Equivalence.** The precedence of every refusal mirrors `acquire` ->
//! `authenticate` -> `Certificate::issue`, so for any capture both readers give
//! the same verdict and the same certificate; the test suite asserts that on
//! every fixture shape.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use super::*;

/// What the streaming certifier held at its peak, so the bound is measured,
/// not asserted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamStats {
    pub bytes_read: u64,
    pub lines_read: u64,
    /// Largest number of identity tuples held at once (expected + persisted
    /// of open windows).
    pub peak_held_tuples: u64,
    /// Bytes of those tuples at that peak.
    pub peak_held_bytes: u64,
    pub windows_tracked: u64,
}

#[derive(Debug, Default)]
struct FileSummary {
    name: String,
    closed: bool,
    declared: Option<u64>,
    counted: u64,
    start: Option<(u32, String)>,
    next: Option<String>,
    has_run_start: bool,
    duplicate_start: bool,
    start_name_mismatch: Option<String>,
}

/// Visits every line of a file, in order.
fn for_each_line(path: &Path, mut f: impl FnMut(&[u8], bool) -> Result<(), String>) -> Result<(), AcquisitionError> {
    let file = File::open(path).map_err(AcquisitionError::Io)?;
    let mut reader = BufReader::with_capacity(1 << 20, file);
    let mut raw = Vec::with_capacity(4096);
    loop {
        raw.clear();
        let n = reader.read_until(b'\n', &mut raw).map_err(AcquisitionError::Io)?;
        if n == 0 {
            break;
        }
        let terminated = raw.last() == Some(&b'\n');
        let body = if terminated { &raw[..raw.len() - 1] } else { &raw[..] };
        f(body, terminated).map_err(|e| AcquisitionError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        if !terminated {
            break;
        }
    }
    Ok(())
}

/// Pass 1 over one file: closure and acquisition errors, with the same
/// precedence as `read_file`. Holds nothing per line.
fn scan_file(dir: &Path, name: &str, stats: &mut StreamStats) -> Result<FileSummary, AcquisitionError> {
    let mut s = FileSummary { name: name.to_string(), ..FileSummary::default() };
    let mut lines = 0u64;
    let mut close_index: Option<u64> = None;
    let mut duplicate_close = false;
    let mut first_malformed: Option<u64> = None;
    let mut unterminated = false;
    for_each_line(&dir.join(name), |body, terminated| {
        stats.bytes_read += body.len() as u64 + u64::from(terminated);
        if !terminated {
            unterminated = true;
            if body.is_empty() {
                return Ok(());
            }
        }
        let i = lines;
        lines += 1;
        match serde_json::from_slice::<ObservationRecord>(body) {
            Ok(ObservationRecord::FileClose { records_written, next_file, .. }) => {
                if close_index.is_some() {
                    duplicate_close = true;
                } else {
                    close_index = Some(i);
                    s.declared = Some(records_written);
                    s.next = next_file;
                }
            }
            Ok(ObservationRecord::FileStart { file_name, sequence, previous_file, .. }) => {
                if s.start.is_some() {
                    s.duplicate_start = true;
                }
                if file_name != name {
                    s.start_name_mismatch = Some(file_name);
                }
                s.start = Some((sequence, previous_file));
            }
            Ok(ObservationRecord::RunStart { .. }) => s.has_run_start = true,
            Ok(_) => {}
            Err(_) => {
                if first_malformed.is_none() {
                    first_malformed = Some(i + 1);
                }
            }
        }
        Ok(())
    })?;
    let Some(close_index) = close_index else {
        s.closed = false;
        s.counted = lines;
        return Ok(s);
    };
    if duplicate_close {
        return Err(AcquisitionError::DuplicateFileClose { file: name.to_string() });
    }
    if unterminated {
        return Err(AcquisitionError::TrailingPartialLine { file: name.to_string() });
    }
    if close_index + 1 != lines {
        return Err(AcquisitionError::RecordsAfterFileClose { file: name.to_string(), line: close_index as usize + 2 });
    }
    if let Some(line) = first_malformed {
        return Err(AcquisitionError::MalformedLine { file: name.to_string(), line: line as usize });
    }
    s.closed = true;
    s.counted = close_index;
    Ok(s)
}

/// The chain check of `verify_rotation_chain`, on summaries.
fn verify_chain(files: &[FileSummary]) -> Result<(), AuthenticationFailure> {
    let broken = |detail: String| AuthenticationFailure::BrokenRotationChain { detail };
    for f in files {
        if f.duplicate_start {
            return Err(broken(format!("{}: more than one opening record", f.name)));
        }
        if let Some(named) = &f.start_name_mismatch {
            return Err(broken(format!("{}: opening record names {named}", f.name)));
        }
    }
    let heads: Vec<&str> =
        files.iter().filter(|f| f.has_run_start && f.start.is_none()).map(|f| f.name.as_str()).collect();
    if heads.len() != 1 {
        return Err(broken(format!("expected exactly one head file, found {}", heads.len())));
    }
    if files[0].name != heads[0] {
        return Err(broken(format!("head {} is not the first acquired file", heads[0])));
    }
    for i in 0..files.len() {
        let f = &files[i];
        if i > 0 {
            let Some((sequence, previous)) = &f.start else {
                return Err(broken(format!("{}: rotated file has no opening record", f.name)));
            };
            if *sequence as usize != i {
                return Err(broken(format!("{}: opening sequence {sequence}, expected {i}", f.name)));
            }
            if *previous != files[i - 1].name {
                return Err(broken(format!(
                    "{}: names predecessor {previous}, acquired predecessor is {}",
                    f.name,
                    files[i - 1].name
                )));
            }
        }
        match (&f.next, files.get(i + 1)) {
            (Some(named), Some(actual)) if *named == actual.name => {}
            (Some(named), Some(actual)) => {
                return Err(broken(format!("{}: names successor {named}, acquired {}", f.name, actual.name)))
            }
            (Some(named), None) => return Err(broken(format!("{}: names successor {named}, which is missing", f.name))),
            (None, Some(actual)) => {
                return Err(broken(format!("{}: ends the run, but {} was acquired after it", f.name, actual.name)))
            }
            (None, None) => {}
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
struct WindowState {
    declared_entries: Option<u64>,
    declared_open_set: Option<u64>,
    cohort_truncated: bool,
    counted: u64,
    /// Held only while the window is open; freed at its close.
    expected: Option<Vec<String>>,
    persisted: Vec<String>,
    seen: BTreeSet<String>,
    eligible: Vec<String>,
    begun: bool,
    closed: bool,
    /// Final set comparison: None = not yet compared, Some(None) = equal,
    /// Some(Some(refusal)) = mismatch/missing begin.
    set_result: Option<Option<CertificateRefusal>>,
    invalid: Option<(bool, bool)>,
}

/// Streams, authenticates and certifies a run directory in bounded memory.
pub fn assess_streaming(run_dir: &Path) -> (CaptureVerdict, StreamStats) {
    let mut stats = StreamStats::default();
    // ---- file set ------------------------------------------------------------
    let mut names: Vec<String> = Vec::new();
    let entries = match std::fs::read_dir(run_dir) {
        Ok(e) => e,
        Err(e) => return (CaptureVerdict::Indeterminate(Indeterminate::NoEvidence { detail: AcquisitionError::Io(e).to_string() }), stats),
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(CLOSE_FAILED_SUFFIX) {
            return (CaptureVerdict::Fail(AcquisitionError::CloseFailed { marker: name }.to_string()), stats);
        }
        if name.ends_with(OBSERVATION_FILE_SUFFIX) && entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            names.push(name);
        }
    }
    names.sort_by_key(|name| (rotation_index(name), name.clone()));
    if names.is_empty() {
        let e = AcquisitionError::NoFiles { run_dir: run_dir.to_path_buf() };
        return (CaptureVerdict::Indeterminate(Indeterminate::NoEvidence { detail: e.to_string() }), stats);
    }
    // ---- pass 1: acquisition -------------------------------------------------
    let mut files = Vec::with_capacity(names.len());
    for name in &names {
        match scan_file(run_dir, name, &mut stats) {
            Ok(s) => files.push(s),
            Err(e @ AcquisitionError::Io(_)) => {
                return (CaptureVerdict::Indeterminate(Indeterminate::NoEvidence { detail: e.to_string() }), stats)
            }
            Err(e) => return (CaptureVerdict::Fail(e.to_string()), stats),
        }
    }
    // ---- authentication preconditions --------------------------------------
    let open: Vec<String> = files.iter().filter(|f| !f.closed).map(|f| f.name.clone()).collect();
    if !open.is_empty() {
        let e = AuthenticationFailure::OpenFilePresent { names: open };
        return (CaptureVerdict::Indeterminate(Indeterminate::EvidenceIncomplete { detail: e.to_string() }), stats);
    }
    for f in &files {
        if let Some(declared) = f.declared {
            if declared != f.counted {
                let e = AuthenticationFailure::FileRecordCountMismatch { file: f.name.clone(), declared, counted: f.counted };
                return (CaptureVerdict::Fail(e.to_string()), stats);
            }
        }
    }
    if let Err(e) = verify_chain(&files) {
        return (CaptureVerdict::Fail(e.to_string()), stats);
    }
    // ---- pass 2: authentication + certification, streamed -------------------
    let mut run_starts: Vec<String> = Vec::new();
    let mut identity: (Option<String>, Option<String>) = (None, None);
    let mut run_ends = 0usize;
    let mut ids: BTreeSet<String> = BTreeSet::new();
    let mut next_seq = 1u64;
    let mut seq_failure: Option<AuthenticationFailure> = None;
    let mut next_status_seq = 1u64;
    let mut status_seq_failure: Option<AuthenticationFailure> = None;
    let mut status_summary = StatusSummary::default();
    let mut upstream_skipped = 0u64;
    let mut counters = WriterCounters::default();
    let mut stopped: Option<StopReason> = None;
    let mut windows: BTreeMap<String, WindowState> = BTreeMap::new();
    let mut open_windows: BTreeSet<String> = BTreeSet::new();
    let mut held_tuples = 0u64;
    let mut held_bytes = 0u64;
    let mut first_duplicate: Option<CertificateRefusal> = None;
    let mut first_begin_or_lag: Option<CertificateRefusal> = None;
    let mut first_eligible_invalid: Option<CertificateRefusal> = None;
    let mut lag_pending = false;
    // certificate accumulators
    let (mut eligible, mut candidates, mut multiplicity, mut establishable, mut detector_confirmed, mut fresh) =
        (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    let mut by_reason: BTreeMap<String, u64> = BTreeMap::new();
    let mut negative_sources: BTreeMap<String, u64> = BTreeMap::new();

    let bump_peak = |t: u64, b: u64, stats: &mut StreamStats| {
        if t > stats.peak_held_tuples {
            stats.peak_held_tuples = t;
            stats.peak_held_bytes = b;
        }
    };

    for f in &files {
        let path = run_dir.join(&f.name);
        let result = for_each_line(&path, |body, _| {
            stats.lines_read += 1;
            let record: ObservationRecord = match serde_json::from_slice(body) {
                Ok(r) => r,
                Err(e) => return Err(e.to_string()),
            };
            ids.insert(record.run_id().to_string());
            match record {
                ObservationRecord::RunStart { protocol_version, implementation_sha, preregistration_sha256, .. } => {
                    if run_starts.is_empty() {
                        identity = (implementation_sha, preregistration_sha256);
                    }
                    run_starts.push(protocol_version);
                }
                ObservationRecord::Receipt { sequence, .. } => {
                    if seq_failure.is_none() {
                        if sequence != next_seq {
                            seq_failure = Some(AuthenticationFailure::SequenceGap { expected: next_seq, found: sequence });
                        }
                        next_seq = next_seq.saturating_add(1);
                    }
                }
                ObservationRecord::Lag { skipped, .. } => {
                    upstream_skipped = upstream_skipped.saturating_add(skipped);
                    lag_pending = true;
                }
                ObservationRecord::Status { status_sequence, .. } => {
                    if status_seq_failure.is_none() {
                        if status_sequence != next_status_seq {
                            status_seq_failure = Some(AuthenticationFailure::StatusSequenceGap {
                                expected: next_status_seq,
                                found: status_sequence,
                            });
                        }
                        next_status_seq = next_status_seq.saturating_add(1);
                    }
                }
                ObservationRecord::StatusStream { .. } => {}
                ObservationRecord::RunEnd { counters: c, stopped: st, status, .. } => {
                    run_ends += 1;
                    counters = c;
                    status_summary = status;
                    if stopped.is_none() {
                        stopped = st;
                    }
                }
                ObservationRecord::Stopped { reason, .. } => {
                    if stopped.is_none() {
                        stopped = Some(reason);
                    }
                }
                ObservationRecord::WindowBegin { window_id, mut expected, .. } => {
                    let w = windows.entry(window_id.clone()).or_default();
                    if w.begun {
                        if first_begin_or_lag.is_none() {
                            first_begin_or_lag = Some(CertificateRefusal::DuplicateWindowBegin { window_id });
                        }
                        return Ok(());
                    }
                    w.begun = true;
                    expected.sort();
                    held_tuples += expected.len() as u64;
                    held_bytes += expected.iter().map(|e| e.len() as u64).sum::<u64>();
                    w.expected = Some(expected);
                    open_windows.insert(window_id);
                    bump_peak(held_tuples, held_bytes, &mut stats);
                }
                ObservationRecord::Candidate { window_id, opportunity_id, eligibility, provenance, confirmation_receipts, .. } => {
                    let w = windows.entry(window_id.clone()).or_default();
                    w.counted += 1;
                    if !w.closed {
                        let tuple = canonical_candidate(&opportunity_id, &eligibility, provenance.as_ref());
                        held_tuples += 1;
                        held_bytes += tuple.len() as u64;
                        w.persisted.push(tuple);
                        if !w.seen.insert(opportunity_id.clone()) && first_duplicate.is_none() {
                            first_duplicate = Some(CertificateRefusal::DuplicateCandidate {
                                window_id: window_id.clone(),
                                opportunity_id: opportunity_id.clone(),
                            });
                        }
                        if eligibility.eligible {
                            w.eligible.push(opportunity_id.clone());
                        }
                        open_windows.insert(window_id.clone());
                        bump_peak(held_tuples, held_bytes, &mut stats);
                    }
                    // certificate statistics, exactly as `issue` computes them
                    let confirmed = confirmation_receipts >= 1;
                    if confirmed {
                        detector_confirmed += 1;
                    }
                    if let Some(p) = &provenance {
                        if confirmed {
                            establishable += 1;
                            let stale = eligibility.reasons.iter().any(|r| {
                                matches!(
                                    r,
                                    IneligibilityReason::MarketAgeExceeded
                                        | IneligibilityReason::ReceiptAgeExceeded
                                        | IneligibilityReason::NegativeMarketAge
                                        | IneligibilityReason::PriceReceivedAfterProcessingStart
                                )
                            });
                            if !stale {
                                fresh += 1;
                            }
                        }
                        if eligibility.reasons.contains(&IneligibilityReason::NegativeMarketAge) {
                            let key = if p.market_time_derived {
                                format!("{}+derived", p.source_event_type)
                            } else {
                                p.source_event_type.to_string()
                            };
                            *negative_sources.entry(key).or_insert(0) += 1;
                        }
                    }
                    candidates += 1;
                    if eligibility.eligible {
                        eligible += 1;
                    }
                    for reason in &eligibility.reasons {
                        if *reason == IneligibilityReason::ConfirmationMultiplicity {
                            multiplicity += 1;
                        }
                        *by_reason.entry(reason_key(*reason).to_string()).or_insert(0) += 1;
                    }
                }
                ObservationRecord::WindowClose {
                    window_id, entry_count, open_set_size, cohort_truncated, source_lag_invalid, mapping_ambiguous, ..
                } => {
                    if lag_pending && !source_lag_invalid && first_begin_or_lag.is_none() {
                        first_begin_or_lag = Some(CertificateRefusal::LagNotApplied { window_id: window_id.clone() });
                    }
                    lag_pending = false;
                    let w = windows.entry(window_id.clone()).or_default();
                    w.declared_entries = Some(entry_count);
                    w.declared_open_set = Some(open_set_size);
                    w.cohort_truncated = cohort_truncated;
                    if source_lag_invalid || mapping_ambiguous {
                        w.invalid = Some((source_lag_invalid, mapping_ambiguous));
                        if let Some(first) = w.eligible.first() {
                            if first_eligible_invalid.is_none() {
                                first_eligible_invalid = Some(CertificateRefusal::EligibleInInvalidWindow {
                                    window_id: window_id.clone(),
                                    opportunity_id: first.clone(),
                                });
                            }
                        }
                    }
                    // Compare and release this window's sets.
                    let mut persisted = std::mem::take(&mut w.persisted);
                    persisted.sort();
                    let released_tuples = persisted.len() as u64 + w.expected.as_ref().map(|e| e.len() as u64).unwrap_or(0);
                    let released_bytes = persisted.iter().map(|p| p.len() as u64).sum::<u64>()
                        + w.expected.as_ref().map(|e| e.iter().map(|x| x.len() as u64).sum::<u64>()).unwrap_or(0);
                    w.set_result = Some(match w.expected.take() {
                        None => Some(CertificateRefusal::MissingWindowBegin { window_id: window_id.clone() }),
                        Some(expected) if expected == persisted => None,
                        Some(expected) => {
                            let e: BTreeSet<&String> = expected.iter().collect();
                            let p: BTreeSet<&String> = persisted.iter().collect();
                            Some(CertificateRefusal::WindowSetMismatch {
                                window_id: window_id.clone(),
                                expected: expected.len() as u64,
                                persisted: persisted.len() as u64,
                                differing: e.symmetric_difference(&p).count() as u64,
                            })
                        }
                    });
                    w.eligible.clear();
                    // Freed at close, like the sets: a late row for a closed
                    // window already fails its declared-count check, which
                    // `issue` reports before any duplicate. Keeping this set
                    // would make memory grow with the capture, not the window.
                    w.seen = BTreeSet::new();
                    w.closed = true;
                    held_tuples = held_tuples.saturating_sub(released_tuples);
                    held_bytes = held_bytes.saturating_sub(released_bytes);
                    open_windows.remove(&window_id);
                }
                ObservationRecord::FileStart { .. } | ObservationRecord::FileClose { .. } => {}
            }
            Ok(())
        });
        if let Err(e) = result {
            return (CaptureVerdict::Fail(e.to_string()), stats);
        }
    }
    stats.windows_tracked = windows.len() as u64;

    // ---- authentication verdict (same order as `authenticate`) --------------
    if run_starts.is_empty() {
        return (CaptureVerdict::Indeterminate(Indeterminate::EvidenceIncomplete { detail: AuthenticationFailure::MissingRunStart.to_string() }), stats);
    }
    if run_starts.len() > 1 {
        return (CaptureVerdict::Fail(AuthenticationFailure::MultipleRunStart { count: run_starts.len() }.to_string()), stats);
    }
    if run_starts[0] != PROTOCOL_VERSION {
        return (CaptureVerdict::Fail(AuthenticationFailure::ProtocolMismatch { found: run_starts[0].clone(), expected: PROTOCOL_VERSION }.to_string()), stats);
    }
    if ids.len() > 1 {
        return (CaptureVerdict::Fail(AuthenticationFailure::MixedRunIds { found: ids.into_iter().collect() }.to_string()), stats);
    }
    match run_ends {
        0 => return (CaptureVerdict::Indeterminate(Indeterminate::EvidenceIncomplete { detail: AuthenticationFailure::MissingRunEnd.to_string() }), stats),
        1 => {}
        n => return (CaptureVerdict::Fail(AuthenticationFailure::MultipleRunEnd { count: n }.to_string()), stats),
    }
    if let Some(e) = seq_failure {
        return (CaptureVerdict::Fail(e.to_string()), stats);
    }
    if let Some(e) = status_seq_failure {
        return (CaptureVerdict::Fail(e.to_string()), stats);
    }

    // ---- certificate verdict (same order as `Certificate::issue`) -----------
    let refuse = |r: CertificateRefusal| match r {
        CertificateRefusal::NoWindows => CaptureVerdict::Indeterminate(Indeterminate::NoWindows),
        other => CaptureVerdict::Fail(other.to_string()),
    };
    if counters.overflowed {
        return (refuse(CertificateRefusal::CounterOverflow { counters }), stats);
    }
    if !counters.identity_holds() {
        return (refuse(CertificateRefusal::CountersIdentityViolated { counters }), stats);
    }
    if counters.dropped > 0 || counters.write_errors > 0 {
        return (refuse(CertificateRefusal::RecordsLost { dropped: counters.dropped, write_errors: counters.write_errors }), stats);
    }
    if let Some(reason) = stopped {
        return (refuse(CertificateRefusal::CaptureStopped { reason }), stats);
    }
    // Include declared windows even when all rows and the close are missing.
    // Otherwise an incomplete window could disappear from certification.
    let real: Vec<(&String, &WindowState)> =
        windows.iter().filter(|(_, w)| w.begun || w.counted > 0 || w.declared_entries.is_some()).collect();
    if real.is_empty() {
        return (refuse(CertificateRefusal::NoWindows), stats);
    }
    for (id, w) in &real {
        match w.declared_entries {
            None => return (refuse(CertificateRefusal::WindowWithoutClose { window_id: (*id).clone() }), stats),
            Some(d) if d != w.counted => {
                return (refuse(CertificateRefusal::WindowEntryCountMismatch { window_id: (*id).clone(), declared: d, counted: w.counted }), stats)
            }
            Some(_) => {}
        }
        if w.cohort_truncated {
            return (refuse(CertificateRefusal::CohortTruncated { window_id: (*id).clone() }), stats);
        }
        if let Some(open) = w.declared_open_set {
            if open != w.counted {
                return (refuse(CertificateRefusal::OpenSetMismatch { window_id: (*id).clone(), declared: open, counted: w.counted }), stats);
            }
        }
    }
    if let Some(r) = first_duplicate {
        return (refuse(r), stats);
    }
    if let Some(r) = first_begin_or_lag {
        return (refuse(r), stats);
    }
    for (_, w) in &real {
        if let Some(Some(r)) = &w.set_result {
            return (refuse(clone_refusal(r)), stats);
        }
    }
    if let Some(r) = first_eligible_invalid {
        return (refuse(r), stats);
    }
    let mut invalid_by_reason: BTreeMap<String, u64> = BTreeMap::new();
    let mut invalid_windows = 0u64;
    for (_, w) in &real {
        if let Some((lag, ambiguous)) = w.invalid {
            invalid_windows += 1;
            if lag {
                *invalid_by_reason.entry("source_lag".to_string()).or_insert(0) += 1;
            }
            if ambiguous {
                *invalid_by_reason.entry("mapping_ambiguous".to_string()).or_insert(0) += 1;
            }
        }
    }
    let rate = |num: u64, den: u64| if den == 0 { f64::NAN } else { num as f64 / den as f64 };
    let run_id = ids.into_iter().next().unwrap_or_default();
    let cert = Certificate {
        run_id,
        protocol_version: PROTOCOL_VERSION.to_string(),
        anchor: "ranking_completion".to_string(),
        freshness_max_age_secs: FRESHNESS_MAX_AGE_SECS,
        windows: real.len() as u64,
        candidates,
        eligible,
        excluded_multiplicity: multiplicity,
        ineligible_by_reason: by_reason,
        detector_confirmed,
        provenance_establishable: establishable,
        fresh,
        eligibility_rate: rate(eligible, establishable),
        provenance_establishment_rate: rate(establishable, detector_confirmed),
        freshness_eligibility_rate: rate(fresh, establishable),
        negative_market_age_sources: negative_sources,
        upstream_skipped_events: upstream_skipped,
        invalid_windows,
        invalid_windows_by_reason: invalid_by_reason,
        byte_integrity_established: false,
        crash_durability_established: false,
        implementation_sha: identity.0,
        preregistration_sha256: identity.1,
        status: status_summary,
    };
    (CaptureVerdict::Pass(Box::new(cert)), stats)
}

fn clone_refusal(r: &CertificateRefusal) -> CertificateRefusal {
    match r {
        CertificateRefusal::MissingWindowBegin { window_id } => CertificateRefusal::MissingWindowBegin { window_id: window_id.clone() },
        CertificateRefusal::WindowSetMismatch { window_id, expected, persisted, differing } => CertificateRefusal::WindowSetMismatch {
            window_id: window_id.clone(),
            expected: *expected,
            persisted: *persisted,
            differing: *differing,
        },
        other => CertificateRefusal::MissingWindowBegin { window_id: format!("{other:?}") },
    }
}

/// `assess_streaming`, then identity binding.
pub fn assess_streaming_bound(run_dir: &Path, expected: &BoundIdentity) -> (CaptureVerdict, StreamStats) {
    let (v, s) = assess_streaming(run_dir);
    (bind_verdict(v, expected), s)
}

/// Streams every record of a run in chain order, one line at a time.
///
/// For offline analysis **after** the run has certified: it re-reads the
/// files; it does not re-authenticate them. Memory is one line.
pub fn for_each_record(run_dir: &Path, mut f: impl FnMut(ObservationRecord)) -> Result<(), String> {
    let mut names: Vec<String> = std::fs::read_dir(run_dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(OBSERVATION_FILE_SUFFIX))
        .collect();
    names.sort_by_key(|name| (rotation_index(name), name.clone()));
    for name in names {
        for_each_line(&run_dir.join(&name), |body, _| {
            let record: ObservationRecord = serde_json::from_slice(body).map_err(|e| e.to_string())?;
            f(record);
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}
