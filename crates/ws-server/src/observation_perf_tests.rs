//! Step 4B-perf: every hot-path optimization must be byte-identical to the
//! code it replaced. Synthetic values only.

use chrono::{DateTime, NaiveDate, TimeZone, Timelike, Utc};

use super::*;

/// chrono's own serialization of a timestamp, as a JSON string literal.
fn chrono_json(dt: &DateTime<Utc>) -> String {
    serde_json::to_string(dt).unwrap()
}

fn fast_json(dt: &DateTime<Utc>) -> String {
    serde_json::to_string(&fast_time::Wrap(dt)).unwrap()
}

/// Deterministic SplitMix64, so the random sample is reproducible.
fn next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[test]
fn the_fast_timestamp_serializer_is_byte_identical_to_chrono() {
    let base = Utc.with_ymd_and_hms(2026, 9, 29, 13, 30, 0).unwrap();
    let mut cases = vec![base];
    // Every AutoSi fraction class, and their boundaries.
    for nanos in [0u32, 1, 9, 999, 1_000, 1_001, 999_000, 1_000_000, 1_000_001, 123_000_000, 123_456_000, 123_456_789, 999_999_999] {
        cases.push(base.with_nanosecond(nanos).unwrap());
    }
    // Calendar edges inside the fast range.
    for (y, m, d, h, mi, s) in [(0, 1, 1, 0, 0, 0), (9999, 12, 31, 23, 59, 59), (1970, 1, 1, 0, 0, 0), (2024, 2, 29, 12, 0, 1), (2026, 11, 27, 18, 0, 0)] {
        cases.push(Utc.with_ymd_and_hms(y, m, d, h, mi, s).unwrap());
    }
    // Delegated to chrono: out-of-range years and a leap second.
    cases.push(Utc.with_ymd_and_hms(-1, 1, 1, 0, 0, 0).unwrap());
    cases.push(Utc.with_ymd_and_hms(10_000, 1, 1, 0, 0, 0).unwrap());
    let leap = NaiveDate::from_ymd_opt(2016, 12, 31).unwrap().and_hms_nano_opt(23, 59, 59, 1_500_000_000).unwrap();
    cases.push(Utc.from_utc_datetime(&leap));
    let leap0 = NaiveDate::from_ymd_opt(2016, 12, 31).unwrap().and_hms_nano_opt(23, 59, 59, 1_000_000_000).unwrap();
    cases.push(Utc.from_utc_datetime(&leap0));
    // 200,000 random instants over years 0..=9999 with every fraction class.
    let mut st = 0x5EED_u64;
    for i in 0..200_000u64 {
        let secs = (next(&mut st) % 315_537_897_600) as i64 - 62_167_219_200; // 0000-01-01 .. 9999-12-31
        let nanos = match i % 4 {
            0 => 0,
            1 => (next(&mut st) % 1_000) as u32 * 1_000_000,
            2 => (next(&mut st) % 1_000_000) as u32 * 1_000,
            _ => (next(&mut st) % 1_000_000_000) as u32,
        };
        cases.push(Utc.timestamp_opt(secs, nanos).unwrap());
    }
    for dt in &cases {
        assert_eq!(fast_json(dt), chrono_json(dt), "{dt:?}");
    }
}

/// The pre-optimization implementation, verbatim, as the reference.
fn canonical_candidate_reference(opportunity_id: &str, eligibility: &Eligibility, provenance: Option<&PriceProvenance>) -> String {
    let decision = if eligibility.eligible {
        "eligible".to_string()
    } else {
        let reasons: Vec<&str> = eligibility.reasons.iter().map(|r| reason_key(*r)).collect();
        format!("ineligible:{}", reasons.join(","))
    };
    let source = match provenance {
        Some(p) => format!("{}#{}", p.source_run_id, p.source_sequence),
        None => "none".to_string(),
    };
    format!("{opportunity_id}|{decision}|{source}")
}

#[test]
fn the_canonical_identity_builder_matches_the_original_exactly() {
    use IneligibilityReason::*;
    let all = [
        UnknownPriceProvenance, NegativeMarketAge, MarketAgeExceeded, ReceiptAgeExceeded,
        PriceReceivedAfterProcessingStart, RankBracketInverted, AmbiguousLifecycleMapping,
        WindowMappingAmbiguous, WindowSourceLag, ConfirmationTrackingIncomplete, LeftCensored,
        ConfirmationOrderingAmbiguous, NoConfirmationReceipt, ConfirmationMultiplicity,
    ];
    let prov = PriceProvenance {
        source_run_id: "srv1170872-12345-20260929T000000000Z-0".into(),
        source_sequence: 18_446_744_073_709_551_615,
        price: 1.0,
        market_at: Utc.timestamp_opt(1, 0).unwrap(),
        received_at: Utc.timestamp_opt(1, 0).unwrap(),
        received_mono_nanos: 0,
        revision: PriceRevision::Forward,
        source_event_type: "ignition_event".into(),
        market_time_derived: false,
    };
    let mut st = 7u64;
    for i in 0..5_000usize {
        let reasons: Vec<IneligibilityReason> =
            all.iter().copied().filter(|_| next(&mut st) % 3 == 0).collect();
        let e = Eligibility::from_reasons(reasons);
        let id = format!("SYM{i}:2026-09-29:{}", 28_800_000 + i);
        let p = if i % 2 == 0 { Some(&prov) } else { None };
        assert_eq!(canonical_candidate(&id, &e, p), canonical_candidate_reference(&id, &e, p));
    }
    // Empty reason list and the plain eligible case.
    let e = Eligibility::from_reasons(vec![]);
    assert_eq!(canonical_candidate("X:2026-09-29:1", &e, None), canonical_candidate_reference("X:2026-09-29:1", &e, None));
}


