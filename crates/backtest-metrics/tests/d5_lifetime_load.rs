//! D5 capacity re-measurement: opportunity lifetime and open population
//! under `move-v1` versus `symbol-activity-v1`, on one synthetic session.
//!
//! The contract (appendix D5.2) requires the capacity assumption
//! `supported_lifetime_secs` (4,800 s, sized from the September-16 measured
//! mean of 3,752 s under the symbol-activity unit) to be re-measured once the
//! unit becomes a move. There is no raw `ScanEvent` corpus to replay (see
//! `oi_replay`'s module docs), so this measures a **synthetic** session whose
//! event mix follows the production shape the contract names -- a bar, a
//! funnel reading and a momentum reading per tracked symbol per minute, a halt
//! warning on every trade -- with detector setups arriving as sporadic
//! bursts. It uses no outcome data. The numbers describe this fixture, not a
//! market; what transfers is the ratio between the two lifecycles on
//! identical input, and whether the structural bound still holds.
//!
//! `#[ignore]`d because it is a load run:
//!
//! ```text
//! cargo test -p backtest-metrics --release --locked --test d5_lifetime_load -- --ignored --nocapture
//! ```

use backtest_metrics::opportunity::{Lifecycle, OiConfig, OpportunityIntelligence};
use chrono::{DateTime, Duration, TimeZone, Utc};
use market_data::events::{ConsolidationStrategy, HaltAlertLevel};
use market_data::{ConsolidationEventKind, IgnitionEventKind, ScanEvent};

/// 2026-09-14T13:30:00Z, the regular open.
fn at(secs: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 14, 13, 30, 0).unwrap() + Duration::seconds(secs)
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn chance(&mut self, per_mille: u64) -> bool {
        self.next() % 1_000 < per_mille
    }
}

/// One synthetic regular session: `symbols` tracked symbols for `minutes`.
///
/// Per symbol per minute: a bar, a funnel reading (level `true` all session
/// for the 60% that passed the gap filter), a momentum reading, and three halt
/// warnings (15 s apart). A setup starts with probability 1.1% per minute
/// and lasts 2-20 minutes: an ignition candidate opens it, momentum
/// qualifies throughout, an ignition follow-through (confirmed or rejected,
/// even odds) fires with 50% chance per setup minute, and a micropullback
/// entry with 20%. Outside a setup, stray ignition candidates/rejections
/// appear at 2% per minute.
fn session(symbols: usize, minutes: i64, seed: u64) -> Vec<(ScanEvent, DateTime<Utc>)> {
    let mut rng = Lcg(seed);
    let mut setup_left = vec![0i64; symbols];
    let passes_funnel: Vec<bool> = (0..symbols).map(|_| rng.chance(600)).collect();
    let mut out = Vec::new();
    for minute in 0..minutes {
        for s in 0..symbols {
            let symbol = format!("S{s:05}");
            let base = minute * 60 + (s as i64 % 60);
            let t = at(base);
            let price = 2.0 + (s % 50) as f64 * 0.3 + (minute % 17) as f64 * 0.01;
            if setup_left[s] == 0 && rng.chance(11) {
                setup_left[s] = 2 + (rng.next() % 19) as i64;
                out.push((
                    ScanEvent::IgnitionEvent {
                        symbol: symbol.clone(),
                        timestamp: t,
                        price,
                        kind: IgnitionEventKind::CandidateOpened,
                    },
                    t,
                ));
            } else if setup_left[s] > 0 {
                let follow_through = setup_left[s] > 1 && rng.chance(500);
                if follow_through {
                    let kind = if rng.chance(500) {
                        IgnitionEventKind::FollowThroughConfirmed
                    } else {
                        IgnitionEventKind::FollowThroughRejected
                    };
                    out.push((
                        ScanEvent::IgnitionEvent {
                            symbol: symbol.clone(),
                            timestamp: t,
                            price,
                            kind,
                        },
                        t,
                    ));
                }
                if rng.chance(200) {
                    out.push((
                        ScanEvent::ConsolidationEvent {
                            symbol: symbol.clone(),
                            timestamp: t,
                            price,
                            kind: ConsolidationEventKind::EntryTriggered,
                            strategy: ConsolidationStrategy::Micropullback,
                        },
                        t,
                    ));
                }
            } else if rng.chance(20) {
                let kind = if rng.chance(500) {
                    IgnitionEventKind::CandidateOpened
                } else {
                    IgnitionEventKind::FollowThroughRejected
                };
                out.push((
                    ScanEvent::IgnitionEvent {
                        symbol: symbol.clone(),
                        timestamp: t,
                        price,
                        kind,
                    },
                    t,
                ));
            }
            let qualifies = setup_left[s] > 0;
            if setup_left[s] > 0 {
                setup_left[s] -= 1;
            }
            out.push((
                ScanEvent::BarUpdate {
                    symbol: symbol.clone(),
                    timestamp: t,
                    interval_secs: 60,
                    open: price,
                    high: price * 1.01,
                    low: price * 0.99,
                    close: price,
                    volume: 1_000,
                    is_final: false,
                },
                t,
            ));
            out.push((
                ScanEvent::FunnelSignal {
                    symbol: symbol.clone(),
                    timestamp: t,
                    price,
                    gap_pct: 12.0,
                    session_volume: 100_000,
                    price_ok: true,
                    float_ok: true,
                    rel_vol_ok: true,
                    gap_ok: passes_funnel[s],
                    passed: passes_funnel[s],
                },
                t,
            ));
            out.push((
                ScanEvent::MomentumUpdate {
                    symbol: symbol.clone(),
                    timestamp: t,
                    volume_confirmation: 0.6,
                    structure: 0.6,
                    ma_slope: 0.5,
                    wick_rejection: 0.7,
                    overall: if qualifies { 0.7 } else { 0.4 },
                    qualifies,
                },
                t,
            ));
            for q in 1..4 {
                let th = t + Duration::seconds(q * 15);
                out.push((
                    ScanEvent::HaltWarning {
                        estimated_bands: true,
                        symbol: symbol.clone(),
                        timestamp: th,
                        reference_price: price,
                        current_price: price,
                        band_width_dollars: 0.5,
                        band_doubled: false,
                        proximity_ratio: 0.2,
                        relative_volume: None,
                        level: HaltAlertLevel::Calm,
                        luld_in_effect: true,
                    },
                    th,
                ));
            }
        }
    }
    out.sort_by_key(|(_, t)| *t);
    out
}

