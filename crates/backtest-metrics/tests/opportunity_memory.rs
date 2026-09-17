//! Measures the *real* heap cost of a held-open opportunity, so the engine's
//! capacity bound can be chosen against a measured footprint rather than a
//! guess at `size_of`.
//!
//! `size_of::<Opportunity>()` is nearly useless here: an opportunity owns two
//! `SignalContext`s, a `BTreeMap` of detector arrivals and several `String`s,
//! all of which live behind pointers. A counting allocator is the only honest
//! answer, and it needs its own test binary to install one.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use backtest_metrics::opportunity::{OiConfig, OpportunityIntelligence};
use chrono::{DateTime, TimeZone, Utc};
use market_data::{IgnitionEventKind, ScanEvent};

static LIVE: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE.fetch_add(layout.size(), Ordering::Relaxed);
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LIVE.fetch_add(new_size, Ordering::Relaxed);
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_757_000_000 + secs, 0).unwrap()
}

/// A confirmed ignition is the cheapest way to open one opportunity per symbol,
/// which is exactly the shape the capacity bound has to hold.
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
        volume_confirmation: 0.5,
        structure: 0.5,
        ma_slope: 0.5,
        wick_rejection: 0.5,
        overall,
        qualifies: overall >= 0.6,
    }
}

/// Reports bytes per open opportunity at a realistic population.
///
/// Not an assertion about a magic number — the assertion is the *bound*: the
/// capacity the engine declares must fit in a footprint a realtime process can
/// actually carry. The printed figure is what the capacity derivation cites.
#[test]
fn measured_heap_cost_of_a_held_open_opportunity() {
    const N: usize = 4_000;

    // Warm: the engine's own fixed structures must not be attributed to the
    // opportunities.
    // Capacity deliberately lifted out of the way: this test measures the
    // footprint *per* opportunity, and must not instead measure the engine's
    // bound. (Against the deployed default of 3,750 it stops at 3,750, which
    // is the September-16 defect reproduced in-process — asserted separately
    // in the engine-capacity load tests, not here.)
    let mut engine = OpportunityIntelligence::new(OiConfig {
        supported_open_rate_centi: 1_000_000,
        ..OiConfig::default()
    });
    engine.observe(&confirmed("WARMUP", at(0), 10.0), at(0));
    let base = LIVE.load(Ordering::Relaxed);
    let base_open = engine.open_count();

    for i in 0..N {
        let symbol = format!("SYM{i:05}");
        let t = at(1);
        // Two events per symbol: the open, then a momentum update, so the
        // latest-context refresh path has actually run and the opportunity
        // carries the surface it would carry in production. Measuring a
        // freshly-opened opportunity would understate it.
        engine.observe(&confirmed(&symbol, t, 10.0 + i as f64 * 0.001), t);
        engine.observe(&momentum(&symbol, t, 0.7), t);
    }

    let grown = engine.open_count() - base_open;
    assert_eq!(grown, N, "every symbol must hold exactly one open opportunity");

    let used = LIVE.load(Ordering::Relaxed).saturating_sub(base);
    let per = used as f64 / N as f64;
    println!("open opportunities      : {N}");
    println!("live heap attributable  : {used} bytes");
    println!("per open opportunity    : {per:.0} bytes");
    for cap in [3_750usize, 13_001, 16_375, 20_000] {
        println!("  at capacity {cap:>6}    : {:.1} MB", cap as f64 * per / (1024.0 * 1024.0));
    }

    // The engine is a realtime subsystem sharing a box with the live scan.
    // A per-opportunity cost above this would make any universe-scale bound
    // unaffordable, and the capacity derivation would have to be revisited
    // rather than quietly shipped.
    assert!(
        per < 8_192.0,
        "an open opportunity costs {per:.0} bytes; the capacity derivation assumes far less"
    );
}

/// The derived capacity, printed so the report can cite the same arithmetic the
/// build enforces.
#[test]
fn derived_capacity_matches_the_documented_arithmetic() {
    let c = OiConfig::default();
    println!("supported_open_rate_centi  {}", c.supported_open_rate_centi);
    println!("supported_lifetime_secs    {}", c.supported_lifetime_secs);
    println!("supported_symbol_universe  {}", c.supported_symbol_universe);
    println!("safety                     {}/{}", c.bound_safety_num, c.bound_safety_den);
    println!("required_open_capacity     {}", c.required_open_capacity());
    println!("max_open_opportunities     {}", c.max_open_opportunities());
    println!("inactivity_secs            {}", c.inactivity_secs);
    println!("config fingerprint         {}", c.fingerprint());
    assert_eq!(c.max_open_opportunities(), 16_375);
    assert_eq!(c.inactivity_secs, 300, "the inactivity boundary is out of scope and must not move");
    assert!(c.capacity_invariant().is_ok());
}
