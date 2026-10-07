//! **Combined load** (brief section 9).
//!
//! Each subsystem is bounded and tested on its own elsewhere. That is not
//! sufficient: on September 16 all four failed *at once*, under one market, and
//! the interaction is the thing the deployment has to survive. So this drives
//! them simultaneously —
//!
//! * the opportunity engine opening and holding its population,
//! * ranking windows emitting the whole cohort synchronously,
//! * the OI writer draining those records,
//! * measurement settlement bursts,
//! * discovery scan ticks and the ignition print stream,
//!
//! — and requires that *nothing* is lost inside the declared envelope.
//!
//! # How the profile relates to the real session
//!
//! Volumes are driven at full burst shape and full population, compressed in
//! wall-clock time: the test emits as fast as it can rather than spreading a
//! 6.5-hour session over 6.5 hours. That is strictly harsher than production,
//! which is the right direction for a load test — a writer that keeps up with
//! an unthrottled producer certainly keeps up with a throttled one.
//!
//! What is *not* compressed is anything the bounds depend on: the open
//! population, the ranking cohort size, the settlement burst size and the scan
//! tick's record count and byte size are all the measured September-16 figures.
//!
//! Measured from the preserved artifacts:
//!
//! | quantity | September 16 |
//! |---|---|
//! | open opportunities, max (uncensored) | 4,808 |
//! | OI snapshot mean size | 3,854 B |
//! | measurement settlement burst, max | 459 episodes |
//! | discovery scan tick | 68-69 records, 2.85 MB |
//! | discovery heaviest second | 3,190 records, 2.28 MB |

use std::sync::atomic::Ordering;
use std::sync::Arc;

use backtest_metrics::opportunity::OiConfig;
use chrono::{DateTime, TimeZone, Utc};
use market_data::events::ConsolidationStrategy;
use market_data::{ConsolidationEventKind, IgnitionEventKind, ScanEvent};
use serde_json::json;

use crate::measurement::{MeasurementCollector, MeasurementRecorder};
use crate::opportunity_shadow::{ShadowDriver, ShadowRecorder};

// --- the measured September-16 profile ---------------------------------------

/// Uncensored maximum open population of the September-16 regular session.
const S16_OPEN_POPULATION: i64 = 4_808;
/// Largest settlement burst observed (episodes sharing one `opened_at`).
const S16_SETTLEMENT_BURST: usize = 459;
/// Ranking windows driven per scenario. Not the session's 780 -- one window
/// emits the whole cohort, so 780 of them at this population would be 9.7 GB.
/// The bound being tested is per-window burst, not session total.
const WINDOWS: usize = 10;

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

fn momentum(symbol: &str, t: DateTime<Utc>, overall: f64) -> ScanEvent {
    ScanEvent::MomentumUpdate {
        symbol: symbol.into(),
        timestamp: t,
        volume_confirmation: 0.55,
        structure: 0.45,
        ma_slope: 0.6,
        wick_rejection: 0.7,
        overall,
        qualifies: overall >= 0.6,
    }
}

