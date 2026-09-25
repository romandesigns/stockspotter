//! D5 `move-v1` lifecycle tests.
//!
//! The rule under test is frozen in
//! `docs/opportunity-lifecycle-move-v1-preregistration-2026-09-25.md`
//! (sections 1-12 plus amendment A1). Two lists are covered, and each test
//! is named after the one it proves:
//!
//! * `p01..p16`: the P2 contract's D5 test matrix
//!   (`docs/measurement-correctness-contract-2026-09-25.md`, appendix D5).
//! * `b01..b20`: the P3 brief's list. Test b11 (outcome anchors) runs end to
//!   end through the real drivers in
//!   `ws-server/src/opportunity_lifecycle_disposition_tests.rs`, because the
//!   anchor is created there.
//!
//! Every test drives `OiConfig::default()`, which is `move-v1`, unless it says
//! otherwise.

use super::*;
use chrono::TimeZone;
use market_data::events::{ConsolidationStrategy, HaltAlertLevel};

use crate::episode::EpisodeTracker;
use crate::membership::{map_memberships, AmbiguityReason};

// ---------------------------------------------------------------------------
// Fixture vocabulary
// ---------------------------------------------------------------------------

/// 2026-09-14 (Monday, EDT) 14:00:00Z = 10:00 ET, regular session.
fn at(secs: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 14, 14, 0, 0).unwrap() + Duration::seconds(secs)
}

fn utc(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, min, s).unwrap()
}

fn ign(symbol: &str, t: DateTime<Utc>, price: f64, kind: IgnitionEventKind) -> ScanEvent {
    ScanEvent::IgnitionEvent {
        symbol: symbol.into(),
        timestamp: t,
        price,
        kind,
    }
}
fn confirmed(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ign(symbol, t, price, IgnitionEventKind::FollowThroughConfirmed)
}
fn rejected(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ign(symbol, t, price, IgnitionEventKind::FollowThroughRejected)
}
fn candidate(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ign(symbol, t, price, IgnitionEventKind::CandidateOpened)
}
fn consolidation(
    symbol: &str,
    t: DateTime<Utc>,
    price: f64,
    kind: ConsolidationEventKind,
    strategy: ConsolidationStrategy,
) -> ScanEvent {
    ScanEvent::ConsolidationEvent {
        symbol: symbol.into(),
        timestamp: t,
        price,
        kind,
        strategy,
    }
}
fn micro(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    consolidation(
        symbol,
        t,
        price,
        ConsolidationEventKind::EntryTriggered,
        ConsolidationStrategy::Micropullback,
    )
}
fn breakout(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    consolidation(
        symbol,
        t,
        price,
        ConsolidationEventKind::EntryTriggered,
        ConsolidationStrategy::ConsolidationBreakout,
    )
}
fn momentum(symbol: &str, t: DateTime<Utc>, qualifies: bool) -> ScanEvent {
    ScanEvent::MomentumUpdate {
        symbol: symbol.into(),
        timestamp: t,
        volume_confirmation: 0.7,
        structure: 0.6,
        ma_slope: 0.5,
        wick_rejection: 0.8,
        overall: if qualifies { 0.7 } else { 0.3 },
        qualifies,
    }
}
fn funnel(symbol: &str, t: DateTime<Utc>, price: f64, passed: bool) -> ScanEvent {
    ScanEvent::FunnelSignal {
        symbol: symbol.into(),
        timestamp: t,
        price,
        gap_pct: 12.0,
        session_volume: 100_000,
        price_ok: true,
        float_ok: true,
        rel_vol_ok: true,
        gap_ok: passed,
        passed,
    }
}
/// An in-progress bar, so its data time is `t` itself (no R2 shift).
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
        is_final: false,
    }
}
fn halt(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ScanEvent::HaltWarning {
        estimated_bands: true,
        symbol: symbol.into(),
        timestamp: t,
        reference_price: price,
        current_price: price,
        band_width_dollars: 0.5,
        band_doubled: false,
        proximity_ratio: 0.9,
        relative_volume: Some(3.0),
        level: HaltAlertLevel::Calm,
        luld_in_effect: true,
    }
}
fn catalyst(symbol: &str, t: DateTime<Utc>) -> ScanEvent {
    ScanEvent::CatalystUpdate {
        symbol: symbol.into(),
        timestamp: t,
        catalyst_tags: vec!["earnings".into()],
        headline_count: 1,
        most_recent_headline: Some("headline".into()),
        most_recent_published_at: None,
    }
}

/// The engine plus every close it has produced, in order.
struct Run {
    oi: OpportunityIntelligence,
    closed: Vec<Opportunity>,
}

impl Run {
    fn new() -> Self {
        Self::with(OiConfig::default())
    }
    fn with(config: OiConfig) -> Self {
        Self {
            oi: OpportunityIntelligence::new(config),
            closed: Vec::new(),
        }
    }
    /// One event, received at its own data time.
    fn step(&mut self, event: ScanEvent) -> Vec<Opportunity> {
        let t = event_symbol_time_price(&event)
            .map(|(_, t, _)| t)
            .unwrap_or_else(|| at(0));
        self.step_at(event, t)
    }
    fn step_at(&mut self, event: ScanEvent, received_at: DateTime<Utc>) -> Vec<Opportunity> {
        let closed = self.oi.observe(&event, received_at);
        self.closed.extend(closed.iter().cloned());
        closed
    }
    /// Moves the receipt clock with an unrelated symbol's bar -- which, under
    /// move-v1, can neither open nor extend anything.
    fn tick(&mut self, t: DateTime<Utc>) -> Vec<Opportunity> {
        self.step_at(bar("ZZTICK", t, 1.0), t)
    }
    fn open(&self, symbol: &str) -> Option<&Opportunity> {
        self.oi.open_opportunities().find(|o| o.symbol == symbol)
    }
    fn closed_for(&self, symbol: &str) -> Vec<&Opportunity> {
        self.closed.iter().filter(|o| o.symbol == symbol).collect()
    }
}

// ===========================================================================
// P2 contract matrix (D5, 16 cases)
// ===========================================================================

/// P2-1. Bars, halt warnings and catalysts alone never keep an opportunity
/// alive past `T_move`.
#[test]
fn p01_market_data_alone_never_extends_life() {
    let mut r = Run::new();
    r.step(confirmed("AAA", at(0), 10.0));
    for s in (5..=900).step_by(5) {
        r.step(bar("AAA", at(s), 10.0 + s as f64 / 1_000.0));
        r.step(halt("AAA", at(s), 10.0));
        r.step(catalyst("AAA", at(s)));
    }
    let closes = r.closed_for("AAA");
    assert_eq!(closes.len(), 1);
    assert_eq!(
        closes[0].close_reason,
        Some(OpportunityCloseReason::SetupInactivity)
    );
    assert_eq!(
        closes[0].closed_at,
        Some(at(300)),
        "last evidence 0 + T_move"
    );
    assert!(r.open("AAA").is_none(), "state events never re-open either");
}

