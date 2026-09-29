//! Step 4B-perf §9: whole-capture semantic equivalence between the
//! pre-optimization observer (`de38761`) and the optimized one.
//!
//! A deterministic, adversarial synthetic scenario drives an `Observer` over a
//! real `RotatingSink` (rotation forced), then the capture's every line is
//! normalized and compared against a golden produced by `de38761` itself,
//! together with the streaming certificate. Normalization removes only
//! values that are run-to-run non-deterministic by nature -- absolute
//! monotonic offsets (the observer's epoch is `Instant::now()` at start), the
//! guard's and writer's self-timing, and writer-thread wall-clock stamps --
//! and nothing the protocol decides: identities, sequences, market and
//! receipt times, ages (deterministic here: injected instants), provenance,
//! eligibility, expected sets, loss state, counters and the certificate are
//! all compared exactly.
//!
//! The golden was generated on a detached `de38761` worktree with this same
//! file and `OBS_EQUIV_OUT=<path> cargo test ... equivalence_dump --ignored`.

use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use market_data::status_tap::{StatusMessage, StatusTapEvent};
use market_data::{IgnitionEventKind, ScanEvent};

use super::*;

const GOLDEN: &str = "ops/observation/testdata/perf-equivalence-golden-de38761.txt";

fn t(ms: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 29, 14, 0, 0).unwrap() + chrono::Duration::milliseconds(ms)
}

fn sym(i: usize) -> String {
    format!("S{i:03}")
}

/// Keys whose values are non-deterministic across runs by nature.
const VOLATILE: [&str; 6] = ["receivedMonoNanos", "processingStartedMonoNanos", "rankCompletedMonoNanos", "overhead", "telemetry", "closedAt"];

fn normalize(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(m) => {
            for k in VOLATILE {
                if m.contains_key(k) {
                    m.insert(k.to_string(), serde_json::Value::String("<volatile>".into()));
                }
            }
            for (_, x) in m.iter_mut() {
                normalize(x);
            }
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(normalize),
        _ => {}
    }
}

