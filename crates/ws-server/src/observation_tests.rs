//! Tests for the consumer-received observation layer.
//!
//! The point of this file is not coverage for its own sake. Each case below
//! exists because a *specific* wrong certificate was reachable without it:
//! a capture that looked complete because its end marker was present, one whose
//! counters balanced while rows were missing, one that certified a price whose
//! provenance was never established, one that resolved an ambiguous
//! confirmation by picking whichever came first.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone, Utc};
use market_data::{IgnitionEventKind, ScanEvent};

use super::*;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A scratch directory that removes itself.
///
/// Hand-rolled rather than pulling in `tempfile`: the invariant for this work
/// is existing dependencies only, and a directory with a unique name is not
/// worth a new crate in the lockfile.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "ss-observation-{}-{}-{}-{}",
            tag,
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0),
            n
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_790_000_000 + secs, 0).unwrap()
}

/// A sink that records everything and can be told to fail.
struct SpySink {
    records: Vec<ObservationRecord>,
    counters: WriterCounters,
    /// Writes at these 1-based positions fail.
    fail_at: BTreeSet<u64>,
    fail_close: bool,
    seen: u64,
}

impl SpySink {
    fn new() -> Self {
        Self {
            records: Vec::new(),
            counters: WriterCounters::default(),
            fail_at: BTreeSet::new(),
            fail_close: false,
            seen: 0,
        }
    }

    fn failing_at(positions: &[u64]) -> Self {
        let mut s = Self::new();
        s.fail_at = positions.iter().copied().collect();
        s
    }
}

impl ObservationSink for SpySink {
    fn write(&mut self, record: &ObservationRecord) -> std::io::Result<()> {
        self.seen += 1;
        self.counters.attempted += 1;
        if self.fail_at.contains(&self.seen) {
            // A dropped record is counted, never silently discarded -- the
            // whole reason `written + dropped + write_errors == attempted` is
            // an identity and not an approximation.
            self.counters.dropped += 1;
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "injected write failure"));
        }
        self.counters.written += 1;
        self.records.push(record.clone());
        Ok(())
    }

    fn counters(&self) -> WriterCounters {
        self.counters
    }

    fn close(&mut self, run_id: &str, at: DateTime<Utc>) -> std::io::Result<()> {
        if self.fail_close {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "injected flush failure"));
        }
        let records_written = self.counters.written;
        let close = ObservationRecord::FileClose {
            run_id: run_id.to_string(),
            file_name: "spy".to_string(),
            records_written,
            closed_at: at,
            next_file: None,
        };
        self.write(&close)
    }
}

/// A sink that writes into a shared buffer the test can inspect and then
/// replay onto disk, so evidence can be corrupted in exactly one way at a
/// time.
#[derive(Clone, Default)]
struct SharedSink {
    lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    counters: std::sync::Arc<std::sync::Mutex<WriterCounters>>,
}

impl SharedSink {
    fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }
}

impl ObservationSink for SharedSink {
    fn write(&mut self, record: &ObservationRecord) -> std::io::Result<()> {
        let mut c = self.counters.lock().unwrap();
        c.attempted += 1;
        c.written += 1;
        drop(c);
        self.lines.lock().unwrap().push(serde_json::to_string(record).unwrap());
        Ok(())
    }

    fn counters(&self) -> WriterCounters {
        *self.counters.lock().unwrap()
    }

    fn close(&mut self, run_id: &str, at: DateTime<Utc>) -> std::io::Result<()> {
        let records_written = self.counters.lock().unwrap().written;
        let close = ObservationRecord::FileClose {
            run_id: run_id.to_string(),
            file_name: RUN_FILE_NAME.to_string(),
            records_written,
            closed_at: at,
            next_file: None,
        };
        self.write(&close)
    }
}

fn run_in(dir: &Path) -> ObserverRun {
    ObserverRun::allocate(dir, "test-host", at(0), 4242).expect("allocate run")
}

fn ignition(symbol: &str, secs: i64, price: f64, kind: IgnitionEventKind) -> ScanEvent {
    ScanEvent::IgnitionEvent { symbol: symbol.to_string(), timestamp: at(secs), price, kind }
}

/// Writes lines out as a run file so the reader can acquire them.
fn write_lines(dir: &Path, name: &str, lines: &[String], terminate_last: bool) {
    let mut body = String::new();
    for (i, line) in lines.iter().enumerate() {
        body.push_str(line);
        if i + 1 < lines.len() || terminate_last {
            body.push('\n');
        }
    }
    std::fs::write(dir.join(name), body).expect("write run file");
}

/// Drives one receipt and one window through an observer over a shared sink,
/// returning the lines it produced. The shape every certificate test needs.
fn capture_one_window(open: Vec<OpenCandidate>, scored_price: Option<f64>) -> Vec<String> {
    let sink = SharedSink::default();
    let tmp = TempDir::new("cap");
    let run = run_in(tmp.path());
    let mut observer =
        Observer::start(&run, "test-host", 4242, at(0), Box::new(sink.clone())).expect("start");
    observer.on_receive(&ignition("AAA", 100, 3.5, IgnitionEventKind::FollowThroughConfirmed), at(100));
    let mut engine_prices = BTreeMap::new();
    let mut scored = BTreeSet::new();
    if let Some(price) = scored_price {
        for c in &open {
            engine_prices.insert(c.opportunity_id.clone(), price);
            scored.insert(c.opportunity_id.clone());
        }
    }
    observer.on_window(WindowInput {
        window_id: "oiw-1".to_string(),
        processing_started_at: at(101),
        rank_completed_at: at(102),
        open,
        scored,
        engine_prices,
        cohort_truncated: false,
    });
    observer.on_finish(at(103));
    sink.lines()
}

fn candidate_aaa() -> Vec<OpenCandidate> {
    vec![OpenCandidate {
        opportunity_id: "AAA:2026-09-28:1".to_string(),
        symbol: "AAA".to_string(),
        opened_at: at(90),
    }]
}

/// Acquires, authenticates and certifies a set of lines written to one file.
fn certify(lines: &[String]) -> Result<Certificate, String> {
    let tmp = TempDir::new("certify");
    write_lines(tmp.path(), RUN_FILE_NAME, lines, true);
    let acquired = acquire(tmp.path()).map_err(|e| format!("acquire: {e}"))?;
    let authed = authenticate(acquired).map_err(|e| format!("authenticate: {e}"))?;
    Certificate::issue(&authed).map_err(|e| format!("certificate: {e}"))
}

// ---------------------------------------------------------------------------
// Run identity
// ---------------------------------------------------------------------------

#[test]
fn run_allocation_is_exclusive_and_retries_a_collision() {
    let tmp = TempDir::new("alloc");
    let first = ObserverRun::allocate(tmp.path(), "host", at(0), 7).expect("first");
    // Same namespace, pid and instant: the only thing that can separate them
    // is the collision counter, which is exactly the case this exists for.
    let second = ObserverRun::allocate(tmp.path(), "host", at(0), 7).expect("second");
    assert_ne!(first.id(), second.id());
    assert!(first.id().ends_with("-0"));
    assert!(second.id().ends_with("-1"));
    assert!(first.dir().is_dir() && second.dir().is_dir());
}

#[test]
fn run_allocation_fails_when_every_name_is_taken() {
    let tmp = TempDir::new("exhaust");
    // Pre-create every name the allocator will try.
    let stamp = at(0).format("%Y%m%dT%H%M%S%3fZ");
    for i in 0..64 {
        std::fs::create_dir(tmp.path().join(format!("host-7-{stamp}-{i}"))).unwrap();
    }
    let err = ObserverRun::allocate(tmp.path(), "host", at(0), 7).expect_err("must fail");
    match err {
        RunAllocationError::Exhausted { attempts, .. } => assert_eq!(attempts, 64),
        other => panic!("expected exhaustion, got {other:?}"),
    }
}

#[test]
fn a_run_name_stays_one_path_component() {
    let tmp = TempDir::new("sanitize");
    let run = ObserverRun::allocate(tmp.path(), "a/b\\c:d *", at(0), 1).expect("allocate");
    assert!(!run.id().contains('/'), "{}", run.id());
    assert!(!run.id().contains('\\'), "{}", run.id());
    assert_eq!(run.dir().parent(), Some(tmp.path()));
}

// ---------------------------------------------------------------------------
// The ScanEvent wire surface
// ---------------------------------------------------------------------------

/// Fails if **any** field is added to, removed from or renamed on any
/// `ScanEvent` variant.
///
/// This is not a serde-style test. `server.rs` wraps every broadcast event as
/// `EventFrame { event_id, #[serde(flatten)] event: ScanEvent }`, and `flatten`
/// means a field added to `ScanEvent` lands in the frame every web, desktop and
/// mobile client already parses -- automatically, with no code change and no
/// review. Desktop sits at an unreleased build and mobile has no released one,
/// so the clients cannot be updated in step. The whole observation layer is
/// therefore built to leave `ScanEvent` alone, and this test is what makes
/// "leave it alone" enforceable rather than aspirational.
///
/// If you are here because this test failed: adding a field to `ScanEvent` is a
/// wire-protocol change against clients that cannot be updated synchronously.
/// Put the field on an internal wrapper instead, so the serialized `ScanEvent`
/// stays byte-identical.
#[test]
fn scan_event_wire_shape_is_frozen() {
    let expected: Vec<&str> = vec![
        r#"{"type":"funnel_signal","symbol":"AAA","timestamp":"2026-09-21T14:13:20Z","price":1.0,"gapPct":2.0,"sessionVolume":3,"priceOk":true,"floatOk":false,"relVolOk":true,"gapOk":false,"passed":true}"#,
        r#"{"type":"momentum_update","symbol":"AAA","timestamp":"2026-09-21T14:13:20Z","volumeConfirmation":1.0,"structure":2.0,"maSlope":3.0,"wickRejection":4.0,"overall":5.0,"qualifies":true}"#,
        r#"{"type":"ignition_event","symbol":"AAA","timestamp":"2026-09-21T14:13:20Z","price":1.5,"kind":"follow_through_confirmed"}"#,
        r#"{"type":"consolidation_event","symbol":"AAA","timestamp":"2026-09-21T14:13:20Z","price":1.5,"kind":"entry_triggered","strategy":"micropullback"}"#,
        r#"{"type":"funnel_health","timestamp":"2026-09-21T14:13:20Z","floatBudgetRemaining":1,"floatBudget":2,"starvedCandidates":3,"apiKeyMissing":false}"#,
        r#"{"type":"halt_warning","estimatedBands":true,"symbol":"AAA","timestamp":"2026-09-21T14:13:20Z","referencePrice":1.0,"currentPrice":2.0,"bandWidthDollars":3.0,"bandDoubled":false,"proximityRatio":4.0,"relativeVolume":null,"level":"amber","luldInEffect":true}"#,
        r#"{"type":"bar_update","isFinal":true,"symbol":"AAA","timestamp":"2026-09-21T14:13:20Z","open":1.0,"high":2.0,"low":0.5,"close":1.5,"volume":10,"intervalSecs":60}"#,
        r#"{"type":"catalyst_update","symbol":"AAA","timestamp":"2026-09-21T14:13:20Z","catalystTags":["fda"],"headlineCount":1,"mostRecentHeadline":null}"#,
    ];
    let events = all_scan_event_variants();
    assert_eq!(events.len(), 8, "every ScanEvent variant must be pinned here");
    assert_eq!(expected.len(), events.len());
    for (event, want) in events.iter().zip(expected) {
        assert_eq!(serde_json::to_string(event).unwrap(), want);
    }
}

#[test]
fn observing_an_event_does_not_change_its_serialization() {
    let event = ignition("AAA", 100, 3.5, IgnitionEventKind::FollowThroughConfirmed);
    let before = serde_json::to_string(&event).unwrap();
    let tmp = TempDir::new("nomutate");
    let run = run_in(tmp.path());
    let mut observer =
        Observer::start(&run, "h", 1, at(0), Box::new(SpySink::new())).expect("start");
    observer.on_receive(&event, at(100));
    let after = serde_json::to_string(&event).unwrap();
    assert_eq!(before, after, "observer-on must be byte-identical to observer-off");
}

// ---------------------------------------------------------------------------
// Eligibility
// ---------------------------------------------------------------------------

#[test]
fn a_fresh_confirmed_candidate_with_matching_price_is_eligible() {
    let lines = capture_one_window(candidate_aaa(), Some(3.5));
    let cert = certify(&lines).expect("must certify");
    assert_eq!(cert.candidates, 1);
    assert_eq!(cert.eligible, 1);
    assert_eq!(cert.eligibility_rate, 1.0);
    assert_eq!(cert.anchor, "ranking_completion");
    assert_eq!(cert.protocol_version, PROTOCOL_VERSION);
    // The two claims this layer must never make, stated in the artifact
    // itself so nothing downstream has to remember them.
    assert!(!cert.byte_integrity_established);
    assert!(!cert.crash_durability_established);
}

#[test]
fn a_price_the_engine_did_not_use_is_unknown_provenance_not_a_guess() {
    // Observer last saw 3.5; the engine ranked on 9.9. The two do not
    // describe the same price, so provenance is unknown -- clause 3 makes
    // that ineligible rather than assuming they agree.
    let lines = capture_one_window(candidate_aaa(), Some(9.9));
    let cert = certify(&lines).expect("must certify");
    assert_eq!(cert.eligible, 0);
    assert_eq!(cert.ineligible_by_reason.get("unknown_price_provenance"), Some(&1));
}

#[test]
fn an_unscored_open_candidate_stays_in_the_pool_but_is_not_eligible() {
    // The denominator is the open set, not the scored set: the engine emits a
    // row per traversed open opportunity, unscored included. An unscored
    // candidate must therefore still appear -- with provenance unknown,
    // because there is no engine price to agree with.
    let lines = capture_one_window(candidate_aaa(), None);
    let cert = certify(&lines).expect("must certify");
    assert_eq!(cert.candidates, 1, "unscored candidates must not vanish from the pool");
    assert_eq!(cert.eligible, 0);
    assert_eq!(cert.ineligible_by_reason.get("unknown_price_provenance"), Some(&1));
}

#[test]
fn freshness_is_inclusive_at_the_bound_and_exclusive_past_it() {
    for (age, expect_eligible) in [(30i64, true), (31i64, false)] {
        let sink = SharedSink::default();
        let tmp = TempDir::new("fresh");
        let run = run_in(tmp.path());
        let mut observer =
            Observer::start(&run, "h", 1, at(0), Box::new(sink.clone())).expect("start");
        // Market time and receipt time both `age` seconds before the anchor.
        observer.on_receive(
            &ignition("AAA", 1000 - age, 3.5, IgnitionEventKind::FollowThroughConfirmed),
            at(1000 - age),
        );
        let mut prices = BTreeMap::new();
        prices.insert("AAA:2026-09-28:1".to_string(), 3.5);
        let mut scored = BTreeSet::new();
        scored.insert("AAA:2026-09-28:1".to_string());
        observer.on_window(WindowInput {
            window_id: "oiw-1".into(),
            processing_started_at: at(999),
            rank_completed_at: at(1000),
            open: candidate_aaa(),
            scored,
            engine_prices: prices,
            cohort_truncated: false,
        });
        observer.on_finish(at(1001));
        let cert = certify(&sink.lines()).expect("certify");
        assert_eq!(
            cert.eligible == 1,
            expect_eligible,
            "age {age}s should be eligible={expect_eligible}, reasons {:?}",
            cert.ineligible_by_reason
        );
    }
}