/// P2-2. A level-true funnel on every bar does not keep it alive; the
/// false -> true flip opens it.
#[test]
fn p02_funnel_level_is_state_its_flip_is_the_edge() {
    let mut r = Run::new();
    r.step(funnel("AAA", at(0), 10.0, true)); // first true a process sees: edge
    assert_eq!(
        r.open("AAA").map(|o| o.first_detector),
        Some(Strategy::FastFunnel)
    );
    for s in (60..=600).step_by(60) {
        r.step(funnel("AAA", at(s), 10.0, true));
    }
    let closes = r.closed_for("AAA");
    assert_eq!(closes.len(), 1);
    assert_eq!(closes[0].closed_at, Some(at(300)));
    assert!(
        r.open("AAA").is_none(),
        "the level cannot re-open (section 6)"
    );
    r.step(funnel("AAA", at(660), 10.0, false));
    r.step(funnel("AAA", at(720), 10.2, true)); // flip again: a new edge
    let reopened = r.open("AAA").expect("the flip re-opens");
    assert_eq!(reopened.opened_at, at(720));
}

/// P2-3. `qualifies: true` readings keep it alive; `qualifies: false` do not.
#[test]
fn p03_momentum_level_true_keeps_it_alive_false_does_not() {
    let mut r = Run::new();
    r.step(bar("AAA", at(0), 10.0)); // momentum carries no price
    r.step(momentum("AAA", at(1), true)); // edge
    for s in (61..=1_201).step_by(60) {
        r.step(momentum("AAA", at(s), true));
    }
    assert!(
        r.open("AAA").is_some(),
        "20 minutes of re-asserted momentum is one live move"
    );
    for s in (1_261..=2_000).step_by(60) {
        r.step(momentum("AAA", at(s), false));
    }
    let closes = r.closed_for("AAA");
    assert_eq!(closes.len(), 1);
    assert_eq!(closes[0].closed_at, Some(at(1_201 + 300)));
    assert_eq!(
        closes[0].close_reason,
        Some(OpportunityCloseReason::SetupInactivity)
    );
}

/// P2-4. Confirm -> reject -> confirm inside `T_move` is one opportunity with
/// one absorbed invalidation.
#[test]
fn p04_confirm_reject_confirm_is_one_opportunity() {
    let mut r = Run::new();
    r.step(confirmed("AAA", at(0), 10.0));
    r.step(rejected("AAA", at(100), 9.8));
    r.step(confirmed("AAA", at(200), 10.3));
    let op = r.open("AAA").unwrap();
    assert_eq!(op.opened_at, at(0));
    assert_eq!(op.invalidations_absorbed, 1);
    assert_eq!(op.episode_fragments, 2);
    assert_eq!(op.last_evidence_kind, Some(EvidenceKind::Positive));
    assert_eq!(op.last_relevant_at, Some(at(200)));
    assert_eq!(r.oi.health().opportunities_opened, 1);
}

/// P2-5. A rejection followed by `T_move` of silence closes as `Invalidated`
/// at `last_positive + T_move` -- the rejection does not refresh the clock.
#[test]
fn p05_reject_then_silence_is_invalidated_at_last_positive_plus_t_move() {
    let mut r = Run::new();
    r.step(confirmed("AAA", at(0), 10.0));
    r.step(rejected("AAA", at(250), 9.5));
    assert!(r.tick(at(299)).is_empty());
    let closed = r.tick(at(300));
    assert_eq!(closed.len(), 1);
    assert_eq!(
        closed[0].close_reason,
        Some(OpportunityCloseReason::Invalidated)
    );
    assert_eq!(closed[0].closed_at, Some(at(300)), "0 + 300, not 250 + 300");
}

/// P2-6. Evidence silence gives `SetupInactivity` even while bars continue.
#[test]
fn p06_setup_inactivity_while_bars_continue() {
    let mut r = Run::new();
    r.step(confirmed("AAA", at(0), 10.0));
    r.step(candidate("AAA", at(100), 10.1));
    for s in (110..=500).step_by(10) {
        r.step(bar("AAA", at(s), 10.2));
    }
    let closes = r.closed_for("AAA");
    assert_eq!(closes.len(), 1);
    assert_eq!(
        closes[0].close_reason,
        Some(OpportunityCloseReason::SetupInactivity)
    );
    assert_eq!(closes[0].closed_at, Some(at(400)));
}

/// P2-7 (and b07). A second setup after a close gets a new id, and its
/// detection-time state is frozen at the SECOND open.
#[test]
fn p07_second_setup_gets_a_new_id_and_refrozen_detection_state() {
    let mut r = Run::new();
    for s in (0..600).step_by(60) {
        r.step(bar("AAA", at(s - 600), 8.0 + s as f64 / 600.0));
    }
    r.step(confirmed("AAA", at(0), 9.0));
    let first = r.open("AAA").unwrap().clone();
    for s in (60..=1_200).step_by(60) {
        r.step(bar("AAA", at(s), 9.0 + s as f64 / 300.0)); // price runs to 13
    }
    r.step(micro("AAA", at(1_260), 13.0));
    let second = r.open("AAA").unwrap().clone();

    assert_ne!(first.id, second.id);
    assert_eq!(second.opened_at, at(1_260));
    assert_eq!(second.first_detector, Strategy::Micropullback);
    assert_eq!(second.opening_price, 13.0);
    assert_eq!(
        (second.observed_high, second.observed_low),
        (13.0, 13.0),
        "extremes restart"
    );
    assert_eq!(
        second.detection_context.as_ref().map(|c| c.detected_at),
        Some(at(1_260)),
        "detection context frozen at this move's open"
    );
    assert_eq!(
        first.detection_context.as_ref().map(|c| c.detected_at),
        Some(at(0))
    );
    assert_ne!(
        second.move_before_detection_pct, first.move_before_detection_pct,
        "earliness is measured to this move's own open"
    );
    assert_eq!(
        second.move_before_detection_pct,
        second
            .detection_context
            .as_ref()
            .unwrap()
            .pre_detection
            .as_ref()
            .unwrap()
            .move_before_detection_pct
    );
    assert_eq!(
        second.detectors_seen.len(),
        1,
        "confluence restarts with the move"
    );
    assert_eq!(second.invalidations_absorbed, 0);
}

