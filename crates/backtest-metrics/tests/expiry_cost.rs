//! Does per-event cost scale with the *open population*?
//!
//! `expire_inactive` runs on every observation and, as written, scans the whole
//! open set. That was affordable at a bound of 3,750. The capacity repair
//! raises the bound to 16,375, so the question has to be answered rather than
//! assumed -- this is the same trap the measurement collector fell into when
//! its pending capacity went 4,096 -> 38,400 and both hot paths turned out to
//! be linear in it.

use backtest_metrics::opportunity::{OiConfig, OpportunityIntelligence};
use chrono::{DateTime, TimeZone, Utc};
use market_data::{IgnitionEventKind, ScanEvent};

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

/// Opens `population` opportunities, then times `probes` further observations
/// against that standing population. No ranking: this isolates the per-event
/// path, which is what runs on every broadcast event.
fn per_event_nanos(population: i64, probes: i64) -> (usize, u128) {
    let mut oi = OpportunityIntelligence::new(OiConfig {
        supported_symbol_universe: 200_000,
        ..OiConfig::default()
    });
    for i in 0..population {
        let t = at(1);
        oi.observe(&confirmed(&format!("SYM{i:06}"), t, 10.0), t);
    }
    let open = oi.open_count();
    // All probes at the same instant, so nothing expires and the cost measured
    // is the scan itself rather than the closing.
    let t = at(2);
    let started = std::time::Instant::now();
    for i in 0..probes {
        oi.observe(&confirmed(&format!("SYM{:06}", i % population), t, 10.5), t);
    }
    (open, started.elapsed().as_nanos() / probes.max(1) as u128)
}

/// Per-event cost must stay roughly flat as the open population grows.
///
/// Measured before the `by_last_seen` index, on this machine, debug build:
///
/// ```text
/// open   1000  ->     64747 ns/event
/// open  16000  ->   1123547 ns/event      17.4x for a 16.0x population
/// ```
///
/// and after:
///
/// ```text
/// open   1000  ->      3748 ns/event
/// open  16000  ->      4851 ns/event       1.3x for a 16.0x population
/// ```
///
/// The threshold is deliberately loose -- 4x for a 16x population -- because
/// this runs on shared CI hardware and the failure being guarded against is
/// *linear* growth, which overshoots 4x by a wide margin. A tight bound here
/// would fail on timing noise and get deleted, which is worse than no bound.
#[test]
fn per_event_cost_does_not_grow_with_the_open_population() {
    let (small_open, small) = per_event_nanos(1_000, 2_000);
    let (large_open, large) = per_event_nanos(16_000, 2_000);
    println!("open {small_open:>6}  ->  {small:>8} ns/event");
    println!("open {large_open:>6}  ->  {large:>8} ns/event");
    let growth = large as f64 / small.max(1) as f64;
    println!("ratio {growth:.1}x for a {:.1}x population",
        large_open as f64 / small_open as f64);

    assert!(small_open >= 1_000 && large_open >= 16_000, "the fixture must build both populations");
    assert!(
        growth < 4.0,
        "per-event cost grew {growth:.1}x for a 16x population; something is scanning the          open set per event, and at the repaired capacity of 16,375 that would put real          load on a broadcast subscriber (before the index it was 17.4x)"
    );
}
