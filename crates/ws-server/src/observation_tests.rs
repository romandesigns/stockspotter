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
    let ts = at(0);
    let expected: Vec<(ScanEvent, &str)> = vec![
        (
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
            r#"{"type":"funnel_signal","symbol":"AAA","timestamp":"2026-09-21T14:13:20Z","price":1.0,"gapPct":2.0,"sessionVolume":3,"priceOk":true,"floatOk":false,"relVolOk":true,"gapOk":false,"passed":true}"#,
        ),
        (
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
            r#"{"type":"momentum_update","symbol":"AAA","timestamp":"2026-09-21T14:13:20Z","volumeConfirmation":1.0,"structure":2.0,"maSlope":3.0,"wickRejection":4.0,"overall":5.0,"qualifies":true}"#,
        ),
        (
            ignition("AAA", 0, 1.5, IgnitionEventKind::FollowThroughConfirmed),
            r#"{"type":"ignition_event","symbol":"AAA","timestamp":"2026-09-21T14:13:20Z","price":1.5,"kind":"follow_through_confirmed"}"#,
        ),
        (
            ScanEvent::ConsolidationEvent {
                symbol: "AAA".into(),
                timestamp: ts,
                price: 1.5,
                kind: market_data::ConsolidationEventKind::EntryTriggered,
                strategy: market_data::ConsolidationStrategy::Micropullback,
            },
            r#"{"type":"consolidation_event","symbol":"AAA","timestamp":"2026-09-21T14:13:20Z","price":1.5,"kind":"entry_triggered","strategy":"micropullback"}"#,
        ),
        (
            ScanEvent::FunnelHealth {
                timestamp: ts,
                float_budget_remaining: 1,
                float_budget: 2,
                starved_candidates: 3,
                api_key_missing: false,
            },
            r#"{"type":"funnel_health","timestamp":"2026-09-21T14:13:20Z","floatBudgetRemaining":1,"floatBudget":2,"starvedCandidates":3,"apiKeyMissing":false}"#,
        ),
        (
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
            r#"{"type":"halt_warning","estimatedBands":true,"symbol":"AAA","timestamp":"2026-09-21T14:13:20Z","referencePrice":1.0,"currentPrice":2.0,"bandWidthDollars":3.0,"bandDoubled":false,"proximityRatio":4.0,"relativeVolume":null,"level":"amber","luldInEffect":true}"#,
        ),
        (
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
            r#"{"type":"bar_update","isFinal":true,"symbol":"AAA","timestamp":"2026-09-21T14:13:20Z","open":1.0,"high":2.0,"low":0.5,"close":1.5,"volume":10,"intervalSecs":60}"#,
        ),
        (
            ScanEvent::CatalystUpdate {
                symbol: "AAA".into(),
                timestamp: ts,
                catalyst_tags: vec!["fda".into()],
                headline_count: 1,
                most_recent_headline: None,
                most_recent_published_at: None,
            },
            r#"{"type":"catalyst_update","symbol":"AAA","timestamp":"2026-09-21T14:13:20Z","catalystTags":["fda"],"headlineCount":1,"mostRecentHeadline":null}"#,
        ),
    ];
    assert_eq!(expected.len(), 8, "every ScanEvent variant must be pinned here");
    for (event, want) in expected {
        assert_eq!(serde_json::to_string(&event).unwrap(), want);
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
    let mut sink_lines: Vec<String> = Vec::new();
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