/// P2-8 (and b08). Ignition, consolidation and a momentum flip at the same
/// instant: one opportunity, confluence 3, no duplicate.
#[test]
fn p08_overlapping_detectors_make_one_opportunity() {
    let mut r = Run::new();
    r.step(bar("AAA", at(0), 10.0));
    r.step(confirmed("AAA", at(1), 10.0));
    r.step(micro("AAA", at(1), 10.0));
    r.step(momentum("AAA", at(1), true));
    r.step(breakout("AAA", at(1), 10.0));
    assert_eq!(r.oi.open_count(), 1);
    assert_eq!(r.oi.health().opportunities_opened, 1);
    let op = r.open("AAA").unwrap();
    assert_eq!(op.confluence_count(), 4);
    assert_eq!(op.detector_transitions, 3);
    assert!(op
        .detectors_seen
        .values()
        .all(|d| d.first_confirmed_at == Some(at(1))));
}

/// P2-9 (and b14). Premarket -> regular with continuous evidence is one
/// opportunity whose `openedPhase` is premarket.
#[test]
fn p09_premarket_to_regular_continuation() {
    let mut r = Run::new();
    let open = utc(2026, 9, 14, 13, 20, 0); // 09:20 EDT
    r.step(confirmed("AAA", open, 5.0));
    for k in 1..=10 {
        let t = open + Duration::seconds(120 * k); // through 09:40 EDT
        r.step(candidate("AAA", t, 5.0 + k as f64 * 0.01));
    }
    let op = r.open("AAA").unwrap();
    assert_eq!(op.opened_at, open);
    assert_eq!(op.opened_phase, Some(TradingSession::Premarket));
    assert!(r.closed.is_empty(), "the bell does not split a move");
    let snaps = r.oi.rank(open + Duration::seconds(1_200)).unwrap();
    assert_eq!(snaps[0].opened_phase, Some(TradingSession::Premarket));
    assert!(serde_json::to_string(&snaps[0])
        .unwrap()
        .contains("\"openedPhase\":\"premarket\""));
}

/// P2-10. Premarket evidence, `T_move` of quiet, then regular evidence: two.
#[test]
fn p10_premarket_quiet_then_regular_is_two() {
    let mut r = Run::new();
    let pre = utc(2026, 9, 14, 13, 20, 0);
    r.step(confirmed("AAA", pre, 5.0));
    for k in 1..=12 {
        r.step(bar("AAA", pre + Duration::seconds(60 * k), 5.1));
    }
    let rth = utc(2026, 9, 14, 13, 33, 0); // 09:33 EDT
    r.step(confirmed("AAA", rth, 5.2));
    assert_eq!(r.closed_for("AAA").len(), 1);
    let second = r.open("AAA").unwrap();
    assert_eq!(second.opened_phase, Some(TradingSession::Regular));
    assert_ne!(second.id, r.closed_for("AAA")[0].id);
}

/// P2-11 (and b15). Regular -> after-hours continuation stays one.
#[test]
fn p11_regular_to_after_hours_continuation() {
    let mut r = Run::new();
    let open = utc(2026, 9, 14, 19, 50, 0); // 15:50 EDT
    r.step(confirmed("AAA", open, 5.0));
    for k in 1..=15 {
        r.step(momentum("AAA", open + Duration::seconds(60 * k), true));
    }
    let op = r.open("AAA").unwrap();
    assert_eq!(op.opened_at, open);
    assert_eq!(op.opened_phase, Some(TradingSession::Regular));
    assert!(r.closed.is_empty());
    assert_eq!(
        classify_session(open + Duration::seconds(900)),
        TradingSession::AfterHours,
        "the fixture really crossed the close"
    );
}

/// P2-12. The market day changing closes via `SessionBoundary` with
/// `closedAt >= openedAt`, and the next day's edge opens a new id.
#[test]
fn p12_market_day_change_closes_and_the_next_day_reopens() {
    let mut r = Run::new();
    let late = utc(2026, 1, 16, 8, 58, 0); // 03:58 EST, market day 01-15
    r.step(confirmed("AAA", late, 5.0));
    r.step(candidate("AAA", late + Duration::seconds(60), 5.0));
    let new_day = utc(2026, 1, 16, 9, 0, 30); // 04:00:30 EST, market day 01-16
    let closed = r.step(confirmed("AAA", new_day, 5.1));
    assert_eq!(closed.len(), 1);
    assert_eq!(
        closed[0].close_reason,
        Some(OpportunityCloseReason::SessionBoundary)
    );
    assert!(closed[0].closed_at.unwrap() >= closed[0].opened_at);
    assert_eq!(
        closed[0].closed_at,
        Some(new_day),
        "event path: dated at the event"
    );
    let next = r.open("AAA").unwrap();
    assert_eq!(next.opened_at, new_day);
    assert_ne!(next.id, closed[0].id);
    assert_eq!(next.opened_phase, Some(TradingSession::Premarket));
}

/// P2-13 (and b19). Capacity eviction under move-v1 stays explicit and
/// counted, and the victim is the least recently RELEVANT (amendment A1.10).
#[test]
fn p13_capacity_eviction_is_explicit_and_counted() {
    let config = OiConfig {
        supported_symbol_universe: 2,
        bound_safety_num: 1,
        bound_safety_den: 1,
        max_rank_cohort: 2,
        ..OiConfig::default()
    };
    assert_eq!(config.max_open_opportunities(), 2);
    let mut r = Run::with(config);
    r.step(confirmed("AAA", at(0), 10.0));
    r.step(confirmed("BBB", at(10), 10.0));
    r.step(candidate("AAA", at(20), 10.0)); // AAA's evidence is now the newer
    for s in 21..60 {
        r.step(bar("BBB", at(s), 10.0)); // BBB trades, but that is not evidence
    }
    let closed = r.step(confirmed("CCC", at(60), 10.0));
    assert_eq!(closed.len(), 1);
    assert_eq!(
        closed[0].symbol, "BBB",
        "least recently relevant, not least recently traded"
    );
    assert_eq!(
        closed[0].close_reason,
        Some(OpportunityCloseReason::CapacityReached)
    );
    assert_eq!(r.oi.health().capacity_evictions, 1);
    assert_eq!(r.oi.health().closed_by_reason.capacity_reached, 1);
    let markers = r.oi.take_capacity_evictions();
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].symbol, "BBB");
    assert_eq!(r.oi.open_count(), 2);
}

