use super::*;

fn bar(i: i64, open: f64, high: f64, low: f64, close: f64, volume: u64) -> Bar {
    let t = (30_000 + i) * 60_000;
    Bar {
        market_open_ms: t,
        received_at_ms: t + 60_000,
        decision_at_ms: t + 60_000,
        market_day: "2026-08-26".into(),
        open,
        high,
        low,
        close,
        volume,
        is_final: true,
        interval_secs: 60,
    }
}
fn monitor(iac: bool, extreme: bool) -> Monitor {
    Monitor::new("TEST".into(), "run".into(), Config::default(), iac, extreme).unwrap()
}
fn warm_iac(m: &mut Monitor) {
    for i in 0..20 {
        assert!(m.on_bar(bar(i, 10.0, 10.2, 9.8, 10.0, 1000)).is_empty());
    }
    for i in 20..25 {
        assert!(m.on_bar(bar(i, 10.0, 10.05, 9.95, 10.0, 400)).is_empty());
    }
}
fn anchor() -> Bar {
    bar(25, 10.0, 10.5, 9.99, 10.45, 2000)
}

#[test]
fn independent_flags_and_extreme_without_compression() {
    let mut off = monitor(false, false);
    warm_iac(&mut off);
    assert!(off.on_bar(anchor()).is_empty());
    assert_eq!(off.counters.accepted_bars, 0);
    let mut m = monitor(false, true);
    for i in 0..20 {
        m.on_bar(bar(i, 10.0, 10.05, 9.95, 10.0, 1000));
    }
    m.on_bar(bar(20, 10.0, 10.12, 9.99, 10.1, 4000));
    m.on_bar(bar(21, 10.1, 10.24, 10.09, 10.22, 4000));
    let es = m.on_bar(bar(22, 10.22, 10.45, 10.21, 10.44, 4000));
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].strategy, Strategy::ExtremeMomentum);
    assert_eq!(es[0].kind, Kind::Candidate);
}

#[test]
fn iac_prospective_hold_and_continuation_keep_frozen_evidence() {
    let mut m = monitor(true, false);
    warm_iac(&mut m);
    let candidate = m.on_bar(anchor()).pop().unwrap();
    assert_eq!(candidate.kind, Kind::Candidate);
    assert_eq!(candidate.evidence.float_evidence, "unavailable");
    assert!(m.on_bar(bar(26, 10.45, 10.6, 10.4, 10.5, 1000)).is_empty());
    let hold = m
        .on_bar(bar(27, 10.5, 10.65, 10.48, 10.6, 1000))
        .pop()
        .unwrap();
    assert_eq!(hold.kind, Kind::Held);
    assert_eq!(hold.evidence, candidate.evidence);
    assert_eq!(hold.anchor_available_at_ms, candidate.decision_at_ms);
    assert_eq!(hold.supersedes, Some(candidate.event_id));
    let continued = m
        .on_bar(bar(28, 10.6, 11.05, 10.6, 11.0, 1000))
        .pop()
        .unwrap();
    assert_eq!(continued.kind, Kind::Continued);
    assert_eq!(continued.supersedes, Some(hold.event_id));
}

#[test]
fn candidate_cannot_confirm_from_future_bar_in_same_input() {
    let mut m = monitor(true, false);
    warm_iac(&mut m);
    let es = m.on_bar(anchor());
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].kind, Kind::Candidate);
    assert!(m.on_bar(anchor()).is_empty());
    assert_eq!(m.counters.duplicates, 1);
}

#[test]
fn corrections_invalidate_without_relabeling_candidate() {
    let mut m = monitor(true, false);
    warm_iac(&mut m);
    let candidate = m.on_bar(anchor()).pop().unwrap();
    let before = candidate.clone();
    let mut revised = anchor();
    revised.volume += 100;
    revised.received_at_ms += 1000;
    revised.decision_at_ms += 1000;
    let es = m.on_bar(revised);
    assert_eq!(es[0].kind, Kind::Invalidated);
    assert_eq!(es[0].supersedes, Some(candidate.event_id.clone()));
    assert_eq!(candidate, before);
    assert_eq!(m.counters.revisions, 1);
    assert!(m.history.is_empty());
}

#[test]
fn gap_is_unknown_not_failure_or_hold() {
    let mut m = monitor(true, false);
    warm_iac(&mut m);
    m.on_bar(anchor());
    let es = m.on_bar(bar(28, 10.45, 10.6, 10.4, 10.5, 1000));
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].kind, Kind::Invalidated);
    assert_eq!(es[0].reason, "missing_minute");
    assert_eq!(m.counters.gaps, 1);
}