fn micro(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ScanEvent::ConsolidationEvent {
        symbol: symbol.into(),
        timestamp: t,
        price,
        kind: ConsolidationEventKind::EntryTriggered,
        strategy: ConsolidationStrategy::Micropullback,
    }
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "oi-combined-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A discovery scan tick, at its measured shape: one `scan_started`, 65
/// `snapshot_batch`, a `coverage`, a `scan_completed` and a `snapshot_complete`.
fn emit_scan_tick(scan: usize) {
    use market_data::discovery_audit::emit;
    emit("scan_started", json!({"scan_id": scan, "feed": "sip",
        "universe": vec!["SYM"; 13_001]}));
    for batch in 0..65 {
        emit("snapshot_batch", json!({"scan_id": scan, "batch": batch,
            "requested": vec!["SYM"; 200], "pad": "x".repeat(38_000)}));
    }
    emit("coverage", json!({"scan_id": scan, "pad": "x".repeat(250_000)}));
    emit("scan_completed", json!({"scan_id": scan, "pad": "x".repeat(150_000)}));
    emit("snapshot_complete", json!({"scan_id": scan}));
}

/// The continuous ignition print stream, at its measured 235-byte shape.
fn emit_ignition_prints(count: usize) {
    use market_data::discovery_audit::emit;
    for i in 0..count {
        emit(
            "ignition",
            json!({"market_at": at(i as i64 % 600), "price": 1.91,
                "stage": if i % 3 == 0 { "confirmed" } else { "rejected" },
                "symbol": format!("S{:05}", i % 8_886)}),
        );
    }
}

/// Everything one scenario measured.
#[derive(Debug)]
struct Outcome {
    label: String,
    population: i64,
    elapsed: std::time::Duration,
    oi: backtest_metrics::completeness::WriterCapture,
    measurement: backtest_metrics::completeness::WriterCapture,
    discovery_queue_lost: u64,
    discovery_write_errors: u64,
    discovery_attempted: u64,
    discovery_queue_peak: u64,
    discovery_queued_bytes_peak: u64,
    engine_evictions: u64,
    engine_peak: usize,
    engine_capacity: usize,
}

impl Outcome {
    fn report(&self) {
        println!("\n--- {} (population {}) ---", self.label, self.population);
        println!("  wall clock                 {:?}", self.elapsed);
        println!(
            "  OI          attempted {:>8}  written {:>8}  dropped {:>6}  errors {:>4}  queue peak {:>6}/{} ({} B peak)",
            self.oi.attempted, self.oi.written, self.oi.dropped, self.oi.write_errors,
            self.oi.queue_peak, self.oi.queue_capacity, self.oi.queued_bytes_peak
        );
        println!(
            "  measurement attempted {:>8}  written {:>8}  dropped {:>6}  errors {:>4}  queue peak {:>6}/{}",
            self.measurement.attempted, self.measurement.written, self.measurement.dropped,
            self.measurement.write_errors, self.measurement.queue_peak,
            self.measurement.queue_capacity
        );
        println!(
            "  discovery   attempted {:>8}  queue lost {:>5}  errors {:>4}  queue peak {:>6} ({} B peak)",
            self.discovery_attempted, self.discovery_queue_lost, self.discovery_write_errors,
            self.discovery_queue_peak, self.discovery_queued_bytes_peak
        );
        println!(
            "  engine      peak open {:>8}  capacity {:>7}  evictions {:>6}",
            self.engine_peak, self.engine_capacity, self.engine_evictions
        );
        println!("  bytes written              OI {} / measurement {}",
            self.oi.bytes_written, self.measurement.bytes_written);
    }

    /// Everything the section-9 envelope requires.
    fn assert_lossless(&self) {
        assert_eq!(self.oi.dropped, 0, "{}: OI dropped records", self.label);
        assert_eq!(self.oi.write_errors, 0, "{}: OI write errors", self.label);
        assert_eq!(
            self.oi.written + self.oi.dropped + self.oi.write_errors,
            self.oi.attempted,
            "{}: OI accounting must reconcile",
            self.label
        );
        assert_eq!(self.measurement.dropped, 0, "{}: measurement dropped", self.label);
        assert_eq!(self.measurement.write_errors, 0, "{}: measurement write errors", self.label);
        assert_eq!(
            self.measurement.written + self.measurement.dropped + self.measurement.write_errors,
            self.measurement.attempted,
            "{}: measurement accounting must reconcile",
            self.label
        );
        assert_eq!(self.discovery_queue_lost, 0, "{}: discovery queue loss", self.label);
        assert_eq!(self.discovery_write_errors, 0, "{}: discovery write errors", self.label);
        assert_eq!(self.engine_evictions, 0, "{}: engine capacity evictions", self.label);
        assert!(
            self.engine_peak < self.engine_capacity,
            "{}: engine peak {} reached capacity {}",
            self.label,
            self.engine_peak,
            self.engine_capacity
        );
        // Bounded: occupancy never exceeded what was declared.
        assert!(
            self.oi.queue_peak <= self.oi.queue_capacity,
            "{}: OI queue exceeded its bound",
            self.label
        );
        assert!(
            self.oi.queued_bytes_peak <= self.oi.queue_capacity_bytes,
            "{}: OI queue exceeded its byte bound",
            self.label
        );
        assert!(self.measurement.queue_peak <= self.measurement.queue_capacity);
    }
}

/// Drives one combined scenario.
fn run_scenario(label: &str, population: i64, windows: usize) -> Outcome {
    let oi_dir = temp_dir("oi");
    let meas_dir = temp_dir("meas");

    let oi_recorder = ShadowRecorder::start(oi_dir.clone()).expect("OI writer");
    let oi_health = oi_recorder.health().clone();
    let meas_recorder = MeasurementRecorder::start(meas_dir.clone()).expect("measurement writer");
    let meas_health = meas_recorder.health().clone();

    let mut driver = ShadowDriver::new(OiConfig::default(), Some(oi_recorder));
    let engine_health = driver.engine_health().clone();

    let discovery_before = market_data::discovery_audit::health();
    let started = std::time::Instant::now();

    // --- discovery, on its own thread ------------------------------------
    // Concurrent on purpose: the point of this test is that the three writers
    // and the engine are under load *at the same time*, sharing one disk.
    let discovery = std::thread::spawn(move || {
        for scan in 0..8 {
            // 69 records / 2.85 MB per tick, the measured September-16 shape.
            emit_scan_tick(scan);
            emit_ignition_prints(2_000);
        }
    });

    // --- measurement, on its own thread ----------------------------------
    let measurement = std::thread::spawn(move || {
        let mut collector = MeasurementCollector::new();
        for n in 0..(S16_SETTLEMENT_BURST as i64 * 4) {
            collector.observe(&confirmed(&format!("E{n:05}"), at(n), 10.0), at(n));
        }
        // Settlement releases everything whose deadline matured, as one burst.
        let episodes = collector.finish(at(1_000_000));
        for chunk in episodes.chunks(S16_SETTLEMENT_BURST) {
            for episode in chunk {
                meas_recorder.record_episode(episode);
            }
        }
        collector.publish_health();
        meas_recorder.flush(std::time::Duration::from_secs(60));
        (collector.capacity_evictions(), collector.pending_peak())
    });

    // --- the opportunity engine and its writer, on this thread ------------
    // Open the whole population at one instant, exactly as a busy open does.
    for i in 0..population {
        let t = at(1);
        let _ = driver.observe(&confirmed(&format!("S{i:06}"), t, 10.0 + (i % 97) as f64 * 0.05), t);
    }
    // Give every opportunity a momentum surface so the cohort is scoreable
    // rather than a column of nulls -- an unscoreable cohort would understate
    // the record size, which is the quantity the byte bound is sized against.
    for i in 0..population {
        let t = at(2);
        let _ = driver.observe(&momentum(&format!("S{i:06}"), t, 0.7), t);
    }
    // Ranking windows: each emits the entire cohort synchronously. This is the
    // burst the OI queue exists to absorb, and the one 64 slots could not.
    //
    // Every symbol is refreshed in every window, which is both what a real
    // session does -- a symbol that keeps trading keeps its opportunity alive,
    // which is precisely why measured lifetime is 3,752s and not 300s -- and
    // what keeps the cohort at full size. Touching only one symbol let the rest
    // cross the 300s inactivity boundary mid-run, so the final windows ranked an
    // almost-empty set and the burst under test quietly stopped happening.
    for w in 0..windows {
        let t = at(3 + (w as i64 + 1) * 30);
        for i in 0..population {
            let _ = driver.observe(&momentum(&format!("S{i:06}"), t, 0.7), t);
        }
    }
    driver.finish(at(3 + (windows as i64 + 2) * 30));

    let (meas_evictions, meas_pending_peak) = measurement.join().expect("measurement thread");
    discovery.join().expect("discovery thread");
    let elapsed = started.elapsed();

    let discovery_after = market_data::discovery_audit::health();
    let outcome = Outcome {
        label: label.to_string(),
        population,
        elapsed,
        oi: oi_health.snapshot(),
        measurement: meas_health.snapshot(),
        discovery_queue_lost: discovery_after.queue_lost - discovery_before.queue_lost,
        discovery_write_errors: discovery_after.write_errors - discovery_before.write_errors,
        discovery_attempted: discovery_after.attempted - discovery_before.attempted,
        discovery_queue_peak: discovery_after.queue_peak,
        discovery_queued_bytes_peak: discovery_after.queued_bytes_peak,
        engine_evictions: engine_health.capacity_evictions.load(Ordering::Relaxed),
        engine_peak: engine_health.peak.load(Ordering::Relaxed),
        engine_capacity: engine_health.capacity.load(Ordering::Relaxed),
    };
    assert_eq!(meas_evictions, 0, "measurement pending capacity must not bind in this profile");
    assert!(meas_pending_peak > 0, "the measurement profile must actually build a pending set");

    let _ = std::fs::remove_dir_all(&oi_dir);
    let _ = std::fs::remove_dir_all(&meas_dir);
    outcome
}

/// Points the process-global discovery recorder at a temp directory.
///
/// `RECORDER` is a `OnceLock`, so the first caller in this test binary decides
/// where discovery writes for the whole run. That is why the combined scenarios
/// are one test rather than three: sharing one recorder across tests that run
/// in parallel would make the counter deltas meaningless.
fn init_discovery() -> std::path::PathBuf {
    let dir = temp_dir("discovery");
    std::env::set_var("DISCOVERY_AUDIT_DIR", &dir);
    // Generous, so the daily budget and the directory ceiling are never what
    // this test measures -- those have their own tests in `discovery_audit`.
    std::env::set_var("DISCOVERY_AUDIT_PER_FILE_BYTES", "1073741824");
    std::env::set_var("DISCOVERY_AUDIT_DAILY_BYTES", "137438953472");
    std::env::set_var("DISCOVERY_AUDIT_MAX_BYTES", "274877906944");
    assert!(market_data::discovery_audit::enabled(), "discovery capture must be on");
    dir
}

/// The whole section-9 requirement, in three phases.
#[test]
fn combined_load_september_16_then_double_then_overload() {
    let discovery_dir = init_discovery();

    // --- phase 1: the September-16 profile -------------------------------
    let s16 = run_scenario("September-16 pressure", S16_OPEN_POPULATION, WINDOWS);
    s16.report();
    s16.assert_lossless();
    assert_eq!(
        s16.engine_peak, S16_OPEN_POPULATION as usize,
        "every opportunity the session opened must still have been held"
    );
    assert!(
        s16.oi.attempted >= (S16_OPEN_POPULATION as u64) * (WINDOWS as u64),
        "the profile must actually emit a full cohort per window"
    );

    // --- phase 2: twice that -----------------------------------------------
    let double = run_scenario("2x observed pressure", S16_OPEN_POPULATION * 2, WINDOWS);
    double.report();
    double.assert_lossless();
    assert!(
        double.oi.attempted > s16.oi.attempted,
        "2x pressure must actually be more work"
    );

    // --- phase 3: overload, then recovery ----------------------------------
    // Past the engine's declared envelope. Loss is now *expected*; what is
    // required is that it stays bounded, is counted exactly, and that the
    // subsystem returns to normal afterwards.
    let capacity = OiConfig::default().max_open_opportunities();
    let overload = run_scenario("above envelope", capacity as i64 + 1_200, 2);
    overload.report();
    assert!(
        overload.engine_evictions > 0,
        "above the envelope the engine must evict explicitly, not silently absorb"
    );
    assert_eq!(
        overload.engine_peak, capacity,
        "and the open set must sit exactly at its bound, never above it"
    );
    assert_eq!(
        overload.oi.written + overload.oi.dropped + overload.oi.write_errors,
        overload.oi.attempted,
        "accounting must still reconcile under overload"
    );
    assert!(
        overload.oi.queued_bytes_peak <= overload.oi.queue_capacity_bytes,
        "memory must stay bounded under overload"
    );

    // --- recovery ----------------------------------------------------------
    let recovered = run_scenario("recovery", S16_OPEN_POPULATION, WINDOWS);
    recovered.report();
    recovered.assert_lossless();

    println!(
        "\ncombined load complete: {:?} + {:?} + {:?} + {:?}",
        s16.elapsed, double.elapsed, overload.elapsed, recovered.elapsed
    );
    let _ = std::fs::remove_dir_all(&discovery_dir);
}

/// Exact engine peak tracking (section 13).
///
/// `peak` is what an operator reads against `capacity` to decide whether a
/// session had headroom, so an approximate peak is worse than none.
#[test]
fn engine_peak_is_tracked_exactly() {
    let mut driver = ShadowDriver::new(OiConfig::default(), None);
    let health = driver.engine_health().clone();

    for i in 0..500i64 {
        let t = at(1);
        let _ = driver.observe(&confirmed(&format!("P{i:05}"), t, 10.0), t);
        assert_eq!(
            health.open.load(Ordering::Relaxed),
            (i + 1) as usize,
            "open count must be exact at every step"
        );
        assert_eq!(health.peak.load(Ordering::Relaxed), (i + 1) as usize);
    }

    // A quiet period retires the set. The peak must *not* fall with it.
    let quiet = at(1 + 300 + 10);
    let _ = driver.observe(&confirmed("RECOVER", quiet, 10.0), quiet);
    assert!(health.open.load(Ordering::Relaxed) < 10, "inactivity must clear the set");
    assert_eq!(
        health.peak.load(Ordering::Relaxed),
        500,
        "the high-water mark must survive the population falling"
    );
    assert_eq!(health.capacity.load(Ordering::Relaxed), 16_375);
    assert_eq!(health.capacity_evictions.load(Ordering::Relaxed), 0);
    assert_eq!(health.opportunities_opened.load(Ordering::Relaxed), 501);
}

/// The completeness health surface aggregates every capture (section 13).
#[test]
fn completeness_health_aggregates_every_capture() {
    let research = Arc::new(crate::research_health::ResearchHealth::default());

    // Nothing registered: every capture reads as absent, and the checker must
    // therefore answer INDETERMINATE rather than VALID.
    let empty = research.report();
    assert!(empty.opportunity_intelligence.is_none());
    assert!(empty.measurement.is_none());
    assert!(empty.opportunity_engine.is_none());
    assert!(!empty.any_known_loss(), "absent is not the same as lossy");

    let oi_dir = temp_dir("agg-oi");
    let meas_dir = temp_dir("agg-meas");
    let oi_recorder = ShadowRecorder::start(oi_dir.clone()).unwrap();
    research.set_opportunity_intelligence(oi_recorder.health().clone());
    let meas_recorder = MeasurementRecorder::start(meas_dir.clone()).unwrap();
    research.set_measurement(meas_recorder.health().clone());

    let mut driver = ShadowDriver::new(OiConfig::default(), Some(oi_recorder));
    research.set_engine(driver.engine_health().clone());
    research.set_oi_config_fingerprint(OiConfig::default().fingerprint());

    for i in 0..200i64 {
        let t = at(1);
        let _ = driver.observe(&confirmed(&format!("A{i:04}"), t, 10.0), t);
    }
    let t = at(64);
    let _ = driver.observe(&micro("A0000", t, 11.0), t);
    driver.finish(at(200));

    let report = research.report();
    assert!(!report.any_known_loss(), "a clean run must not report loss");
    let oi = report.opportunity_intelligence.clone().expect("OI registered");
    assert!(oi.attempted > 0, "the surface must see real traffic");
    assert_eq!(oi.written + oi.dropped + oi.write_errors, oi.attempted);
    assert_eq!(oi.dropped, 0);
    let engine = report.opportunity_engine.clone().expect("engine registered");
    assert_eq!(engine.opportunities_opened, 200);
    assert_eq!(engine.capacity, 16_375);
    assert_eq!(engine.capacity_evictions, 0);
    assert_eq!(
        report.oi_config_fingerprint.as_deref(),
        Some(OiConfig::default().fingerprint().as_str())
    );
    assert!(report.measurement.is_some());

    // And the surface feeds the verdict directly: a clean report plus complete
    // artifacts is what VALID is made of.
    assert!(report.discovery.is_some(), "discovery is always reported, even when off");

    let _ = std::fs::remove_dir_all(&oi_dir);
    let _ = std::fs::remove_dir_all(&meas_dir);
}