/// P2-14. `finish` closes everything still open as `CaptureEnded`.
#[test]
fn p14_finish_is_capture_ended() {
    let mut r = Run::new();
    r.step(confirmed("AAA", at(0), 10.0));
    r.step(confirmed("BBB", at(5), 10.0));
    let closed = r.oi.finish(at(10));
    assert_eq!(closed.len(), 2);
    assert!(closed
        .iter()
        .all(|o| o.close_reason == Some(OpportunityCloseReason::CaptureEnded)));
    assert_eq!(r.oi.health().closed_by_reason.capture_ended, 2);
    assert!(
        r.oi.finish(at(11)).is_empty(),
        "a second finish closes nothing"
    );
}

/// P2-15. Causality: replaying any prefix gives the same decisions, whatever
/// the later events are. See `b20` for the property form over every cut.
#[test]
fn p15_a_prefix_is_blind_to_what_follows() {
    let base = mixed_stream(7, 1_500);
    let cut = 700;
    let mut altered = base[..cut].to_vec();
    // An adversarial future: a flood of positive evidence for every symbol,
    // then a different price path. Nothing before the cut may notice.
    for (k, (_, t)) in base[cut..].iter().enumerate() {
        let symbol = SYMBOLS[k % SYMBOLS.len()];
        altered.push((confirmed(symbol, *t, 99.0), *t));
    }
    let a = decisions(&base);
    let b = decisions(&altered);
    assert_eq!(a[..cut], b[..cut]);
    assert_ne!(a[cut..], b[cut..], "the futures really differ");
}

/// P2-16. Determinism and uniqueness: two replays are byte-identical, a
/// restart split reuses no id, and an out-of-order equal-millisecond reopen
/// is refused and counted.
#[test]
fn p16_determinism_uniqueness_and_the_duplicate_guard() {
    let stream = mixed_stream(11, 2_000);
    assert_eq!(decisions(&stream), decisions(&stream));

    // Restart split: the first half, then a fresh engine for the second.
    let (first, second) = stream.split_at(1_000);
    let mut ids = std::collections::BTreeSet::new();
    let mut count = 0usize;
    for half in [first, second] {
        let mut r = Run::new();
        for (event, t) in half {
            r.step_at(event.clone(), *t);
        }
        let last = half.last().unwrap().1;
        for op in r.closed.iter().chain(r.oi.finish(last).iter()) {
            ids.insert(op.id.as_key());
            count += 1;
        }
    }
    assert_eq!(ids.len(), count, "no id re-issued across the restart");

    // The residual case: an out-of-order event at exactly an earlier open.
    let mut r = Run::new();
    r.step(confirmed("AAA", at(0), 10.0));
    r.tick(at(400)); // closes AAA
    assert_eq!(r.closed_for("AAA").len(), 1);
    let replayed = r.step_at(confirmed("AAA", at(0), 10.0), at(401));
    assert!(replayed.is_empty());
    assert!(r.open("AAA").is_none(), "refused, never minted twice");
    assert_eq!(r.oi.health().duplicate_identity_refused, 1);
    assert_eq!(r.oi.health().opportunities_opened, 1);
    // A different millisecond is a different identity and opens normally.
    r.step_at(
        confirmed("AAA", at(0) + Duration::milliseconds(1), 10.0),
        at(402),
    );
    assert!(r.open("AAA").is_some());
    assert_eq!(r.oi.health().duplicate_identity_refused, 1);
}

// ===========================================================================
// P3 brief list (20 cases)
// ===========================================================================

/// b01. One continuous move is one opportunity, however long it lasts.
#[test]
fn b01_one_continuous_move_is_one_opportunity() {
    let mut r = Run::new();
    r.step(confirmed("AAA", at(0), 10.0));
    for s in (1..=3_600).step_by(10) {
        r.step(bar("AAA", at(s), 10.0));
        if s % 200 == 1 {
            r.step(candidate("AAA", at(s), 10.0));
        }
    }
    assert!(r.closed.is_empty());
    let op = r.open("AAA").unwrap();
    assert_eq!(op.opened_at, at(0));
    assert_eq!(r.oi.health().opportunities_opened, 1);
    assert_eq!(op.age_secs(at(3_600)), 3_600);
}

/// b02. Unrelated bar traffic -- the symbol's own bars and every other
/// symbol's -- cannot keep a move alive.
#[test]
fn b02_bar_traffic_cannot_keep_a_move_alive() {
    let mut r = Run::new();
    r.step(confirmed("AAA", at(0), 10.0));
    for s in (1..=600).step_by(3) {
        r.step(bar("AAA", at(s), 10.0));
        r.step(bar("BBB", at(s), 20.0));
    }
    let closes = r.closed_for("AAA");
    assert_eq!(closes.len(), 1);
    assert_eq!(closes[0].closed_at, Some(at(300)));
    assert_eq!(
        closes[0].last_seen_at,
        at(298),
        "bars kept updating its state"
    );
    assert!(r.open("BBB").is_none(), "bars never open anything");
}

/// b03. Halt warnings alone -- "sent on every trade" -- cannot keep a dead
/// move alive.
#[test]
fn b03_halt_warnings_cannot_keep_a_dead_move_alive() {
    let mut r = Run::new();
    r.step(confirmed("AAA", at(0), 10.0));
    for s in 1..=400 {
        r.step(halt("AAA", at(s), 10.0 + s as f64 / 100.0));
    }
    let closes = r.closed_for("AAA");
    assert_eq!(closes.len(), 1);
    assert_eq!(closes[0].closed_at, Some(at(300)));
    assert_eq!(
        closes[0].close_reason,
        Some(OpportunityCloseReason::SetupInactivity)
    );
    // They did update its state: the price track followed them.
    assert!(closes[0].observed_high > 12.9);
}

/// b04. Catalyst updates alone cannot keep a move alive.
#[test]
fn b04_catalyst_updates_cannot_keep_a_move_alive() {
    let mut r = Run::new();
    r.step(confirmed("AAA", at(0), 10.0));
    for s in (10..=400).step_by(10) {
        r.step(catalyst("AAA", at(s)));
    }
    let closes = r.closed_for("AAA");
    assert_eq!(closes.len(), 1);
    assert_eq!(closes[0].closed_at, Some(at(300)));
}