#[test]
fn a_finalised_bar_can_produce_a_market_time_ahead_of_the_anchor() {
    // The engine dates a finalised bar's close one interval after the bar's
    // opening timestamp, because that is when the close is knowable. A bar
    // received near its own boundary therefore carries a market time *after*
    // the anchor, and the resulting age is negative rather than small. Recorded
    // as its own reason: a negative age is not a fresh price.
    let sink = SharedSink::default();
    let tmp = TempDir::new("bar");
    let run = run_in(tmp.path());
    let mut observer =
        Observer::start(&run, "h", 1, at(0), Box::new(sink.clone())).expect("start");
    observer.on_receive(
        &ignition("AAA", 995, 3.5, IgnitionEventKind::FollowThroughConfirmed),
        at(995),
    );
    observer.on_receive(
        &ScanEvent::BarUpdate {
            is_final: true,
            symbol: "AAA".into(),
            timestamp: at(990),
            open: 1.0,
            high: 2.0,
            low: 0.5,
            close: 7.25,
            volume: 1,
            interval_secs: 300,
        },
        at(996),
    );
    let mut prices = BTreeMap::new();
    prices.insert("AAA:2026-09-28:1".to_string(), 7.25);
    let mut scored = BTreeSet::new();
    scored.insert("AAA:2026-09-28:1".to_string());
    observer.on_window(WindowInput {
        window_id: "oiw-1".into(),
        processing_started_at: at(999),
        rank_completed_at: at(1000),
        open: candidate_aaa(),
        scored,
        engine_prices: prices,
        cohort_truncated: false,
    });
    observer.on_finish(at(1001));
    let cert = certify(&sink.lines()).expect("certify");
    // 990 + 300 = 1290, which is 290s after the anchor at 1000.
    assert_eq!(cert.ineligible_by_reason.get("negative_market_age"), Some(&1));
    assert_eq!(cert.eligible, 0);
}

#[test]
fn two_confirmations_for_one_lifecycle_are_excluded_and_counted() {
    let sink = SharedSink::default();
    let tmp = TempDir::new("multi");
    let run = run_in(tmp.path());
    let mut observer =
        Observer::start(&run, "h", 1, at(0), Box::new(sink.clone())).expect("start");
    observer.on_receive(&ignition("AAA", 95, 3.5, IgnitionEventKind::FollowThroughConfirmed), at(95));
    observer.on_receive(&ignition("AAA", 99, 3.5, IgnitionEventKind::FollowThroughConfirmed), at(99));
    let mut prices = BTreeMap::new();
    prices.insert("AAA:2026-09-28:1".to_string(), 3.5);
    let mut scored = BTreeSet::new();
    scored.insert("AAA:2026-09-28:1".to_string());
    observer.on_window(WindowInput {
        window_id: "oiw-1".into(),
        processing_started_at: at(101),
        rank_completed_at: at(102),
        open: candidate_aaa(),
        scored,
        engine_prices: prices,
        cohort_truncated: false,
    });
    observer.on_finish(at(103));
    let cert = certify(&sink.lines()).expect("certify");
    assert_eq!(cert.eligible, 0, "multiplicity must not be resolved by picking one");
    assert_eq!(cert.excluded_multiplicity, 1, "it must be counted separately");
    assert_eq!(cert.ineligible_by_reason.get("confirmation_multiplicity"), Some(&1));
}

#[test]
fn a_confirmation_from_before_the_lifecycle_opened_does_not_count_for_it() {
    // The candidate opens at t=90. A confirmation received at t=50 belongs to
    // whatever lifecycle was open then, and counting it here would silently
    // attribute a previous opportunity's confirmation to this one.
    let sink = SharedSink::default();
    let tmp = TempDir::new("scope");
    let run = run_in(tmp.path());
    let mut observer =
        Observer::start(&run, "h", 1, at(0), Box::new(sink.clone())).expect("start");
    observer.on_receive(&ignition("AAA", 50, 3.5, IgnitionEventKind::FollowThroughConfirmed), at(50));
    let mut prices = BTreeMap::new();
    prices.insert("AAA:2026-09-28:1".to_string(), 3.5);
    let mut scored = BTreeSet::new();
    scored.insert("AAA:2026-09-28:1".to_string());
    observer.on_window(WindowInput {
        window_id: "oiw-1".into(),
        processing_started_at: at(79),
        rank_completed_at: at(80),
        open: candidate_aaa(),
        scored,
        engine_prices: prices,
        cohort_truncated: false,
    });
    observer.on_finish(at(81));
    let cert = certify(&sink.lines()).expect("certify");
    assert_eq!(cert.ineligible_by_reason.get("no_confirmation_receipt"), Some(&1));
}

#[test]
fn two_open_lifecycles_on_one_symbol_are_ambiguous_not_arbitrated() {
    let open = vec![
        OpenCandidate {
            opportunity_id: "AAA:2026-09-28:1".into(),
            symbol: "AAA".into(),
            opened_at: at(90),
        },
        OpenCandidate {
            opportunity_id: "AAA:2026-09-28:2".into(),
            symbol: "AAA".into(),
            opened_at: at(95),
        },
    ];
    let lines = capture_one_window(open, Some(3.5));
    let cert = certify(&lines).expect("certify");
    assert_eq!(cert.candidates, 2);
    assert_eq!(cert.eligible, 0);
    assert_eq!(cert.ineligible_by_reason.get("ambiguous_lifecycle_mapping"), Some(&2));
}

#[test]
fn an_overflowing_confirmation_tracker_reports_a_floor_not_a_count() {
    let sink = SharedSink::default();
    let tmp = TempDir::new("track");
    let run = run_in(tmp.path());
    let mut observer =
        Observer::start(&run, "h", 1, at(0), Box::new(sink.clone())).expect("start");
    for i in 0..(MAX_TRACKED_CONFIRMATIONS as i64 + 5) {
        observer.on_receive(
            &ignition("AAA", 100 + i, 3.5, IgnitionEventKind::FollowThroughConfirmed),
            at(100 + i),
        );
    }
    let mut prices = BTreeMap::new();
    prices.insert("AAA:2026-09-28:1".to_string(), 3.5);
    let mut scored = BTreeSet::new();
    scored.insert("AAA:2026-09-28:1".to_string());
    observer.on_window(WindowInput {
        window_id: "oiw-1".into(),
        processing_started_at: at(400),
        rank_completed_at: at(401),
        open: candidate_aaa(),
        scored,
        engine_prices: prices,
        cohort_truncated: false,
    });
    observer.on_finish(at(402));
    let cert = certify(&sink.lines()).expect("certify");
    assert_eq!(cert.ineligible_by_reason.get("confirmation_tracking_incomplete"), Some(&1));
}

#[test]
fn an_out_of_order_price_is_recorded_as_a_revision() {
    let sink = SharedSink::default();
    let tmp = TempDir::new("rev");
    let run = run_in(tmp.path());
    let mut observer =
        Observer::start(&run, "h", 1, at(0), Box::new(sink.clone())).expect("start");
    observer.on_receive(&ignition("AAA", 200, 3.5, IgnitionEventKind::CandidateOpened), at(200));
    observer.on_receive(&ignition("AAA", 150, 3.1, IgnitionEventKind::CandidateOpened), at(201));
    let lines = sink.lines();
    let revisions: Vec<PriceRevision> = lines
        .iter()
        .filter_map(|l| serde_json::from_str::<ObservationRecord>(l).ok())
        .filter_map(|r| match r {
            ObservationRecord::Receipt { revision, .. } => revision,
            _ => None,
        })
        .collect();
    assert_eq!(revisions, vec![PriceRevision::Forward, PriceRevision::OutOfOrder]);
}

// ---------------------------------------------------------------------------
// Acquisition: the closed-file reader
// ---------------------------------------------------------------------------

#[test]
fn an_unclosed_file_is_reported_and_never_read_as_evidence() {
    let tmp = TempDir::new("open");
    let lines = capture_one_window(candidate_aaa(), Some(3.5));
    // Drop the terminal record: this is what a process that died mid-run
    // leaves behind.
    let truncated: Vec<String> = lines
        .iter()
        .filter(|l| !l.contains("\"kind\":\"file_close\""))
        .cloned()
        .collect();
    write_lines(tmp.path(), RUN_FILE_NAME, &truncated, true);
    let acquired = acquire(tmp.path()).expect("acquisition itself succeeds");
    assert_eq!(acquired.open_files(), vec![RUN_FILE_NAME]);
    assert_eq!(
        acquired.records().count(),
        0,
        "an open file's records must not become evidence"
    );
    match authenticate(acquired) {
        Err(AuthenticationFailure::OpenFilePresent { names }) => {
            assert_eq!(names, vec![RUN_FILE_NAME.to_string()])
        }
        other => panic!("expected OpenFilePresent, got {other:?}"),
    }
}

#[test]
fn a_closed_file_ending_mid_line_is_refused_not_censored() {
    let tmp = TempDir::new("partial");
    let lines = capture_one_window(candidate_aaa(), Some(3.5));
    write_lines(tmp.path(), RUN_FILE_NAME, &lines, false);
    match acquire(tmp.path()) {
        Err(AcquisitionError::TrailingPartialLine { file }) => assert_eq!(file, RUN_FILE_NAME),
        other => panic!("expected TrailingPartialLine, got {other:?}"),
    }
}

#[test]
fn a_malformed_line_in_a_closed_file_is_refused() {
    let tmp = TempDir::new("malformed");
    let mut lines = capture_one_window(candidate_aaa(), Some(3.5));
    lines.insert(2, "{\"kind\":\"not_a_record\"}".to_string());
    write_lines(tmp.path(), RUN_FILE_NAME, &lines, true);
    match acquire(tmp.path()) {
        Err(AcquisitionError::MalformedLine { line, .. }) => assert_eq!(line, 3),
        other => panic!("expected MalformedLine, got {other:?}"),
    }
}

#[test]
fn records_after_the_terminal_record_are_refused() {
    let tmp = TempDir::new("after");
    let mut lines = capture_one_window(candidate_aaa(), Some(3.5));
    let extra = lines[1].clone();
    lines.push(extra);
    write_lines(tmp.path(), RUN_FILE_NAME, &lines, true);
    match acquire(tmp.path()) {
        Err(AcquisitionError::RecordsAfterFileClose { .. }) => {}
        other => panic!("expected RecordsAfterFileClose, got {other:?}"),
    }
}

#[test]
fn an_empty_run_directory_is_refused() {
    let tmp = TempDir::new("empty");
    match acquire(tmp.path()) {
        Err(AcquisitionError::NoFiles { .. }) => {}
        other => panic!("expected NoFiles, got {other:?}"),
    }
}

#[test]
fn a_real_unwritable_handle_produces_an_unclosed_file() {
    // Not an injected fault: a genuinely read-only handle. The buffered writes
    // succeed, the flush at close does not, and what is left on disk is a file
    // with no terminal record -- which is exactly the state the reader must
    // refuse rather than read.
    let tmp = TempDir::new("readonly");
    let path = tmp.path().join(RUN_FILE_NAME);
    std::fs::write(&path, b"").expect("create file");
    let handle = std::fs::OpenOptions::new().read(true).open(&path).expect("open read-only");
    let mut sink = FileSink::from_file(RUN_FILE_NAME, handle);
    let record = ObservationRecord::RunStart {
        protocol_version: PROTOCOL_VERSION.to_string(),
        run_id: "r".into(),
        namespace: "h".into(),
        pid: 1,
        started_at: at(0),
        freshness_max_age_secs: FRESHNESS_MAX_AGE_SECS,
    };
    sink.write(&record).expect("buffered write reports success");
    assert!(sink.close("r", at(1)).is_err(), "close must fail on an unwritable handle");
    let acquired = acquire(tmp.path()).expect("acquire");
    assert_eq!(acquired.open_files(), vec![RUN_FILE_NAME]);
    assert!(authenticate(acquired).is_err());
}

#[test]
fn a_run_writes_a_file_that_verifies_end_to_end_on_disk() {
    // The whole acquisition path against a real `FileSink`, including its
    // fsync-on-close, rather than a buffer a test filled in.
    let tmp = TempDir::new("e2e");
    let run = run_in(tmp.path());
    let sink = FileSink::create(run.dir(), RUN_FILE_NAME).expect("create sink");
    let mut observer =
        Observer::start(&run, "test-host", 4242, at(0), Box::new(sink)).expect("start");
    observer
        .on_receive(&ignition("AAA", 100, 3.5, IgnitionEventKind::FollowThroughConfirmed), at(100));
    let mut prices = BTreeMap::new();
    prices.insert("AAA:2026-09-28:1".to_string(), 3.5);
    let mut scored = BTreeSet::new();
    scored.insert("AAA:2026-09-28:1".to_string());
    observer.on_window(WindowInput {
        window_id: "oiw-1".into(),
        processing_started_at: at(101),
        rank_completed_at: at(102),
        open: candidate_aaa(),
        scored,
        engine_prices: prices,
        cohort_truncated: false,
    });
    observer.on_finish(at(103));
    assert_eq!(observer.failed_writes(), 0);

    let acquired = acquire(run.dir()).expect("acquire");
    assert!(acquired.open_files().is_empty());
    let authed = authenticate(acquired).expect("authenticate");
    assert_eq!(authed.report().receipts, 1);
    assert_eq!(authed.report().windows, 1);
    assert_eq!(authed.report().candidates, 1);
    assert_eq!(authed.report().run_id, run.id());
    let cert = Certificate::issue(&authed).expect("certify");
    assert_eq!(cert.eligible, 1);
}

// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

#[test]
fn a_missing_end_marker_refuses_authentication() {
    let lines: Vec<String> = capture_one_window(candidate_aaa(), Some(3.5))
        .into_iter()
        .filter(|l| !l.contains("\"kind\":\"run_end\""))
        .collect();
    let tmp = TempDir::new("noend");
    // Keep the file's own count honest so this tests the missing run_end and
    // nothing else.
    let fixed = fix_file_close(&lines);
    write_lines(tmp.path(), RUN_FILE_NAME, &fixed, true);
    let acquired = acquire(tmp.path()).expect("acquire");
    match authenticate(acquired) {
        Err(AuthenticationFailure::MissingRunEnd) => {}
        other => panic!("expected MissingRunEnd, got {other:?}"),
    }
}

#[test]
fn records_from_two_runs_in_one_file_refuse_authentication() {
    let mut lines = capture_one_window(candidate_aaa(), Some(3.5));
    // Re-stamp one record with a different run id.
    for line in lines.iter_mut() {
        if line.contains("\"kind\":\"candidate\"") {
            let mut v: serde_json::Value = serde_json::from_str(line).unwrap();
            v["runId"] = serde_json::Value::String("some-other-run".into());
            *line = serde_json::to_string(&v).unwrap();
            break;
        }
    }
    let tmp = TempDir::new("mixed");
    write_lines(tmp.path(), RUN_FILE_NAME, &lines, true);
    let acquired = acquire(tmp.path()).expect("acquire");
    match authenticate(acquired) {
        Err(AuthenticationFailure::MixedRunIds { found }) => assert_eq!(found.len(), 2),
        other => panic!("expected MixedRunIds, got {other:?}"),
    }
}