/// Serializes a candidate through the fast path, via a throwaway prefix.
fn fast_candidate_json(prefix: &mut fast_candidate::CandidatePrefix, r: &ObservationRecord) -> String {
    let ObservationRecord::Candidate {
        run_id, window_id, anchor_at, processing_started_at, opportunity_id, symbol, opened_at, scored,
        provenance, market_age_nanos, receipt_age_nanos, confirmation_receipts, confirmation_sequence, eligibility,
    } = r
    else {
        panic!("candidate expected")
    };
    let mut out = Vec::new();
    fast_candidate::write(
        prefix, &mut out, run_id, window_id, anchor_at, processing_started_at, opportunity_id, symbol, opened_at,
        *scored, provenance.as_ref(), *market_age_nanos, *receipt_age_nanos, *confirmation_receipts,
        *confirmation_sequence, eligibility,
    );
    String::from_utf8(out).unwrap()
}

#[test]
fn the_fast_candidate_row_is_byte_identical_to_serde_on_randomized_rows() {
    use IneligibilityReason::*;
    let all = [
        UnknownPriceProvenance, NegativeMarketAge, MarketAgeExceeded, ReceiptAgeExceeded,
        PriceReceivedAfterProcessingStart, RankBracketInverted, AmbiguousLifecycleMapping,
        WindowMappingAmbiguous, WindowSourceLag, ConfirmationTrackingIncomplete, LeftCensored,
        ConfirmationOrderingAmbiguous, NoConfirmationReceipt, ConfirmationMultiplicity, WindowIncomplete,
    ];
    // Strings that exercise every escaping class serde_json has.
    let odd = ["plain", "with \"quote\"", "back\\slash", "tab\tnew\nline", "ctrl\u{1}\u{1f}", "unicode é 漢 🚀", "", "slash/ok"];
    let prices = [1.0, 0.0001, 3.3966, 1e16, 123456.789, f64::MIN_POSITIVE, 1.5e-7, f64::NAN, f64::INFINITY];
    let mut st = 99u64;
    let mut prefix = fast_candidate::CandidatePrefix::default();
    let ts = |st: &mut u64| {
        let secs = 1_790_000_000 + (next(st) % 10_000_000) as i64;
        let nanos = match next(st) % 4 { 0 => 0, 1 => (next(st) % 1000) as u32 * 1_000_000, 2 => (next(st) % 1_000_000) as u32 * 1_000, _ => (next(st) % 1_000_000_000) as u32 };
        Utc.timestamp_opt(secs, nanos).unwrap()
    };
    for i in 0..60_000u64 {
        let pick = |st: &mut u64| odd[(next(st) % odd.len() as u64) as usize].to_string();
        // Windows change every ~50 rows, so the prefix cache is exercised
        // both warm and invalidated (including a same-window, new-anchor case).
        let window = format!("oiw-{}", i / 50);
        let anchor = Utc.timestamp_opt(1_790_000_000 + (i / 50) as i64 * 30, if i % 97 == 0 { 1 } else { 0 }).unwrap();
        let provenance = (next(&mut st) % 3 != 0).then(|| PriceProvenance {
            source_run_id: if i % 5 == 0 { pick(&mut st) } else { "srv1170872-12345-20260929T000000000Z-0".into() },
            source_sequence: next(&mut st),
            price: prices[(next(&mut st) % prices.len() as u64) as usize],
            market_at: ts(&mut st),
            received_at: ts(&mut st),
            received_mono_nanos: next(&mut st),
            revision: if next(&mut st) % 2 == 0 { PriceRevision::Forward } else { PriceRevision::OutOfOrder },
            source_event_type: pick(&mut st).into(),
            market_time_derived: next(&mut st) % 2 == 0,
        });
        let opt_i64 = |st: &mut u64| match next(st) % 3 { 0 => None, 1 => Some(-(next(st) as i64 >> 2)), _ => Some(next(st) as i64 >> 1) };
        let r = ObservationRecord::Candidate {
            run_id: if i % 1_000 == 999 { pick(&mut st) } else { "srv1170872-12345-20260929T000000000Z-0".into() },
            window_id: window,
            anchor_at: anchor,
            processing_started_at: anchor - chrono::Duration::milliseconds(500),
            opportunity_id: format!("{}:2026-09-29:{}", pick(&mut st), next(&mut st) % 100_000_000),
            symbol: pick(&mut st),
            opened_at: ts(&mut st),
            scored: next(&mut st) % 2 == 0,
            provenance,
            market_age_nanos: opt_i64(&mut st),
            receipt_age_nanos: opt_i64(&mut st),
            confirmation_receipts: next(&mut st) % 4,
            confirmation_sequence: (next(&mut st) % 2 == 0).then(|| next(&mut st)),
            eligibility: Eligibility::from_reasons(all.iter().copied().filter(|_| next(&mut st) % 4 == 0).collect()),
        };
        assert_eq!(fast_candidate_json(&mut prefix, &r), serde_json::to_string(&r).unwrap(), "row {i}");
    }
}