/// b05. Same-move evidence of every positive kind extends the deadline to
/// exactly `last_positive + T_move`, and not a second more.
#[test]
fn b05_same_move_evidence_extends_correctly() {
    let mut r = Run::new();
    r.step(bar("AAA", at(-10), 10.0));
    r.step(confirmed("AAA", at(0), 10.0));
    let surge = ConsolidationEventKind::SurgeDetected;
    let consolidated = ConsolidationEventKind::ConsolidationConfirmed;
    let breakout_s = ConsolidationStrategy::ConsolidationBreakout;
    let steps: Vec<ScanEvent> = vec![
        candidate("AAA", at(250), 10.0),
        consolidation("AAA", at(500), 10.0, surge, breakout_s),
        consolidation("AAA", at(750), 10.0, consolidated, breakout_s),
        momentum("AAA", at(1_000), true), // an edge, and positive
        momentum("AAA", at(1_250), true), // level, still positive
        confirmed("AAA", at(1_500), 10.0),
        micro("AAA", at(1_750), 10.0),
    ];
    for event in steps {
        let (_, t, _) = event_symbol_time_price(&event).unwrap();
        r.tick(t - Duration::seconds(1));
        assert!(
            r.open("AAA").is_some(),
            "alive just before the evidence at {t}"
        );
        r.step(event);
        assert_eq!(r.open("AAA").unwrap().last_relevant_at, Some(t));
    }
    // A funnel level reading is not evidence: it does not move the clock.
    r.step(funnel("AAA", at(1_760), 10.0, true)); // first funnel reading: an edge...
    assert_eq!(
        r.open("AAA").unwrap().last_relevant_at,
        Some(at(1_760)),
        "...so it counts"
    );
    r.step(funnel("AAA", at(1_900), 10.0, true)); // level: state only
    assert_eq!(r.open("AAA").unwrap().last_relevant_at, Some(at(1_760)));
    assert!(r.tick(at(2_059)).is_empty());
    let closed = r.tick(at(2_060));
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].closed_at, Some(at(2_060)));
    assert_eq!(r.oi.health().opportunities_opened, 1);
}

/// b06. Invalidation semantics, both halves: confirm -> reject -> confirm
/// inside `T_move` is one opportunity; reject then silence is `invalidated`
/// at `last_positive + T_move`.
#[test]
fn b06_invalidation_semantics() {
    let mut r = Run::new();
    r.step(confirmed("AAA", at(0), 10.0));
    r.step(rejected("AAA", at(120), 9.9));
    r.step(rejected("AAA", at(150), 9.8));
    r.step(confirmed("AAA", at(290), 10.1));
    assert_eq!(r.oi.health().opportunities_opened, 1);
    assert_eq!(r.open("AAA").unwrap().invalidations_absorbed, 2);
    r.step(rejected("AAA", at(400), 9.7));
    r.tick(at(589));
    assert!(
        r.open("AAA").is_some(),
        "a rejection is never a close by itself"
    );
    let closed = r.tick(at(590));
    assert_eq!(closed.len(), 1);
    assert_eq!(
        closed[0].close_reason,
        Some(OpportunityCloseReason::Invalidated)
    );
    assert_eq!(
        closed[0].closed_at,
        Some(at(590)),
        "last positive 290 + 300"
    );
    assert_eq!(closed[0].invalidations_absorbed, 3);
    assert_eq!(r.oi.health().closed_by_reason.invalidated, 1);
    // A rejection with no opportunity open opens nothing.
    r.step(rejected("AAA", at(600), 9.6));
    assert!(r.open("AAA").is_none());
}

/// b07. A second independent setup opens a second opportunity with a
/// distinct id and re-frozen detection state (see also p07).
#[test]
fn b07_second_independent_setup() {
    let mut r = Run::new();
    r.step(bar("AAA", at(-60), 9.0));
    r.step(confirmed("AAA", at(0), 10.0));
    r.step(bar("AAA", at(100), 12.0));
    r.tick(at(301));
    r.step(breakout("AAA", at(900), 15.0));
    let first = &r.closed_for("AAA")[0];
    let second = r.open("AAA").unwrap();
    assert_ne!(first.id.as_key(), second.id.as_key());
    assert_eq!(second.first_detector, Strategy::ConsolidationBreakout);
    assert_eq!(
        second.detection_context.as_ref().unwrap().detected_at,
        at(900)
    );
    assert_eq!(
        second.detection_context.as_ref().unwrap().signal_price,
        15.0
    );
    assert_eq!(second.move_from_start_pct, Some(0.0));
}

/// b08. Overlapping detector confirmations -- the same detector repeatedly
/// and several at once -- never duplicate the opportunity.
#[test]
fn b08_overlapping_confirmations_do_not_duplicate() {
    let mut r = Run::new();
    for s in 0..50 {
        r.step(confirmed("AAA", at(s), 10.0));
        r.step(micro("AAA", at(s), 10.0));
    }
    assert_eq!(r.oi.open_count(), 1);
    assert_eq!(r.oi.health().opportunities_opened, 1);
    let op = r.open("AAA").unwrap();
    assert_eq!(op.confluence_count(), 2);
    assert_eq!(op.detectors_seen["IgnitionDetector"].events, 50);
}

/// b09. Confluence joins the opportunity it belongs to: the open one for its
/// symbol, never another symbol's, and never a closed one.
#[test]
fn b09_confluence_joins_the_correct_opportunity() {
    let mut r = Run::new();
    r.step(bar("BBB", at(-1), 20.0));
    r.step(confirmed("AAA", at(0), 10.0));
    r.step(micro("AAA", at(10), 10.0));
    r.step(momentum("BBB", at(10), true)); // BBB's own edge: opens BBB
    let aaa = r.open("AAA").unwrap();
    assert_eq!(aaa.confluence_count(), 2);
    assert!(!aaa.detectors_seen.contains_key("MomentumScorer"));
    assert_eq!(
        r.open("BBB").unwrap().first_detector,
        Strategy::MomentumScorer
    );
    r.tick(at(400)); // both close
    r.step(breakout("AAA", at(500), 11.0));
    let reopened = r.open("AAA").unwrap();
    assert_eq!(reopened.detectors_seen.len(), 1);
    assert!(reopened
        .detectors_seen
        .contains_key("ConsolidationBreakout"));
    let closed_aaa = r.closed_for("AAA")[0];
    assert!(!closed_aaa
        .detectors_seen
        .contains_key("ConsolidationBreakout"));
}

/// b10. One symbol, many opportunities in a day: every id is unique.
#[test]
fn b10_same_symbol_multiple_opportunities_have_unique_ids() {
    let mut r = Run::new();
    for k in 0..20 {
        r.step(confirmed("AAA", at(k * 400), 10.0));
    }
    r.oi.finish(at(20 * 400))
        .into_iter()
        .for_each(|o| r.closed.push(o));
    let ids: std::collections::BTreeSet<String> = r.closed.iter().map(|o| o.id.as_key()).collect();
    assert_eq!(r.closed.len(), 20);
    assert_eq!(ids.len(), 20);
    assert!(r.closed.iter().all(|o| o.session_date == "2026-09-14"));
}