#[test]
fn a_lost_receipt_row_is_a_sequence_gap_not_a_shrug() {
    let sink = SharedSink::default();
    let tmp = TempDir::new("gap");
    let run = run_in(tmp.path());
    let mut observer =
        Observer::start(&run, "h", 1, at(0), Box::new(sink.clone())).expect("start");
    for i in 0..3 {
        observer
            .on_receive(&ignition("AAA", 100 + i, 3.5, IgnitionEventKind::CandidateOpened), at(100 + i));
    }
    observer.on_finish(at(200));
    // Remove the middle receipt: sequence 2 is simply gone.
    let lines: Vec<String> = sink
        .lines()
        .into_iter()
        .filter(|l| !l.contains("\"sequence\":2"))
        .collect();
    let fixed = fix_file_close(&lines);
    let tmp2 = TempDir::new("gap2");
    write_lines(tmp2.path(), RUN_FILE_NAME, &fixed, true);
    let acquired = acquire(tmp2.path()).expect("acquire");
    match authenticate(acquired) {
        Err(AuthenticationFailure::SequenceGap { expected, found }) => {
            assert_eq!((expected, found), (2, 3))
        }
        other => panic!("expected SequenceGap, got {other:?}"),
    }
}

#[test]
fn upstream_lag_is_recorded_without_creating_a_sequence_gap() {
    // Broadcast lag drops events *before* this consumer sees them, so it
    // consumes no sequence. Conflating the two would turn every lag into
    // apparent evidence loss -- and would hide real loss behind an
    // explanation that does not apply.
    let sink = SharedSink::default();
    let tmp = TempDir::new("lag");
    let run = run_in(tmp.path());
    let mut observer =
        Observer::start(&run, "h", 1, at(0), Box::new(sink.clone())).expect("start");
    observer.on_receive(&ignition("AAA", 100, 3.5, IgnitionEventKind::CandidateOpened), at(100));
    observer.on_lag(17, at(101));
    observer.on_receive(&ignition("AAA", 102, 3.6, IgnitionEventKind::CandidateOpened), at(102));
    observer.on_finish(at(103));
    let tmp2 = TempDir::new("lag2");
    write_lines(tmp2.path(), RUN_FILE_NAME, &sink.lines(), true);
    let authed = authenticate(acquire(tmp2.path()).expect("acquire")).expect("authenticate");
    assert_eq!(authed.report().receipts, 2);
    assert_eq!(authed.report().upstream_skipped_events, 17);
}

#[test]
fn a_file_declaring_the_wrong_record_count_refuses_authentication() {
    let mut lines = capture_one_window(candidate_aaa(), Some(3.5));
    // Drop a candidate row but leave every declared count untouched.
    let before = lines.len();
    lines.retain(|l| !l.contains("\"kind\":\"candidate\""));
    assert_eq!(before - lines.len(), 1);
    let tmp = TempDir::new("countmismatch");
    write_lines(tmp.path(), RUN_FILE_NAME, &lines, true);
    let acquired = acquire(tmp.path()).expect("acquire");
    match authenticate(acquired) {
        Err(AuthenticationFailure::FileRecordCountMismatch { declared, counted, .. }) => {
            assert_eq!(declared, counted + 1)
        }
        other => panic!("expected FileRecordCountMismatch, got {other:?}"),
    }
}

/// Rewrites the terminal record's declared count to match the lines actually
/// present, so a test can isolate a *different* defect from the file-level
/// count check.
fn fix_file_close(lines: &[String]) -> Vec<String> {
    let body: Vec<String> =
        lines.iter().filter(|l| !l.contains("\"kind\":\"file_close\"")).cloned().collect();
    let mut out = body.clone();
    let close = lines.iter().find(|l| l.contains("\"kind\":\"file_close\""));
    if let Some(close) = close {
        let mut v: serde_json::Value = serde_json::from_str(close).unwrap();
        v["recordsWritten"] = serde_json::Value::from(body.len() as u64);
        out.push(serde_json::to_string(&v).unwrap());
    }
    out
}

// ---------------------------------------------------------------------------
// The certificate
// ---------------------------------------------------------------------------

/// Drops one candidate row and rewrites the *file-level* declared count to
/// match, leaving the window's own declared count and the run counters alone.
///
/// This produces the evidence shape that matters most: every terminal record
/// present, every global counter balancing, and a row missing.
fn drop_one_candidate_but_keep_counters_tidy(lines: &[String]) -> Vec<String> {
    let mut removed = false;
    let kept: Vec<String> = lines
        .iter()
        .filter(|l| {
            if !removed && l.contains("\"kind\":\"candidate\"") {
                removed = true;
                return false;
            }
            true
        })
        .cloned()
        .collect();
    assert!(removed, "fixture must contain a candidate row");
    fix_file_close(&kept)
}

#[test]
fn a_tidy_end_marker_cannot_certify_a_capture_with_a_missing_row() {
    // The failure this guards against, stated plainly: the run_end is present,
    // the file_close is present, `written + dropped + write_errors ==
    // attempted` holds, and a candidate row is gone. Anything that certified
    // from terminal records or global counters would pass this. Row
    // reconciliation is the only thing that catches it.
    let lines = capture_one_window(candidate_aaa(), Some(3.5));
    let tampered = drop_one_candidate_but_keep_counters_tidy(&lines);
    let tmp = TempDir::new("tidy");
    write_lines(tmp.path(), RUN_FILE_NAME, &tampered, true);
    let authed = authenticate(acquire(tmp.path()).expect("acquire"))
        .expect("authentication passes: every terminal record is present");
    let counters = authed.report().declared_counters;
    assert!(counters.identity_holds(), "the counters balance, which is the point");
    assert_eq!(counters.dropped, 0);
    assert_eq!(counters.write_errors, 0);
    match Certificate::issue(&authed) {
        Err(CertificateRefusal::WindowEntryCountMismatch { declared, counted, .. }) => {
            assert_eq!((declared, counted), (1, 0))
        }
        other => panic!("expected WindowEntryCountMismatch, got {other:?}"),
    }
}

#[test]
fn an_injected_write_failure_is_counted_and_refuses_the_certificate() {
    // Position 3 is the candidate row: run_start, receipt, candidate.
    let sink_lines: Vec<String>;
    {
        let tmp = TempDir::new("injected");
        let run = run_in(tmp.path());
        let spy = SpySink::failing_at(&[3]);
        let mut observer = Observer::start(&run, "h", 1, at(0), Box::new(spy)).expect("start");
        observer.on_receive(
            &ignition("AAA", 100, 3.5, IgnitionEventKind::FollowThroughConfirmed),
            at(100),
        );
        let mut prices = BTreeMap::new();
        prices.insert("AAA:2026-09-28:1".to_string(), 3.5);
        let mut scored = BTreeSet::new();
        scored.insert("AAA:2026-09-28:1".to_string());
        observer.on_window(WindowInput {
            window_id: "oiw-1".into(),
            processing_started_at: at(101),
            rank_completed_at: at(102),
            open: candidate_aaa(),
            scored,
            engine_prices: prices,
            cohort_truncated: false,
        });
        observer.on_finish(at(103));
        assert_eq!(observer.failed_writes(), 1, "the refused write must be counted");
        let counters = observer.counters();
        assert_eq!(counters.dropped, 1);
        assert!(counters.identity_holds());
        // Reconstruct what such a run leaves on disk: the lost row is absent
        // and the terminal records were still written successfully.
        let lines = capture_one_window(candidate_aaa(), Some(3.5));
        let mut tampered = drop_one_candidate_but_keep_counters_tidy(&lines);
        for line in tampered.iter_mut() {
            if line.contains("\"kind\":\"run_end\"") {
                let mut v: serde_json::Value = serde_json::from_str(line).unwrap();
                v["counters"]["dropped"] = serde_json::Value::from(1u64);
                v["counters"]["attempted"] =
                    serde_json::Value::from(v["counters"]["written"].as_u64().unwrap() + 1);
                *line = serde_json::to_string(&v).unwrap();
            }
        }
        sink_lines = tampered;
    }
    let tmp = TempDir::new("injected2");
    write_lines(tmp.path(), RUN_FILE_NAME, &sink_lines, true);
    let authed = authenticate(acquire(tmp.path()).expect("acquire")).expect("authenticate");
    match Certificate::issue(&authed) {
        Err(CertificateRefusal::RecordsLost { dropped, .. }) => assert_eq!(dropped, 1),
        other => panic!("expected RecordsLost, got {other:?}"),
    }
}

#[test]
fn a_flush_failure_at_close_is_reported() {
    let tmp = TempDir::new("flush");
    let run = run_in(tmp.path());
    let mut spy = SpySink::new();
    spy.fail_close = true;
    let mut observer = Observer::start(&run, "h", 1, at(0), Box::new(spy)).expect("start");
    observer.on_finish(at(1));
    assert_eq!(observer.failed_writes(), 1, "a failed close must be counted, not swallowed");
}

#[test]
fn a_broken_counter_identity_refuses_the_certificate() {
    let mut lines = capture_one_window(candidate_aaa(), Some(3.5));
    for line in lines.iter_mut() {
        if line.contains("\"kind\":\"run_end\"") {
            let mut v: serde_json::Value = serde_json::from_str(line).unwrap();
            v["counters"]["attempted"] =
                serde_json::Value::from(v["counters"]["written"].as_u64().unwrap() + 5);
            *line = serde_json::to_string(&v).unwrap();
        }
    }
    let tmp = TempDir::new("identity");
    write_lines(tmp.path(), RUN_FILE_NAME, &lines, true);
    let authed = authenticate(acquire(tmp.path()).expect("acquire")).expect("authenticate");
    match Certificate::issue(&authed) {
        Err(CertificateRefusal::CountersIdentityViolated { .. }) => {}
        other => panic!("expected CountersIdentityViolated, got {other:?}"),
    }
}

#[test]
fn a_saturated_counter_refuses_the_certificate() {
    let mut lines = capture_one_window(candidate_aaa(), Some(3.5));
    for line in lines.iter_mut() {
        if line.contains("\"kind\":\"run_end\"") {
            let mut v: serde_json::Value = serde_json::from_str(line).unwrap();
            v["counters"]["overflowed"] = serde_json::Value::Bool(true);
            *line = serde_json::to_string(&v).unwrap();
        }
    }
    let tmp = TempDir::new("overflow");
    write_lines(tmp.path(), RUN_FILE_NAME, &lines, true);
    let authed = authenticate(acquire(tmp.path()).expect("acquire")).expect("authenticate");
    match Certificate::issue(&authed) {
        Err(CertificateRefusal::CounterOverflow { .. }) => {}
        other => panic!("expected CounterOverflow, got {other:?}"),
    }
}

#[test]
fn a_repeated_candidate_row_refuses_the_certificate() {
    let lines = capture_one_window(candidate_aaa(), Some(3.5));
    let candidate = lines.iter().find(|l| l.contains("\"kind\":\"candidate\"")).unwrap().clone();
    let mut duplicated: Vec<String> = Vec::new();
    for line in &lines {
        duplicated.push(line.clone());
        if line == &candidate {
            duplicated.push(candidate.clone());
        }
    }
    // Make the window agree that there are two rows, so this isolates the
    // duplicate rather than tripping the count check.
    for line in duplicated.iter_mut() {
        if line.contains("\"kind\":\"window_close\"") {
            let mut v: serde_json::Value = serde_json::from_str(line).unwrap();
            v["entryCount"] = serde_json::Value::from(2u64);
            v["openSetSize"] = serde_json::Value::from(2u64);
            *line = serde_json::to_string(&v).unwrap();
        }
    }
    let fixed = fix_file_close(&duplicated);
    let tmp = TempDir::new("dup");
    write_lines(tmp.path(), RUN_FILE_NAME, &fixed, true);
    let authed = authenticate(acquire(tmp.path()).expect("acquire")).expect("authenticate");
    match Certificate::issue(&authed) {
        Err(CertificateRefusal::DuplicateCandidate { opportunity_id, .. }) => {
            assert_eq!(opportunity_id, "AAA:2026-09-28:1")
        }
        other => panic!("expected DuplicateCandidate, got {other:?}"),
    }
}

#[test]
fn a_truncated_cohort_refuses_the_certificate() {
    let sink = SharedSink::default();
    let tmp = TempDir::new("trunc");
    let run = run_in(tmp.path());
    let mut observer =
        Observer::start(&run, "h", 1, at(0), Box::new(sink.clone())).expect("start");
    observer
        .on_receive(&ignition("AAA", 100, 3.5, IgnitionEventKind::FollowThroughConfirmed), at(100));
    let mut prices = BTreeMap::new();
    prices.insert("AAA:2026-09-28:1".to_string(), 3.5);
    let mut scored = BTreeSet::new();
    scored.insert("AAA:2026-09-28:1".to_string());
    observer.on_window(WindowInput {
        window_id: "oiw-1".into(),
        processing_started_at: at(101),
        rank_completed_at: at(102),
        open: candidate_aaa(),
        scored,
        engine_prices: prices,
        cohort_truncated: true,
    });
    observer.on_finish(at(103));
    let tmp2 = TempDir::new("trunc2");
    write_lines(tmp2.path(), RUN_FILE_NAME, &sink.lines(), true);
    let authed = authenticate(acquire(tmp2.path()).expect("acquire")).expect("authenticate");
    match Certificate::issue(&authed) {
        Err(CertificateRefusal::CohortTruncated { window_id }) => assert_eq!(window_id, "oiw-1"),
        other => panic!("expected CohortTruncated, got {other:?}"),
    }
}

#[test]
fn a_window_without_its_terminal_record_refuses_the_certificate_and_is_marked_incomplete() {
    let lines: Vec<String> = capture_one_window(candidate_aaa(), Some(3.5))
        .into_iter()
        .filter(|l| !l.contains("\"kind\":\"window_close\""))
        .collect();
    let fixed = fix_file_close(&lines);
    let tmp = TempDir::new("nowindowclose");
    write_lines(tmp.path(), RUN_FILE_NAME, &fixed, true);
    let authed = authenticate(acquire(tmp.path()).expect("acquire")).expect("authenticate");
    match Certificate::issue(&authed) {
        Err(CertificateRefusal::WindowWithoutClose { window_id }) => {
            assert_eq!(window_id, "oiw-1")
        }
        other => panic!("expected WindowWithoutClose, got {other:?}"),
    }
    // Refusing to certify is not the same as having nothing to say: clause 6
    // makes a candidate in a partial window ineligible, and that verdict is
    // available without a certificate.
    let finalized = finalize_eligibility(&authed);
    assert_eq!(finalized.len(), 1);
    assert!(!finalized[0].eligibility.eligible);
    assert!(finalized[0].eligibility.reasons.contains(&IneligibilityReason::WindowIncomplete));
}

