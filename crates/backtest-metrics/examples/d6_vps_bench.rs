//! D6 capacity benchmark for production-class hardware (P3 §14).
//!
//! Same synthetic population, counting allocator and rank() call as
//! `tests/d6_rank_load.rs`, but repeated so percentiles mean something, at
//! the levels the P3 brief names: 4,096 (old cap), 4,678 (observed peak),
//! 6,000, 10,000 and 16,375 (open capacity = the new rank bound).
//!
//! It touches no live state: it builds its own in-memory engine. Run as a
//! plain binary (a static musl build is what went to the VPS):
//!
//! ```text
//! d6_vps_bench [reps]      # default 30
//! ```
//!
//! Output: one CSV row per level -- p50/p95/p99/max rank() wall ms, peak
//! transient MB, truncation counts, and whether forward and reversed
//! insertion serialize byte-identically.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use backtest_metrics::opportunity::{
    OiConfig, OpportunityIntelligence, OpportunityScoreSnapshot, DEFAULT_MAX_OPEN_OPPORTUNITIES,
};
use chrono::{DateTime, TimeZone, Utc};
use market_data::{IgnitionEventKind, ScanEvent};

/// Counts live heap bytes and their high-water mark, so `rank()`'s transient
/// can be measured rather than guessed.
struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let now = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        PEAK.fetch_max(now, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let now = LIVE.fetch_add(new_size, Ordering::Relaxed) + new_size;
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        PEAK.fetch_max(now, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_789_378_200 + secs, 0).unwrap()
}

fn confirmed(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ScanEvent::IgnitionEvent {
        symbol: symbol.into(),
        timestamp: t,
        price,
        kind: IgnitionEventKind::FollowThroughConfirmed,
    }
}

fn momentum(symbol: &str, t: DateTime<Utc>, overall: f64, ma_slope: f64) -> ScanEvent {
    ScanEvent::MomentumUpdate {
        symbol: symbol.into(),
        timestamp: t,
        volume_confirmation: 0.3 + (overall * 7.0) % 0.6,
        structure: 0.2 + (overall * 13.0) % 0.7,
        ma_slope,
        wick_rejection: 0.1 + (overall * 3.0) % 0.8,
        overall,
        qualifies: overall >= 0.6,
    }
}

/// Deterministic LCG, so every run builds the identical population.
fn lcg(state: &mut u64) -> f64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    ((*state >> 11) as f64) / ((1u64 << 53) as f64)
}

/// A configuration whose open capacity holds `n`, with the rank bound
/// following it -- the D6 invariant, satisfied for a lifted universe.
fn config_for(n: usize) -> OiConfig {
    let mut config = OiConfig {
        supported_symbol_universe: n.max(13_100),
        ..OiConfig::default()
    };
    config.max_rank_cohort = config.max_open_opportunities();
    assert!(config.capacity_invariant().is_ok());
    config
}

fn build(config: OiConfig, n: usize, reverse: bool) -> OpportunityIntelligence {
    let mut oi = OpportunityIntelligence::new(config);
    let mut seed = 42u64;
    let params: Vec<(String, f64, f64, f64)> = (0..n)
        .map(|i| {
            (
                format!("S{i:06}"),
                lcg(&mut seed),
                lcg(&mut seed) - 0.3,
                1.0 + lcg(&mut seed) * 0.2,
            )
        })
        .collect();
    let order: Vec<usize> = if reverse {
        (0..n).rev().collect()
    } else {
        (0..n).collect()
    };
    for &i in &order {
        let (symbol, overall, ma, mv) = &params[i];
        oi.observe(
            &momentum(symbol, at(0), (*overall * 100.0).round() / 100.0, *ma),
            at(0),
        );
        oi.observe(&confirmed(symbol, at(1), 10.0), at(1));
        oi.observe(&confirmed(symbol, at(2), 10.0 * mv), at(2));
    }
    oi
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

struct Measured {
    snaps: Vec<OpportunityScoreSnapshot>,
    ms: f64,
    peak_extra_mb: f64,
    live_mb: f64,
}

fn measure(oi: &mut OpportunityIntelligence) -> Measured {
    let live = LIVE.load(Ordering::Relaxed);
    PEAK.store(live, Ordering::Relaxed);
    let started = std::time::Instant::now();
    let snaps = oi.rank(at(60)).expect("a window is due");
    let ms = started.elapsed().as_secs_f64() * 1e3;
    let peak_extra_mb = (PEAK.load(Ordering::Relaxed) - live) as f64 / 1_048_576.0;
    Measured {
        snaps,
        ms,
        peak_extra_mb,
        live_mb: live as f64 / 1_048_576.0,
    }
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    let idx = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len()) - 1;
    sorted[idx]
}

fn main() {
    let reps: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(30);
    let _ = DEFAULT_MAX_OPEN_OPPORTUNITIES;
    println!("level,open,early_N,cont_N,reps,p50_ms,p95_ms,p99_ms,max_ms,peak_extra_MB_max,truncations,deterministic");
    for &n in &[4_096usize, 4_678, 6_000, 10_000, 16_375] {
        let mut ms = Vec::with_capacity(reps);
        let mut peak_mb: f64 = 0.0;
        let mut truncations = 0u64;
        let (mut early_n, mut cont_n) = (0usize, 0usize);
        let mut forward_hash = 0u64;
        for rep in 0..reps {
            let mut oi = build(config_for(n), n, false);
            let m = measure(&mut oi);
            truncations += oi.health().cohort_truncations
                + oi.health().early_cohort_truncations
                + oi.health().continuation_cohort_truncations;
            early_n = m
                .snaps
                .iter()
                .filter(|s| s.early_quality.value.is_some())
                .count();
            cont_n = m
                .snaps
                .iter()
                .filter(|s| s.continuation.value.is_some())
                .count();
            let h = fnv(&serde_json::to_vec(&m.snaps).unwrap());
            if rep == 0 {
                forward_hash = h;
            } else {
                assert_eq!(h, forward_hash, "n={n}: run-to-run output differs");
            }
            ms.push(m.ms);
            peak_mb = peak_mb.max(m.peak_extra_mb);
            if rep == 0 {
                eprintln!("n={n}: engine live heap before rank() {:.1} MB", m.live_mb);
            }
        }
        let mut reversed = build(config_for(n), n, true);
        let rev_hash = fnv(&serde_json::to_vec(&reversed.rank(at(60)).unwrap()).unwrap());
        ms.sort_by(f64::total_cmp);
        println!(
            "{n},{n},{early_n},{cont_n},{reps},{:.2},{:.2},{:.2},{:.2},{:.1},{truncations},{}",
            pct(&ms, 0.50),
            pct(&ms, 0.95),
            pct(&ms, 0.99),
            ms[ms.len() - 1],
            peak_mb,
            if rev_hash == forward_hash {
                "yes"
            } else {
                "NO"
            }
        );
    }
}