/// b12. Episodes bind to the correct move. The episode lifecycle is
/// unchanged and runs on the same stream; membership keeps its algorithm.
#[test]
fn b12_episodes_bind_to_the_correct_move() {
    let mut r = Run::new();
    let mut episodes = EpisodeTracker::new();
    let mut closed_episodes = Vec::new();
    let mut feed = |r: &mut Run, e: ScanEvent, t: DateTime<Utc>| {
        closed_episodes.extend(episodes.observe(&e, t));
        r.step_at(e, t);
    };
    // Move 1: confirm, reject (episode 1 ends), confirm (episode 2).
    feed(&mut r, confirmed("AAA", at(0), 10.0), at(0));
    feed(&mut r, rejected("AAA", at(60), 9.9), at(60));
    feed(&mut r, confirmed("AAA", at(90), 10.1), at(90));
    // Complete silence, then an unrelated event: both units expire at 390.
    feed(&mut r, bar("ZZTICK", at(500), 1.0), at(500));
    // Move 2, much later.
    feed(&mut r, confirmed("AAA", at(1_000), 11.0), at(1_000));
    feed(&mut r, bar("ZZTICK", at(1_400), 1.0), at(1_400));
    let mut ops = r.closed.clone();
    ops.extend(r.oi.finish(at(1_500)));
    closed_episodes.extend(episodes.finish(at(1_500)));
    let aaa_ops: Vec<Opportunity> = ops.into_iter().filter(|o| o.symbol == "AAA").collect();
    let aaa_eps: Vec<_> = closed_episodes
        .into_iter()
        .filter(|e| e.id.symbol == "AAA")
        .collect();
    assert_eq!(aaa_ops.len(), 2);
    assert_eq!(aaa_eps.len(), 3);

    let report = map_memberships(&aaa_ops, &aaa_eps);
    let move1 = aaa_ops
        .iter()
        .find(|o| o.opened_at == at(0))
        .unwrap()
        .id
        .as_key();
    let move2 = aaa_ops
        .iter()
        .find(|o| o.opened_at == at(1_000))
        .unwrap()
        .id
        .as_key();
    assert_eq!(
        report.members_of(&move1).len(),
        2,
        "both fragments of move 1"
    );
    assert_eq!(report.members_of(&move2).len(), 1);
    for ep in &aaa_eps {
        let expected = if ep.opened_at < at(1_000) {
            &move1
        } else {
            &move2
        };
        assert_eq!(
            report.opportunity_of(&ep.id.as_key()),
            Some(expected.as_str())
        );
    }
}

/// b12, the other half: an episode kept alive by bars after the move ended
/// is REPORTED ambiguous against the move it opened in -- never silently
/// attached to the next move (preregistration section 8).
#[test]
fn b12_an_episode_outliving_its_move_is_reported_not_reassigned() {
    let mut r = Run::new();
    let mut episodes = EpisodeTracker::new();
    let mut eps = Vec::new();
    let mut feed = |r: &mut Run, e: ScanEvent, t: DateTime<Utc>| {
        eps.extend(episodes.observe(&e, t));
        r.step_at(e, t);
    };
    feed(&mut r, confirmed("AAA", at(0), 10.0), at(0));
    for s in (60..=900).step_by(60) {
        feed(&mut r, bar("AAA", at(s), 10.0), at(s)); // episode stays alive
    }
    feed(&mut r, confirmed("AAA", at(960), 10.5), at(960)); // move 2
    let mut ops = r.closed.clone();
    ops.extend(r.oi.finish(at(1_300)));
    eps.extend(episodes.finish(at(1_300)));
    let report = map_memberships(&ops, &eps);
    let move1 = ops
        .iter()
        .find(|o| o.opened_at == at(0))
        .unwrap()
        .id
        .as_key();
    let move2 = ops
        .iter()
        .find(|o| o.opened_at == at(960))
        .unwrap()
        .id
        .as_key();
    let ambiguous = report
        .ambiguous_episodes
        .iter()
        .find(|a| a.opened_at == at(0))
        .expect("the long-lived episode is reported");
    match &ambiguous.ambiguity {
        AmbiguityReason::EpisodeOutlivesOpportunity { opportunity_id, .. } => {
            assert_eq!(opportunity_id, &move1)
        }
        other => panic!("unexpected ambiguity {other:?}"),
    }
    assert!(report.members_of(&move2).iter().all(|e| !e.is_empty()));
    assert_ne!(
        report.opportunity_of(&ambiguous.episode_id),
        Some(move2.as_str())
    );
}

/// b13. Every ranking row names the move that was open at its timestamp.
#[test]
fn b13_ranking_rows_bind_to_the_correct_move() {
    let mut r = Run::new();
    let mut rows = Vec::new();
    let mut open_at: Vec<(DateTime<Utc>, Vec<(String, DateTime<Utc>)>)> = Vec::new();
    for s in (0..=2_400).step_by(10) {
        let t = at(s);
        match s {
            0 | 1_000 | 2_000 => {
                r.step(confirmed("AAA", t, 10.0));
            }
            500 => {
                r.step(confirmed("BBB", t, 5.0));
            }
            _ if s % 120 == 0 && s < 1_200 && s > 500 => {
                r.step(candidate("BBB", t, 5.0));
            }
            _ => {
                r.tick(t);
            }
        }
        let state: Vec<(String, DateTime<Utc>)> =
            r.oi.open_opportunities()
                .map(|o| (o.id.as_key(), o.opened_at))
                .collect();
        if let Some(snaps) = r.oi.rank(t) {
            open_at.push((t, state));
            rows.extend(snaps);
        }
    }
    assert!(rows.len() > 50);
    for row in &rows {
        let (_, state) = open_at.iter().find(|(t, _)| *t == row.timestamp).unwrap();
        let (_, opened) = state
            .iter()
            .find(|(id, _)| *id == row.opportunity_id)
            .unwrap_or_else(|| panic!("{} was not open at {}", row.opportunity_id, row.timestamp));
        assert_eq!(row.opened_at, Some(*opened));
        assert_eq!(
            row.opportunity_age_secs,
            (row.timestamp - *opened).num_seconds()
        );
    }
    let aaa: std::collections::BTreeSet<&str> = rows
        .iter()
        .filter(|r| r.symbol == "AAA")
        .map(|r| r.opportunity_id.as_str())
        .collect();
    assert_eq!(
        aaa.len(),
        3,
        "three moves, three ids, rows split between them"
    );
    for row in rows.iter().filter(|r| r.symbol == "AAA") {
        let opened = row.opened_at.unwrap();
        assert!(
            row.timestamp < opened + Duration::seconds(300),
            "no row after its move ended"
        );
    }
}