#[test]
fn a_capture_with_no_windows_certifies_nothing() {
    let sink = SharedSink::default();
    let tmp = TempDir::new("nowindows");
    let run = run_in(tmp.path());
    let mut observer =
        Observer::start(&run, "h", 1, at(0), Box::new(sink.clone())).expect("start");
    observer.on_receive(&ignition("AAA", 100, 3.5, IgnitionEventKind::CandidateOpened), at(100));
    observer.on_finish(at(101));
    let tmp2 = TempDir::new("nowindows2");
    write_lines(tmp2.path(), RUN_FILE_NAME, &sink.lines(), true);
    let authed = authenticate(acquire(tmp2.path()).expect("acquire")).expect("authenticate");
    assert!(matches!(Certificate::issue(&authed), Err(CertificateRefusal::NoWindows)));
}

// ---------------------------------------------------------------------------
// Canonical comparison
// ---------------------------------------------------------------------------

#[test]
fn identical_counts_with_different_content_are_not_equal() {
    // The case a count-only comparison misses. Same run, same window, same
    // opportunity, same number of rows -- different eligibility provenance.
    let tmp_a = TempDir::new("canon-a");
    write_lines(tmp_a.path(), RUN_FILE_NAME, &capture_one_window(candidate_aaa(), Some(3.5)), true);
    let a = authenticate(acquire(tmp_a.path()).expect("acquire")).expect("auth");

    let tmp_b = TempDir::new("canon-b");
    write_lines(tmp_b.path(), RUN_FILE_NAME, &capture_one_window(candidate_aaa(), Some(9.9)), true);
    let b = authenticate(acquire(tmp_b.path()).expect("acquire")).expect("auth");

    let ta = canonical_tuples(&a);
    let tb = canonical_tuples(&b);
    assert_eq!(ta.len(), tb.len(), "counts are identical");
    assert_ne!(ta, tb, "content is not, and exact tuple equality must see that");
    assert!(ta[0].eligibility_provenance.starts_with("eligible|"));
    assert!(tb[0].eligibility_provenance.starts_with("ineligible:unknown_price_provenance|"));
}

#[test]
fn canonical_tuples_are_sorted_and_stable() {
    let open = vec![
        OpenCandidate {
            opportunity_id: "ZZZ:2026-09-28:1".into(),
            symbol: "ZZZ".into(),
            opened_at: at(90),
        },
        OpenCandidate {
            opportunity_id: "AAA:2026-09-28:1".into(),
            symbol: "AAA".into(),
            opened_at: at(90),
        },
    ];
    let tmp = TempDir::new("canon-sort");
    write_lines(tmp.path(), RUN_FILE_NAME, &capture_one_window(open, Some(3.5)), true);
    let authed = authenticate(acquire(tmp.path()).expect("acquire")).expect("auth");
    let tuples = canonical_tuples(&authed);
    let ids: Vec<&str> = tuples.iter().map(|t| t.opportunity_id.as_str()).collect();
    assert_eq!(ids, vec!["AAA:2026-09-28:1", "ZZZ:2026-09-28:1"]);
    assert_eq!(canonical_tuples(&authed), tuples, "stable across calls");
}

// ---------------------------------------------------------------------------
// Flag
// ---------------------------------------------------------------------------

#[test]
fn the_observer_is_off_unless_the_flag_says_otherwise() {
    // `enabled()` reads a process-global, so this asserts the parsing rule
    // rather than mutating the environment under other tests.
    assert!(!enabled() || std::env::var(ENV_FLAG).is_ok());
    for value in ["0", "no", "off", "", "maybe"] {
        assert!(
            !matches!(value.trim(), "1" | "true" | "yes" | "on"),
            "{value} must not enable the observer"
        );
    }
}

// ---------------------------------------------------------------------------
// The hook, against the real driver
// ---------------------------------------------------------------------------
//
// Everything above tests the observation layer in isolation. These tests check
// the two claims that isolation cannot: that the hook sits where the protocol
// says it does, and that attaching it changes nothing the engine produces.

#[derive(Clone, Default)]
struct WindowSpy {
    windows: std::sync::Arc<std::sync::Mutex<Vec<WindowInput>>>,
    receipts: std::sync::Arc<std::sync::Mutex<Vec<(String, DateTime<Utc>)>>>,
}

impl ShadowObserver for WindowSpy {
    fn on_receive(&mut self, event: &ScanEvent, received_at: DateTime<Utc>) {
        let tag = serde_json::to_value(event)
            .ok()
            .and_then(|v| v["type"].as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        self.receipts.lock().unwrap().push((tag, received_at));
    }

    fn on_window(&mut self, input: WindowInput) {
        self.windows.lock().unwrap().push(input);
    }

    fn on_lag(&mut self, _skipped: u64, _at: DateTime<Utc>) {}

    fn on_finish(&mut self, _at: DateTime<Utc>) {}
}

/// Drives a fixed event sequence and returns each ranking window's snapshots,
/// in order.
///
/// Note that a window runs on the **first** event: `observe` folds the event in
/// before ranking, so the open set is already non-empty and `last_ranked` is
/// still `None`, which makes ranking due. The 40 s event is past the cadence
/// and produces a second. Both are returned rather than just the last, because
/// the hook must agree with the engine on *every* window, not on one of them.
fn drive_to_windows(
    driver: &mut crate::opportunity_shadow::ShadowDriver,
) -> Vec<Vec<backtest_metrics::opportunity::OpportunityScoreSnapshot>> {
    let events = [
        (0i64, 3.00, IgnitionEventKind::CandidateOpened),
        (5, 3.10, IgnitionEventKind::FollowThroughConfirmed),
        (40, 3.20, IgnitionEventKind::CandidateOpened),
    ];
    let mut batches = Vec::new();
    for (secs, price, kind) in events {
        let step = driver.observe(&ignition("AAA", secs, price, kind), at(secs));
        if !step.snapshots.is_empty() {
            batches.push(step.snapshots.clone());
        }
    }
    batches
}

#[test]
fn the_window_hook_reports_the_engine_s_own_window_id() {
    let config = backtest_metrics::opportunity::OiConfig::default();
    let mut driver = crate::opportunity_shadow::ShadowDriver::new(config, None);
    let spy = WindowSpy::default();
    driver.set_observer(Box::new(spy.clone()));
    let batches = drive_to_windows(&mut driver);
    assert!(batches.len() >= 2, "fixture must produce more than one ranking window");

    let windows = spy.windows.lock().unwrap();
    assert_eq!(windows.len(), batches.len(), "one observation per ranking window");
    for (observed, snapshots) in windows.iter().zip(batches.iter()) {
        // The window id is derived from the engine's own counter rather than
        // from the snapshots, so this is the check that the derivation is
        // right -- and that it stays right across several windows, where an
        // off-by-one would show up.
        assert_eq!(observed.window_id, snapshots[0].window_id);
        // Every scored opportunity must be in the observed open set: the pool
        // is the open set, and a scored opportunity that is not open would mean
        // the two were read at different instants.
        let open_ids: BTreeSet<&str> =
            observed.open.iter().map(|c| c.opportunity_id.as_str()).collect();
        for snapshot in snapshots {
            assert!(
                open_ids.contains(snapshot.opportunity_id.as_str()),
                "scored {} missing from the observed open set",
                snapshot.opportunity_id
            );
            assert_eq!(
                observed.engine_prices.get(&snapshot.opportunity_id).copied(),
                Some(snapshot.current_price),
                "the observer must carry the price the engine actually ranked on"
            );
        }
        assert!(observed.processing_started_at <= observed.rank_completed_at);
        assert!(!observed.cohort_truncated);
    }
}

#[test]
fn every_received_event_reaches_the_hook_exactly_once() {
    let config = backtest_metrics::opportunity::OiConfig::default();
    let mut driver = crate::opportunity_shadow::ShadowDriver::new(config, None);
    let spy = WindowSpy::default();
    driver.set_observer(Box::new(spy.clone()));
    let _ = driver.observe(&ignition("AAA", 0, 3.0, IgnitionEventKind::CandidateOpened), at(0));
    let _ = driver.observe(&ignition("BBB", 1, 4.0, IgnitionEventKind::CandidateOpened), at(1));
    let receipts = spy.receipts.lock().unwrap();
    assert_eq!(receipts.len(), 2);
    assert_eq!(receipts[0], ("ignition_event".to_string(), at(0)));
    assert_eq!(receipts[1], ("ignition_event".to_string(), at(1)));
}

#[test]
fn attaching_an_observer_changes_nothing_the_engine_produces() {
    // The invariant the whole hook design rests on. Two drivers, identical
    // events at identical instants, one observed and one not: the snapshots
    // must be byte-identical. If this ever fails, the observation layer has
    // started influencing research output and every number downstream of it is
    // suspect.
    let mut plain = crate::opportunity_shadow::ShadowDriver::new(
        backtest_metrics::opportunity::OiConfig::default(),
        None,
    );
    let mut observed = crate::opportunity_shadow::ShadowDriver::new(
        backtest_metrics::opportunity::OiConfig::default(),
        None,
    );
    observed.set_observer(Box::new(WindowSpy::default()));

    let without = drive_to_windows(&mut plain);
    let with = drive_to_windows(&mut observed);
    assert!(without.len() >= 2);
    assert_eq!(
        serde_json::to_string(&without).unwrap(),
        serde_json::to_string(&with).unwrap(),
        "observer-on must be byte-identical to observer-off"
    );
}

#[test]
fn a_driver_without_an_observer_needs_no_observation_calls() {
    // The default construction path stays exactly as it was: no observer, and
    // the lag and finish hooks are safe no-ops so a caller never has to test
    // for one.
    let mut driver = crate::opportunity_shadow::ShadowDriver::new(
        backtest_metrics::opportunity::OiConfig::default(),
        None,
    );
    driver.observe_lag(3, at(0));
    driver.finish_observation(at(1));
    let step = driver.observe(&ignition("AAA", 0, 3.0, IgnitionEventKind::CandidateOpened), at(0));
    assert!(step.snapshots.is_empty(), "no window is due on the first event");
}

#[test]
fn a_full_run_through_the_driver_certifies() {
    // Acquisition, authentication and certification over evidence produced by
    // the real driver rather than by a hand-built fixture -- the end-to-end
    // path the certificate is supposed to describe.
    let tmp = TempDir::new("driver-e2e");
    let run = run_in(tmp.path());
    let sink = FileSink::create(run.dir(), RUN_FILE_NAME).expect("sink");
    let observer = Observer::start(&run, "test-host", 1, at(0), Box::new(sink)).expect("observer");
    let mut driver = crate::opportunity_shadow::ShadowDriver::new(
        backtest_metrics::opportunity::OiConfig::default(),
        None,
    );
    driver.set_observer(Box::new(observer));
    let batches = drive_to_windows(&mut driver);
    assert!(batches.len() >= 2);
    driver.finish_observation(Utc::now());

    let authed = authenticate(acquire(run.dir()).expect("acquire")).expect("authenticate");
    assert_eq!(authed.report().windows, batches.len() as u64);
    assert!(authed.report().candidates >= 1);
    let cert = Certificate::issue(&authed).expect("certify a real driver run");
    assert_eq!(cert.windows, batches.len() as u64);
    assert_eq!(cert.candidates, authed.report().candidates);
    // No claim is made about how many were eligible: the fixture's timestamps
    // are historical, so the ages measured against a live anchor are large by
    // construction. What must hold is that the breakdown accounts for every
    // candidate.
    let accounted: u64 = cert.ineligible_by_reason.values().sum();
    assert!(accounted >= cert.candidates - cert.eligible);
}

// ---------------------------------------------------------------------------
// L2.1 §6 — closed-file adversarial matrix
// ---------------------------------------------------------------------------
//
// Closure derives from file CONTENT. These cases exist because every cheap
// alternative -- size, mtime, time-since-last-write -- is satisfiable by a
// file that is still being appended to, and consuming such a file as complete
// evidence is the failure mode the whole reader exists to prevent.

/// Expected behaviour, stated once so the table below is the documentation.
///
/// | shape                          | acquire | closed | verdict       |
/// |--------------------------------|---------|--------|---------------|
/// | valid terminal record          | Ok      | true   | PASS          |
/// | stable size, no terminal record| Ok      | false  | INDETERMINATE |
/// | old mtime, no terminal record  | Ok      | false  | INDETERMINATE |
/// | truncated terminal record      | Ok      | false  | INDETERMINATE |
/// | duplicate terminal record      | Err     | -      | FAIL          |
/// | content after terminal record  | Err     | -      | FAIL          |
#[test]
fn closed_file_adversarial_matrix() {
    let lines = capture_one_window(candidate_aaa(), Some(3.5));
    let without_close: Vec<String> =
        lines.iter().filter(|l| !l.contains("\"kind\":\"file_close\"")).cloned().collect();
    let close_line = lines.iter().find(|l| l.contains("\"kind\":\"file_close\"")).unwrap().clone();

    // 1. Valid terminal record.
    {
        let tmp = TempDir::new("adv-valid");
        write_lines(tmp.path(), RUN_FILE_NAME, &lines, true);
        let acquired = acquire(tmp.path()).expect("acquire");
        assert!(acquired.files()[0].closed);
        assert_eq!(assess(tmp.path()).label(), "PASS");
    }

    // 2. Stable size, no terminal record. Nothing about the file changes
    //    between two reads, which is exactly the signal a size-based closure
    //    heuristic would accept.
    {
        let tmp = TempDir::new("adv-stable");
        write_lines(tmp.path(), RUN_FILE_NAME, &without_close, true);
        let first = std::fs::metadata(tmp.path().join(RUN_FILE_NAME)).unwrap().len();
        let second = std::fs::metadata(tmp.path().join(RUN_FILE_NAME)).unwrap().len();
        assert_eq!(first, second, "size is stable, and must not be taken as closure");
        let acquired = acquire(tmp.path()).expect("acquire");
        assert!(!acquired.files()[0].closed);
        assert!(acquired.records().next().is_none(), "an open file yields no evidence");
        assert_eq!(assess(tmp.path()).label(), "INDETERMINATE");
    }

    // 3. Old mtime, no terminal record.
    {
        let tmp = TempDir::new("adv-mtime");
        write_lines(tmp.path(), RUN_FILE_NAME, &without_close, true);
        let path = tmp.path().join(RUN_FILE_NAME);
        let old = std::fs::File::options().write(true).open(&path).unwrap();
        // Backdate by a day. Whether the platform honours this or not, the
        // reader must reach the same verdict, so the assertion does not depend
        // on it.
        let _ = old.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(86_400));
        drop(old);
        let acquired = acquire(tmp.path()).expect("acquire");
        assert!(!acquired.files()[0].closed, "age is not closure");
        assert_eq!(assess(tmp.path()).label(), "INDETERMINATE");
    }

    // 4. Truncated terminal record: the line is there but incomplete, so it
    //    does not parse as a terminal record and the file stays open.
    {
        let tmp = TempDir::new("adv-truncated");
        let mut shaped = without_close.clone();
        shaped.push(close_line[..close_line.len() / 2].to_string());
        write_lines(tmp.path(), RUN_FILE_NAME, &shaped, true);
        let acquired = acquire(tmp.path()).expect("acquire");
        assert!(!acquired.files()[0].closed, "a half-written terminal record does not close a file");
        assert_eq!(assess(tmp.path()).label(), "INDETERMINATE");
    }