/// Runs the scenario in `dir`; returns the normalized capture plus certificate.
fn scenario(dir: &Path, adversarial: bool) -> String {
    std::fs::create_dir_all(dir).unwrap();
    let run = ObserverRun::allocate(dir, "equiv", t(0), 4242).unwrap();
    let sink = RotatingSink::create(run.dir(), run.id(), 64 * 1024, 65_536, 256 << 20).unwrap();
    let no_stop = OverheadLimits { stop_window_micros: u64::MAX, stop_duty_ppm: u64::MAX, ..OverheadLimits::default() };
    let mut o = Observer::start(&run, "equiv", 4242, t(0), Box::new(sink)).unwrap().with_overhead_limits(no_stop)
        // A slow disk must never change the capture: the drain may take as
        // long as it needs.
        .with_close_timeout(Duration::from_secs(300));
    let base = Instant::now();
    let mono = |ms: u64| base + Duration::from_millis(ms);

    o.on_status(&StatusTapEvent::StreamStarted { connection: 1, full_market: true, at: t(0) });
    let confirm = |s: &str, m: i64, p: f64| ScanEvent::IgnitionEvent {
        symbol: s.into(),
        timestamp: t(m),
        price: p,
        kind: IgnitionEventKind::FollowThroughConfirmed,
    };
    // Receipts: 240 symbols, one confirmation each, with deliberate variety.
    for i in 0..240usize {
        let m = 1_000 + i as i64 * 50;
        let p = 1.0 + i as f64 * 0.037;
        o.on_receive_mono(&confirm(&sym(i), m, p), t(m + 200), mono(m as u64 + 200));
        match i % 12 {
            0 => o.on_receive_mono(&confirm(&sym(i), m + 5, p), t(m + 210), mono(m as u64 + 210)), // multiplicity
            1 => o.on_receive_mono(&confirm(&sym(i), m - 900_000, p), t(m + 220), mono(m as u64 + 220)), // out of order
            2 => o.on_receive_mono(
                &ScanEvent::BarUpdate { symbol: sym(i), timestamp: t(m - 60_000), open: p, high: p, low: p, close: p + 0.01, volume: 1_000, interval_secs: 60, is_final: true },
                t(m + 230),
                mono(m as u64 + 230),
            ),
            3 => o.on_receive_mono(
                &ScanEvent::BarUpdate { symbol: sym(i), timestamp: t(m), open: p, high: p, low: p, close: p, volume: 1, interval_secs: 60, is_final: false },
                t(m + 240),
                mono(m as u64 + 240),
            ),
            _ => {}
        }
    }
    let status = |s: &str, code: &str, m: i64| StatusTapEvent::Status(StatusMessage {
        symbol: s.into(),
        status_code: code.into(),
        status_message: Some("x \"quoted\"".into()),
        reason_code: Some("T12".into()),
        reason_message: None,
        tape: Some("C".into()),
        market_at: t(m),
        received_at: t(m + 1),
    });
    o.on_status(&status("S005", "H", 20_000));
    o.on_status(&status("S005", "T", 21_000));

    // Windows.
    let window = |o: &mut Observer, id: &str, rank_ms: i64, rank_mono_ms: u64, range: std::ops::Range<usize>, truncated: bool, extra: Vec<OpenCandidate>| {
        let mut open = Vec::new();
        let mut scored = BTreeSet::new();
        let mut prices = BTreeMap::new();
        for i in range {
            let oid = format!("{}:2026-09-29:{i}", sym(i));
            let p = 1.0 + i as f64 * 0.037;
            let opened = if i % 17 == 0 { t(-5_000) } else { t(500) }; // left-censored
            if i % 9 != 4 {
                scored.insert(oid.clone()); // unscored every 9th
                // Engine price disagrees every 11th: unknown provenance.
                let engine = if i % 11 == 6 { p + 1.0 } else if i % 12 == 2 { p + 0.01 } else { p };
                prices.insert(oid.clone(), engine);
            }
            open.push(OpenCandidate { opportunity_id: oid, symbol: sym(i), opened_at: opened });
        }
        open.extend(extra);
        o.on_window(WindowInput {
            window_id: id.into(),
            processing_started_at: t(rank_ms - 400),
            rank_completed_at: t(rank_ms),
            processing_started_mono: Some(mono(rank_mono_ms - 400)),
            rank_completed_mono: Some(mono(rank_mono_ms)),
            open,
            scored,
            engine_prices: prices,
            cohort_truncated: truncated,
        });
    };
    // W1: fresh.
    window(&mut o, "oiw-1", 13_000, 13_000, 0..120, false, vec![]);
    // W2: market age just over 30 s for the early symbols (30.001 s stale).
    window(&mut o, "oiw-2", 31_001 + 1_000, 13_100, 0..60, false, vec![]);
    // W3: receipt age over 30 s on the monotonic clock only.
    window(&mut o, "oiw-3", 13_200, 45_000, 60..140, false, vec![]);
    // W4: negative market age (anchor before the prices' market time), and an
    // ambiguous mapping (a second open lifecycle for S150).
    let dup = OpenCandidate { opportunity_id: format!("{}:2026-09-29:dup", sym(150)), symbol: sym(150), opened_at: t(600) };
    window(&mut o, "oiw-4", 900, 13_300, 140..200, false, if adversarial { vec![dup] } else { vec![] });
    // Lag, then W5 (invalid by lag, tainted) and W6 (cohort truncated), and
    // a price received after processing start.
    if adversarial {
        o.on_lag(3, t(14_000));
    }
    window(&mut o, "oiw-5", 14_500, 14_500, 150..240, false, vec![]);
    o.on_receive_mono(&confirm(&sym(239), 12_950, 99.0), t(15_000), mono(15_000));
    window(&mut o, "oiw-6", 15_100, 14_900, 200..240, adversarial, vec![]);
    window(&mut o, "oiw-7", 15_200, 15_200, 0..240, false, vec![]);
    o.on_status(&StatusTapEvent::StreamEnded { connection: 1, at: t(16_000) });
    o.on_finish(t(20_000));

    let mut out = String::new();
    let mut i = 0u32;
    loop {
        let p = run.dir().join(rotation_file_name(i));
        if !p.exists() {
            break;
        }
        for line in std::fs::read_to_string(&p).unwrap().lines() {
            let mut v: serde_json::Value = serde_json::from_str(line).unwrap();
            normalize(&mut v);
            out.push_str(&v.to_string());
            out.push('\n');
        }
        i += 1;
    }
    if i <= 1 {
        let names: Vec<_> = std::fs::read_dir(run.dir()).unwrap().map(|e| e.unwrap().file_name()).collect();
        panic!("rotation must be exercised: {i} files; dir {:?} has {names:?}", run.dir());
    }
    let (verdict, _) = stream::assess_streaming(run.dir());
    out.push_str(&format!("CERTIFICATE {}\n", verdict.label()));
    match &verdict {
        CaptureVerdict::Pass(c) => {
            let mut v = serde_json::to_value(c.as_ref()).unwrap();
            normalize(&mut v);
            out.push_str(&v.to_string());
        }
        CaptureVerdict::Indeterminate(x) => out.push_str(&format!("{x:?}")),
        CaptureVerdict::Fail(reason) => out.push_str(reason),
    }
    out.push('\n');
    out
}

fn tmp(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("obs-equiv-{tag}-{}-{}", std::process::id(), Utc::now().timestamp_nanos_opt().unwrap_or(0)));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// Writes the normalized capture to `$OBS_EQUIV_OUT` (golden generation).
#[test]
#[ignore = "golden generation; run on the baseline tree"]
fn equivalence_dump() {
    let dir = tmp("dump");
    let mut out = scenario(&dir.join("adversarial"), true);
    out.push_str(&scenario(&dir.join("clean"), false));
    std::fs::write(std::env::var("OBS_EQUIV_OUT").expect("OBS_EQUIV_OUT"), out).unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_optimized_observer_reproduces_the_pre_optimization_capture() {
    let dir = tmp("check");
    let mut out = scenario(&dir.join("adversarial"), true);
    out.push_str(&scenario(&dir.join("clean"), false));
    let _ = std::fs::remove_dir_all(&dir);
    let golden_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(GOLDEN);
    let golden = std::fs::read_to_string(&golden_path).unwrap();
    assert!(golden.contains("CERTIFICATE FAIL") && golden.contains("CERTIFICATE PASS"), "golden covers both verdicts");
    if out != golden {
        for (i, (a, b)) in out.lines().zip(golden.lines()).enumerate() {
            assert_eq!(a, b, "first difference at line {i}");
        }
        assert_eq!(out.lines().count(), golden.lines().count(), "line count");
    }
}
