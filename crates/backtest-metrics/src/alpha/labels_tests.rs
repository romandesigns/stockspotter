//! Tests for the frozen reference-opportunity label.
//!
//! The label decides what counts as a missed opportunity, so a defect here
//! would not produce a wrong number — it would produce a wrong *question*.
//! Every semantic clause in the module doc gets a test.

use super::*;

use chrono::TimeZone;

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 17).unwrap()
}

/// A price point at `hh:mm:ss` UTC on the session date.
fn p(h: u32, m: u32, s: u32, price: f64) -> PricePoint {
    (Utc.with_ymd_and_hms(2026, 9, 17, h, m, s).unwrap(), price)
}

fn spec() -> ReferenceLabelSpec {
    ReferenceLabelSpec::default()
}

/// A dense series: one print a minute from the open, so no gap ever exceeds
/// `MAX_GAP_SECS` and every question is answerable.
fn dense(start: f64, path: &[(u32, u32, f64)]) -> Vec<PricePoint> {
    let mut out = vec![p(13, 30, 0, start)];
    let mut cursor = (13u32, 31u32);
    for &(h, m, price) in path {
        // Fill the minutes between the previous point and this one.
        while (cursor.0, cursor.1) < (h, m) {
            out.push(p(cursor.0, cursor.1, 0, start));
            cursor.1 += 1;
            if cursor.1 == 60 {
                cursor.1 = 0;
                cursor.0 += 1;
            }
        }
        out.push(p(h, m, 0, price));
        cursor = (h, m + 1);
        if cursor.1 == 60 {
            cursor.1 = 0;
            cursor.0 += 1;
        }
    }
    // Dense to the close, so the tail gap never censors.
    while (cursor.0, cursor.1) < (20, 0) {
        out.push(p(cursor.0, cursor.1, 0, start));
        cursor.1 += 1;
        if cursor.1 == 60 {
            cursor.1 = 0;
            cursor.0 += 1;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Crossing semantics
// ---------------------------------------------------------------------------

#[test]
fn a_symbol_that_reaches_every_target_is_labelled_at_every_target() {
    // 10.00 -> 11.50 is +15%, so all three targets are reached.
    let series = dense(10.0, &[(14, 0, 10.30), (15, 0, 11.50)]);
    let r = label("AAA", day(), &series, &spec());

    assert_eq!(r.ineligible, None);
    assert_eq!(r.start_price, Some(10.0));
    assert_eq!(r.start_at, Some(p(13, 30, 0, 0.0).0));
    for index in 0..3 {
        assert!(r.crossings[index].crossed(), "target {index} must be reached");
        assert_eq!(r.is_opportunity(index), Some(true));
    }
    // +2% at 10.20 is reached by the 14:00 print; +5% and +10% only at 15:00.
    match (&r.crossings[0], &r.crossings[2]) {
        (Crossing::Crossed { at: a2, .. }, Crossing::Crossed { at: a10, .. }) => {
            assert!(a2 < a10, "a nearer target must be reached no later than a further one");
        }
        other => panic!("expected both crossed: {other:?}"),
    }
    assert!((r.mfe_pct.unwrap() - 15.0).abs() < 1e-9);
}

#[test]
fn a_symbol_that_reaches_only_the_nearest_target_is_labelled_only_there() {
    // +3%: clears +2, misses +5 and +10.
    let series = dense(10.0, &[(14, 0, 10.30)]);
    let r = label("BBB", day(), &series, &spec());

    assert_eq!(r.is_opportunity(0), Some(true), "+2% reached");
    assert_eq!(r.is_opportunity(1), Some(false), "+5% not reached");
    assert_eq!(r.is_opportunity(2), Some(false), "+10% not reached");
}

#[test]
fn the_label_uses_the_observed_high_not_the_closing_price() {
    // Spikes to +12% and gives it all back. The move was available, so it is an
    // opportunity — the question is availability, not persistence.
    let series = dense(10.0, &[(14, 0, 11.20), (15, 0, 9.50)]);
    let r = label("CCC", day(), &series, &spec());
    assert_eq!(r.is_opportunity(2), Some(true), "a round trip is still a reachable move");
    assert!((r.mfe_pct.unwrap() - 12.0).abs() < 1e-9);
    assert!(r.mae_pct.unwrap() < 0.0, "and the adverse excursion is recorded too");
}

#[test]
fn the_start_price_is_the_first_print_at_or_after_the_open() {
    let mut series = dense(10.0, &[(14, 0, 10.50)]);
    // Pre-market prints, which must be ignored entirely.
    series.push(p(9, 0, 0, 4.0));
    series.push(p(13, 29, 59, 4.0));
    let r = label("DDD", day(), &series, &spec());
    assert_eq!(r.start_price, Some(10.0), "pre-market must not set the start price");
    // Against a 4.00 start the +10% target would have been reached; against the
    // correct 10.00 start it is not.
    assert_eq!(r.is_opportunity(2), Some(false));
}

#[test]
fn after_hours_prints_are_outside_the_window() {
    let mut series = dense(10.0, &[(14, 0, 10.10)]);
    series.push(p(20, 30, 0, 20.0)); // a huge after-hours move
    let r = label("EEE", day(), &series, &spec());
    assert_eq!(
        r.is_opportunity(2),
        Some(false),
        "an after-hours move is not an intraday opportunity"
    );
    assert!(r.session_high.unwrap() < 11.0);
}

// ---------------------------------------------------------------------------
// Censoring -- absence of evidence is never evidence of absence
// ---------------------------------------------------------------------------

#[test]
fn a_sparse_series_yields_unknown_not_not_crossed() {
    // Two prints, hours apart: nothing can be said about what happened between.
    let series = vec![p(13, 30, 0, 10.0), p(17, 0, 0, 10.05)];
    let r = label("FFF", day(), &series, &spec());

    for index in 0..3 {
        assert!(
            matches!(r.crossings[index], Crossing::Unknown { .. }),
            "target {index} must be unknown, not NotCrossed"
        );
        assert_eq!(r.is_opportunity(index), None);
        assert!(!r.crossings[index].known());
    }
    assert!(r.largest_gap_secs > spec().max_gap_secs);
}

#[test]
fn a_gap_after_a_confirmed_crossing_does_not_uncross_it() {
    // A dense ramp from the open through +10%, then silence to the close.
    let series: Vec<PricePoint> = (0..120u32)
        .map(|i| {
            let (h, m) = (13 + (30 + i) / 60, (30 + i) % 60);
            (
                Utc.with_ymd_and_hms(2026, 9, 17, h, m, 0).unwrap(),
                10.0 + f64::from(i) * 0.02,
            )
        })
        .collect();
    let r = label("GGG", day(), &series, &spec());

    assert!(r.crossings[2].crossed(), "the +10% crossing is established before the silence");
    assert_eq!(r.is_opportunity(2), Some(true));
}

#[test]
fn a_series_that_stops_early_censors_the_targets_it_had_not_reached() {
    // Dense from the open to 14:00, then nothing for six hours.
    let series: Vec<PricePoint> = (0..30u32)
        .map(|i| {
            (
                Utc.with_ymd_and_hms(2026, 9, 17, 13, 30 + i, 0).unwrap(),
                10.0 + f64::from(i) * 0.005,
            )
        })
        .collect();
    let r = label("HHH", day(), &series, &spec());

    // +2% was never reached and the tail is a six-hour gap, so it is unknown.
    assert!(
        matches!(r.crossings[0], Crossing::Unknown { .. }),
        "a symbol that stopped printing cannot be said not to have run: {:?}",
        r.crossings[0]
    );
    assert_eq!(r.is_opportunity(0), None);
}

// ---------------------------------------------------------------------------
// Eligibility -- out of scope is not a miss
// ---------------------------------------------------------------------------

#[test]
fn a_sub_floor_price_is_out_of_scope_not_a_miss() {
    let series = dense(0.10, &[(14, 0, 0.20)]); // +100%, but below the floor
    let r = label("III", day(), &series, &spec());
    assert_eq!(r.ineligible, Some(Ineligible::BelowPriceFloor));
    assert_eq!(
        r.is_opportunity(2),
        None,
        "an ineligible symbol has no label at all, and must never count as a miss"
    );
}

#[test]
fn a_price_above_the_ceiling_is_out_of_scope() {
    let series = dense(50.0, &[(14, 0, 60.0)]);
    let r = label("JJJ", day(), &series, &spec());
    assert_eq!(r.ineligible, Some(Ineligible::AbovePriceCeiling));
}

#[test]
fn the_price_band_is_the_funnels_own() {
    let s = spec();
    assert_eq!(s.min_price, 0.25, "inherited from fast_funnel, not invented here");
    assert_eq!(s.max_price, 20.00);
}

#[test]
fn a_symbol_with_no_session_price_is_ineligible() {
    let series = vec![p(9, 0, 0, 10.0), p(21, 0, 0, 12.0)];
    let r = label("KKK", day(), &series, &spec());
    assert_eq!(r.ineligible, Some(Ineligible::NoSessionPrice));
    assert_eq!(r.observations, 0);
}

// ---------------------------------------------------------------------------
// Determinism and provenance
// ---------------------------------------------------------------------------

#[test]
fn the_label_does_not_depend_on_input_order() {
    let series = dense(10.0, &[(14, 0, 10.30), (15, 0, 11.50)]);
    let forward = label("LLL", day(), &series, &spec());

    let mut reversed = series.clone();
    reversed.reverse();
    let backward = label("LLL", day(), &reversed, &spec());

    assert_eq!(
        forward, backward,
        "a caller must not be able to change the answer by changing read order"
    );
}

#[test]
fn the_spec_is_versioned_and_carries_every_frozen_parameter() {
    let s = spec();
    assert_eq!(s.version, "reference-opportunity-v1");
    assert_eq!(s.targets_pct, crate::horizon::TARGET_PCTS.to_vec(), "the preregistered family");
    assert_eq!(s.max_gap_secs, crate::horizon::MAX_GAP_SECS);
    assert!(!s.flat_base_required, "a flat-base filter would bias the reference population");
    assert!(!s.start_price_rule.is_empty());
    assert!(!s.crossing_rule.is_empty());
    assert!(!s.censoring_rule.is_empty());

    // Round-trips, so a report can carry the exact definition that produced it.
    let text = serde_json::to_string(&s).unwrap();
    let back: ReferenceLabelSpec = serde_json::from_str(&text).unwrap();
    assert_eq!(s, back);
}

#[test]
fn unknown_never_silently_becomes_false() {
    // The single property the whole recall figure depends on.
    let sparse = vec![p(13, 30, 0, 10.0), p(19, 0, 0, 10.0)];
    let r = label("MMM", day(), &sparse, &spec());
    let known: Vec<bool> = (0..3).map(|i| r.crossings[i].known()).collect();
    assert_eq!(known, vec![false, false, false]);
    assert!(
        (0..3).all(|i| r.is_opportunity(i).is_none()),
        "not one of these may read as a negative"
    );
}