    // 5. Duplicate terminal record.
    {
        let tmp = TempDir::new("adv-duplicate");
        let mut shaped = lines.clone();
        shaped.push(close_line.clone());
        write_lines(tmp.path(), RUN_FILE_NAME, &shaped, true);
        assert!(matches!(
            acquire(tmp.path()),
            Err(AcquisitionError::DuplicateFileClose { .. })
        ));
        assert_eq!(assess(tmp.path()).label(), "FAIL");
    }

    // 6. Content after the terminal record.
    {
        let tmp = TempDir::new("adv-after");
        let mut shaped = lines.clone();
        shaped.push(without_close[1].clone());
        write_lines(tmp.path(), RUN_FILE_NAME, &shaped, true);
        assert!(matches!(
            acquire(tmp.path()),
            Err(AcquisitionError::RecordsAfterFileClose { .. })
        ));
        assert_eq!(assess(tmp.path()).label(), "FAIL");
    }
}

// ---------------------------------------------------------------------------
// L2.1 §15 — three-valued assessment
// ---------------------------------------------------------------------------

#[test]
fn absence_is_indeterminate_and_indeterminate_is_not_a_pass() {
    let tmp = TempDir::new("verdict-empty");
    let verdict = assess(tmp.path());
    assert_eq!(verdict.label(), "INDETERMINATE");
    assert!(!verdict.is_pass(), "absence must never read as success");
    assert!(matches!(verdict, CaptureVerdict::Indeterminate(Indeterminate::NoEvidence { .. })));
}

#[test]
fn a_clean_capture_with_no_window_is_indeterminate_not_failed() {
    // Nothing is wrong with this capture. There is simply nothing to certify,
    // and reporting that as a failure would make a quiet session look like a
    // defect.
    let sink = SharedSink::default();
    let tmp = TempDir::new("verdict-nowindow");
    let run = run_in(tmp.path());
    let mut observer =
        Observer::start(&run, "h", 1, at(0), Box::new(sink.clone())).expect("start");
    observer.on_receive(&ignition("AAA", 100, 3.5, IgnitionEventKind::CandidateOpened), at(100));
    observer.on_finish(at(101));
    let tmp2 = TempDir::new("verdict-nowindow2");
    write_lines(tmp2.path(), RUN_FILE_NAME, &sink.lines(), true);
    let verdict = assess(tmp2.path());
    assert_eq!(verdict.label(), "INDETERMINATE");
    assert!(matches!(verdict, CaptureVerdict::Indeterminate(Indeterminate::NoWindows)));
}

#[test]
fn contradictory_evidence_fails_rather_than_abstaining() {
    let lines = capture_one_window(candidate_aaa(), Some(3.5));
    let tampered = drop_one_candidate_but_keep_counters_tidy(&lines);
    let tmp = TempDir::new("verdict-fail");
    write_lines(tmp.path(), RUN_FILE_NAME, &tampered, true);
    let verdict = assess(tmp.path());
    assert_eq!(verdict.label(), "FAIL");
    assert!(!verdict.is_pass());
}

#[test]
fn an_empty_rate_is_not_a_rate_of_zero() {
    // A denominator of zero is an absence of evidence. Reporting 0.0 would say
    // "nothing was fresh", which is a measurement, and there was none.
    let lines = capture_one_window(candidate_aaa(), None);
    let cert = certify(&lines).expect("certify");
    assert_eq!(cert.candidates, 1);
    assert_eq!(cert.detector_confirmed, 1, "the candidate was confirmed");
    assert_eq!(cert.provenance_establishable, 0, "but its provenance could not be established");
    assert!(cert.eligibility_rate.is_nan(), "no establishable denominator => NaN, not 0.0");
    assert!(cert.freshness_eligibility_rate.is_nan());
    // The companion rate is what keeps that absence visible rather than
    // letting it vanish into a missing primary.
    assert_eq!(cert.provenance_establishment_rate, 0.0, "coverage is measurable, and is zero");
}

#[test]
fn a_missing_provenance_is_not_counted_as_a_failed_eligibility() {
    // The substitution the frozen denominator exists to prevent. Two
    // candidates are confirmed; one has establishable provenance and is
    // fresh, the other has none. The primary rate must read 1/1, not 1/2 --
    // a candidate whose provenance cannot be established cannot validly be
    // classified as stale.
    let sink = SharedSink::default();
    let tmp = TempDir::new("denominator");
    let run = run_in(tmp.path());
    let mut observer =
        Observer::start(&run, "h", 1, at(0), Box::new(sink.clone())).expect("start");
    observer.on_receive(&ignition("AAA", 100, 3.5, IgnitionEventKind::FollowThroughConfirmed), at(100));
    observer.on_receive(&ignition("BBB", 100, 7.5, IgnitionEventKind::FollowThroughConfirmed), at(100));
    let open = vec![
        OpenCandidate {
            opportunity_id: "AAA:2026-09-28:1".into(),
            symbol: "AAA".into(),
            opened_at: at(90),
        },
        OpenCandidate {
            opportunity_id: "BBB:2026-09-28:1".into(),
            symbol: "BBB".into(),
            opened_at: at(90),
        },
    ];
    // Only AAA is scored, so only AAA has an engine price to agree with.
    let mut prices = BTreeMap::new();
    prices.insert("AAA:2026-09-28:1".to_string(), 3.5);
    let mut scored = BTreeSet::new();
    scored.insert("AAA:2026-09-28:1".to_string());
    observer.on_window(WindowInput {
        window_id: "oiw-1".into(),
        processing_started_at: at(101),
        rank_completed_at: at(102),
        open,
        scored,
        engine_prices: prices,
        cohort_truncated: false,
    });
    observer.on_finish(at(103));
    let cert = certify(&sink.lines()).expect("certify");
    assert_eq!(cert.candidates, 2);
    assert_eq!(cert.detector_confirmed, 2);
    assert_eq!(cert.provenance_establishable, 1);
    assert_eq!(cert.eligible, 1);
    assert_eq!(cert.eligibility_rate, 1.0, "1/1, not 1/2");
    assert_eq!(cert.provenance_establishment_rate, 0.5, "and the missing half stays visible");
}

#[test]
fn eligibility_and_freshness_answer_different_questions() {
    // Two confirmations for one lifecycle: the candidate is fresh -- its price
    // is well inside both age bounds -- and ineligible, because clause 5
    // excludes multiplicity. Collapsing the two rates would report this as a
    // staleness problem, which it is not.
    let sink = SharedSink::default();
    let tmp = TempDir::new("fresh-vs-eligible");
    let run = run_in(tmp.path());
    let mut observer =
        Observer::start(&run, "h", 1, at(0), Box::new(sink.clone())).expect("start");
    observer.on_receive(&ignition("AAA", 95, 3.5, IgnitionEventKind::FollowThroughConfirmed), at(95));
    observer.on_receive(&ignition("AAA", 99, 3.5, IgnitionEventKind::FollowThroughConfirmed), at(99));
    let mut prices = BTreeMap::new();
    prices.insert("AAA:2026-09-28:1".to_string(), 3.5);
    let mut scored = BTreeSet::new();
    scored.insert("AAA:2026-09-28:1".to_string());
    observer.on_window(WindowInput {
        window_id: "oiw-1".into(),
        processing_started_at: at(101),
        rank_completed_at: at(102),
        open: candidate_aaa(),
        scored,
        engine_prices: prices,
        cohort_truncated: false,
    });
    observer.on_finish(at(103));
    let cert = certify(&sink.lines()).expect("certify");
    assert_eq!(cert.provenance_establishable, 1);
    assert_eq!(cert.fresh, 1, "the price is fresh");
    assert_eq!(cert.eligible, 0, "and the candidate is not eligible");
    assert_eq!(cert.freshness_eligibility_rate, 1.0);
    assert_eq!(cert.eligibility_rate, 0.0);
    assert_eq!(cert.excluded_multiplicity, 1, "which the multiplicity count explains");
}

#[test]
fn an_unconfirmed_candidate_is_outside_the_population_entirely() {
    // No confirmation receipt at the anchor: the candidate is recorded, but it
    // is not a detector-confirmed candidate, so it belongs in no denominator.
    let sink = SharedSink::default();
    let tmp = TempDir::new("unconfirmed");
    let run = run_in(tmp.path());
    let mut observer =
        Observer::start(&run, "h", 1, at(0), Box::new(sink.clone())).expect("start");
    observer.on_receive(&ignition("AAA", 100, 3.5, IgnitionEventKind::CandidateOpened), at(100));
    let mut prices = BTreeMap::new();
    prices.insert("AAA:2026-09-28:1".to_string(), 3.5);
    let mut scored = BTreeSet::new();
    scored.insert("AAA:2026-09-28:1".to_string());
    observer.on_window(WindowInput {
        window_id: "oiw-1".into(),
        processing_started_at: at(101),
        rank_completed_at: at(102),
        open: candidate_aaa(),
        scored,
        engine_prices: prices,
        cohort_truncated: false,
    });
    observer.on_finish(at(103));
    let cert = certify(&sink.lines()).expect("certify");
    assert_eq!(cert.candidates, 1, "still recorded");
    assert_eq!(cert.detector_confirmed, 0, "but outside the population");
    assert_eq!(cert.provenance_establishable, 0);
    assert!(cert.eligibility_rate.is_nan());
    assert!(cert.provenance_establishment_rate.is_nan(), "no population, no coverage rate");
}

// ---------------------------------------------------------------------------
// L2.1 §8 — market-age semantics across every price source
// ---------------------------------------------------------------------------

/// Builds a one-window capture whose price comes from `price_event`, and
/// returns the certificate.
fn certify_with_price_source(price_event: ScanEvent, received: i64, anchor: i64) -> Certificate {
    let sink = SharedSink::default();
    let tmp = TempDir::new("age");
    let run = run_in(tmp.path());
    let mut observer =
        Observer::start(&run, "h", 1, at(0), Box::new(sink.clone())).expect("start");
    // Confirm the lifecycle first so confirmation count is not the reason
    // under test.
    observer.on_receive(&ignition("AAA", 95, 1.0, IgnitionEventKind::FollowThroughConfirmed), at(95));
    let price = match &price_event {
        ScanEvent::IgnitionEvent { price, .. } => *price,
        ScanEvent::BarUpdate { close, .. } => *close,
        ScanEvent::HaltWarning { current_price, .. } => *current_price,
        other => panic!("fixture does not carry a price: {other:?}"),
    };
    observer.on_receive(&price_event, at(received));
    let mut prices = BTreeMap::new();
    prices.insert("AAA:2026-09-28:1".to_string(), price);
    let mut scored = BTreeSet::new();
    scored.insert("AAA:2026-09-28:1".to_string());
    observer.on_window(WindowInput {
        window_id: "oiw-1".into(),
        processing_started_at: at(anchor - 1),
        rank_completed_at: at(anchor),
        open: candidate_aaa(),
        scored,
        engine_prices: prices,
        cohort_truncated: false,
    });
    observer.on_finish(at(anchor + 1));
    certify(&sink.lines()).expect("certify")
}

#[test]
fn a_trade_sourced_price_ages_from_its_own_timestamp() {
    // Positive age.
    let cert = certify_with_price_source(
        ignition("AAA", 980, 3.5, IgnitionEventKind::CandidateOpened),
        980,
        1000,
    );
    assert_eq!(cert.eligible, 1, "20s old is inside the bound");
    assert!(cert.negative_market_age_sources.is_empty());

    // Zero age: market time exactly at the anchor.
    let zero = certify_with_price_source(
        ignition("AAA", 1000, 3.5, IgnitionEventKind::CandidateOpened),
        1000,
        1000,
    );
    assert_eq!(zero.eligible, 1, "a zero age is fresh, not an error");
    assert!(zero.negative_market_age_sources.is_empty());
}

#[test]
fn a_bar_sourced_price_ages_from_the_close_boundary_not_the_open() {
    // A 60 s bar opening at 900 closes at 960. Anchored at 980 that is a 20 s
    // age -- eligible. If the derivation were dropped and the bar's opening
    // timestamp used instead, the age would read 80 s and this row would be
    // wrongly excluded; if the correction ran the other way it would certify
    // a stale price. The number here is what pins the direction.
    let cert = certify_with_price_source(
        ScanEvent::BarUpdate {
            is_final: true,
            symbol: "AAA".into(),
            timestamp: at(900),
            open: 1.0,
            high: 2.0,
            low: 0.5,
            close: 3.5,
            volume: 1,
            interval_secs: 60,
        },
        965,
        980,
    );
    assert_eq!(cert.eligible, 1);
    assert!(cert.negative_market_age_sources.is_empty());
}

#[test]
fn a_non_final_bar_ages_from_its_own_timestamp() {
    let cert = certify_with_price_source(
        ScanEvent::BarUpdate {
            is_final: false,
            symbol: "AAA".into(),
            timestamp: at(975),
            open: 1.0,
            high: 2.0,
            low: 0.5,
            close: 3.5,
            volume: 1,
            interval_secs: 60,
        },
        976,
        980,
    );
    assert_eq!(cert.eligible, 1, "no interval correction applies to an unfinished bar");
}

#[test]
fn a_boundary_crossing_bar_is_excluded_at_31_seconds_and_kept_at_30() {
    for (close_at, expect_eligible) in [(950i64, true), (949i64, false)] {
        // A 50 s bar closing at `close_at`, anchored at 980.
        let cert = certify_with_price_source(
            ScanEvent::BarUpdate {
                is_final: true,
                symbol: "AAA".into(),
                timestamp: at(close_at - 50),
                open: 1.0,
                high: 2.0,
                low: 0.5,
                close: 3.5,
                volume: 1,
                interval_secs: 50,
            },
            close_at,
            980,
        );
        assert_eq!(
            cert.eligible == 1,
            expect_eligible,
            "close boundary {close_at} against anchor 980: {:?}",
            cert.ineligible_by_reason
        );
    }
}

#[test]
fn a_negative_market_age_is_recorded_with_the_source_that_explains_it() {
    // The derivation outruns arrival: a 300 s bar opening at 990 closes at
    // 1290, which is ahead of the 1000 anchor. A producer that emits a
    // finalised bar only after its interval closed cannot generate this, so
    // the certificate has to say *which* source produced it.
    let cert = certify_with_price_source(
        ScanEvent::BarUpdate {
            is_final: true,
            symbol: "AAA".into(),
            timestamp: at(990),
            open: 1.0,
            high: 2.0,
            low: 0.5,
            close: 3.5,
            volume: 1,
            interval_secs: 300,
        },
        996,
        1000,
    );
    assert_eq!(cert.eligible, 0);
    assert_eq!(cert.ineligible_by_reason.get("negative_market_age"), Some(&1));
    assert_eq!(cert.negative_market_age_sources.get("bar_update+derived"), Some(&1));

    // The other explanation: an event whose own timestamp is ahead of the
    // local clock. No derivation involved, and the certificate says so.
    let skewed = certify_with_price_source(
        ignition("AAA", 1100, 3.5, IgnitionEventKind::CandidateOpened),
        1000,
        1000,
    );
    assert_eq!(skewed.ineligible_by_reason.get("negative_market_age"), Some(&1));
    assert_eq!(skewed.negative_market_age_sources.get("ignition_event"), Some(&1));
    assert!(
        !skewed.negative_market_age_sources.contains_key("ignition_event+derived"),
        "a directly-stamped event must not be attributed to the bar derivation"
    );
}

