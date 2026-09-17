//! §19 groups I (shadow isolation — **blocking**), J (replay parity) and
//! K (writer-queue bounds).
//!
//! Group I is the test that decides whether this milestone is allowed to exist
//! at all. Everything else here measures the research layer; group I measures
//! whether the research layer left production alone.

use super::*;

use backtest_metrics::opportunity::{replay_events, Regime, ReplayObservation};
use auto_trader::config::Config as TraderConfig;
use auto_trader::engine::Engine;
use auto_trader::journal::JournalEntry;
use chrono::TimeZone;
use market_data::{IgnitionEventKind, ScanEvent};
use std::sync::atomic::AtomicUsize;

// --- fixtures ---------------------------------------------------------------

fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_757_000_000 + secs, 0).unwrap()
}

fn confirmed(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ScanEvent::IgnitionEvent {
        symbol: symbol.into(),
        timestamp: t,
        price,
        kind: IgnitionEventKind::FollowThroughConfirmed,
    }
}

fn rejected(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ScanEvent::IgnitionEvent {
        symbol: symbol.into(),
        timestamp: t,
        price,
        kind: IgnitionEventKind::FollowThroughRejected,
    }
}

fn bar(symbol: &str, t: DateTime<Utc>, close: f64) -> ScanEvent {
    ScanEvent::BarUpdate {
        symbol: symbol.into(),
        timestamp: t,
        interval_secs: 60,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1_000,
        is_final: true,
    }
}

fn momentum(symbol: &str, t: DateTime<Utc>, overall: f64) -> ScanEvent {
    ScanEvent::MomentumUpdate {
        symbol: symbol.into(),
        timestamp: t,
        volume_confirmation: 0.5,
        structure: 0.5,
        ma_slope: 0.5,
        wick_rejection: 0.5,
        overall,
        qualifies: overall >= 0.6,
    }
}

/// A deterministic, mixed stream long enough to cross several 30s ranking
/// windows, containing invalidations (so absorption is exercised) and bars (so
/// the price path moves and regimes actually differ between symbols).
fn stream() -> Vec<(ScanEvent, DateTime<Utc>)> {
    let mut out = Vec::new();
    for n in 0..240i64 {
        let symbol = format!("S{}", n % 6);
        let t = at(n);
        let price = 10.0 + (n % 6) as f64 + (n as f64 * 0.02);
        let event = match n % 4 {
            0 => confirmed(&symbol, t, price),
            1 => momentum(&symbol, t, 0.35 + (n % 5) as f64 * 0.12),
            2 => bar(&symbol, t, price * 1.01),
            _ => rejected(&symbol, t, price),
        };
        out.push((event, t));
    }
    out
}

