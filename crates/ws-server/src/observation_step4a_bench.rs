//! Step 4A capacity characterisation for the observation layer.
//!
//! **Measurements, not assertions**, and `#[ignore]`d so they never gate CI.
//! Everything here is synthetic: no observation capture, no market data, no
//! outcome. Run it as a release build on the host whose numbers matter:
//!
//! ```text
//! cargo test --release --offline -p ws-server step4a_bench -- --ignored --nocapture --test-threads=1
//! ```
//!
//! It reports, for cohorts of 3,400 / 6,000 / 10,000 / 16,375 candidates per
//! window, the consumer-thread cost of a ranking window (eligibility,
//! canonical tuples, serialization, and enqueue onto the real async writer),
//! per-record serialized sizes for the storage decomposition, writer drain
//! throughput, rotation latency, and offline reader / authentication /
//! certification cost with the reader's memory where the OS exposes it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use market_data::{IgnitionEventKind, ScanEvent};

use super::*;

const COHORTS: [usize; 4] = [3_400, 6_000, 10_000, 16_375];

struct Tmp(PathBuf);
impl Tmp {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "obs-4a-{tag}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn at_ms(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(1_790_000_000_000 + ms).unwrap()
}

/// Counts bytes and records; writes nothing. Isolates consumer-thread compute
/// and serialization from any I/O.
#[derive(Default)]
struct CountingSink {
    records: u64,
    bytes: u64,
    by_kind: BTreeMap<&'static str, (u64, u64)>,
}
impl ObservationSink for CountingSink {
    fn write(&mut self, record: &ObservationRecord) -> std::io::Result<()> {
        let line = serde_json::to_string(record).unwrap();
        self.write_serialized(record, &line)
    }
    fn write_serialized(&mut self, record: &ObservationRecord, line: &str) -> std::io::Result<()> {
        self.records += 1;
        self.bytes += line.len() as u64 + 1;
        let kind = match record {
            ObservationRecord::RunStart { .. } => "run_start",
            ObservationRecord::Receipt { .. } => "receipt",
            ObservationRecord::Lag { .. } => "lag",
            ObservationRecord::WindowBegin { .. } => "window_begin",
            ObservationRecord::Candidate { .. } => "candidate",
            ObservationRecord::Status { .. } => "status",
            ObservationRecord::StatusStream { .. } => "status_stream",
            ObservationRecord::WindowClose { .. } => "window_close",
            ObservationRecord::Stopped { .. } => "stopped",
            ObservationRecord::FileStart { .. } => "file_start",
            ObservationRecord::FileClose { .. } => "file_close",
            ObservationRecord::RunEnd { .. } => "run_end",
        };
        let e = self.by_kind.entry(kind).or_default();
        e.0 += 1;
        e.1 += line.len() as u64 + 1;
        KIND_BYTES.with(|k| {
            let mut k = k.borrow_mut();
            let e = k.entry(kind).or_default();
            e.0 += 1;
            e.1 += line.len() as u64 + 1;
        });
        Ok(())
    }
    fn counters(&self) -> WriterCounters {
        WriterCounters { attempted: self.records, written: self.records, ..Default::default() }
    }
    fn close(&mut self, _run_id: &str, _at: DateTime<Utc>) -> std::io::Result<()> {
        Ok(())
    }
}

/// Realistic-width symbols: 1-5 letters, like the live universe.
fn symbol(i: usize) -> String {
    let mut s = String::new();
    let mut n = i;
    loop {
        s.push((b'A' + (n % 26) as u8) as char);
        n /= 26;
        if n == 0 || s.len() >= 5 {
            break;
        }
    }
    s
}

/// Production-shaped identifiers: a host namespace run id and
/// `SYMBOL:YYYY-MM-DD:msOfDay` opportunity ids.
const NAMESPACE: &str = "srv1170872";

fn run_for(dir: &Path) -> ObserverRun {
    ObserverRun::allocate(dir, NAMESPACE, at_ms(0), 12_345).unwrap()
}

