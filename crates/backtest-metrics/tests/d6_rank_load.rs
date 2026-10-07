//! D6 load test: the ranking cohort at, above and far above the old 4,096 cap.
//!
//! `docs/measurement-correctness-contract-2026-09-25.md` D6, "Tests required".
//! `#[ignore]`d because its numbers only mean something in a release build:
//!
//! ```text
//! cargo test -p backtest-metrics --release --locked --test d6_rank_load -- --ignored --nocapture
//! ```
//!
//! For every level it asserts **zero truncation**, `*CohortSize ==` the scored
//! count, every scored row ranked `1..N` contiguously on both surfaces, and
//! byte-identical output between forward and reversed insertion order. It
//! reports `rank()` wall time and the transient heap `rank()` allocates, and
//! asserts only loose, shape-level bounds on them (roughly linear growth;
//! memory within 1.1x of the capped engine), so it measures the code rather
//! than the machine.
//!
//! The fixture is synthetic -- every row scorable on both surfaces, compact
//! features -- so absolute ms/MB are lower than production (mean persisted row
//! 3,854 B). What transfers is the capped-vs-uncapped delta and the growth.

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

/// Every scored row ranked `1..N`, contiguous, `*CohortSize == N`, on both
/// surfaces. Returns the two Ns.
fn assert_fully_ranked(snaps: &[OpportunityScoreSnapshot]) -> (usize, usize) {
    let mut out = [0usize; 2];
    for (k, (rank_of, cohort_of, value_of)) in [
        (
            (|s: &OpportunityScoreSnapshot| s.early_quality_rank)
                as fn(&OpportunityScoreSnapshot) -> Option<usize>,
            (|s: &OpportunityScoreSnapshot| s.early_cohort_size)
                as fn(&OpportunityScoreSnapshot) -> usize,
            (|s: &OpportunityScoreSnapshot| s.early_quality.value)
                as fn(&OpportunityScoreSnapshot) -> Option<f64>,
        ),
        (
            (|s: &OpportunityScoreSnapshot| s.continuation_rank)
                as fn(&OpportunityScoreSnapshot) -> Option<usize>,
            (|s: &OpportunityScoreSnapshot| s.continuation_cohort_size)
                as fn(&OpportunityScoreSnapshot) -> usize,
            (|s: &OpportunityScoreSnapshot| s.continuation.value)
                as fn(&OpportunityScoreSnapshot) -> Option<f64>,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let scored = snaps.iter().filter(|s| value_of(s).is_some()).count();
        let mut ranks: Vec<usize> = snaps.iter().filter_map(|s| rank_of(s)).collect();
        ranks.sort_unstable();
        assert_eq!(ranks.len(), scored, "every scored row is ranked");
        assert!(
            ranks.iter().copied().eq(1..=scored),
            "ranks are 1..N, contiguous and unique"
        );
        assert!(
            snaps.iter().all(|s| cohort_of(s) == scored),
            "cohort size is the true N"
        );
        out[k] = scored;
    }
    (out[0], out[1])
}

#[test]
#[ignore = "release-mode load measurement; run with --release -- --ignored --nocapture"]
fn d6_rank_load_levels() {
    println!(
        "level,open,early_N,cont_N,truncations,rank_ms,rank_peak_extra_MB,engine_live_MB,deterministic"
    );
    let mut ms_at = std::collections::BTreeMap::new();
    for &n in &[4_096usize, 4_678, 6_000, 16_384] {
        let mut oi = build(config_for(n), n, false);
        let m = measure(&mut oi);
        let (early_n, cont_n) = assert_fully_ranked(&m.snaps);
        assert_eq!(oi.health().cohort_truncations, 0, "n={n}: no truncation");
        assert_eq!(
            oi.health().early_cohort_truncations + oi.health().continuation_cohort_truncations,
            0
        );
        assert!(oi.take_cohort_truncations().is_empty());
        assert_eq!(oi.open_count(), n);
        // Determinism: a reversed insertion order serializes byte-identically.
        let forward = serde_json::to_vec(&m.snaps).unwrap();
        let mut reversed_oi = build(config_for(n), n, true);
        let reversed = serde_json::to_vec(&reversed_oi.rank(at(60)).unwrap()).unwrap();
        assert_eq!(fnv(&forward), fnv(&reversed), "n={n}: nondeterministic");
        assert_eq!(forward, reversed);
        println!(
            "{n},{},{early_n},{cont_n},0,{:.1},{:.1},{:.1},yes",
            oi.open_count(),
            m.ms,
            m.peak_extra_mb,
            m.live_mb
        );
        ms_at.insert(n, m.ms);
    }
    // Growth is about linear in the cohort, never quadratic: 16,384 / 4,678
    // is 3.5x the rows, so a loose 8x bound fails only a real regression.
    let ratio = ms_at[&16_384] / ms_at[&4_678];
    println!("rank() time ratio 16,384 / 4,678 = {ratio:.2} (rows ratio 3.50)");
    assert!(
        ratio < 8.0,
        "rank() grew super-linearly: {ratio:.2}x for 3.5x the rows"
    );
}

/// Above capacity: 32,768 opens against the shipped 16,375. Open is held at
/// capacity by eviction (counted), so the ranked N stays <= capacity and
/// truncation stays structurally zero.
#[test]
#[ignore = "release-mode load measurement; run with --release -- --ignored --nocapture"]
fn d6_rank_load_above_capacity() {
    let config = OiConfig::default();
    let mut oi = build(config.clone(), 32_768, false);
    assert_eq!(oi.open_count(), DEFAULT_MAX_OPEN_OPPORTUNITIES);
    assert!(
        oi.health().capacity_evictions > 0,
        "the excess was evicted, and counted"
    );
    let m = measure(&mut oi);
    let (early_n, cont_n) = assert_fully_ranked(&m.snaps);
    assert!(early_n <= config.max_rank_cohort && cont_n <= config.max_rank_cohort);
    assert_eq!(oi.health().cohort_truncations, 0);
    println!(
        "stress 32,768 opens vs capacity {}: open {}, early_N {early_n}, cont_N {cont_n}, \
         evictions {}, truncations 0, rank {:.1} ms, peak extra {:.1} MB",
        DEFAULT_MAX_OPEN_OPPORTUNITIES,
        oi.open_count(),
        oi.health().capacity_evictions,
        m.ms,
        m.peak_extra_mb
    );
}

/// Memory: removing the cap costs almost nothing at 16,384, because the cap
/// was applied after every row had been scored, sorted and built.
#[test]
#[ignore = "release-mode load measurement; run with --release -- --ignored --nocapture"]
fn d6_rank_load_memory_versus_the_capped_engine() {
    let n = 16_384;
    let mut capped_config = config_for(n);
    capped_config.max_rank_cohort = 4_096;
    let mut capped = build(capped_config, n, false);
    let c = measure(&mut capped);
    assert_eq!(
        capped.health().cohort_truncations,
        1,
        "the old cap really did cut"
    );
    drop(c.snaps);
    let mut full = build(config_for(n), n, false);
    let f = measure(&mut full);
    println!(
        "n={n}: capped {:.1} ms / {:.1} MB transient, uncapped {:.1} ms / {:.1} MB transient",
        c.ms, c.peak_extra_mb, f.ms, f.peak_extra_mb
    );
    assert!(
        f.peak_extra_mb <= c.peak_extra_mb * 1.1,
        "uncapped transient {:.1} MB exceeds 1.1x the capped {:.1} MB",
        f.peak_extra_mb,
        c.peak_extra_mb
    );
}