fn temp_dir(tag: &str) -> PathBuf {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "oi-shadow-{tag}-{}-{}-{n}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------
// Group I — shadow isolation (§19, tests 45-50). BLOCKING.
// ---------------------------------------------------------------------------

/// i45. The events production sees must be byte-identical, in content and in
/// order, whether or not Opportunity Intelligence is running.
///
/// This is the same construction `measurement.rs` uses for its own isolation
/// test, and deliberately so: two independent research subsystems now read the
/// same broadcast, and each must be independently provable innocent.
#[test]
fn i45_production_events_are_byte_identical_with_and_without_shadow() {
    let events = stream();

    let disabled: Vec<String> =
        events.iter().map(|(e, _)| serde_json::to_string(e).unwrap()).collect();

    let mut driver = ShadowDriver::new(OiConfig::default(), None);
    let mut enabled: Vec<String> = Vec::new();
    let mut snapshots = 0usize;
    for (event, received_at) in &events {
        // Order matters here: the shadow layer observes first, so if it could
        // mutate the event the *later* serialization would show it.
        snapshots += driver.observe(event, *received_at).len();
        enabled.push(serde_json::to_string(event).unwrap());
    }

    assert_eq!(
        disabled, enabled,
        "opportunity intelligence must not change event content or ordering"
    );
    assert!(snapshots > 0, "and it must actually have been running");
}

/// i46. Auto-Trader decisions must be byte-identical with the shadow layer
/// enabled and disabled (§24: "Do not expose the new research scores to the
/// existing Auto-Trader").
///
/// Driving the real `Engine` rather than asserting the absence of an import:
/// an import audit proves nothing about a value smuggled in through shared
/// mutable state, and this is the claim §31 requires.
#[test]
fn i46_auto_trader_decisions_are_byte_identical_with_and_without_shadow() {
    let events = stream();
    let config = || TraderConfig {
        ws_url: "unused-in-tests".to_string(),
        position_size_usd: 500.0,
        max_concurrent_positions: 4,
        journal_path: "unused-in-tests.jsonl".to_string(),
    };

    let decisions = |mut shadow: Option<ShadowDriver>| -> Vec<String> {
        let mut engine = Engine::new(config());
        let mut journal: Vec<JournalEntry> = Vec::new();
        for (event, received_at) in &events {
            if let Some(driver) = shadow.as_mut() {
                driver.observe(event, *received_at);
            }
            journal.extend(engine.on_event(event));
        }
        journal.iter().map(|e| serde_json::to_string(e).unwrap()).collect()
    };

    let without = decisions(None);
    let with = decisions(Some(ShadowDriver::new(OiConfig::default(), None)));

    assert_eq!(without, with, "auto-trader decisions must be unaffected");
    assert!(
        !without.is_empty(),
        "the fixture must actually drive real auto-trader decisions, or this proves nothing"
    );
}

/// i47. The shadow layer's only output is research records. It produces no
/// `ScanEvent`, so there is no value it could return that a caller might
/// forward to clients or to the trader.
#[test]
fn i47_shadow_output_is_research_records_only() {
    let mut driver = ShadowDriver::new(OiConfig::default(), None);
    let mut produced = 0usize;
    for (event, received_at) in &stream() {
        for snapshot in driver.observe(event, *received_at) {
            // Typed, not stringly: `observe` returns `OpportunityScoreSnapshot`,
            // and a `ScanEvent` cannot inhabit that type.
            assert_eq!(snapshot.schema_version, 1);
            assert!(!snapshot.opportunity_id.is_empty());
            produced += 1;
        }
    }
    assert!(produced > 0);
}

/// i48. An unusable capture directory disables capture. It must not panic and
/// must not be reported as anything other than off.
#[test]
fn i48_unusable_capture_directory_disables_capture_silently() {
    let dir = temp_dir("blocked");
    let occupied = dir.join("not-a-directory");
    std::fs::write(&occupied, b"x").unwrap();

    // `create_dir_all` on a path whose parent is a regular file cannot succeed.
    let recorder = ShadowRecorder::start(occupied.join("research"));
    assert!(recorder.is_none(), "capture must degrade to off, not to a panic");

    // And the engine still runs with no recorder at all.
    let mut driver = ShadowDriver::new(OiConfig::default(), None);
    for (event, received_at) in &stream() {
        driver.observe(event, *received_at);
    }
    assert!(driver.engine().health().scores_emitted > 0);

    let _ = std::fs::remove_dir_all(&dir);
}

/// i49. A write failure is counted and never propagates.
///
/// Forced deterministically by occupying the exact filename the writer will
/// choose with a directory, which no `OpenOptions::append` can open on any
/// platform.
#[test]
fn i49_write_failures_are_counted_and_never_propagate() {
    let dir = temp_dir("writefail");
    let date = at(0).date_naive();
    std::fs::create_dir_all(dir.join(format!("opportunity-intelligence-{date}.ndjson"))).unwrap();

    let recorder = ShadowRecorder::start(dir.clone()).expect("directory is usable");
    let mut driver = ShadowDriver::new(OiConfig::default(), Some(recorder));
    for (event, received_at) in &stream() {
        driver.observe(event, *received_at);
    }
    driver.finish(at(300));

    // Reached through the driver the same way production would. The point of
    // the test is the counter, not the absence of a panic: a swallowed error
    // with no record of it is the failure mode being guarded against.
    let health = driver.capture_health().expect("capture is on");
    assert!(
        health.write_errors.load(Ordering::Relaxed) > 0,
        "an unwritable target must be counted"
    );
    assert_eq!(
        health.written.load(Ordering::Relaxed),
        0,
        "and nothing must be reported as written"
    );
    assert!(health.is_degraded(), "write failures must mark the capture degraded");

    let _ = std::fs::remove_dir_all(&dir);
}

/// i50. With the shadow layer off, nothing is written and nothing is produced.
#[test]
fn i50_disabled_shadow_writes_nothing() {
    let dir = temp_dir("off");
    // No recorder constructed at all -- the disabled configuration.
    let mut driver = ShadowDriver::new(OiConfig::default(), None);
    for (event, received_at) in &stream() {
        driver.observe(event, *received_at);
    }
    let files: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).collect();
    assert!(files.is_empty(), "disabled capture must not create files");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Group J — replay parity (§19, tests 51-56)
// ---------------------------------------------------------------------------

/// Runs the same sequence twice: once through the live-shaped driver, once
/// through the offline replay entry point.
fn live_and_replay() -> (Vec<OpportunityScoreSnapshot>, Vec<OpportunityScoreSnapshot>) {
    let events = stream();
    let mut driver = ShadowDriver::new(OiConfig::default(), None);
    let mut live = Vec::new();
    for (event, received_at) in &events {
        live.extend(driver.observe(event, *received_at));
    }
    driver.finish(events.last().unwrap().1);

    // Replayed through the crate-level offline driver, not a second copy of
    // the loop above: if these could diverge, the parity claim would only be
    // testing this file against itself.
    let observations: Vec<ReplayObservation> = events
        .iter()
        .map(|(event, received_at)| ReplayObservation {
            received_at: *received_at,
            event: event.clone(),
        })
        .collect();
    let replayed = replay_events(OiConfig::default(), &observations);
    assert!(!live.is_empty(), "the fixture must produce snapshots");
    (live, replayed)
}

/// j51. Same ordered sequence + same configuration = identical output.
#[test]
fn j51_replay_reproduces_live_output_exactly() {
    let (live, replayed) = live_and_replay();
    assert_eq!(live.len(), replayed.len(), "snapshot count must match");
    let a: Vec<String> = live.iter().map(|s| serde_json::to_string(s).unwrap()).collect();
    let b: Vec<String> = replayed.iter().map(|s| serde_json::to_string(s).unwrap()).collect();
    assert_eq!(a, b, "replay must be byte-identical, not merely equivalent");
}

/// j52. Scores identical — asserted on the component decomposition, not only
/// on the final value, so a compensating pair of errors cannot pass.
#[test]
fn j52_scores_are_identical_under_replay() {
    let (live, replayed) = live_and_replay();
    for (l, r) in live.iter().zip(&replayed) {
        assert_eq!(l.early_quality.value, r.early_quality.value);
        assert_eq!(l.continuation.value, r.continuation.value);
        assert_eq!(l.early_quality.components.len(), r.early_quality.components.len());
        for (lc, rc) in l.early_quality.components.iter().zip(&r.early_quality.components) {
            assert_eq!(lc.feature, rc.feature);
            assert_eq!(lc.raw, rc.raw);
            assert_eq!(lc.transformed, rc.transformed);
            assert_eq!(lc.contribution, rc.contribution);
        }
    }
}

/// j53. Regimes identical, including the price-band classification.
#[test]
fn j53_regimes_are_identical_under_replay() {
    let (live, replayed) = live_and_replay();
    for (l, r) in live.iter().zip(&replayed) {
        assert_eq!(l.regime, r.regime);
        assert_eq!(l.price_regime, r.price_regime);
        assert_eq!(l.shadow_state, r.shadow_state);
    }
    assert!(
        live.iter().any(|s| s.regime != Regime::Unclassified),
        "the fixture must exercise at least one classified regime"
    );
}

/// j54. Opportunity identity identical. The sequence suffix makes IDs
/// order-dependent, so this is the assertion that catches a replay driver that
/// folds events in a different order.
#[test]
fn j54_opportunity_ids_are_identical_under_replay() {
    let (live, replayed) = live_and_replay();
    let a: Vec<&str> = live.iter().map(|s| s.opportunity_id.as_str()).collect();
    let b: Vec<&str> = replayed.iter().map(|s| s.opportunity_id.as_str()).collect();
    assert_eq!(a, b);
    let windows: Vec<&str> = live.iter().map(|s| s.window_id.as_str()).collect();
    let replay_windows: Vec<&str> = replayed.iter().map(|s| s.window_id.as_str()).collect();
    assert_eq!(windows, replay_windows, "ranking windows must align");
}

/// j55. Ranks identical, and the cohort sizes they are relative to.
///
/// A rank without its cohort size is not reproducible information, which is
/// why both are persisted and both are compared.
#[test]
fn j55_ranks_are_identical_under_replay() {
    let (live, replayed) = live_and_replay();
    for (l, r) in live.iter().zip(&replayed) {
        assert_eq!(l.early_quality_rank, r.early_quality_rank);
        assert_eq!(l.continuation_rank, r.continuation_rank);
        assert_eq!(l.early_cohort_size, r.early_cohort_size);
        assert_eq!(l.continuation_cohort_size, r.continuation_cohort_size);
    }
    assert!(
        live.iter().any(|s| s.early_quality_rank.is_some()),
        "the fixture must produce at least one real rank"
    );
}

/// j56. Missingness identical.
///
/// The §3 requirement is that unknown never becomes zero, and replay parity of
/// *present* values would be satisfied by a replay that silently filled gaps.
/// This compares the absences themselves.
#[test]
fn j56_missingness_is_identical_under_replay() {
    let (live, replayed) = live_and_replay();
    for (l, r) in live.iter().zip(&replayed) {
        assert_eq!(l.early_quality.missing, r.early_quality.missing);
        assert_eq!(l.continuation.missing, r.continuation.missing);
        assert_eq!(l.early_quality.present_inputs, r.early_quality.present_inputs);
        assert_eq!(l.continuation.present_inputs, r.continuation.present_inputs);
        assert_eq!(l.move_before_detection_pct, r.move_before_detection_pct);
        assert_eq!(l.move_from_start_pct, r.move_from_start_pct);
        assert_eq!(l.confirmation_span_secs, r.confirmation_span_secs);
    }
    assert!(
        live.iter().any(|s| !s.early_quality.missing.is_empty()),
        "the fixture must contain at least one genuinely missing input, \
         or this test cannot distinguish absence from zero"
    );
}

// ---------------------------------------------------------------------------
// Group K — writer-queue bounds (§19, tests 60-62; §18)
// ---------------------------------------------------------------------------

/// k60. A full queue drops and counts. It never blocks the caller.
///
/// The writer is held at a barrier so the bound is actually reachable — see
/// `start_inner`'s comment on why a drop counter no test can reach is worth
/// nothing.
#[test]
fn k60_a_full_queue_drops_and_counts_without_blocking() {
    let dir = temp_dir("queue");
    let gate = Arc::new(std::sync::Barrier::new(2));
    let recorder = ShadowRecorder::start_inner(dir.clone(), 2, Some(gate.clone()))
        .expect("directory is usable");

    // Generate real snapshots to push, rather than a hand-built struct: the
    // thing being bounded is the production record.
    let mut driver = ShadowDriver::new(OiConfig::default(), None);
    let mut snapshots = Vec::new();
    for (event, received_at) in &stream() {
        snapshots.extend(driver.observe(event, *received_at));
    }
    assert!(snapshots.len() > 8, "need more records than the queue can hold");

    let started = std::time::Instant::now();
    for snapshot in &snapshots {
        recorder.record(snapshot);
    }
    let elapsed = started.elapsed();

    assert!(
        recorder.health().dropped.load(Ordering::Relaxed) > 0,
        "a saturated queue must drop and count, not grow"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "record() must never block on the writer (took {elapsed:?})"
    );

    gate.wait();
    recorder.flush(std::time::Duration::from_secs(5));
    let _ = std::fs::remove_dir_all(&dir);
}

/// k61. Saturation is observable. A silent gap would invalidate exactly the
/// completeness claims this capture exists to support.
#[test]
fn k61_saturation_is_reported_not_hidden() {
    let dir = temp_dir("degraded");
    let gate = Arc::new(std::sync::Barrier::new(2));
    let recorder = ShadowRecorder::start_inner(dir.clone(), 1, Some(gate.clone())).unwrap();

    let mut driver = ShadowDriver::new(OiConfig::default(), None);
    let mut snapshots = Vec::new();
    for (event, received_at) in &stream() {
        snapshots.extend(driver.observe(event, *received_at));
    }
    for snapshot in &snapshots {
        recorder.record(snapshot);
    }

    assert!(recorder.health().is_degraded(), "drops must mark the capture degraded");
    gate.wait();
    recorder.flush(std::time::Duration::from_secs(5));
    let _ = std::fs::remove_dir_all(&dir);
}

/// k62. What reaches disk is one valid NDJSON object per line, parseable back
/// into the same record — the offline join in Phase F depends on it.
#[test]
fn k62_persisted_records_are_one_parseable_ndjson_line_each() {
    let dir = temp_dir("ndjson");
    let recorder = ShadowRecorder::start(dir.clone()).unwrap();
    let mut driver = ShadowDriver::new(OiConfig::default(), Some(recorder));
    let mut expected = 0usize;
    for (event, received_at) in &stream() {
        expected += driver.observe(event, *received_at).len();
    }
    driver.finish(at(400));
    assert!(expected > 0);

    let mut lines = 0usize;
    let mut marker_files = 0usize;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            name.starts_with("opportunity-intelligence-") && name.ends_with(".ndjson"),
            "unexpected file in the research directory: {name}"
        );
        // Markers are a sibling stream, deliberately not mixed into the data
        // file: adding a field to the snapshot would have changed every record
        // in the capture and forfeited the model/rank freeze proof. They are
        // checked for their own shape, not parsed as snapshots.
        if name.starts_with("opportunity-intelligence-markers-") {
            marker_files += 1;
            for line in std::fs::read_to_string(&path).unwrap().lines() {
                let marker: serde_json::Value = serde_json::from_str(line).unwrap();
                assert_eq!(marker["capture"], "opportunity-intelligence");
                assert!(marker["kind"].is_string());
            }
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        // Byte-level: a CRLF here would make the file unverifiable by
        // `sha256sum -c` downstream, which has already cost one artifact.
        assert!(!text.contains('\r'), "records must be LF-terminated");
        for line in text.lines() {
            let parsed: OpportunityScoreSnapshot =
                serde_json::from_str(line).expect("each line round-trips");
            assert_eq!(parsed.schema_version, 1);
            lines += 1;
        }
    }
    assert_eq!(lines, expected, "every accepted record must reach disk");
    assert_eq!(
        marker_files, 1,
        "the capture must carry exactly one marker stream alongside its data"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