/// Feeds one price-carrying confirmation per symbol, then builds the window.
fn prime(observer: &mut Observer, n: usize, base_ms: i64) -> WindowInput {
    let mut open = Vec::with_capacity(n);
    let mut engine_prices = BTreeMap::new();
    let mut scored = BTreeSet::new();
    for i in 0..n {
        let sym = symbol(i);
        let price = 1.0 + (i % 2_400) as f64 * 0.0103;
        let ts = at_ms(base_ms + (i as i64 % 25_000));
        observer.on_receive(
            &ScanEvent::IgnitionEvent {
                symbol: sym.clone(),
                timestamp: ts,
                price,
                kind: IgnitionEventKind::FollowThroughConfirmed,
            },
            ts,
        );
        let id = format!("{sym}:2026-09-29:{}", 28_800_000 + i);
        engine_prices.insert(id.clone(), price);
        scored.insert(id.clone());
        open.push(OpenCandidate { opportunity_id: id, symbol: sym, opened_at: at_ms(base_ms - 60_000) });
    }
    WindowInput {
        window_id: format!("oiw-{base_ms}"),
        processing_started_at: at_ms(base_ms + 29_000),
        rank_completed_at: at_ms(base_ms + 29_500),
        processing_started_mono: None,
        rank_completed_mono: None,
        open,
        scored,
        engine_prices,
        cohort_truncated: false,
    }
}

fn pct(mut v: Vec<f64>) -> String {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |p: f64| v[((v.len() as f64 - 1.0) * p).round() as usize];
    format!("n={} p50={:.2} p95={:.2} p99={:.2} max={:.2}", v.len(), q(0.50), q(0.95), q(0.99), v[v.len() - 1])
}