#[test]
fn stale_backwards_receipt_and_invalid_prices_fail_closed() {
    for mode in 0..3 {
        let mut m = monitor(true, false);
        warm_iac(&mut m);
        m.on_bar(anchor());
        let mut b = bar(26, 10.45, 10.6, 10.4, 10.5, 1000);
        if mode == 0 {
            b.received_at_ms += 91_000;
            b.decision_at_ms = b.received_at_ms;
        }
        if mode == 1 {
            b.received_at_ms -= 120_000;
        }
        if mode == 2 {
            b.close = f64::NAN;
        }
        let es = m.on_bar(b);
        assert_eq!(es[0].kind, Kind::Invalidated);
        assert_eq!(m.counters.invalid_inputs, 1);
        assert!(serde_json::to_string(&es[0])
            .unwrap()
            .contains("invalid_or_stale_input"));
    }
}

#[test]
fn provisional_bars_do_not_warm_or_change_existing_lifecycle() {
    let mut m = monitor(true, false);
    warm_iac(&mut m);
    m.on_bar(anchor());
    let mut b = bar(26, 10.45, 10.6, 10.4, 10.5, 1000);
    b.is_final = false;
    assert!(m.on_bar(b).is_empty());
    assert_eq!(m.history.len(), 26);
    assert_eq!(m.counters.ignored_provisional, 1);
}

#[test]
fn zero_denominators_do_not_create_explosion_signals() {
    let mut m = monitor(true, true);
    for i in 0..25 {
        m.on_bar(bar(i, 10.0, 10.0, 10.0, 10.0, 0));
    }
    assert!(m.on_bar(anchor()).is_empty());
}

#[test]
fn future_suffix_does_not_change_prefix() {
    let mut a = monitor(true, true);
    let mut b = monitor(true, true);
    warm_iac(&mut a);
    warm_iac(&mut b);
    let prefix_a = a.on_bar(anchor());
    let prefix_b = b.on_bar(anchor());
    for i in 26..40 {
        b.on_bar(bar(i, 10.0, 50.0, 9.0, 40.0, 999_999));
    }
    assert_eq!(prefix_a, prefix_b);
}

#[test]
fn terminal_failure_expiry_and_cooldown() {
    let mut m = monitor(true, false);
    warm_iac(&mut m);
    m.on_bar(anchor());
    let failure = m.on_bar(bar(26, 10.4, 10.5, 9.9, 10.0, 1000));
    assert_eq!(failure[0].kind, Kind::Failed);
    assert!(m.on_bar(bar(27, 10.0, 11.0, 9.99, 10.9, 4000)).is_empty());
    let mut m = monitor(true, false);
    warm_iac(&mut m);
    m.on_bar(anchor());
    let mut all = Vec::new();
    for i in 26..31 {
        all.extend(m.on_bar(bar(i, 10.3, 10.4, 10.2, 10.3, 500)));
    }
    assert_eq!(all.last().unwrap().kind, Kind::Expired);
}

#[test]
fn quiet_timeout_invalidation_and_new_day_reset() {
    let mut m = monitor(true, false);
    warm_iac(&mut m);
    let candidate = m.on_bar(anchor())[0].clone();
    assert_eq!(
        m.on_gap(candidate.decision_at_ms + 120_000, "source_idle")[0].kind,
        Kind::Invalidated
    );
    assert!(m
        .on_gap(candidate.decision_at_ms + 180_000, "source_idle")
        .is_empty());
    let mut b = bar(26, 10.0, 10.2, 9.8, 10.0, 1000);
    b.market_day = "2026-08-27".into();
    m.on_bar(b);
    assert_eq!(m.history.len(), 1);
    assert_eq!(m.cooldown, [None, None]);
}

#[test]
fn bounded_history_and_config_validation() {
    let mut m = monitor(true, true);
    for i in 0..200 {
        m.on_bar(bar(i, 10.0, 10.1, 9.9, 10.0, 1000));
    }
    assert_eq!(m.history.len(), 64);
    let cfg = Config {
        baseline_bars: usize::MAX,
        ..Config::default()
    };
    assert!(cfg.validate().is_err());
    let cfg = Config {
        extreme_efficiency_min: f64::NAN,
        ..Config::default()
    };
    assert!(cfg.validate().is_err());
}

#[test]
fn admitted_receipt_jitter_does_not_change_market_clock_progression() {
    let mut a = monitor(true, false);
    let mut b = monitor(true, false);
    warm_iac(&mut a);
    warm_iac(&mut b);
    let mut delayed = anchor();
    delayed.received_at_ms += 40_000;
    delayed.decision_at_ms += 40_000;
    a.on_bar(anchor());
    b.on_bar(delayed);
    let mut kinds_a = Vec::new();
    let mut kinds_b = Vec::new();
    for i in 26..31 {
        let next = bar(i, 10.45, 10.6, 10.4, 10.5, 1000);
        let mut jittered = next.clone();
        jittered.received_at_ms += 1_000;
        jittered.decision_at_ms += 1_000;
        kinds_a.extend(a.on_bar(next).iter().map(|e| e.kind));
        kinds_b.extend(b.on_bar(jittered).iter().map(|e| e.kind));
    }
    assert_eq!(kinds_a, kinds_b);
    assert_eq!(kinds_a, vec![Kind::Held, Kind::Expired]);
}