/// b16. The market-day boundary: a winter after-hours move crossing UTC
/// midnight does NOT close; 04:00 ET does, on the event path and on the
/// expiry path.
#[test]
fn b16_market_day_boundary() {
    // Winter: 18:50 EST on 01-15 is 23:50Z; UTC midnight is 19:00 EST.
    let mut r = Run::new();
    let start = utc(2026, 1, 15, 23, 50, 0);
    r.step(confirmed("AAA", start, 5.0));
    for k in 1..=30 {
        r.step(momentum("AAA", start + Duration::seconds(60 * k), true));
    }
    assert!(
        r.closed.is_empty(),
        "the UTC date changed; the market day did not"
    );
    let op = r.open("AAA").unwrap();
    assert_eq!(
        op.session_date, "2026-01-15",
        "identity keeps its UTC partition"
    );
    assert_eq!(op.opened_phase, Some(TradingSession::AfterHours));

    // Summer: the same crossing at 20:00 EDT.
    let mut r = Run::new();
    let start = utc(2026, 9, 14, 23, 58, 0);
    r.step(confirmed("AAA", start, 5.0));
    let closed = r.step(candidate("AAA", start + Duration::seconds(240), 5.0));
    assert!(closed.is_empty());

    // 04:00 ET, via expiry: last evidence 03:57 EST, the next processed
    // event is another symbol's at 04:10.
    let mut r = Run::new();
    let last = utc(2026, 1, 16, 8, 57, 0);
    r.step(confirmed("AAA", last, 5.0));
    let closed = r.tick(utc(2026, 1, 16, 9, 10, 0));
    assert_eq!(closed.len(), 1);
    assert_eq!(
        closed[0].close_reason,
        Some(OpportunityCloseReason::SessionBoundary)
    );
    assert_eq!(
        closed[0].closed_at,
        Some(last),
        "expiry path: last evidence, then the floor"
    );

    // Due before 04:00 and found before 04:00 is inactivity, not a boundary.
    let mut r = Run::new();
    r.step(confirmed("AAA", utc(2026, 1, 16, 8, 50, 0), 5.0));
    let closed = r.tick(utc(2026, 1, 16, 8, 56, 0));
    assert_eq!(
        closed[0].close_reason,
        Some(OpportunityCloseReason::SetupInactivity)
    );
}

/// D4-5 under move-v1: a boundary close found by expiry is never dated before
/// a ranking instant the move took part in.
#[test]
fn d4_5_move_v1_session_boundary_floor() {
    let mut r = Run::new();
    let last = utc(2026, 1, 16, 8, 57, 0); // 03:57 EST
    r.step(confirmed("AAA", last, 5.0));
    let ranked = last + Duration::seconds(120);
    r.tick(ranked);
    assert!(r.oi.rank(ranked).unwrap().iter().any(|s| s.symbol == "AAA"));
    let closed = r.tick(utc(2026, 1, 16, 9, 10, 0));
    assert_eq!(
        closed[0].close_reason,
        Some(OpportunityCloseReason::SessionBoundary)
    );
    assert_eq!(closed[0].closed_at, Some(ranked));
}

/// b17. Reconnect without a restart: a gap is only a period without events.
/// Edge state and the open move persist through it.
#[test]
fn b17_reconnect_is_just_a_gap() {
    let mut r = Run::new();
    r.step(bar("AAA", at(0), 10.0));
    r.step(momentum("AAA", at(1), true)); // opens
                                          // Short outage: nothing at all for 200 s.
    r.step(momentum("AAA", at(201), true));
    assert!(r.closed.is_empty(), "a gap inside T_move changes nothing");
    // Long outage: nothing for 400 s. The first event after it closes the
    // move on the ordinary clock.
    let closed = r.step(momentum("AAA", at(601), true));
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].closed_at, Some(at(501)));
    // Momentum was still qualifying across the outage: the edge state
    // persisted, so this reading was a level, not an edge, and re-opens nothing.
    assert!(r.open("AAA").is_none());
    r.step(momentum("AAA", at(661), false));
    r.step(momentum("AAA", at(721), true));
    assert_eq!(
        r.open("AAA").unwrap().opened_at,
        at(721),
        "a real flip re-opens"
    );
}

/// b18. Process restart: a fresh engine has no edge state, so the first true
/// reading it sees is an edge (section 2), and its ids are new.
#[test]
fn b18_restart_first_true_reading_is_an_edge() {
    let mut before = Run::new();
    before.step(bar("AAA", at(0), 10.0));
    before.step(momentum("AAA", at(1), true));
    before.step(funnel("BBB", at(1), 5.0, true));
    before.step(momentum("AAA", at(61), true));
    let old: Vec<String> = before
        .oi
        .finish(at(62))
        .iter()
        .map(|o| o.id.as_key())
        .collect();
    assert_eq!(old.len(), 2);

    let mut after = Run::new();
    after.step(bar("AAA", at(90), 10.0));
    after.step(momentum("AAA", at(91), true)); // a level in reality, an edge here
    after.step(funnel("BBB", at(91), 5.0, true));
    let aaa = after
        .open("AAA")
        .expect("first true reading after restart opens");
    assert_eq!(aaa.opened_at, at(91));
    assert!(after.open("BBB").is_some());
    let new: Vec<String> = after
        .oi
        .open_opportunities()
        .map(|o| o.id.as_key())
        .collect();
    assert!(new.iter().all(|id| !old.contains(id)));
}

/// b19. Capacity, at the shipped configuration: unchanged 16,375, with a full
/// universe of simultaneously live moves and no eviction.
#[test]
fn b19_capacity_is_unchanged_and_holds_the_universe() {
    let config = OiConfig::default();
    assert_eq!(config.max_open_opportunities(), 16_375);
    let mut r = Run::with(config);
    for i in 0..13_100 {
        r.step(confirmed(&format!("S{i:05}"), at(i as i64 % 200), 10.0));
    }
    assert_eq!(r.oi.open_count(), 13_100);
    assert_eq!(r.oi.health().capacity_evictions, 0);
}