fn rss() -> String {
    std::fs::read_to_string("/proc/self/status")
        .map(|s| {
            s.lines()
                .filter(|l| l.starts_with("VmRSS") || l.starts_with("VmHWM"))
                .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_else(|_| "n/a (no /proc on this OS)".to_string())
}

#[test]
#[ignore = "Step 4A characterisation; release build, run with --ignored"]
fn step4a_bench() {
    println!("== host: {} logical CPUs; release={}", std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0), !cfg!(debug_assertions));

    // ------------------------------------------------------------------
    // 1. Record sizes (storage decomposition inputs)
    // ------------------------------------------------------------------
    {
        let t = Tmp::new("sizes");
        let run = run_for(t.path());
        let sink = CountingSink::default();
        let mut o = Observer::start(&run, NAMESPACE, 12_345, at_ms(0), Box::new(sink)).unwrap();
        // Receipt shapes: finalised bar, live bar, momentum, ignition.
        let bar = |final_: bool| ScanEvent::BarUpdate {
            symbol: "ABCD".into(), timestamp: at_ms(1_000), open: 3.14, high: 3.2, low: 3.1, close: 3.17,
            volume: 123_456, interval_secs: 60, is_final: final_,
        };
        let receipts = [
            ("bar_update(final)", bar(true)),
            ("bar_update(live)", bar(false)),
            ("ignition_event", ScanEvent::IgnitionEvent { symbol: "ABCD".into(), timestamp: at_ms(1_000), price: 3.17, kind: IgnitionEventKind::CandidateOpened }),
        ];
        for (label, ev) in &receipts {
            let before = sink_bytes(&o);
            o.on_receive(ev, at_ms(1_001));
            println!("SIZE receipt {label}: {} B", sink_bytes(&o) - before);
        }
        for n in [1usize, 100, 3_400] {
            let mut w = prime(&mut o, n, 100_000 * n as i64);
            w.window_id = format!("oiw-size-{n}");
            let kinds_before = sink_kinds(&o);
            o.on_window(w);
            let kinds_after = sink_kinds(&o);
            let d = |k: &str| {
                let a = kinds_after.get(k).copied().unwrap_or((0, 0));
                let b = kinds_before.get(k).copied().unwrap_or((0, 0));
                (a.0 - b.0, a.1 - b.1)
            };
            let (cn, cb) = d("candidate");
            let (_, wb) = d("window_begin");
            let (_, wc) = d("window_close");
            println!(
                "SIZE window n={n}: candidate {} B/row, window_begin {} B total ({} B/candidate), window_close {} B",
                cb / cn.max(1), wb, wb / n as u64, wc
            );
        }
        o.on_lag(3, at_ms(2_000));
        let before = sink_bytes(&o);
        o.on_finish(at_ms(3_000));
        println!("SIZE run_end: {} B", sink_bytes(&o) - before);
        println!("SIZE lag: {} B", serde_json::to_string(&ObservationRecord::Lag { run_id: run.id().into(), sequence: 123_456_789, skipped: 17, at: at_ms(1) }).unwrap().len() + 1);
        println!("SIZE file_start: {} B, file_close: {} B",
            serde_json::to_string(&ObservationRecord::FileStart { run_id: run.id().into(), file_name: "observations-12.ndjson".into(), sequence: 12, previous_file: "observations-11.ndjson".into() }).unwrap().len() + 1,
            serde_json::to_string(&ObservationRecord::FileClose { run_id: run.id().into(), file_name: "observations-12.ndjson".into(), records_written: 1_234_567, closed_at: at_ms(1), next_file: Some("observations-13.ndjson".into()) }).unwrap().len() + 1);
    }

    // ------------------------------------------------------------------
    // 2. Consumer-thread window cost: compute + serialize (counting sink),
    //    and compute + serialize + enqueue (real async writer).
    // ------------------------------------------------------------------
    for &n in &COHORTS {
        let reps = if n <= 6_000 { 40 } else { 15 };
        let mut compute = Vec::new();
        let t = Tmp::new("win");
        let run = run_for(t.path());
        let mut o = Observer::start(&run, NAMESPACE, 1, at_ms(0), Box::new(CountingSink::default())).unwrap()
            .with_capture_max_bytes(u64::MAX);
        let w = prime(&mut o, n, 1_000_000);
        for r in 0..reps {
            let mut wi = w.clone();
            wi.window_id = format!("oiw-{r}");
            let s = Instant::now();
            o.on_window(wi);
            compute.push(s.elapsed().as_secs_f64() * 1e3);
        }
        let per_row: Vec<f64> = compute.iter().map(|ms| ms * 1e3 / n as f64).collect();
        println!("WINDOW n={n} compute+serialize ms: {}", pct(compute));
        println!("WINDOW n={n} per-candidate us:    {}", pct(per_row));

        // Real async file writer, queue sized to hold a full window.
        let t2 = Tmp::new("win-disk");
        let run2 = run_for(t2.path());
        let writer = FileRecordWriter::create(run2.dir(), RUN_FILE_NAME).unwrap();
        let sink = AsyncSink::with_capacity(RUN_FILE_NAME, Box::new(writer), 65_536, 256 << 20);
        let mut o2 = Observer::start(&run2, NAMESPACE, 1, at_ms(0), Box::new(sink)).unwrap()
            .with_capture_max_bytes(u64::MAX);
        let w2 = prime(&mut o2, n, 1_000_000);
        let mut enq = Vec::new();
        let mut drain = Vec::new();
        for r in 0..reps {
            let mut wi = w2.clone();
            wi.window_id = format!("oiw-{r}");
            let s = Instant::now();
            o2.on_window(wi);
            enq.push(s.elapsed().as_secs_f64() * 1e3);
            let s = Instant::now();
            o2.sink.drain(Duration::from_secs(60)).unwrap();
            drain.push(s.elapsed().as_secs_f64() * 1e3);
        }
        let tel = o2.sink.telemetry().unwrap();
        let c = o2.sink.counters();
        println!("WINDOW n={n} +enqueue (consumer) ms: {}", pct(enq));
        println!("WINDOW n={n} writer drain ms:        {}", pct(drain));
        println!("WINDOW n={n} queue peak {} records / {} B; dropped {}", tel.queue_peak, tel.queued_bytes_peak, c.dropped);
        o2.on_finish(at_ms(9_999_999));
    }

    // ------------------------------------------------------------------
    // 3. Receive hook (consumer thread), counting sink and real writer.
    // ------------------------------------------------------------------
    {
        let t = Tmp::new("recv");
        let run = run_for(t.path());
        let mut o = Observer::start(&run, NAMESPACE, 1, at_ms(0), Box::new(CountingSink::default())).unwrap()
            .with_capture_max_bytes(u64::MAX);
        let events: Vec<ScanEvent> = (0..3_400).map(|i| ScanEvent::BarUpdate {
            symbol: symbol(i), timestamp: at_ms(i as i64), open: 3.1, high: 3.2, low: 3.0, close: 3.15,
            volume: 1_000 + i as u64, interval_secs: 30, is_final: false,
        }).collect();
        let mut v = Vec::with_capacity(200_000);
        for k in 0..200_000usize {
            let ev = &events[k % events.len()];
            let s = Instant::now();
            o.on_receive(ev, at_ms(k as i64));
            v.push(s.elapsed().as_secs_f64() * 1e6);
        }
        println!("RECEIVE hook+serialize (no I/O) us: {}", pct(v));

        let t2 = Tmp::new("recv-disk");
        let run2 = run_for(t2.path());
        let writer = FileRecordWriter::create(run2.dir(), RUN_FILE_NAME).unwrap();
        let sink = AsyncSink::with_capacity(RUN_FILE_NAME, Box::new(writer), 65_536, 256 << 20);
        let mut o2 = Observer::start(&run2, NAMESPACE, 1, at_ms(0), Box::new(sink)).unwrap()
            .with_capture_max_bytes(u64::MAX);
        let mut v2 = Vec::with_capacity(200_000);
        let s_all = Instant::now();
        for k in 0..200_000usize {
            let ev = &events[k % events.len()];
            let s = Instant::now();
            o2.on_receive(ev, at_ms(k as i64));
            v2.push(s.elapsed().as_secs_f64() * 1e6);
        }
        o2.sink.drain(Duration::from_secs(120)).unwrap();
        let total = s_all.elapsed();
        let c = o2.sink.counters();
        println!("RECEIVE hook+enqueue (real writer) us: {}", pct(v2));
        println!("WRITER sustained: {} receipts in {:?} = {:.0} rec/s; dropped {}", c.written, total, c.written as f64 / total.as_secs_f64(), c.dropped);
        o2.on_finish(at_ms(9_999_999));
    }

    // ------------------------------------------------------------------
    // 4. Rotation latency and a rotated capture's offline read path.
    // ------------------------------------------------------------------
    for &n in &[3_400usize, 16_375] {
        let windows = if n == 3_400 { 20 } else { 4 };
        let t = Tmp::new("rot");
        let run = run_for(t.path());
        let sink = RotatingSink::create(run.dir(), run.id(), 16 << 20, 65_536, 256 << 20).unwrap();
        let mut o = Observer::start(&run, NAMESPACE, 1, at_ms(0), Box::new(sink)).unwrap()
            .with_capture_max_bytes(u64::MAX);
        let w = prime(&mut o, n, 1_000_000);
        let mut per_window = Vec::new();
        for r in 0..windows {
            let mut wi = w.clone();
            wi.window_id = format!("oiw-{r}");
            let s = Instant::now();
            o.on_window(wi);
            per_window.push(s.elapsed().as_secs_f64() * 1e3);
        }
        o.on_finish(at_ms(9_999_999));
        let files: Vec<u64> = std::fs::read_dir(run.dir()).unwrap().map(|e| e.unwrap().metadata().unwrap().len()).collect();
        let bytes: u64 = files.iter().sum();
        println!("ROTATE n={n} x{windows} windows @16MiB: {} files, {} B; window ms incl. rotations: {}", files.len(), bytes, pct(per_window));

        let rss0 = rss();
        let s = Instant::now();
        let acquired = acquire(run.dir()).expect("acquire");
        let t_acq = s.elapsed();
        let rss1 = rss();
        let s = Instant::now();
        let authed = authenticate(acquired).expect("authenticate");
        let t_auth = s.elapsed();
        let s = Instant::now();
        let cert = Certificate::issue(&authed).expect("certificate");
        let t_cert = s.elapsed();
        println!(
            "READ n={n}: {} B, {} candidates; acquire {:?} ({:.1} MB/s), authenticate {:?}, certify {:?}; mem before [{rss0}] after acquire [{rss1}]",
            bytes, cert.candidates, t_acq, bytes as f64 / 1e6 / t_acq.as_secs_f64(), t_auth, t_cert
        );
    }
}

fn sink_bytes(o: &Observer) -> u64 {
    o.capture_bytes
}

fn sink_kinds(_o: &Observer) -> BTreeMap<&'static str, (u64, u64)> {
    KIND_BYTES.with(|k| k.borrow().clone())
}

