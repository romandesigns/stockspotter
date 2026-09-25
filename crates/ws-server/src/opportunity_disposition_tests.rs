//! D4: opportunity disposition, end to end through the real drivers.
//!
//! `docs/measurement-correctness-contract-2026-09-25.md` D4.6. The collector's
//! own rule (visibility, first-terminal-wins, the `(id, openedAt)` guard,
//! ordering, cost) is tested in `backtest_metrics::opportunity_outcome`. These
//! test the PLUMBING: that every engine close path reaches the rows, through
//! `observe_both` / `finish_both` -- the functions `main.rs` calls -- and that
//! the persisted `opportunity_closed` records reproduce the dispositions.
//!
//! Every scenario runs with capture on, and rows are read back from the
//! artifact, so what is asserted is what a reader of the files would see.
//!
//! # Pinned to `symbol-activity-v1` (D5)
//!
//! Every scenario here was written against the pre-D5 lifecycle, where any
//! event refreshes an opportunity and an `inactivity` close needs *all*
//! events for the symbol to stop -- and several assert exactly that (for
//! example "invalidation is not a close today"). They stay on that lifecycle,
//! unchanged, as the proof that D4's plumbing still carries its tokens. The
//! same plumbing under `move-v1` (the default), with `setup_inactivity` and
//! `invalidated`, is proven in `opportunity_lifecycle_disposition_tests.rs`.

use std::path::{Path, PathBuf};

use backtest_metrics::opportunity::OiConfig;
use backtest_metrics::opportunity_outcome::{
    disposition_as_of_settlement, ClosureNotice, OpportunityDisposition, OpportunityOutcomeRow,
    OPPORTUNITY_OUTCOME_VERSION,
};
use chrono::{DateTime, Duration, TimeZone, Utc};
use market_data::{IgnitionEventKind, ScanEvent};

use super::{finish_both, observe_both, OutcomeDriver, OutcomeRecorder};
use crate::opportunity_shadow::{ShadowDriver, ShadowRecorder};
use crate::research_writer::CaptureMarker;

fn at(s: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_789_378_200 + s, 0).unwrap() // 2026-09-14T09:30:00Z
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

/// An in-progress bar: a forward price for anchors (receipt clock), and --
/// crucially for these scenarios -- NOT a qualifying event, so after a close it
/// feeds the anchors without reopening the opportunity.
fn bar(symbol: &str, t: DateTime<Utc>, close: f64) -> ScanEvent {
    ScanEvent::BarUpdate {
        symbol: symbol.into(),
        timestamp: t,
        open: close,
        high: close,
        low: close,
        close,
        volume: 100,
        is_final: false,
        interval_secs: 60,
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "d4-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Both research consumers, capturing into one directory, as `main.rs` wires
/// them.
struct Live {
    shadow: ShadowDriver,
    outcomes: OutcomeDriver,
}

impl Live {
    fn new(dir: &Path, config: OiConfig) -> Self {
        let versions = config.versions();
        let shadow = ShadowDriver::new(config, ShadowRecorder::start(dir.to_path_buf()));
        let outcomes = OutcomeDriver::new(OutcomeRecorder::start(dir.to_path_buf()), &versions);
        Self { shadow, outcomes }
    }

    fn step(&mut self, event: &ScanEvent, now: DateTime<Utc>) {
        observe_both(&mut self.shadow, &mut self.outcomes, event, now);
    }

    fn finish(&mut self, now: DateTime<Utc>) {
        finish_both(&mut self.shadow, &mut self.outcomes, now);
    }
}

fn lines_of(dir: &Path, prefix: &str) -> Vec<String> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            name.starts_with(prefix) && name.ends_with(".ndjson")
        })
        .collect();
    files.sort();
    files
        .iter()
        .flat_map(|p| {
            std::fs::read_to_string(p)
                .unwrap()
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Every outcome row the capture wrote, in file order.
fn rows(dir: &Path) -> Vec<OpportunityOutcomeRow> {
    lines_of(dir, "opportunity-outcomes-")
        .into_iter()
        .filter(|l| !l.contains("\"kind\""))
        .map(|l| serde_json::from_str(&l).expect("an outcome row"))
        .collect()
}

/// The `opportunity_closed` records the OI capture persisted, in write order.
fn persisted_closures(dir: &Path) -> Vec<ClosureNotice> {
    oi_markers(dir)
        .into_iter()
        .filter(|m| m.kind == "opportunity_closed")
        .map(|m| serde_json::from_value(m.data.expect("payload")).expect("a closure notice"))
        .collect()
}

fn oi_markers(dir: &Path) -> Vec<CaptureMarker> {
    lines_of(dir, "opportunity-intelligence-markers-")
        .into_iter()
        .map(|l| serde_json::from_str(&l).expect("a capture marker"))
        .collect()
}

fn rows_for<'a>(rows: &'a [OpportunityOutcomeRow], symbol: &str) -> Vec<&'a OpportunityOutcomeRow> {
    rows.iter().filter(|r| r.symbol == symbol).collect()
}