/// b20. No future information, as a property: truncating the stream at any
/// point yields identical lifecycle decisions for every event before the cut.
/// Also: no close is ever dated after the instant it was decided.
#[test]
fn b20_truncation_at_any_point_changes_no_earlier_decision() {
    let stream = mixed_stream(23, 1_200);
    let full = decisions(&stream);
    for cut in (0..=stream.len()).step_by(37).chain([stream.len()]) {
        let prefix = decisions(&stream[..cut]);
        assert_eq!(prefix[..], full[..cut], "decisions before cut {cut} moved");
    }
    let mut r = Run::new();
    for (event, now) in &stream {
        for op in r.step_at(event.clone(), *now) {
            if op.close_reason != Some(OpportunityCloseReason::SessionBoundary) {
                assert!(
                    op.closed_at.unwrap() <= *now,
                    "a close dated in its own future"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Supporting properties
// ---------------------------------------------------------------------------

/// `symbol-activity-v1` is still the pre-D5 rule: bars keep an opportunity
/// alive, and it writes schema 2 with no move-v1 fields.
#[test]
fn symbol_activity_v1_is_still_selectable_and_unchanged_in_kind() {
    let mut r = Run::with(OiConfig::symbol_activity_v1());
    r.step(confirmed("AAA", at(0), 10.0));
    for s in (10..=900).step_by(10) {
        r.step(bar("AAA", at(s), 10.0));
    }
    let op = r
        .open("AAA")
        .expect("bars keep a symbol-activity opportunity alive");
    assert_eq!(op.schema_version, 2);
    assert!(op.last_relevant_at.is_none() && op.opened_phase.is_none());
    let snaps = r.oi.rank(at(900)).unwrap();
    assert_eq!(snaps[0].schema_version, 2);
    assert_eq!(
        snaps[0].versions.lifecycle,
        LIFECYCLE_SYMBOL_ACTIVITY_V1_VERSION
    );
    assert!(!serde_json::to_string(&snaps[0])
        .unwrap()
        .contains("openedPhase"));
}

/// Versions and schema say which lifecycle produced a row; old rows and old
/// configurations read as the lifecycle they actually ran.
#[test]
fn versions_and_legacy_reads() {
    let v = OiConfig::default().versions();
    assert_eq!(v.opportunity_schema, OPPORTUNITY_SCHEMA_VERSION);
    assert_eq!(OPPORTUNITY_SCHEMA_VERSION, 3);
    assert_eq!(v.lifecycle, "opportunity-lifecycle-move-v1");

    let mut old = serde_json::to_value(&v).unwrap();
    old.as_object_mut().unwrap().remove("lifecycle");
    let back: OiVersions = serde_json::from_value(old).unwrap();
    assert_eq!(back.lifecycle, "opportunity-lifecycle-symbol-activity-v1");

    let mut cfg = serde_json::to_value(OiConfig::default()).unwrap();
    let obj = cfg.as_object_mut().unwrap();
    assert_eq!(obj["lifecycle"], "move-v1");
    assert_eq!(obj["moveInactivitySecs"], 300);
    obj.remove("lifecycle");
    obj.remove("moveInactivitySecs");
    let back: OiConfig = serde_json::from_value(cfg).unwrap();
    assert_eq!(back.lifecycle, Lifecycle::SymbolActivityV1);
    assert_eq!(back.move_inactivity_secs, 300);

    // The legacy close token still reads.
    let legacy: OpportunityCloseReason = serde_json::from_str("\"inactivity\"").unwrap();
    assert_eq!(legacy, OpportunityCloseReason::Inactivity);
    assert_eq!(
        serde_json::to_string(&OpportunityCloseReason::SetupInactivity).unwrap(),
        "\"setup_inactivity\""
    );
    assert_eq!(
        serde_json::to_string(&OpportunityCloseReason::Invalidated).unwrap(),
        "\"invalidated\""
    );
}

/// Health reports every close by reason, and the totals reconcile.
#[test]
fn closes_are_counted_by_the_new_reasons() {
    let mut r = Run::new();
    r.step(confirmed("AAA", at(0), 10.0));
    r.step(confirmed("BBB", at(0), 10.0));
    r.step(rejected("BBB", at(10), 10.0));
    r.tick(at(400));
    let c = r.oi.health().closed_by_reason;
    assert_eq!((c.setup_inactivity, c.invalidated, c.inactivity), (1, 1, 0));
    assert_eq!(c.total(), r.oi.health().opportunities_closed);
    let json = serde_json::to_value(c).unwrap();
    assert_eq!(json["setupInactivity"], 1);
    assert_eq!(json["invalidated"], 1);
}

// ---------------------------------------------------------------------------
// A deterministic mixed stream, and the decision log the causality tests
// compare
// ---------------------------------------------------------------------------

const SYMBOLS: [&str; 5] = ["AAA", "BBB", "CCC", "DDD", "EEE"];

/// Every event kind, several symbols, gaps both inside and beyond `T_move`,
/// and a market-day crossing -- generated by a fixed LCG, so any failure
/// reproduces exactly.
fn mixed_stream(seed: u64, n: usize) -> Vec<(ScanEvent, DateTime<Utc>)> {
    let mut state = seed;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as u32
    };
    // 07:50Z = 03:50 EDT, so the stream crosses the 04:00 market-day open.
    let mut t = utc(2026, 9, 14, 7, 50, 0);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let roll = next();
        // Mostly short steps, occasionally a long quiet stretch.
        let step = if roll % 50 == 0 {
            400 + roll % 300
        } else {
            roll % 20
        };
        t += Duration::seconds(i64::from(step));
        let symbol = SYMBOLS[(next() % SYMBOLS.len() as u32) as usize];
        let price = 5.0 + f64::from(next() % 500) / 100.0;
        let event = match next() % 14 {
            0 => confirmed(symbol, t, price),
            1 => rejected(symbol, t, price),
            2 => candidate(symbol, t, price),
            3 => micro(symbol, t, price),
            4 => breakout(symbol, t, price),
            5 => consolidation(
                symbol,
                t,
                price,
                ConsolidationEventKind::SurgeDetected,
                ConsolidationStrategy::Micropullback,
            ),
            6 | 7 => momentum(symbol, t, next() % 2 == 0),
            8 => funnel(symbol, t, price, next() % 3 != 0),
            9 => halt(symbol, t, price),
            10 => catalyst(symbol, t),
            _ => bar(symbol, t, price),
        };
        out.push((event, t));
    }
    out
}

/// One entry per event: everything the lifecycle decided while processing
/// it -- closes (id, reason, closedAt), the open set after it, and any
/// ranking rows. Serialized, so equality is byte equality.
fn decisions(stream: &[(ScanEvent, DateTime<Utc>)]) -> Vec<String> {
    let mut oi = OpportunityIntelligence::new(OiConfig::default());
    stream
        .iter()
        .map(|(event, now)| {
            let closed: Vec<(
                String,
                Option<OpportunityCloseReason>,
                Option<DateTime<Utc>>,
            )> = oi
                .observe(event, *now)
                .into_iter()
                .map(|o| (o.id.as_key(), o.close_reason, o.closed_at))
                .collect();
            let mut open: Vec<(String, Option<DateTime<Utc>>)> = oi
                .open_opportunities()
                .map(|o| (o.id.as_key(), o.last_relevant_at))
                .collect();
            open.sort();
            let rows = oi.rank(*now).unwrap_or_default();
            serde_json::to_string(&(closed, open, rows)).unwrap()
        })
        .collect()
}
