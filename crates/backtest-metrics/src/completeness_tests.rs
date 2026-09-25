//! Section 13 fixtures for the machine-readable completeness verdict.
//!
//! Each fixture starts from a session that is genuinely sound and breaks
//! exactly one thing, so a test that passes proves the checker reacts to *that*
//! condition rather than to some incidental gap in a hand-built input.

use super::*;

fn utc(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn clean_writer(attempted: u64) -> WriterCapture {
    WriterCapture {
        attempted,
        written: attempted,
        dropped: 0,
        write_errors: 0,
        loss_spans: 0,
        queue_depth: 0,
        queue_peak: 512,
        queue_capacity: 16_384,
        queued_bytes: 0,
        queued_bytes_peak: 2_000_000,
        queue_capacity_bytes: 96 * 1024 * 1024,
        bytes_written: attempted * 3_854,
        batches_written: attempted / 512 + 1,
        last_write: Some(utc("2026-09-17T20:00:00Z")),
        current_file: "data/research/opportunity-intelligence-2026-09-17.ndjson".into(),
        current_file_bytes: attempted * 3_854,
        degraded: false,
    }
}

fn clean_discovery() -> DiscoveryCapture {
    DiscoveryCapture {
        attempted: 5_951_808,
        written: 5_951_808,
        queue_lost: 0,
        write_errors: 0,
        sampled_out: 0,
        budget_dropped: 0,
        lost_records_total: 0,
        queue_depth: 0,
        queue_peak: 3_000,
        queue_capacity: 16_384,
        queued_bytes: 0,
        queued_bytes_peak: 40_000_000,
        queue_capacity_bytes: 128 * 1024 * 1024,
        bytes_written: 17_641_037_139,
        batches_written: 12_000,
        last_write: Some(utc("2026-09-17T20:00:00Z")),
        current_file: "data/discovery-audit/2026-09-17-1-1-3.jsonl".into(),
        current_file_bytes: 1_000_000,
        degraded: false,
    }
}

fn clean_engine() -> EngineCapture {
    EngineCapture {
        open: 3_100,
        // Below the 16,375 bound: the September-16 population with room to
        // spare, which is the point of the capacity repair.
        peak: 4_808,
        capacity: 16_375,
        capacity_evictions: 0,
        eviction_markers_dropped: 0,
        opportunities_opened: 19_963,
        opportunities_closed: 16_863,
        cohort_truncations: 0,
        scores_emitted: 2_558_786,
        // D6/D4 observability fields: a report written before they existed
        // carries none of them, which is what this fixture models.
        ..EngineCapture::default()
    }
}

fn clean_report() -> CompletenessReport {
    CompletenessReport {
        report_schema_version: 0,
        generated_at: utc("2026-09-17T20:05:00Z"),
        commit: Some("79c21e16c3fac00f36d52a20828ff65f56657acd".into()),
        oi_config_fingerprint: Some("oi-cfg-b4f21c8b311a1b99".into()),
        oi_versions: None,
        outcome_measurement_version: None,
        episode_schema: None,
        signal_context_schema: None,
        opportunity_intelligence: Some(clean_writer(2_558_786)),
        measurement: Some(clean_writer(140_204)),
        discovery: Some(clean_discovery()),
        opportunity_engine: Some(clean_engine()),
        opportunity_outcomes: None,
        opportunity_outcome_engine: None,
    }
}

fn artifact(path: &str, records: u64) -> ArtifactEvidence {
    ArtifactEvidence {
        path: path.into(),
        present: true,
        bytes: records * 3_000,
        records,
        malformed_records: 0,
        truncated: false,
    }
}

fn clean_evidence() -> SessionEvidence {
    SessionEvidence {
        session_date: "2026-09-17".into(),
        expected_commit: Some("79c21e16c3fac00f36d52a20828ff65f56657acd".into()),
        expected_oi_config_fingerprint: Some("oi-cfg-b4f21c8b311a1b99".into()),
        health: Some(clean_report()),
        artifacts: vec![
            artifact("research/opportunity-intelligence-2026-09-17.ndjson", 2_558_786),
            artifact("research/episodes-2026-09-17.ndjson", 140_204),
            artifact("discovery-audit/2026-09-17-1-1-1.jsonl", 5_951_808),
        ],
        required_artifacts: vec![
            "research/opportunity-intelligence-2026-09-17.ndjson".into(),
            "research/episodes-2026-09-17.ndjson".into(),
            "discovery-audit/2026-09-17-1-1-1.jsonl".into(),
        ],
        settlement: Some(SettlementEvidence {
            settled: 140_204,
            unsettled: 0,
            capacity_evicted: 0,
        }),
    }
}

// ---------------------------------------------------------------------------

#[test]
fn a_complete_session_is_valid() {
    let outcome = check(&clean_evidence());
    assert_eq!(
        outcome.verdict,
        Verdict::Valid,
        "blocking: {:?}  missing: {:?}",
        outcome.blocking,
        outcome.missing
    );
    assert!(outcome.blocking.is_empty());
    assert!(outcome.missing.is_empty());
}

#[test]
fn an_oi_drop_is_invalid() {
    let mut e = clean_evidence();
    let oi = e.health.as_mut().unwrap().opportunity_intelligence.as_mut().unwrap();
    oi.dropped = 1;
    oi.written -= 1;
    oi.loss_spans = 1;
    let outcome = check(&e);
    assert_eq!(outcome.verdict, Verdict::Invalid);
    assert!(
        outcome.blocking.iter().any(|b| b.contains("opportunityIntelligence") && b.contains("dropped")),
        "{:?}",
        outcome.blocking
    );
}

#[test]
fn a_measurement_drop_is_invalid() {
    let mut e = clean_evidence();
    let m = e.health.as_mut().unwrap().measurement.as_mut().unwrap();
    m.dropped = 32;
    m.written -= 32;
    m.loss_spans = 1;
    let outcome = check(&e);
    assert_eq!(outcome.verdict, Verdict::Invalid);
    assert!(outcome.blocking.iter().any(|b| b.starts_with("measurement:")), "{:?}", outcome.blocking);
}

#[test]
fn a_measurement_write_error_is_invalid() {
    let mut e = clean_evidence();
    let m = e.health.as_mut().unwrap().measurement.as_mut().unwrap();
    m.write_errors = 1;
    m.written -= 1;
    assert_eq!(check(&e).verdict, Verdict::Invalid);
}

#[test]
fn a_discovery_queue_loss_is_invalid() {
    let mut e = clean_evidence();
    let d = e.health.as_mut().unwrap().discovery.as_mut().unwrap();
    d.queue_lost = 4_096;
    d.lost_records_total = 4_096;
    let outcome = check(&e);
    assert_eq!(outcome.verdict, Verdict::Invalid);
    assert!(
        outcome.blocking.iter().any(|b| b.contains("discovery") && b.contains("queue pressure")),
        "{:?}",
        outcome.blocking
    );
}

/// Deliberate downsampling is a policy that describes itself in-band. It must
/// be reported, and it must not be a failure — conflating the two would make
/// the gate unusable on any budget-constrained session.
#[test]
fn discovery_downsampling_is_reported_but_not_blocking() {
    let mut e = clean_evidence();
    let d = e.health.as_mut().unwrap().discovery.as_mut().unwrap();
    d.sampled_out = 120_000;
    d.budget_dropped = 4_000;
    let outcome = check(&e);
    assert_eq!(outcome.verdict, Verdict::Valid);
    assert_eq!(outcome.notes.len(), 2, "{:?}", outcome.notes);
    assert!(outcome.notes.iter().any(|n| n.contains("downsampled")));
}

#[test]
fn an_engine_capacity_eviction_is_invalid() {
    let mut e = clean_evidence();
    let g = e.health.as_mut().unwrap().opportunity_engine.as_mut().unwrap();
    g.capacity_evictions = 1;
    let outcome = check(&e);
    assert_eq!(outcome.verdict, Verdict::Invalid);
    assert!(
        outcome.blocking.iter().any(|b| b.contains("capacity eviction")),
        "{:?}",
        outcome.blocking
    );
}

/// The September-16 engine, exactly as it ran: peak pinned at the bound.
///
/// Blocking even with `capacity_evictions` reported as zero, because a peak
/// that has reached its capacity means the population was being held down —
/// which is the condition the surviving records showed as cohorts pinned at
/// 3,750.
#[test]
fn a_peak_at_capacity_is_invalid_even_without_a_counted_eviction() {
    let mut e = clean_evidence();
    let g = e.health.as_mut().unwrap().opportunity_engine.as_mut().unwrap();
    g.capacity = 3_750;
    g.peak = 3_750;
    g.capacity_evictions = 0;
    let outcome = check(&e);
    assert_eq!(outcome.verdict, Verdict::Invalid);
    assert!(
        outcome.blocking.iter().any(|b| b.contains("reached capacity")),
        "{:?}",
        outcome.blocking
    );
}

#[test]
fn a_cohort_truncation_is_invalid() {
    let mut e = clean_evidence();
    e.health.as_mut().unwrap().opportunity_engine.as_mut().unwrap().cohort_truncations = 3;
    assert_eq!(check(&e).verdict, Verdict::Invalid);
}

/// D6: `any_known_loss` agrees with `check` about a truncated cohort. It used
/// to omit it, so the fast path could report "no known loss" for a session
/// the verdict marked INVALID -- which is what 69 production truncations
/// looked like from the health route.
#[test]
fn a_cohort_truncation_is_a_known_loss() {
    let mut report = clean_report();
    assert!(!report.any_known_loss());
    report.opportunity_engine.as_mut().unwrap().cohort_truncations = 1;
    assert!(report.any_known_loss(), "the OR counter alone");
    let mut report = clean_report();
    report.opportunity_engine.as_mut().unwrap().continuation_cohort_truncations = 1;
    assert!(report.any_known_loss(), "a per-surface counter alone");
}

/// The per-surface counters must reconcile with their OR; a report where a
/// surface was cut but the OR says zero cannot be trusted about truncation.
#[test]
fn per_surface_truncations_without_the_or_are_invalid() {
    let mut e = clean_evidence();
    let g = e.health.as_mut().unwrap().opportunity_engine.as_mut().unwrap();
    g.early_cohort_truncations = 2;
    let outcome = check(&e);
    assert_eq!(outcome.verdict, Verdict::Invalid);
    assert!(outcome.blocking.iter().any(|b| b.contains("do not reconcile")), "{:?}", outcome.blocking);

    let mut e = clean_evidence();
    let g = e.health.as_mut().unwrap().opportunity_engine.as_mut().unwrap();
    g.cohort_truncations = 2;
    g.continuation_cohort_truncations = 2;
    g.rank_cohort_capacity = 4_096;
    let outcome = check(&e);
    assert!(
        outcome.blocking.iter().any(|b| b.contains("continuation 2") && b.contains("rank bound 4096")),
        "the verdict names the surface and the bound: {:?}",
        outcome.blocking
    );
}

/// A pre-D4/D6 report -- none of the new fields -- still parses, with every
/// addition at its empty value.
#[test]
fn a_report_without_the_d4_d6_fields_still_parses() {
    let mut v = serde_json::to_value(clean_report()).unwrap();
    let obj = v.as_object_mut().unwrap();
    obj.remove("reportSchemaVersion");
    let engine = obj.get_mut("opportunityEngine").unwrap().as_object_mut().unwrap();
    for key in [
        "rankCohortCapacity",
        "earlyCohortTruncations",
        "continuationCohortTruncations",
        "truncationMarkersDropped",
        "earlyCohortLast",
        "continuationCohortLast",
        "earlyCohortPeak",
        "continuationCohortPeak",
        "rankingWindows",
        "lastRankMicros",
        "peakRankMicros",
        "closedByReason",
    ] {
        assert!(engine.remove(key).is_some(), "{key} must be serialized");
    }
    let back: CompletenessReport = serde_json::from_value(v).unwrap();
    assert_eq!(back.report_schema_version, 0);
    assert_eq!(back.opportunity_engine.unwrap().early_cohort_truncations, 0);
}

#[test]
fn incomplete_settlement_is_invalid() {
    let mut e = clean_evidence();
    e.settlement = Some(SettlementEvidence { settled: 100, unsettled: 7, capacity_evicted: 0 });
    let outcome = check(&e);
    assert_eq!(outcome.verdict, Verdict::Invalid);
    assert!(
        outcome.blocking.iter().any(|b| b.contains("settlement incomplete")),
        "{:?}",
        outcome.blocking
    );
}

#[test]
fn a_commit_mismatch_is_invalid() {
    let mut e = clean_evidence();
    e.health.as_mut().unwrap().commit = Some("0000000000000000000000000000000000000000".into());
    let outcome = check(&e);
    assert_eq!(outcome.verdict, Verdict::Invalid);
    assert!(outcome.blocking.iter().any(|b| b.contains("commit mismatch")), "{:?}", outcome.blocking);
}

/// Section 30.7: an unstamped build cannot prove which commit produced it, so
/// its session is unprovable rather than wrong.
#[test]
fn an_absent_commit_stamp_is_indeterminate_not_valid() {
    let mut e = clean_evidence();
    e.health.as_mut().unwrap().commit = None;
    let outcome = check(&e);
    assert_eq!(
        outcome.verdict,
        Verdict::Indeterminate,
        "a session whose build cannot identify itself must not pass"
    );
    assert!(outcome.blocking.is_empty(), "nothing is positively broken, only unproven");
    assert!(
        outcome.missing.iter().any(|m| m.contains("commit")),
        "and the reason must name provenance: {:?}",
        outcome.missing
    );
}

/// Section 30.6: a build that identifies itself as a *different* commit is a
/// positively established failure, not an absence.
#[test]
fn a_commit_mismatch_outranks_every_other_signal() {
    let mut e = clean_evidence();
    e.health.as_mut().unwrap().commit = Some("af986b84cd3745f077b48fef912610990b7db725".into());
    // Everything else about this session is perfect.
    let outcome = check(&e);
    assert_eq!(outcome.verdict, Verdict::Invalid);
    assert_eq!(
        outcome.blocking.len(),
        1,
        "provenance alone must be enough to disqualify: {:?}",
        outcome.blocking
    );
    assert!(outcome.blocking[0].contains("commit mismatch"));
}

/// Absent *expectation* is different from absent evidence: a caller that does
/// not say which commit it expects gets no provenance check, not a failure.
#[test]
fn no_expected_commit_means_no_provenance_check() {
    let mut e = clean_evidence();
    e.expected_commit = None;
    e.health.as_mut().unwrap().commit = Some("af986b84cd3745f077b48fef912610990b7db725".into());
    assert_eq!(check(&e).verdict, Verdict::Valid);
}

#[test]
fn a_config_fingerprint_mismatch_is_invalid() {
    let mut e = clean_evidence();
    e.health.as_mut().unwrap().oi_config_fingerprint = Some("oi-cfg-a7b2bf07d227c55e".into());
    let outcome = check(&e);
    assert_eq!(outcome.verdict, Verdict::Invalid);
    assert!(outcome.blocking.iter().any(|b| b.contains("OI config mismatch")), "{:?}", outcome.blocking);
}

#[test]
fn a_missing_required_artifact_is_invalid() {
    let mut e = clean_evidence();
    e.artifacts.retain(|a| !a.path.contains("episodes"));
    let outcome = check(&e);
    assert_eq!(outcome.verdict, Verdict::Invalid);
    assert!(
        outcome.blocking.iter().any(|b| b.contains("required artifact missing")),
        "{:?}",
        outcome.blocking
    );
}

#[test]
fn a_malformed_or_truncated_artifact_is_invalid() {
    let mut e = clean_evidence();
    e.artifacts[0].malformed_records = 1;
    assert_eq!(check(&e).verdict, Verdict::Invalid);

    let mut e = clean_evidence();
    e.artifacts[1].truncated = true;
    let outcome = check(&e);
    assert_eq!(outcome.verdict, Verdict::Invalid);
    assert!(outcome.blocking.iter().any(|b| b.contains("mid-record")), "{:?}", outcome.blocking);
}

// ---------------------------------------------------------------------------
// INDETERMINATE -- the verdict that must never be confused with a pass
// ---------------------------------------------------------------------------

#[test]
fn absent_health_evidence_is_indeterminate_not_valid() {
    let mut e = clean_evidence();
    e.health = None;
    let outcome = check(&e);
    assert_eq!(
        outcome.verdict,
        Verdict::Indeterminate,
        "a session with no health evidence must not be able to pass"
    );
    assert!(outcome.blocking.is_empty(), "nothing is positively broken, only unproven");
    assert!(outcome.missing.iter().any(|m| m.contains("no health evidence")));
}

#[test]
fn an_absent_capture_is_indeterminate() {
    for mutate in [
        (|h: &mut CompletenessReport| h.opportunity_intelligence = None) as fn(&mut CompletenessReport),
        |h: &mut CompletenessReport| h.measurement = None,
        |h: &mut CompletenessReport| h.discovery = None,
        |h: &mut CompletenessReport| h.opportunity_engine = None,
    ] {
        let mut e = clean_evidence();
        mutate(e.health.as_mut().unwrap());
        assert_eq!(check(&e).verdict, Verdict::Indeterminate);
    }
}

#[test]
fn absent_settlement_evidence_is_indeterminate() {
    let mut e = clean_evidence();
    e.settlement = None;
    let outcome = check(&e);
    assert_eq!(outcome.verdict, Verdict::Indeterminate);
    assert!(outcome.missing.iter().any(|m| m.contains("settlement")));
}

#[test]
fn a_capture_that_attempted_nothing_cannot_prove_anything() {
    let mut e = clean_evidence();
    let oi = e.health.as_mut().unwrap().opportunity_intelligence.as_mut().unwrap();
    oi.attempted = 0;
    oi.written = 0;
    let outcome = check(&e);
    assert_eq!(outcome.verdict, Verdict::Indeterminate);
    assert!(outcome.missing.iter().any(|m| m.contains("nothing attempted")));
}

/// A writer whose own counters do not add up cannot be trusted to report loss,
/// which is worse than any single counter being non-zero.
#[test]
fn unreconciled_writer_accounting_is_invalid() {
    let mut e = clean_evidence();
    e.health.as_mut().unwrap().measurement.as_mut().unwrap().written -= 5;
    let outcome = check(&e);
    assert_eq!(outcome.verdict, Verdict::Invalid);
    assert!(
        outcome.blocking.iter().any(|b| b.contains("does not reconcile")),
        "{:?}",
        outcome.blocking
    );
}

/// Invalid dominates indeterminate: a session that is both under-evidenced and
/// positively broken must read as broken.
#[test]
fn invalid_dominates_indeterminate() {
    let mut e = clean_evidence();
    e.settlement = None; // -> would be indeterminate
    e.health.as_mut().unwrap().opportunity_intelligence.as_mut().unwrap().dropped = 1;
    e.health.as_mut().unwrap().opportunity_intelligence.as_mut().unwrap().written -= 1;
    let outcome = check(&e);
    assert_eq!(outcome.verdict, Verdict::Invalid);
    assert!(!outcome.missing.is_empty(), "the missing evidence must still be reported");
}

/// The verdict is a pure function of the evidence.
#[test]
fn the_verdict_is_deterministic() {
    let e = clean_evidence();
    let first = check(&e);
    for _ in 0..16 {
        assert_eq!(check(&e), first);
    }
}

/// The evidence and the verdict both round-trip as JSON, so the checker can run
/// offline against a file rather than only in-process.
#[test]
fn evidence_and_outcome_round_trip_as_json() {
    let e = clean_evidence();
    let text = serde_json::to_string(&e).unwrap();
    let back: SessionEvidence = serde_json::from_str(&text).unwrap();
    assert_eq!(e, back);

    let outcome = check(&e);
    let text = serde_json::to_string(&outcome).unwrap();
    assert!(text.contains("\"VALID\""), "the verdict must serialize as its wire name: {text}");
    let back: Outcome = serde_json::from_str(&text).unwrap();
    assert_eq!(outcome, back);
}

// ---------------------------------------------------------------------------
// The September-16 session itself
// ---------------------------------------------------------------------------

/// The failed session, as measured, must come out INVALID — and for all four
/// of its real reasons at once, not just the first one found.
///
/// This is the regression that matters most: an instrument that cannot
/// recognise the failure it was built in response to has not been repaired.
#[test]
fn the_september_16_session_is_invalid_for_every_reason_it_actually_failed() {
    let mut e = clean_evidence();
    e.session_date = "2026-09-16".into();
    e.expected_oi_config_fingerprint = Some("oi-cfg-a7b2bf07d227c55e".into());
    let h = e.health.as_mut().unwrap();
    h.oi_config_fingerprint = Some("oi-cfg-a7b2bf07d227c55e".into());

    let oi = h.opportunity_intelligence.as_mut().unwrap();
    oi.attempted = 2_558_786;
    oi.written = 282_255;
    oi.dropped = 2_276_531;
    oi.loss_spans = 1_400;
    oi.queue_capacity = 64;
    oi.queue_peak = 64;

    let m = h.measurement.as_mut().unwrap();
    m.attempted = 140_236;
    m.written = 140_204;
    m.dropped = 32;
    m.loss_spans = 1;

    let d = h.discovery.as_mut().unwrap();
    d.queue_lost = 4_096;
    d.lost_records_total = 4_096;
    d.written = d.attempted - 4_096;

    let g = h.opportunity_engine.as_mut().unwrap();
    g.capacity = 3_750;
    g.peak = 3_750;
    g.capacity_evictions = 12_000;

    let outcome = check(&e);
    assert_eq!(outcome.verdict, Verdict::Invalid);
    let joined = outcome.blocking.join(" | ");
    for expected in [
        "opportunityIntelligence",
        "measurement:",
        "discovery:",
        "capacity eviction",
    ] {
        assert!(
            joined.contains(expected),
            "the verdict must name {expected} among its reasons: {joined}"
        );
    }
    assert!(
        outcome.blocking.len() >= 4,
        "all four independent failures must be reported, not just the first: {joined}"
    );
}