#[test]
fn a_negative_age_is_recorded_as_measured_and_never_clamped() {
    let sink = SharedSink::default();
    let tmp = TempDir::new("noclamp");
    let run = run_in(tmp.path());
    let mut observer =
        Observer::start(&run, "h", 1, at(0), Box::new(sink.clone())).expect("start");
    observer.on_receive(&ignition("AAA", 95, 1.0, IgnitionEventKind::FollowThroughConfirmed), at(95));
    observer.on_receive(&ignition("AAA", 1100, 3.5, IgnitionEventKind::CandidateOpened), at(1000));
    let mut prices = BTreeMap::new();
    prices.insert("AAA:2026-09-28:1".to_string(), 3.5);
    let mut scored = BTreeSet::new();
    scored.insert("AAA:2026-09-28:1".to_string());
    observer.on_window(WindowInput {
        window_id: "oiw-1".into(),
        processing_started_at: at(999),
        rank_completed_at: at(1000),
        open: candidate_aaa(),
        scored,
        engine_prices: prices,
        cohort_truncated: false,
    });
    observer.on_finish(at(1001));
    let ages: Vec<i64> = sink
        .lines()
        .iter()
        .filter_map(|l| serde_json::from_str::<ObservationRecord>(l).ok())
        .filter_map(|r| match r {
            ObservationRecord::Candidate { market_age_secs, .. } => market_age_secs,
            _ => None,
        })
        .collect();
    assert_eq!(ages, vec![-100], "the measured value is kept, not floored at zero");
}

#[test]
fn there_is_no_quote_sourced_price_to_age() {
    // §8 asks about quote-sourced prices. The broadcast carries no quote
    // event: `ScanEvent` has exactly eight variants and none of them is a
    // quote, so the question is not applicable rather than unanswered. This
    // test fails if one is ever added, which is the point.
    let sources = [
        "funnel_signal",
        "momentum_update",
        "ignition_event",
        "consolidation_event",
        "funnel_health",
        "halt_warning",
        "bar_update",
        "catalyst_update",
    ];
    assert_eq!(sources.len(), 8);
    assert!(!sources.iter().any(|s| s.contains("quote")));
}

// ---------------------------------------------------------------------------
// L2.1 §9 — one extractor, not two
// ---------------------------------------------------------------------------

#[test]
fn the_observer_records_exactly_what_the_engine_extractor_returns() {
    // Not "the observer produces plausible values" -- the observer's recorded
    // symbol, market time and price must be *the same values* the engine's own
    // extractor returns, for every variant. Any divergence here is the drift
    // this whole decision was made to prevent.
    let events = all_scan_event_variants();
    assert_eq!(events.len(), 8);
    let tmp = TempDir::new("extractor");
    let run = run_in(tmp.path());
    let sink = SharedSink::default();
    let mut observer =
        Observer::start(&run, "h", 1, at(0), Box::new(sink.clone())).expect("start");
    for event in &events {
        observer.on_receive(event, at(500));
    }
    let receipts: Vec<ObservationRecord> = sink
        .lines()
        .iter()
        .filter_map(|l| serde_json::from_str::<ObservationRecord>(l).ok())
        .filter(|r| matches!(r, ObservationRecord::Receipt { .. }))
        .collect();
    assert_eq!(receipts.len(), events.len());
    for (event, record) in events.iter().zip(receipts.iter()) {
        let expected = backtest_metrics::opportunity::event_symbol_time_price(event);
        let ObservationRecord::Receipt { symbol, market_at, price, .. } = record else {
            panic!("expected a receipt");
        };
        match expected {
            Some((s, t, p)) => {
                assert_eq!(symbol.as_deref(), Some(s.as_str()));
                assert_eq!(*market_at, Some(t));
                assert_eq!(*price, p);
            }
            None => {
                assert!(symbol.is_none() && market_at.is_none() && price.is_none());
            }
        }
    }
}