struct Measured {
    opened: u64,
    mean_lifetime_secs: f64,
    open_p50: usize,
    open_p99: usize,
    open_max: usize,
    open_rate_per_sec: f64,
    evictions: u64,
}

fn measure(lifecycle: Lifecycle, events: &[(ScanEvent, DateTime<Utc>)]) -> Measured {
    let mut oi = OpportunityIntelligence::new(OiConfig {
        lifecycle,
        ..OiConfig::default()
    });
    let mut lifetimes: Vec<i64> = Vec::new();
    let mut samples: Vec<usize> = Vec::new();
    let mut next_sample = events[0].1;
    for (event, now) in events {
        for op in oi.observe(event, *now) {
            lifetimes.push((op.closed_at.unwrap() - op.opened_at).num_seconds());
        }
        if *now >= next_sample {
            samples.push(oi.open_count());
            next_sample = *now + Duration::seconds(30);
        }
    }
    let end = events.last().unwrap().1;
    // Still open at the end: censored at the end instant, which understates
    // the symbol-activity lifetime -- the conservative direction for the ratio.
    for op in oi.finish(end) {
        lifetimes.push((op.closed_at.unwrap() - op.opened_at).num_seconds());
    }
    samples.sort_unstable();
    let pct = |p: f64| samples[((samples.len() - 1) as f64 * p) as usize];
    let span = (end - events[0].1).num_seconds().max(1) as f64;
    Measured {
        opened: oi.health().opportunities_opened,
        mean_lifetime_secs: lifetimes.iter().sum::<i64>() as f64 / lifetimes.len().max(1) as f64,
        open_p50: pct(0.50),
        open_p99: pct(0.99),
        open_max: *samples.last().unwrap(),
        open_rate_per_sec: oi.health().opportunities_opened as f64 / span,
        evictions: oi.health().capacity_evictions,
    }
}

#[test]
#[ignore]
fn d5_lifetime_and_open_population_under_both_lifecycles() {
    let symbols = 2_000;
    let minutes = 390; // one regular session
    let events = session(symbols, minutes, 0x5eed_d5);
    println!(
        "synthetic session: {symbols} symbols, {minutes} min, {} events",
        events.len()
    );
    let legacy = measure(Lifecycle::SymbolActivityV1, &events);
    let moving = measure(Lifecycle::MoveV1, &events);
    for (name, m) in [("symbol-activity-v1", &legacy), ("move-v1", &moving)] {
        println!(
            "{name:>19}: opened {:>6}  mean lifetime {:>7.0}s  open p50/p99/max {}/{}/{}  \
             open rate {:.3}/s  Little {:.0}  evictions {}",
            m.opened,
            m.mean_lifetime_secs,
            m.open_p50,
            m.open_p99,
            m.open_max,
            m.open_rate_per_sec,
            m.open_rate_per_sec * m.mean_lifetime_secs,
            m.evictions,
        );
    }
    let config = OiConfig::default();
    println!(
        "capacity {} (supported lifetime {}s, rate {}/100 per s, universe {})",
        config.max_open_opportunities(),
        config.supported_lifetime_secs,
        config.supported_open_rate_centi,
        config.supported_symbol_universe
    );

    // Structural claims, independent of the fixture's absolute numbers.
    assert_eq!(moving.evictions, 0);
    assert_eq!(legacy.evictions, 0);
    assert!(
        moving.open_max <= symbols,
        "at most one open move per symbol"
    );
    assert!(
        moving.mean_lifetime_secs < legacy.mean_lifetime_secs,
        "a move must be shorter-lived than a symbol's whole activity"
    );
    assert!(moving.open_max <= legacy.open_max);
    assert!(
        moving.mean_lifetime_secs <= config.supported_lifetime_secs as f64,
        "the supported lifetime still covers the measured move lifetime"
    );
    assert!(
        moving.open_rate_per_sec * 100.0 <= config.supported_open_rate_centi as f64,
        "and the supported open rate still covers the measured one"
    );
}