thread_local! {
    static KIND_BYTES: std::cell::RefCell<BTreeMap<&'static str, (u64, u64)>> = std::cell::RefCell::new(BTreeMap::new());
}

// ---------------------------------------------------------------------------
// Proposal proof: the proposed queue holds a worst-case window with the
// writer stalled. Asserted, so it runs in CI.
// ---------------------------------------------------------------------------

/// Step 4A proposed queue capacity (not adopted until GPT freezes it).
const PROPOSED_QUEUE_RECORDS: usize = 65_536;
const PROPOSED_QUEUE_BYTES: u64 = 128 * 1024 * 1024;

struct StalledWriter {
    gate: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl RecordWriter for StalledWriter {
    fn write_line(&mut self, _line: &str) -> std::io::Result<()> {
        while self.gate.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }
    fn finish(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A full-capacity window (16,375 candidates + begin + close) plus a burst of
/// receipts is absorbed without a single drop while the writer does nothing.
/// The frozen protocol refuses any capture with a drop, so the queue has to
/// hold the worst case outright rather than rely on the writer keeping up.
#[test]
fn proposed_queue_absorbs_a_capacity_window_with_the_writer_stalled() {
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let sink = AsyncSink::with_capacity(
        RUN_FILE_NAME,
        Box::new(StalledWriter { gate: gate.clone() }),
        PROPOSED_QUEUE_RECORDS,
        PROPOSED_QUEUE_BYTES,
    );
    let t = Tmp::new("queue");
    let run = run_for(t.path());
    let mut o = Observer::start(&run, NAMESPACE, 1, at_ms(0), Box::new(sink))
        .unwrap()
        .with_capture_max_bytes(u64::MAX);
    // 16,375 priming receipts, then the capacity window, all while stalled.
    let w = prime(&mut o, 16_375, 1_000_000);
    o.on_window(w);
    let tel = o.sink.telemetry().unwrap();
    let c = o.sink.counters();
    assert_eq!(c.dropped, 0, "no drop with the writer fully stalled");
    assert!(tel.queue_peak >= 32_000, "the stall was real: peak {}", tel.queue_peak);
    assert!(tel.queued_bytes_peak < PROPOSED_QUEUE_BYTES, "{}", tel.queued_bytes_peak);
    gate.store(false, std::sync::atomic::Ordering::SeqCst);
    o.on_finish(at_ms(9_999_999));
    assert_eq!(o.sink.counters().dropped, 0);
}

/// The current default (4,096 records) cannot: the same window drops, which
/// under the frozen protocol makes the capture uncertifiable.
#[test]
fn current_default_queue_drops_a_capacity_window_with_the_writer_stalled() {
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let sink = AsyncSink::with_capacity(
        RUN_FILE_NAME,
        Box::new(StalledWriter { gate: gate.clone() }),
        DEFAULT_QUEUE_CAPACITY,
        DEFAULT_BYTE_CAPACITY,
    );
    let t = Tmp::new("queue-default");
    let run = run_for(t.path());
    let mut o = Observer::start(&run, NAMESPACE, 1, at_ms(0), Box::new(sink))
        .unwrap()
        .with_capture_max_bytes(u64::MAX);
    let w = prime(&mut o, 16_375, 1_000_000);
    o.on_window(w);
    assert!(o.sink.counters().dropped > 0);
    gate.store(false, std::sync::atomic::Ordering::SeqCst);
    o.on_finish(at_ms(9_999_999));
}