/// A background symbol trading every 10s keeps the receipt clock -- and so
/// expiry and ranking -- moving, exactly as the rest of the market does.
fn tick_other(live: &mut Live, t: i64) {
    live.step(
        &confirmed("OTHER", at(t), 5.0 + (t % 7) as f64 * 0.01),
        at(t),
    );
}

// ---------------------------------------------------------------------------

/// D4.6-1 and D4.6-6. Inactivity reaches the row with the engine's backdated
/// lifecycle instant and the receipt instant it was learned at; the busy
/// symbol's rows stay `still_open`; and the close changes nothing about the
/// measurement -- the row equals the pre-D4 pipeline's except for the three
/// disposition fields.
#[test]
fn d4_1_inactivity_reaches_the_row() {
    let run = |apply_closures: bool| {
        let dir = temp_dir(if apply_closures { "inact" } else { "inact-ctl" });
        let mut live = Live::new(&dir, OiConfig::symbol_activity_v1());
        // The control is the pre-D4 pipeline: identical, except that the
        // engine's closes are dropped on the floor instead of applied.
        let step = |live: &mut Live, event: &ScanEvent, now: DateTime<Utc>| {
            if apply_closures {
                live.step(event, now);
            } else {
                live.outcomes.observe_price(event, now);
                let step = live.shadow.observe(event, now);
                live.outcomes.anchor_and_settle(&step.snapshots, now);
            }
        };
        step(&mut live, &confirmed("AAA", at(0), 10.0), at(0)); // opens, ranks: anchor at 0
        step(&mut live, &confirmed("OTHER", at(0), 5.0), at(0));
        step(&mut live, &bar("AAA", at(10), 10.05), at(10)); // AAA's last activity
        for t in (20..=2_000).step_by(10) {
            if t > 310 && t % 30 == 20 {
                // Prices after the close keep feeding the anchors without
                // reopening the opportunity (a bar is not a qualifying event).
                step(
                    &mut live,
                    &bar("AAA", at(t), 10.0 + t as f64 / 10_000.0),
                    at(t),
                );
            }
            step(
                &mut live,
                &confirmed("OTHER", at(t), 5.0 + (t % 7) as f64 * 0.01),
                at(t),
            );
        }
        live.finish(at(2_010));
        let out = rows(&dir);
        let closures = persisted_closures(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        (out, closures)
    };
    let (rows, closures) = run(true);
    let (control, _) = run(false);

    let first = rows_for(&rows, "AAA")
        .into_iter()
        .find(|r| r.anchor_at == at(0))
        .unwrap();
    assert_eq!(
        first.provenance.measurement_version,
        OPPORTUNITY_OUTCOME_VERSION
    );
    assert_eq!(
        first.opportunity_disposition,
        OpportunityDisposition::Inactivity
    );
    assert_eq!(
        first.opportunity_closed_at,
        Some(at(310)),
        "last_seen 10 + inactivity 300"
    );
    assert_eq!(
        first.opportunity_close_observed_at,
        Some(at(310)),
        "learned from OTHER at 310"
    );

    // Measurement is untouched: identical to the pipeline that dropped closes,
    // field for field, apart from the disposition fields.
    let ctl = rows_for(&control, "AAA")
        .into_iter()
        .find(|r| r.anchor_at == at(0))
        .unwrap();
    assert_eq!(
        ctl.opportunity_disposition,
        OpportunityDisposition::StillOpen
    );
    let mut scrubbed = first.clone();
    scrubbed.opportunity_disposition = OpportunityDisposition::StillOpen;
    scrubbed.opportunity_closed_at = None;
    scrubbed.opportunity_close_observed_at = None;
    assert_eq!(
        &scrubbed, ctl,
        "a close is provenance, never a measurement change"
    );
    assert_eq!(rows.len(), control.len(), "and never adds or removes a row");

    // Every AAA anchor (issued while it was open, up to the 300s window)
    // learned of the close in time.
    for r in rows_for(&rows, "AAA") {
        assert_eq!(
            r.opportunity_disposition,
            OpportunityDisposition::Inactivity,
            "{r:?}"
        );
        assert!(
            r.opportunity_closed_at.unwrap() >= r.anchor_at,
            "never closed before anchored"
        );
    }
    // D4.6-6: OTHER traded throughout, so every row it settled says still open.
    let other = rows_for(&rows, "OTHER");
    let (settled, ended): (Vec<&OpportunityOutcomeRow>, Vec<&OpportunityOutcomeRow>) = other
        .into_iter()
        .partition(|r| r.opportunity_disposition == OpportunityDisposition::StillOpen);
    assert!(!settled.is_empty());
    assert!(
        ended
            .iter()
            .all(|r| r.opportunity_disposition == OpportunityDisposition::CaptureEnded),
        "the only other disposition OTHER can carry is the capture end"
    );
    assert_eq!(
        closures.iter().filter(|c| c.symbol == "AAA").count(),
        1,
        "one close, persisted once"
    );
}

/// D4.6-2, via event: a next-date event for the symbol closes the prior-date
/// opportunity as `session_boundary`, and the reopened opportunity's anchors
/// are a different opportunity entirely.
#[test]
fn d4_2_session_boundary_via_event() {
    let dir = temp_dir("session-event");
    let mut live = Live::new(&dir, OiConfig::symbol_activity_v1());
    let day1 = Utc.with_ymd_and_hms(2026, 9, 14, 23, 59, 0).unwrap();
    let day2 = day1 + Duration::seconds(90);
    live.step(&confirmed("AAA", day1, 10.0), day1); // anchor at day1
    live.step(&confirmed("AAA", day2, 10.5), day2); // closes + reopens AAA
    live.finish(day2 + Duration::seconds(5));

    let rows = rows(&dir);
    let old = rows.iter().find(|r| r.anchor_at == day1).unwrap();
    assert_eq!(
        old.opportunity_disposition,
        OpportunityDisposition::SessionBoundary
    );
    assert_eq!(old.opportunity_close_observed_at, Some(day2));
    assert_eq!(old.opportunity_closed_at, Some(day2));
    let new = rows.iter().find(|r| r.anchor_at == day2).unwrap();
    assert_ne!(new.opportunity_id, old.opportunity_id);
    assert_eq!(
        new.opportunity_disposition,
        OpportunityDisposition::CaptureEnded,
        "the next-day opportunity was still open when capture ended"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// D4.6-2, via expiry across midnight -- the D4-5 regression. The engine used
/// to date this close `last_seen_at`, before anchors it had already issued.
/// Both instants are now at or after every anchor of the opportunity; the
/// disposition decision uses the observed one.
#[test]
fn d4_2_session_boundary_via_expiry_is_never_dated_before_an_anchor() {
    let dir = temp_dir("session-expiry");
    let mut live = Live::new(&dir, OiConfig::symbol_activity_v1());
    let last_seen = Utc.with_ymd_and_hms(2026, 9, 14, 23, 58, 0).unwrap();
    live.step(&confirmed("AAA", last_seen, 10.0), last_seen);
    for s in (10..=600).step_by(10) {
        let t = last_seen + Duration::seconds(s);
        live.step(&confirmed("OTHER", t, 5.0), t);
    }
    live.finish(last_seen + Duration::seconds(610));

    let rows = rows(&dir);
    let aaa = rows_for(&rows, "AAA");
    assert!(
        aaa.len() > 1,
        "AAA was ranked -- anchored -- several times while open"
    );
    for r in &aaa {
        assert_eq!(
            r.opportunity_disposition,
            OpportunityDisposition::SessionBoundary
        );
        let observed = r.opportunity_close_observed_at.unwrap();
        let closed = r.opportunity_closed_at.unwrap();
        assert!(
            observed >= r.anchor_at,
            "the close cannot be learned before the anchor"
        );
        assert!(
            closed >= r.anchor_at,
            "D4-5: closedAt {closed} precedes anchor {} (was last_seen_at {last_seen})",
            r.anchor_at
        );
        assert!(closed > last_seen, "the backdated instant is gone");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// D4.6-3. A capacity eviction reaches the row as `capacity_reached`, and the
/// eviction marker is still written beside it.
#[test]
fn d4_3_capacity_eviction_reaches_the_row() {
    // Capacity 2: universe 2, no safety margin. The rank bound follows it so
    // the configuration still satisfies the D6 invariant.
    let config = OiConfig {
        supported_symbol_universe: 2,
        bound_safety_num: 1,
        bound_safety_den: 1,
        max_rank_cohort: 2,
        ..OiConfig::symbol_activity_v1()
    };
    assert_eq!(config.max_open_opportunities(), 2);
    assert!(config.capacity_invariant().is_ok());
    let dir = temp_dir("capacity");
    let mut live = Live::new(&dir, config);
    live.step(&confirmed("AAA", at(0), 10.0), at(0)); // ranked alone: anchor at 0
    live.step(&confirmed("BBB", at(40), 5.0), at(40)); // ranked: anchors at 40
    live.step(&confirmed("CCC", at(50), 7.0), at(50)); // evicts AAA, least recent
    live.finish(at(60));

    let rows = rows(&dir);
    let aaa = rows_for(&rows, "AAA");
    assert!(!aaa.is_empty());
    for r in aaa {
        assert_eq!(
            r.opportunity_disposition,
            OpportunityDisposition::CapacityReached
        );
        assert_eq!(r.opportunity_close_observed_at, Some(at(50)));
    }
    let markers = oi_markers(&dir);
    assert!(
        markers
            .iter()
            .any(|m| m.kind == "opportunity_capacity_reached"),
        "the eviction marker is still emitted"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// D4.6-4. Capture end gives still-open opportunities' anchors
/// `capture_ended` as disposition and censor; an anchor whose opportunity had
/// already closed keeps that earlier reason (first terminal wins), even though
/// its row is only written at the capture end.
#[test]
fn d4_4_capture_end() {
    let dir = temp_dir("capture-end");
    let mut live = Live::new(&dir, OiConfig::symbol_activity_v1());
    live.step(&confirmed("GONE", at(0), 3.0), at(0)); // anchor at 0
    live.step(&confirmed("LIVE", at(5), 4.0), at(5));
    for t in (10..=400).step_by(10) {
        live.step(&confirmed("LIVE", at(t), 4.0), at(t)); // GONE expires at 300
    }
    live.finish(at(410)); // before GONE's anchor (deadline 1320) settles

    let rows = rows(&dir);
    for r in rows_for(&rows, "GONE") {
        assert_eq!(
            r.opportunity_disposition,
            OpportunityDisposition::Inactivity
        );
        assert!(
            r.censor_reasons
                .contains(&backtest_metrics::horizon::CensorReason::CaptureEnded),
            "the row is still censored by the capture end -- disposition is not a censor"
        );
    }
    let live_rows = rows_for(&rows, "LIVE");
    assert!(!live_rows.is_empty());
    for r in live_rows {
        assert_eq!(
            r.opportunity_disposition,
            OpportunityDisposition::CaptureEnded
        );
        assert_eq!(r.opportunity_close_observed_at, Some(at(410)));
        assert!(r
            .censor_reasons
            .contains(&backtest_metrics::horizon::CensorReason::CaptureEnded));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// D4.6-5. Invalidation is absorbed, not terminal, under today's lifecycle: a
/// `FollowThroughRejected` produces no close and no disposition. Pins the
/// absorb rule until D5 changes it.
#[test]
fn d4_5_invalidation_is_not_a_close_today() {
    let dir = temp_dir("invalidation");
    let mut live = Live::new(&dir, OiConfig::symbol_activity_v1());
    live.step(&confirmed("AAA", at(0), 10.0), at(0));
    for t in (10..=1_500).step_by(10) {
        let e = if t == 100 {
            rejected("AAA", at(t), 9.5)
        } else {
            bar("AAA", at(t), 10.0)
        };
        live.step(&e, at(t));
    }
    let engine_closed = live.shadow.engine_health().snapshot().opportunities_closed;
    live.finish(at(1_510));

    assert_eq!(
        engine_closed, 0,
        "a rejection must not close the opportunity"
    );
    let rows = rows(&dir);
    let settled: Vec<_> = rows
        .iter()
        .filter(|r| r.anchor_at <= at(1_510 - 1_320))
        .collect();
    assert!(!settled.is_empty());
    assert!(settled
        .iter()
        .all(|r| r.opportunity_disposition == OpportunityDisposition::StillOpen));
    assert!(
        persisted_closures(&dir).iter().all(
            |c| c.reason == backtest_metrics::opportunity::OpportunityCloseReason::CaptureEnded
        ),
        "the only close is the capture end"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// D4.6-14 through the drivers: the close and the settlement happen on the
/// same event. AAA's last activity is at 1,020, so the inactivity close is
/// learned at exactly 1,320 -- the anchor-at-0 row's deadline -- and must win.
#[test]
fn d4_14_close_and_settlement_on_the_same_event() {
    let dir = temp_dir("same-step");
    let mut live = Live::new(&dir, OiConfig::symbol_activity_v1());
    live.step(&confirmed("AAA", at(0), 10.0), at(0));
    for t in (10..=1_400).step_by(10) {
        if t <= 1_020 {
            live.step(&bar("AAA", at(t), 10.0), at(t));
        }
        tick_other(&mut live, t);
    }
    live.finish(at(1_410));
    let rows = rows(&dir);
    let first = rows
        .iter()
        .find(|r| r.symbol == "AAA" && r.anchor_at == at(0))
        .unwrap();
    assert_eq!(first.opportunity_close_observed_at, Some(at(1_320)));
    assert_eq!(
        first.opportunity_disposition,
        OpportunityDisposition::Inactivity
    );
    assert!(!first
        .censor_reasons
        .contains(&backtest_metrics::horizon::CensorReason::CaptureEnded));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A deterministic stream with every close path a restart can meet: churn by
/// inactivity, a busy symbol, and opportunities still open at any cut.
fn stream() -> Vec<(ScanEvent, DateTime<Utc>)> {
    let mut out = Vec::new();
    for t in (0..3_000).step_by(10) {
        out.push((
            confirmed("BUSY", at(t), 20.0 + (t % 13) as f64 * 0.01),
            at(t),
        ));
        // Each churn symbol trades for ~200s then goes quiet for good.
        let k = t / 400;
        if t % 400 < 200 {
            let s = format!("C{k:02}");
            out.push((
                confirmed(&s, at(t), 1.0 + k as f64 * 0.1 + (t % 7) as f64 * 0.001),
                at(t),
            ));
        }
    }
    out
}

fn run_stream(dir: &Path, events: &[(ScanEvent, DateTime<Utc>)], restart_at: Option<usize>) {
    let mut live = Live::new(dir, OiConfig::symbol_activity_v1());
    for (i, (event, now)) in events.iter().enumerate() {
        if Some(i) == restart_at {
            // A graceful restart: finish, then brand-new engines. Nothing
            // crosses the process boundary.
            live.finish(*now);
            live = Live::new(dir, OiConfig::symbol_activity_v1());
        }
        live.step(event, *now);
    }
    live.finish(events.last().unwrap().1 + Duration::seconds(1));
}

/// D4.6-13. Restart and replay.
///
/// * the rows are deterministic;
/// * a restart gives pre-restart anchors `capture_ended` (or the reason their
///   opportunity had already closed with), and post-restart opportunities new
///   ids -- no notice crosses processes;
/// * the persisted `opportunity_closed` records reproduce every live
///   disposition offline, through the pure rule, and reconcile with the
///   engine's own close count.
#[test]
fn d4_13_restart_and_replay_from_persisted_closes() {
    let events = stream();

    let a = temp_dir("replay-a");
    run_stream(&a, &events, None);
    let a2 = temp_dir("replay-a2");
    run_stream(&a2, &events, None);
    let (rows_a, rows_a2) = (rows(&a), rows(&a2));
    assert_eq!(
        serde_json::to_string(&rows_a).unwrap(),
        serde_json::to_string(&rows_a2).unwrap(),
        "same stream, same rows"
    );

    // Offline replay: dispositions from the persisted records alone.
    let notices = persisted_closures(&a);
    assert!(notices
        .iter()
        .any(|n| n.reason == backtest_metrics::opportunity::OpportunityCloseReason::Inactivity));
    let finished = oi_markers(&a)
        .into_iter()
        .find(|m| m.kind == "capture_finished")
        .expect("capture_finished");
    let closed_total = finished.data.unwrap()["opportunitiesClosed"]
        .as_u64()
        .unwrap();
    assert_eq!(
        notices.len() as u64,
        closed_total,
        "reconciliation: one opportunity_closed record per close the engine counted"
    );
    let mut checked = 0;
    for row in &rows_a {
        let (disposition, notice) = disposition_as_of_settlement(
            &row.opportunity_id,
            row.opened_at,
            row.anchor_at,
            &notices,
        );
        assert_eq!(disposition, row.opportunity_disposition, "replayed {row:?}");
        assert_eq!(notice.map(|n| n.closed_at), row.opportunity_closed_at);
        assert_eq!(
            notice.map(|n| n.close_observed_at),
            row.opportunity_close_observed_at
        );
        checked += 1;
    }
    assert!(
        checked > 100,
        "the replay must cover a real population, checked {checked}"
    );
    assert!(rows_a
        .iter()
        .any(|r| r.opportunity_disposition == OpportunityDisposition::Inactivity));
    assert!(rows_a
        .iter()
        .any(|r| r.opportunity_disposition == OpportunityDisposition::StillOpen));

    // Restart split.
    let b = temp_dir("replay-b");
    let cut = events.len() / 2;
    run_stream(&b, &events, Some(cut));
    let rows_b = rows(&b);
    let cut_at = events[cut].1;
    // Anchors outstanding at the cut: settled at the restart, as capture end
    // or with the reason their opportunity closed with earlier.
    let at_cut: Vec<_> = rows_b
        .iter()
        .filter(|r| {
            r.anchor_at < cut_at
                && r.anchor_at + Duration::seconds(1_320) > cut_at
                && r.censor_reasons
                    .contains(&backtest_metrics::horizon::CensorReason::CaptureEnded)
        })
        .collect();
    assert!(
        !at_cut.is_empty(),
        "the cut must strand outstanding anchors"
    );
    assert!(at_cut.iter().all(|r| matches!(
        r.opportunity_disposition,
        OpportunityDisposition::CaptureEnded | OpportunityDisposition::Inactivity
    )));
    assert!(at_cut
        .iter()
        .any(|r| r.opportunity_disposition == OpportunityDisposition::CaptureEnded));
    // Post-restart ids never collide with pre-restart ones.
    let pre: std::collections::BTreeSet<_> = rows_b
        .iter()
        .filter(|r| r.anchor_at < cut_at)
        .map(|r| &r.opportunity_id)
        .collect();
    let post: std::collections::BTreeSet<_> = rows_b
        .iter()
        .filter(|r| r.anchor_at >= cut_at)
        .map(|r| &r.opportunity_id)
        .collect();
    assert!(!post.is_empty());
    assert!(
        pre.is_disjoint(&post),
        "a restart must never reuse an opportunity id"
    );
    // And the split run's persisted closes replay its own rows too.
    let notices_b = persisted_closures(&b);
    for row in &rows_b {
        let (disposition, _) = disposition_as_of_settlement(
            &row.opportunity_id,
            row.opened_at,
            row.anchor_at,
            &notices_b,
        );
        assert_eq!(disposition, row.opportunity_disposition);
    }

    for dir in [a, a2, b] {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// The collector's accounting reaches the health surface: dispositions sum to
/// the rows written, and the engine's closes are split by reason.
#[test]
fn d4_health_counts_dispositions_and_closes() {
    let dir = temp_dir("health");
    let mut live = Live::new(&dir, OiConfig::symbol_activity_v1());
    live.step(&confirmed("AAA", at(0), 10.0), at(0));
    for t in (10..=1_500).step_by(10) {
        tick_other(&mut live, t);
    }
    live.finish(at(1_510));
    let outcome = live.outcomes.engine_health().snapshot();
    assert_eq!(outcome.disposition_counts.total(), outcome.anchors_settled);
    assert!(outcome.disposition_counts.inactivity >= 1);
    assert!(
        outcome.closure_notices >= 2,
        "AAA's inactivity close and OTHER's capture end"
    );
    let engine = live.shadow.engine_health().snapshot();
    assert_eq!(engine.closed_by_reason.total(), engine.opportunities_closed);
    assert_eq!(engine.closed_by_reason.inactivity, 1);
    assert_eq!(engine.closed_by_reason.capture_ended, 1);
    assert_eq!(rows(&dir).len() as u64, outcome.anchors_settled);
    let _ = std::fs::remove_dir_all(&dir);
}