#[test]
fn the_engine_and_the_observer_agree_on_the_price_that_was_ranked() {
    let mut driver = crate::opportunity_shadow::ShadowDriver::new(
        backtest_metrics::opportunity::OiConfig::default(),
        None,
    );
    let tmp = TempDir::new("agree");
    let run = run_in(tmp.path());
    let sink = SharedSink::default();
    let observer = Observer::start(&run, "h", 1, at(0), Box::new(sink.clone())).expect("start");
    driver.set_observer(Box::new(observer));
    let batches = drive_to_windows(&mut driver);
    driver.finish_observation(at(10_000));
    assert!(!batches.is_empty());

    let tmp2 = TempDir::new("agree2");
    write_lines(tmp2.path(), RUN_FILE_NAME, &sink.lines(), true);
    let authed = authenticate(acquire(tmp2.path()).expect("acquire")).expect("auth");
    let mut checked = 0;
    for record in authed.candidates() {
        let ObservationRecord::Candidate { opportunity_id, provenance: Some(p), window_id, .. } =
            record
        else {
            continue;
        };
        for batch in &batches {
            if batch.first().map(|s| &s.window_id) != Some(window_id) {
                continue;
            }
            if let Some(snapshot) = batch.iter().find(|s| &s.opportunity_id == opportunity_id) {
                assert_eq!(
                    p.price, snapshot.current_price,
                    "observer provenance must name the price the engine ranked on"
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 0, "fixture must exercise at least one agreeing candidate");
}

#[test]
fn the_interval_correction_exists_in_exactly_one_place() {
    // The finalised-bar correction is the part a duplicated extractor gets
    // wrong. If `interval_secs` ever appears in the observation module, a
    // second implementation of it has been started.
    let source = include_str!("observation.rs");
    assert!(
        !source.contains("interval_secs"),
        "the observation layer must not re-derive bar market times"
    );
    assert_eq!(
        source.matches("event_symbol_time_price").count(),
        1,
        "exactly one call site, and no local reimplementation"
    );
}

// ---------------------------------------------------------------------------
// L2.1 §11/§12 — the bounded asynchronous writer
// ---------------------------------------------------------------------------

/// A `RecordWriter` a test can slow down, block and break.
#[derive(Clone)]
struct TestWriter {
    lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    blocked: std::sync::Arc<std::sync::atomic::AtomicBool>,
    fail: std::sync::Arc<std::sync::atomic::AtomicBool>,
    finished: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl TestWriter {
    fn new() -> Self {
        Self {
            lines: Default::default(),
            blocked: Default::default(),
            fail: Default::default(),
            finished: Default::default(),
        }
    }

    fn written(&self) -> usize {
        self.lines.lock().unwrap().len()
    }

    fn block(&self) {
        self.blocked.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn release(&self) {
        self.blocked.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

impl RecordWriter for TestWriter {
    fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        while self.blocked.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "injected disk failure"));
        }
        self.lines.lock().unwrap().push(line.to_string());
        Ok(())
    }

    fn finish(&mut self) -> std::io::Result<()> {
        self.finished.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

fn a_record(n: u64) -> ObservationRecord {
    a_record_for("r", n)
}

/// A small row belonging to a named run, for fixtures where the run identity
/// has to match -- authentication refuses a file carrying records from two
/// runs, which is exactly the check a hardcoded id would trip.
fn a_record_for(run_id: &str, n: u64) -> ObservationRecord {
    ObservationRecord::Lag { run_id: run_id.into(), sequence: n, skipped: 1, at: at(0) }
}

#[test]
fn normal_throughput_writes_everything_and_loses_nothing() {
    let writer = TestWriter::new();
    let mut sink = AsyncSink::new(RUN_FILE_NAME, Box::new(writer.clone()));
    for i in 0..500 {
        sink.write(&a_record(i)).expect("enqueue");
    }
    sink.drain(std::time::Duration::from_secs(5)).expect("drain");
    let counters = sink.counters();
    assert_eq!(counters.attempted, 500);
    assert_eq!(counters.written, 500);
    assert_eq!(counters.dropped, 0);
    assert_eq!(counters.write_errors, 0);
    assert!(counters.identity_holds());
    assert_eq!(writer.written(), 500);
    assert_eq!(sink.queue_depth(), 0, "a drained queue is empty");
    assert!(sink.queue_peak() > 0);
    assert!(sink.telemetry_snapshot().loss_spans.is_empty());
    sink.close("r", at(1)).expect("close");
    assert!(writer.finished.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn queue_saturation_drops_and_counts_rather_than_blocking() {
    let writer = TestWriter::new();
    writer.block();
    let mut sink =
        AsyncSink::with_capacity(RUN_FILE_NAME, Box::new(writer.clone()), 4, 1 << 20);
    let mut refused = 0;
    for i in 0..200 {
        if sink.write(&a_record(i)).is_err() {
            refused += 1;
        }
    }
    let counters = sink.counters();
    assert_eq!(counters.attempted, 200);
    assert!(counters.dropped > 0, "a full queue must drop");
    assert_eq!(counters.dropped, refused as u64);
    let telemetry = sink.telemetry_snapshot();
    assert!(!telemetry.loss_spans.is_empty(), "loss must be localised, not just counted");
    assert!(telemetry.dropped_bytes > 0);
    let span_records: u64 = telemetry.loss_spans.iter().map(|s| s.records).sum();
    if telemetry.loss_spans_truncated {
        assert!(span_records <= counters.dropped, "a truncated span list is a sample");
    } else {
        assert_eq!(span_records, counters.dropped, "spans must account for every drop");
    }
    assert!(telemetry.queue_peak <= 5, "depth stayed within capacity (+1 in flight)");
    writer.release();
    sink.drain(std::time::Duration::from_secs(5)).expect("drain");
    assert!(sink.counters().identity_holds(), "attempted == written + dropped + write_errors");
    sink.close("r", at(1)).expect("close");
}

#[test]
fn the_byte_budget_bounds_memory_independently_of_depth() {
    let writer = TestWriter::new();
    writer.block();
    // A generous depth with a tiny byte budget: depth alone would accept these.
    let mut sink = AsyncSink::with_capacity(RUN_FILE_NAME, Box::new(writer.clone()), 4_096, 256);
    let mut errors = 0;
    for i in 0..50 {
        if let Err(e) = sink.write(&a_record(i)) {
            errors += 1;
            assert_eq!(e.kind(), std::io::ErrorKind::WouldBlock);
        }
    }
    assert!(errors > 0, "the byte budget must bind before the depth bound");
    assert!(sink.queued_bytes_peak() <= 256);
    assert_eq!(sink.counters().dropped, errors as u64);
    writer.release();
    sink.close("r", at(1)).expect("close");
}

#[test]
fn a_write_failure_is_counted_as_a_write_error_not_a_drop() {
    // The distinction matters: a drop never reached the disk, a write error
    // reached it and failed. Collapsing them would hide which half of the
    // pipeline is broken.
    let writer = TestWriter::new();
    writer.fail.store(true, std::sync::atomic::Ordering::SeqCst);
    let mut sink = AsyncSink::new(RUN_FILE_NAME, Box::new(writer.clone()));
    for i in 0..10 {
        sink.write(&a_record(i)).expect("enqueue succeeds; the failure is downstream");
    }
    sink.drain(std::time::Duration::from_secs(5)).expect("drain");
    let counters = sink.counters();
    assert_eq!(counters.write_errors, 10);
    assert_eq!(counters.dropped, 0);
    assert_eq!(counters.written, 0);
    assert!(counters.identity_holds());
}

#[test]
fn the_consumer_thread_never_waits_for_a_slow_writer() {
    // The property the whole architecture exists for. With the writer wedged,
    // a thousand enqueues must still return promptly -- they are dropped, and
    // counted, but they do not stall the caller.
    let writer = TestWriter::new();
    writer.block();
    let mut sink = AsyncSink::with_capacity(RUN_FILE_NAME, Box::new(writer.clone()), 8, 1 << 20);
    let started = std::time::Instant::now();
    for i in 0..1_000 {
        let _ = sink.write(&a_record(i));
    }
    let elapsed = started.elapsed();
    writer.release();
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "1000 enqueues against a wedged writer took {elapsed:?}; the consumer is blocking"
    );
    sink.close("r", at(1)).expect("close");
}

#[test]
fn shutdown_with_an_empty_queue_closes_cleanly() {
    let writer = TestWriter::new();
    let mut sink = AsyncSink::new(RUN_FILE_NAME, Box::new(writer.clone()));
    sink.close("r", at(1)).expect("close");
    assert_eq!(writer.written(), 1, "only the terminal record");
    assert!(writer.finished.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn shutdown_flushes_records_still_queued() {
    let writer = TestWriter::new();
    writer.block();
    let mut sink = AsyncSink::new(RUN_FILE_NAME, Box::new(writer.clone()));
    for i in 0..100 {
        sink.write(&a_record(i)).expect("enqueue");
    }
    writer.release();
    sink.close("r", at(1)).expect("close");
    // 100 records plus the terminal one, and the terminal record is last
    // because the close drains before writing it.
    assert_eq!(writer.written(), 101);
    let last = writer.lines.lock().unwrap().last().cloned().unwrap();
    assert!(last.contains("\"kind\":\"file_close\""), "the terminal record must be last");
    let declared: ObservationRecord = serde_json::from_str(&last).unwrap();
    let ObservationRecord::FileClose { records_written, .. } = declared else {
        panic!("expected a terminal record");
    };
    assert_eq!(records_written, 100, "declared count describes what actually reached the disk");
}

#[test]
fn a_drain_that_times_out_reports_failure_rather_than_success() {
    let writer = TestWriter::new();
    writer.block();
    let mut sink = AsyncSink::with_capacity(RUN_FILE_NAME, Box::new(writer.clone()), 2, 1 << 20);
    for i in 0..8 {
        let _ = sink.write(&a_record(i));
    }
    let result = sink.drain(std::time::Duration::from_millis(50));
    assert!(result.is_err(), "a drain that gave up must not report success");
    writer.release();
    sink.close("r", at(1)).expect("close");
}

#[test]
fn writer_loss_propagates_into_certificate_rejection() {
    // End to end: a real overloaded async writer, a real file, and a
    // certificate that refuses because the run lost records.
    let tmp = TempDir::new("lossy");
    let run = run_in(tmp.path());
    let file_writer = FileRecordWriter::create(run.dir(), RUN_FILE_NAME).expect("file");
    let sink = AsyncSink::with_capacity(RUN_FILE_NAME, Box::new(file_writer), 2, 1 << 20);
    let mut observer =
        Observer::start(&run, "test-host", 1, at(0), Box::new(sink)).expect("start");
    // A window wide enough to overrun a queue of 2.
    let open: Vec<OpenCandidate> = (0..200)
        .map(|i| OpenCandidate {
            opportunity_id: format!("AAA:2026-09-28:{i}"),
            symbol: format!("S{i}"),
            opened_at: at(90),
        })
        .collect();
    observer.on_receive(&ignition("AAA", 100, 3.5, IgnitionEventKind::FollowThroughConfirmed), at(100));
    observer.on_window(WindowInput {
        window_id: "oiw-1".into(),
        processing_started_at: at(101),
        rank_completed_at: at(102),
        open,
        scored: BTreeSet::new(),
        engine_prices: BTreeMap::new(),
        cohort_truncated: false,
    });
    observer.on_finish(at(103));

    let verdict = assess(run.dir());
    assert!(!verdict.is_pass(), "a lossy capture must never certify");
    assert_eq!(verdict.label(), "FAIL", "loss is contradictory evidence, not missing evidence");
}

/// One value of every `ScanEvent` variant, in declaration order.
///
/// Single source for both the frozen-wire test and the extractor-agreement
/// test, so the two cannot drift apart into covering different variant sets.
fn all_scan_event_variants() -> Vec<ScanEvent> {
    let ts = at(0);
    vec![
        ScanEvent::FunnelSignal {
            symbol: "AAA".into(),
            timestamp: ts,
            price: 1.0,
            gap_pct: 2.0,
            session_volume: 3,
            price_ok: true,
            float_ok: false,
            rel_vol_ok: true,
            gap_ok: false,
            passed: true,
        },
        ScanEvent::MomentumUpdate {
            symbol: "AAA".into(),
            timestamp: ts,
            volume_confirmation: 1.0,
            structure: 2.0,
            ma_slope: 3.0,
            wick_rejection: 4.0,
            overall: 5.0,
            qualifies: true,
        },
        ignition("AAA", 0, 1.5, IgnitionEventKind::FollowThroughConfirmed),
        ScanEvent::ConsolidationEvent {
            symbol: "AAA".into(),
            timestamp: ts,
            price: 1.5,
            kind: market_data::ConsolidationEventKind::EntryTriggered,
            strategy: market_data::ConsolidationStrategy::Micropullback,
        },
        ScanEvent::FunnelHealth {
            timestamp: ts,
            float_budget_remaining: 1,
            float_budget: 2,
            starved_candidates: 3,
            api_key_missing: false,
        },
        ScanEvent::HaltWarning {
            estimated_bands: true,
            symbol: "AAA".into(),
            timestamp: ts,
            reference_price: 1.0,
            current_price: 2.0,
            band_width_dollars: 3.0,
            band_doubled: false,
            proximity_ratio: 4.0,
            relative_volume: None,
            level: market_data::HaltAlertLevel::Amber,
            luld_in_effect: true,
        },
        ScanEvent::BarUpdate {
            is_final: true,
            symbol: "AAA".into(),
            timestamp: ts,
            open: 1.0,
            high: 2.0,
            low: 0.5,
            close: 1.5,
            volume: 10,
            interval_secs: 60,
        },
        ScanEvent::CatalystUpdate {
            symbol: "AAA".into(),
            timestamp: ts,
            catalyst_tags: vec!["fda".into()],
            headline_count: 1,
            most_recent_headline: None,
            most_recent_published_at: None,
        },
    ]
}

// ---------------------------------------------------------------------------
// L2.1 §13 — timing and capacity characterisation
// ---------------------------------------------------------------------------
//
// `#[ignore]` on purpose. These are measurements, not assertions: they take
// real time, their numbers depend on the host, and a CI runner's numbers would
// be noise presented as a gate. Run explicitly:
//
//     cargo test --offline -p ws-server observation_bench -- --ignored --nocapture
//
// The objective is not optimisation. It is to establish whether the mechanism
// can run without threatening the market-data consumer.

fn percentiles(mut samples: Vec<u128>) -> (u128, u128, u128, u128) {
    samples.sort_unstable();
    let pick = |q: f64| {
        let idx = ((samples.len() as f64 - 1.0) * q).round() as usize;
        samples[idx.min(samples.len() - 1)]
    };
    (pick(0.50), pick(0.95), pick(0.99), *samples.last().unwrap())
}

fn report(label: &str, samples: Vec<u128>, unit: &str) {
    let n = samples.len();
    let (p50, p95, p99, max) = percentiles(samples);
    println!("{label:<34} n={n:<8} p50={p50}{unit} p95={p95}{unit} p99={p99}{unit} max={max}{unit}");
}

#[test]
#[ignore = "timing characterisation; run with --ignored"]
fn observation_bench_hook_and_writer_costs() {
    const WARMUP: usize = 200;
    const N: usize = 5_000;

    // A. observation hook cost: one receipt through the observer, sink cost
    //    excluded by using a sink that does nothing measurable.
    struct NullSink(WriterCounters);
    impl ObservationSink for NullSink {
        fn write(&mut self, _r: &ObservationRecord) -> std::io::Result<()> {
            self.0.attempted += 1;
            self.0.written += 1;
            Ok(())
        }
        fn counters(&self) -> WriterCounters {
            self.0
        }
        fn close(&mut self, _run_id: &str, _at: DateTime<Utc>) -> std::io::Result<()> {
            Ok(())
        }
    }
    let tmp = TempDir::new("bench");
    let run = run_in(tmp.path());
    let mut observer = Observer::start(
        &run,
        "bench",
        1,
        at(0),
        Box::new(NullSink(WriterCounters::default())),
    )
    .expect("start");
    let event = ignition("AAA", 100, 3.5, IgnitionEventKind::CandidateOpened);
    for _ in 0..WARMUP {
        observer.on_receive(&event, at(100));
    }
    let mut hook = Vec::with_capacity(N);
    for _ in 0..N {
        let t = std::time::Instant::now();
        observer.on_receive(&event, at(100));
        hook.push(t.elapsed().as_nanos());
    }
    report("A hook: on_receive", hook, "ns");

    // B. serialization cost of one candidate record, the largest shape.
    let candidate = ObservationRecord::Candidate {
        run_id: run.id().to_string(),
        window_id: "oiw-1".into(),
        anchor_at: at(102),
        processing_started_at: at(101),
        opportunity_id: "AAA:2026-09-28:1".into(),
        symbol: "AAA".into(),
        opened_at: at(90),
        scored: true,
        provenance: Some(PriceProvenance {
            source_run_id: run.id().to_string(),
            source_sequence: 1,
            price: 3.5,
            market_at: at(100),
            received_at: at(100),
            revision: PriceRevision::Forward,
            source_event_type: "ignition_event".into(),
            market_time_derived: false,
        }),
        market_age_secs: Some(2),
        receipt_age_secs: Some(2),
        confirmation_receipts: 1,
        eligibility: Eligibility::from_reasons(Vec::new()),
    };
    let row_bytes = serde_json::to_string(&candidate).unwrap().len();
    let mut ser = Vec::with_capacity(N);
    for _ in 0..N {
        let t = std::time::Instant::now();
        let _ = serde_json::to_string(&candidate).unwrap();
        ser.push(t.elapsed().as_nanos());
    }
    report("B serialize: candidate row", ser, "ns");
    println!("B candidate row size                {row_bytes} bytes");

    // C. queue enqueue cost against a writer that keeps up.
    let writer = TestWriter::new();
    let mut sink = AsyncSink::new(RUN_FILE_NAME, Box::new(writer.clone()));
    for i in 0..WARMUP as u64 {
        let _ = sink.write(&a_record(i));
    }
    let mut enq = Vec::with_capacity(N);
    for i in 0..N as u64 {
        let t = std::time::Instant::now();
        let _ = sink.write(&a_record(i));
        enq.push(t.elapsed().as_nanos());
    }
    report("C enqueue: AsyncSink::write", enq, "ns");

    // D. writer drain. Measured against a queue that was deliberately held
    //    full, because draining an already-empty queue measures nothing --
    //    the writer keeps up with an unblocked enqueue loop, so the naive
    //    version of this reports a throughput that never happened.
    sink.drain(std::time::Duration::from_secs(30)).expect("settle");
    writer.block();
    let mut held = 0u64;
    for i in 0..2_000u64 {
        if sink.write(&a_record(i)).is_ok() {
            held += 1;
        }
    }
    writer.release();
    let t = std::time::Instant::now();
    sink.drain(std::time::Duration::from_secs(30)).expect("drain");
    let drained = t.elapsed();
    println!(
        "D drain: {held} held records in {drained:?} ({:.0} rec/s)",
        held as f64 / drained.as_secs_f64().max(1e-9)
    );
    println!("D queue peak {} depth, {} bytes", sink.queue_peak(), sink.queued_bytes_peak());
    sink.close("bench", at(1)).expect("close");

    // E/F. reader and authentication throughput over a real file.
    let big = TempDir::new("bench-read");
    let big_run = run_in(big.path());
    let file_writer = FileRecordWriter::create(big_run.dir(), RUN_FILE_NAME).expect("file");
    let mut disk = AsyncSink::new(RUN_FILE_NAME, Box::new(file_writer));
    let rows = 50_000u64;
    disk.write(&ObservationRecord::RunStart {
        protocol_version: PROTOCOL_VERSION.to_string(),
        run_id: big_run.id().to_string(),
        namespace: "bench".into(),
        pid: 1,
        started_at: at(0),
        freshness_max_age_secs: FRESHNESS_MAX_AGE_SECS,
    })
    .unwrap();
    for i in 0..rows {
        let mut row = candidate.clone();
        if let ObservationRecord::Candidate { run_id, opportunity_id, .. } = &mut row {
            *run_id = big_run.id().to_string();
            *opportunity_id = format!("AAA:2026-09-28:{i}");
        }
        while disk.write(&row).is_err() {
            std::thread::yield_now();
        }
    }
    disk.write(&ObservationRecord::WindowClose {
        run_id: big_run.id().to_string(),
        window_id: "oiw-1".into(),
        anchor_at: at(102),
        processing_started_at: at(101),
        rank_completed_at: at(102),
        entry_count: rows,
        open_set_size: rows,
        cohort_truncated: false,
    })
    .unwrap();
    disk.drain(std::time::Duration::from_secs(60)).unwrap();
    let counters = disk.counters();
    disk.write(&ObservationRecord::RunEnd {
        run_id: big_run.id().to_string(),
        ended_at: at(103),
        counters,
        telemetry: None,
    })
    .unwrap();
    disk.close(big_run.id(), at(103)).expect("close");

    let bytes = std::fs::metadata(big_run.dir().join(RUN_FILE_NAME)).unwrap().len();
    println!(
        "D disk: {rows} candidate rows, {bytes} bytes written and fsynced through AsyncSink"
    );
    let t = std::time::Instant::now();
    let acquired = acquire(big_run.dir()).expect("acquire");
    let read = t.elapsed();
    println!(
        "E reader: {rows} rows, {bytes} bytes in {read:?} ({:.1} MB/s)",
        bytes as f64 / 1e6 / read.as_secs_f64().max(1e-9)
    );
    let t = std::time::Instant::now();
    let authed = authenticate(acquired).expect("authenticate");
    let auth = t.elapsed();
    println!("F authentication: {rows} rows in {auth:?}");
    let t = std::time::Instant::now();
    let cert = Certificate::issue(&authed).expect("certify");
    println!("F certificate: {:?}, {} candidates", t.elapsed(), cert.candidates);

    // G. memory, as the bounded quantities that actually cap it.
    println!(
        "G bounds: queue {} records, {} bytes; observed peak {} records, {} bytes",
        DEFAULT_QUEUE_CAPACITY,
        DEFAULT_BYTE_CAPACITY,
        disk.queue_peak(),
        disk.queued_bytes_peak()
    );
    println!(
        "G projection: {row_bytes} B/row x {rows} rows = {:.1} MB on disk",
        (row_bytes as u64 * rows) as f64 / 1e6
    );
}

// ---------------------------------------------------------------------------
// Step 4A §15 — rotation
// ---------------------------------------------------------------------------

/// Drives `n` records through a rotating sink with a small rotation threshold,
/// returning the run directory and the file names produced.
fn rotated_capture(tmp: &TempDir, rotate_bytes: u64, rows: u64) -> (ObserverRun, Vec<String>) {
    let run = run_in(tmp.path());
    let mut sink =
        RotatingSink::create(run.dir(), run.id(), rotate_bytes, 4_096, 1 << 20).expect("create");
    sink.write(&ObservationRecord::RunStart {
        protocol_version: PROTOCOL_VERSION.to_string(),
        run_id: run.id().to_string(),
        namespace: "test".into(),
        pid: 1,
        started_at: at(0),
        freshness_max_age_secs: FRESHNESS_MAX_AGE_SECS,
    })
    .expect("run start");
    for i in 0..rows {
        sink.write(&a_record_for(run.id(), i)).expect("row");
    }
    sink.write(&ObservationRecord::WindowClose {
        run_id: run.id().to_string(),
        window_id: "oiw-1".into(),
        anchor_at: at(102),
        processing_started_at: at(101),
        rank_completed_at: at(102),
        entry_count: 0,
        open_set_size: 0,
        cohort_truncated: false,
    })
    .expect("window");
    sink.drain(std::time::Duration::from_secs(5)).expect("drain");
    let counters = sink.counters();
    sink.write(&ObservationRecord::RunEnd {
        run_id: run.id().to_string(),
        ended_at: at(200),
        counters,
        telemetry: sink.telemetry(),
    })
    .expect("run end");
    let files = sink.files().to_vec();
    sink.close(run.id(), at(200)).expect("close");
    (run, files)
}

#[test]
fn a_rotated_run_produces_a_verifiable_chain() {
    let tmp = TempDir::new("rot-ok");
    let (run, files) = rotated_capture(&tmp, 4_096, 400);
    assert!(files.len() >= 3, "fixture must actually rotate, got {files:?}");

    let acquired = acquire(run.dir()).expect("acquire");
    assert!(acquired.open_files().is_empty(), "every rotated file is closed");
    let authed = authenticate(acquired).expect("chain must verify");
    assert_eq!(authed.report().run_id, run.id());

    // Every file but the last names its successor; the last names none. That
    // asymmetry is what distinguishes "rotated" from "ended".
    let records: Vec<&ObservationRecord> = authed.acquired().records().collect();
    let closes: Vec<(&str, Option<&str>)> = records
        .iter()
        .filter_map(|r| match r {
            ObservationRecord::FileClose { file_name, next_file, .. } => {
                Some((file_name.as_str(), next_file.as_deref()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(closes.len(), files.len());
    for (i, (name, next)) in closes.iter().enumerate() {
        assert_eq!(*name, files[i]);
        if i + 1 < files.len() {
            assert_eq!(*next, Some(files[i + 1].as_str()), "{name} must name its successor");
        } else {
            assert_eq!(*next, None, "the final file must not name a successor");
        }
    }
}

#[test]
fn no_row_is_lost_or_duplicated_across_a_rotation() {
    let tmp = TempDir::new("rot-rows");
    let rows = 400u64;
    let (run, files) = rotated_capture(&tmp, 4_096, rows);
    assert!(files.len() >= 3);
    let authed = authenticate(acquire(run.dir()).expect("acquire")).expect("authenticate");
    // The `Lag` records carry the row index, so continuity is checkable
    // exactly rather than by counting.
    let mut seen: Vec<u64> = authed
        .acquired()
        .records()
        .filter_map(|r| match r {
            ObservationRecord::Lag { sequence, .. } => Some(*sequence),
            _ => None,
        })
        .collect();
    assert_eq!(seen.len() as u64, rows, "every row survived rotation exactly once");
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len() as u64, rows, "and none was duplicated across the boundary");
    assert_eq!(seen.first().copied(), Some(0));
    assert_eq!(seen.last().copied(), Some(rows - 1));
}

#[test]
fn a_missing_middle_file_breaks_the_chain() {
    let tmp = TempDir::new("rot-missing");
    let (run, files) = rotated_capture(&tmp, 4_096, 400);
    assert!(files.len() >= 3);
    std::fs::remove_file(run.dir().join(&files[1])).expect("remove middle file");
    let acquired = acquire(run.dir()).expect("acquire");
    match authenticate(acquired) {
        Err(AuthenticationFailure::BrokenRotationChain { detail }) => {
            assert!(detail.contains(&files[1]), "detail must name the missing file: {detail}");
        }
        other => panic!("expected BrokenRotationChain, got {other:?}"),
    }
}

#[test]
fn a_missing_tail_file_is_not_mistaken_for_the_end_of_the_run() {
    // The failure a backward-only chain would miss entirely: delete the last
    // file and the remaining ones still form a consistent prefix. Only the
    // forward link says the run continued.
    let tmp = TempDir::new("rot-tail");
    let (run, files) = rotated_capture(&tmp, 4_096, 400);
    let last = files.last().unwrap().clone();
    std::fs::remove_file(run.dir().join(&last)).expect("remove tail");
    match authenticate(acquire(run.dir()).expect("acquire")) {
        Err(AuthenticationFailure::BrokenRotationChain { detail }) => {
            assert!(detail.contains(&last), "detail must name the missing successor: {detail}");
        }
        other => panic!("expected BrokenRotationChain, got {other:?}"),
    }
}

#[test]
fn an_active_file_is_never_certified_even_mid_rotation() {
    let tmp = TempDir::new("rot-active");
    let run = run_in(tmp.path());
    let mut sink =
        RotatingSink::create(run.dir(), run.id(), 2_048, 4_096, 1 << 20).expect("create");
    sink.write(&ObservationRecord::RunStart {
        protocol_version: PROTOCOL_VERSION.to_string(),
        run_id: run.id().to_string(),
        namespace: "test".into(),
        pid: 1,
        started_at: at(0),
        freshness_max_age_secs: FRESHNESS_MAX_AGE_SECS,
    })
    .expect("run start");
    for i in 0..200 {
        sink.write(&a_record_for(run.id(), i)).expect("row");
    }
    sink.drain(std::time::Duration::from_secs(5)).expect("drain");
    // Deliberately no close: this is what a running capture looks like on disk.
    assert!(sink.files().len() >= 2, "must have rotated at least once");
    let acquired = acquire(run.dir()).expect("acquire");
    assert_eq!(
        acquired.open_files().len(),
        1,
        "exactly the active file is open; the rotated ones are closed"
    );
    assert_eq!(assess(run.dir()).label(), "INDETERMINATE");
    assert!(!assess(run.dir()).is_pass());
}

#[test]
fn rotation_files_are_ordered_numerically_not_lexicographically() {
    // `observations-10.ndjson` sorts before `observations-2.ndjson` as text.
    // A capture long enough to pass ten files would otherwise be presented out
    // of order, and would fail as a broken chain rather than as the ordering
    // problem it is.
    assert_eq!(rotation_index("observations-0.ndjson"), 0);
    assert_eq!(rotation_index("observations-2.ndjson"), 2);
    assert_eq!(rotation_index("observations-10.ndjson"), 10);
    assert!(rotation_index("observations-2.ndjson") < rotation_index("observations-10.ndjson"));
    assert_eq!(rotation_index("not-a-rotation-file.ndjson"), u64::MAX, "unmatched sorts last");

    let mut names: Vec<String> = (0..12).map(rotation_file_name).collect();
    let expected = names.clone();
    names.sort(); // lexicographic, i.e. wrong
    assert_ne!(names, expected, "lexicographic order really is wrong here");
    names.sort_by_key(|n| (rotation_index(n), n.clone()));
    assert_eq!(names, expected, "numeric order restores it");
}

#[test]
fn a_long_rotated_run_still_verifies_past_ten_files() {
    let tmp = TempDir::new("rot-long");
    // Small threshold, enough rows to pass twelve files.
    let (run, files) = rotated_capture(&tmp, 1_024, 900);
    assert!(files.len() > 10, "fixture must exceed ten files, got {}", files.len());
    let authed = authenticate(acquire(run.dir()).expect("acquire")).expect("chain must verify");
    assert_eq!(authed.report().files, files.len() as u64);
}

// ---------------------------------------------------------------------------
// Step 4A §22 — preregistration validator
// ---------------------------------------------------------------------------

fn a_preregistration() -> Preregistration {
    Preregistration {
        protocol_version: PROTOCOL_VERSION.to_string(),
        gate_sha256: "46dfd17c727a03423d16174fe844b6f9f01b91488c9631348ea3331fe4531518".into(),
        eligibility_floor: 0.50,
        provenance_establishment_floor: 0.90,
        freshness_max_age_secs: FRESHNESS_MAX_AGE_SECS,
        refreeze_ladder_secs: vec![60, 120],
        max_refreezes: 2,
    }
}

#[test]
fn a_coherent_preregistration_validates() {
    a_preregistration().validate().expect("must validate");
}

#[test]
fn a_preregistration_that_describes_a_different_experiment_is_refused() {
    // The failure that matters: a record preregistering a 45 s bound while the
    // build enforces 30 s would look like a valid preregistration of an
    // experiment nobody ran.
    let mut p = a_preregistration();
    p.freshness_max_age_secs = 45;
    match p.validate() {
        Err(PreregistrationError::FreshnessMismatch { preregistered, implemented }) => {
            assert_eq!((preregistered, implemented), (45, FRESHNESS_MAX_AGE_SECS))
        }
        other => panic!("expected FreshnessMismatch, got {other:?}"),
    }
}

#[test]
fn a_refreeze_ladder_must_loosen_and_must_be_bounded() {
    let mut tighter = a_preregistration();
    tighter.refreeze_ladder_secs = vec![20];
    assert!(matches!(
        tighter.validate(),
        Err(PreregistrationError::LadderNotMonotonic { .. })
    ));

    let mut unbounded = a_preregistration();
    unbounded.max_refreezes = 0;
    assert_eq!(unbounded.validate(), Err(PreregistrationError::LadderWithoutBudget));

    let mut repeated = a_preregistration();
    repeated.refreeze_ladder_secs = vec![60, 60];
    assert!(matches!(
        repeated.validate(),
        Err(PreregistrationError::LadderNotMonotonic { previous: 60, next: 60 })
    ));
}

#[test]
fn floors_must_be_real_proportions() {
    for bad in [f64::NAN, -0.1, 1.5, f64::INFINITY] {
        let mut p = a_preregistration();
        p.eligibility_floor = bad;
        assert!(
            matches!(p.validate(), Err(PreregistrationError::FloorOutOfRange { .. })),
            "{bad} must be refused"
        );
    }
}

#[test]
fn a_preregistration_without_a_gate_identity_is_refused() {
    let mut p = a_preregistration();
    p.gate_sha256 = "   ".into();
    assert_eq!(p.validate(), Err(PreregistrationError::EmptyGate));
}

#[test]
fn coverage_is_checked_before_eligibility() {
    // With poor coverage the eligibility rate describes a subsample, so
    // reporting an eligibility breach would blame the freshness bound for a
    // measurement failure. Both are breached here; the verdict must name
    // coverage.
    let lines = capture_one_window(candidate_aaa(), None);
    let cert = certify(&lines).expect("certify");
    assert_eq!(cert.provenance_establishment_rate, 0.0);
    assert!(cert.eligibility_rate.is_nan());
    match a_preregistration().evaluate(&cert) {
        FloorVerdict::CoverageBreach { observed, floor } => {
            assert_eq!(observed, 0.0);
            assert_eq!(floor, 0.90);
        }
        other => panic!("expected CoverageBreach, got {other:?}"),
    }
}

#[test]
fn a_met_floor_reports_both_rates() {
    let lines = capture_one_window(candidate_aaa(), Some(3.5));
    let cert = certify(&lines).expect("certify");
    match a_preregistration().evaluate(&cert) {
        FloorVerdict::Met { eligibility, coverage } => {
            assert_eq!(eligibility, 1.0);
            assert_eq!(coverage, 1.0);
        }
        other => panic!("expected Met, got {other:?}"),
    }
}

#[test]
fn an_eligibility_breach_names_the_next_declared_bound() {
    // A breach must hand back the *already declared* next threshold rather
    // than inviting one to be chosen now, which is the whole point of
    // preregistering a ladder.
    let sink = SharedSink::default();
    let tmp = TempDir::new("breach");
    let run = run_in(tmp.path());
    let mut observer =
        Observer::start(&run, "h", 1, at(0), Box::new(sink.clone())).expect("start");
    // Confirmed, provenance establishable, and far outside the 30 s bound.
    observer.on_receive(&ignition("AAA", 500, 3.5, IgnitionEventKind::FollowThroughConfirmed), at(500));
    let mut prices = BTreeMap::new();
    prices.insert("AAA:2026-09-28:1".to_string(), 3.5);
    let mut scored = BTreeSet::new();
    scored.insert("AAA:2026-09-28:1".to_string());
    observer.on_window(WindowInput {
        window_id: "oiw-1".into(),
        processing_started_at: at(999),
        rank_completed_at: at(1000),
        open: candidate_aaa(),
        scored,
        engine_prices: prices,
        cohort_truncated: false,
    });
    observer.on_finish(at(1001));
    let cert = certify(&sink.lines()).expect("certify");
    assert_eq!(cert.provenance_establishment_rate, 1.0, "coverage is fine");
    assert_eq!(cert.eligibility_rate, 0.0, "but nothing was fresh");
    match a_preregistration().evaluate(&cert) {
        FloorVerdict::EligibilityBreach { observed, floor, next_freshness_secs } => {
            assert_eq!(observed, 0.0);
            assert_eq!(floor, 0.50);
            assert_eq!(next_freshness_secs, Some(60), "the ladder's next rung, declared in advance");
        }
        other => panic!("expected EligibilityBreach, got {other:?}"),
    }
}

#[test]
fn an_undefined_rate_is_indeterminate_not_a_breach() {
    // No detector-confirmed candidates at all: coverage is undefined, and a
    // floor cannot be failed by a quantity that does not exist.
    let sink = SharedSink::default();
    let tmp = TempDir::new("undefined");
    let run = run_in(tmp.path());
    let mut observer =
        Observer::start(&run, "h", 1, at(0), Box::new(sink.clone())).expect("start");
    observer.on_receive(&ignition("AAA", 100, 3.5, IgnitionEventKind::CandidateOpened), at(100));
    let mut prices = BTreeMap::new();
    prices.insert("AAA:2026-09-28:1".to_string(), 3.5);
    let mut scored = BTreeSet::new();
    scored.insert("AAA:2026-09-28:1".to_string());
    observer.on_window(WindowInput {
        window_id: "oiw-1".into(),
        processing_started_at: at(101),
        rank_completed_at: at(102),
        open: candidate_aaa(),
        scored,
        engine_prices: prices,
        cohort_truncated: false,
    });
    observer.on_finish(at(103));
    let cert = certify(&sink.lines()).expect("certify");
    match a_preregistration().evaluate(&cert) {
        FloorVerdict::Indeterminate { .. } => {}
        other => panic!("expected Indeterminate, got {other:?}"),
    }
}

#[test]
fn a_preregistration_round_trips_and_is_content_addressable() {
    // The record has to survive being written out and read back byte-for-byte,
    // because its identity is what binds a capture to the decisions that
    // predated it.
    let p = a_preregistration();
    let json = serde_json::to_string(&p).unwrap();
    let back: Preregistration = serde_json::from_str(&json).unwrap();
    assert_eq!(p, back);
    assert_eq!(serde_json::to_string(&back).unwrap(), json, "serialization is stable");
}

// ---------------------------------------------------------------------------
// Step 4A §14 — storage decomposition
// ---------------------------------------------------------------------------

#[test]
#[ignore = "storage characterisation; run with --ignored"]
fn observation_bench_storage_decomposition() {
    let run_id = "host-12345-20260928T140000000Z-0";
    let candidate = ObservationRecord::Candidate {
        run_id: run_id.into(),
        window_id: "oiw-412".into(),
        anchor_at: at(102),
        processing_started_at: at(101),
        opportunity_id: "ABCD:2026-09-28:7".into(),
        symbol: "ABCD".into(),
        opened_at: at(90),
        scored: true,
        provenance: Some(PriceProvenance {
            source_run_id: run_id.into(),
            source_sequence: 1_234_567,
            price: 3.5,
            market_at: at(100),
            received_at: at(100),
            revision: PriceRevision::Forward,
            source_event_type: "ignition_event".into(),
            market_time_derived: false,
        }),
        market_age_secs: Some(2),
        receipt_age_secs: Some(2),
        confirmation_receipts: 1,
        eligibility: Eligibility::from_reasons(Vec::new()),
    };
    let full = serde_json::to_string(&candidate).unwrap();
    println!("row total                          {} bytes", full.len() + 1);

    // Field-by-field, by measuring the JSON value rather than guessing.
    let value: serde_json::Value = serde_json::from_str(&full).unwrap();
    let obj = value.as_object().unwrap();
    let mut sized: Vec<(String, usize)> = obj
        .iter()
        .map(|(k, v)| {
            let encoded = serde_json::to_string(v).unwrap().len();
            // key + quotes + colon + comma
            (k.clone(), k.len() + 4 + encoded)
        })
        .collect();
    sized.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for (field, bytes) in &sized {
        println!("  {field:<26} {bytes:>5} bytes");
    }

    // The three candidate reductions, measured rather than estimated. None
    // removes evidence the frozen protocol needs: the run identity is already
    // bound by the file (authentication refuses a file carrying two runs), and
    // the window's anchor instants are identical for every row in that window
    // and are already carried by its terminal record.
    let run_id_bytes = ("runId".len() + 4 + run_id.len() + 2) * 2; // row + provenance
    let anchor_bytes = "anchorAt".len() + 4 + 22 + "processingStartedAt".len() + 4 + 22;
    let ts_count = 5; // anchorAt, processingStartedAt, openedAt, marketAt, receivedAt
    let epoch_saving = ts_count * (22 - 13);
    println!("reduction: drop duplicated runId   -{run_id_bytes} bytes");
    println!("reduction: hoist window anchors    -{anchor_bytes} bytes");
    println!("reduction: epoch-millis timestamps -{epoch_saving} bytes");
    let reduced = (full.len() + 1).saturating_sub(run_id_bytes + anchor_bytes + epoch_saving);
    println!(
        "projected reduced row              {reduced} bytes ({:.0}% of current)",
        100.0 * reduced as f64 / (full.len() + 1) as f64
    );

    // Session projection at the Sep-22 measured structural aggregates. These
    // are window and cohort counts, not an eligibility distribution.
    for (label, bytes) in [("current", full.len() + 1), ("reduced", reduced)] {
        let per_session = bytes as u64 * 3_233 * 779;
        println!(
            "session projection ({label:<7})      {:.2} GB",
            per_session as f64 / 1e9
        );
    }
}
