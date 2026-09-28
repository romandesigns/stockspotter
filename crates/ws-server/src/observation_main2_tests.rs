//! Step 4B-main.2: the D6 -> OI-rank adapter, its binding, and the
//! authenticated Arm-B join. Sources are produced by the real
//! opportunity-intelligence engine from synthetic scan events and serialised
//! exactly as the production writer serialises them; no real capture, trade,
//! eligibility or outcome data is read.

use backtest_metrics::opportunity::{OiConfig, OpportunityIntelligence, OpportunityScoreSnapshot};
use chrono::{DateTime, Duration, Utc};
use market_data::{IgnitionEventKind, ScanEvent};

use super::analysis::*;
use super::observation_main_tests::{extract, ny, wx, SESSION};
use super::oi_extract::*;
use crate::research_writer::{CaptureMarker, QueueLossSpan, MARKER_SCHEMA_VERSION};

const IMPL: &str = "0123456789abcdef0123456789abcdef01234567";

fn momentum(symbol: &str, t: DateTime<Utc>, overall: f64) -> ScanEvent {
    ScanEvent::MomentumUpdate {
        symbol: symbol.into(),
        timestamp: t,
        volume_confirmation: 0.5,
        structure: 0.5,
        ma_slope: 0.2,
        wick_rejection: 0.3,
        overall,
        qualifies: overall >= 0.6,
    }
}

fn confirmed(symbol: &str, t: DateTime<Utc>, price: f64) -> ScanEvent {
    ScanEvent::IgnitionEvent { symbol: symbol.into(), timestamp: t, price, kind: IgnitionEventKind::FollowThroughConfirmed }
}

/// One ranking window from the real engine, at `ny(10, 0, 0)`.
fn snapshots() -> Vec<OpportunityScoreSnapshot> {
    let mut oi = OpportunityIntelligence::new(OiConfig::default());
    for (i, sym) in ["AAA", "BBB", "CCC", "DDD"].iter().enumerate() {
        let t = ny(9, 55, 0) + Duration::seconds(i as i64);
        oi.observe(&momentum(sym, t, 0.5 + 0.1 * i as f64), t);
        oi.observe(&confirmed(sym, t + Duration::seconds(5), 10.0), t + Duration::seconds(5));
        oi.observe(&confirmed(sym, t + Duration::seconds(60), 10.0 + i as f64 * 0.3), t + Duration::seconds(60));
    }
    oi.rank(ny(10, 0, 0)).expect("a ranking window")
}

fn line<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap()
}

fn marker(kind: &str, data: Option<serde_json::Value>, loss: Option<u64>) -> String {
    line(&CaptureMarker {
        schema_version: MARKER_SCHEMA_VERSION,
        recorded_at: ny(9, 0, 0),
        kind: kind.into(),
        capture: SOURCE_CAPTURE.into(),
        queue_loss: loss.map(|lost| QueueLossSpan {
            lost,
            onset: None,
            onset_micros: 0,
            cumulative_dropped: lost,
            reason: "queue_full".into(),
        }),
        data,
    })
}

struct Src {
    data: String,
    markers: String,
}

impl Src {
    /// A clean, complete single-process capture of `rows` data lines.
    fn clean() -> Self {
        let data: Vec<String> = snapshots().iter().map(line).collect();
        let n = data.len() as u64;
        Self { data: data.join("\n") + "\n", markers: Self::bracket(n) }
    }
    fn bracket(scores: u64) -> String {
        [marker("writer_started", None, None), marker("capture_finished", Some(serde_json::json!({ "scoresEmitted": scores })), None)]
            .join("\n")
            + "\n"
    }
    fn run(&self, session: &str) -> Extraction {
        super::oi_extract::extract(session, &[("opportunity-intelligence-2026-09-29.ndjson", self.data.as_bytes())], &[("opportunity-intelligence-markers-2026-09-29.ndjson", self.markers.as_bytes())], IMPL)
            .unwrap()
    }
}

/// A session extract whose single ranking window matches the engine's.
fn session_extract(anchor: DateTime<Utc>, window: &str) -> SessionExtract {
    extract(vec![wx(window, anchor, true, vec![])])
}

fn join(x: &Extraction, anchor: DateTime<Utc>) -> Result<OiRanks, OiJoinFailure> {
    let a = OiArtifact::parse(&x.normalized);
    let expected = OiExpectation { normalized_sha256: &x.binding.normalized_sha256, implementation_sha: IMPL };
    authenticate_oi(&a, &x.binding, &expected, &session_extract(anchor, "oiw-1"), SESSION)
}

#[test]
fn a_clean_source_extracts_completely_and_joins_on_the_engine_ranks() {
    let snaps = snapshots();
    assert!(snaps.len() >= 3);
    assert!(snaps.iter().all(|s| s.session_date == SESSION && s.window_id == "oiw-1"), "the engine's own identities");
    let x = Src::clean().run(SESSION);
    assert!(x.report.completeness_established, "{:?}", x.report.completeness_reasons);
    assert_eq!((x.report.session_rows, x.report.known_loss, x.report.reconciled), (snaps.len() as u64, 0, true));
    let ranks = join(&x, ny(10, 0, 1)).unwrap();
    for s in &snaps {
        let r = ranks.rows[&(s.window_id.clone(), s.opportunity_id.clone())];
        assert_eq!(r, s.early_quality_rank.map(|x| x as u64), "{}", s.opportunity_id);
    }
    // The binding names the source files by content.
    assert_eq!(x.binding.sources.len(), 2);
    assert_eq!(x.binding.sources[0].sha256, super::prereg::sha256_hex(Src::clean().data.as_bytes()));
    assert_eq!(x.binding.source_schema_version, SUPPORTED_SOURCE_SCHEMA);
}

#[test]
fn extraction_is_deterministic_and_verifiable_from_its_sources() {
    let src = Src::clean();
    let (a, b) = (src.run(SESSION), src.run(SESSION));
    assert_eq!(a.normalized, b.normalized, "byte-identical rerun");
    assert_eq!(a.binding, b.binding);
    let data = [("opportunity-intelligence-2026-09-29.ndjson", src.data.as_bytes())];
    let markers = [("opportunity-intelligence-markers-2026-09-29.ndjson", src.markers.as_bytes())];
    verify_extraction(&a.binding, &a.normalized, &data, &markers).unwrap();
    // Row order in the source does not change the normalised rows.
    let mut lines: Vec<&str> = src.data.lines().collect();
    lines.reverse();
    let reversed = Src { data: lines.join("\n") + "\n", markers: src.markers.clone() }.run(SESSION);
    let body = |x: &Extraction| String::from_utf8(x.normalized.clone()).unwrap().lines().skip(1).map(str::to_string).collect::<Vec<_>>();
    assert_eq!(body(&reversed), body(&a));
}

#[test]
fn a_normalised_file_cannot_float_free_of_its_source_identity() {
    let src = Src::clean();
    let x = src.run(SESSION);
    // Wrong source: re-extraction from different bytes refuses the binding.
    let mut other = src.data.clone();
    other.push('\n');
    other.push_str(&src.data.lines().next().unwrap().replace("\"oiw-1\"", "\"oiw-9\""));
    let err = verify_extraction(
        &x.binding,
        &x.normalized,
        &[("opportunity-intelligence-2026-09-29.ndjson", other.as_bytes())],
        &[("opportunity-intelligence-markers-2026-09-29.ndjson", src.markers.as_bytes())],
    )
    .unwrap_err();
    assert!(err.contains("binding"), "{err}");
    // A binding naming a different source SHA does not match the header.
    let mut forged = x.binding.clone();
    forged.sources[0].sha256 = "f".repeat(64);
    let a = OiArtifact::parse(&x.normalized);
    let exp = OiExpectation { normalized_sha256: &x.binding.normalized_sha256, implementation_sha: IMPL };
    let e = authenticate_oi(&a, &forged, &exp, &session_extract(ny(10, 0, 1), "oiw-1"), SESSION).unwrap_err();
    assert!(matches!(e, OiJoinFailure::Binding(_)), "{e:?}");
    // Wrong implementation, wrong contract, edited normalised bytes.
    let exp_impl = OiExpectation { normalized_sha256: &x.binding.normalized_sha256, implementation_sha: &"9".repeat(40) };
    assert!(matches!(authenticate_oi(&a, &x.binding, &exp_impl, &session_extract(ny(10, 0, 1), "oiw-1"), SESSION), Err(OiJoinFailure::Binding(_))));
    let mut contract = x.binding.clone();
    contract.extraction_contract = "d6-oi-extract-v0".into();
    assert!(matches!(authenticate_oi(&a, &contract, &exp, &session_extract(ny(10, 0, 1), "oiw-1"), SESSION), Err(OiJoinFailure::Binding(_))));
    let edited = String::from_utf8(x.normalized.clone()).unwrap().replacen("\"earlyQualityRank\":1", "\"earlyQualityRank\":2", 1);
    let e = authenticate_oi(&OiArtifact::parse(edited.as_bytes()), &x.binding, &exp, &session_extract(ny(10, 0, 1), "oiw-1"), SESSION).unwrap_err();
    assert!(matches!(e, OiJoinFailure::ArtifactIdentity { .. }), "{e:?}");
}

fn fails(src: Src, session: &str, anchor: DateTime<Utc>) -> (ExtractionReport, OiJoinFailure) {
    let x = src.run(session);
    let err = join(&x, anchor).unwrap_err();
    (x.report, err)
}

#[test]
fn every_source_defect_fails_the_join_closed() {
    let clean = Src::clean();
    let first = clean.data.lines().next().unwrap().to_string();
    let n = clean.data.lines().count() as u64;
    let at = ny(10, 0, 1);

    // Malformed row (reconciled count so only the malformation is under test).
    let (r, e) = fails(Src { data: clean.data.clone() + "{not json\n", markers: Src::bracket(n + 1) }, SESSION, at);
    assert_eq!((r.malformed_rows, e), (1, OiJoinFailure::SourceMalformed(1)));

    // Missing opportunity id.
    let mut v: serde_json::Value = serde_json::from_str(&first).unwrap();
    v.as_object_mut().unwrap().remove("opportunityId");
    let (r, e) = fails(Src { data: clean.data.clone() + &v.to_string() + "\n", markers: Src::bracket(n + 1) }, SESSION, at);
    assert_eq!((r.malformed_rows, e), (1, OiJoinFailure::SourceMalformed(1)));

    // Unsupported source schema (symbol-activity-v1).
    let mut v: serde_json::Value = serde_json::from_str(&first).unwrap();
    v["schemaVersion"] = serde_json::json!(2);
    let (r, _) = fails(Src { data: clean.data.clone() + &v.to_string() + "\n", markers: Src::bracket(n + 1) }, SESSION, at);
    assert_eq!(r.unsupported_schema_rows, 1);

    // Conflicting duplicate rank; and ranked vs null.
    for rank in [serde_json::json!(99), serde_json::Value::Null] {
        let mut v: serde_json::Value = serde_json::from_str(&first).unwrap();
        if v["earlyQualityRank"] == rank {
            continue;
        }
        v["earlyQualityRank"] = rank;
        let (r, e) = fails(Src { data: clean.data.clone() + &v.to_string() + "\n", markers: Src::bracket(n + 1) }, SESSION, at);
        assert_eq!(r.duplicate_conflicting, 1);
        assert!(matches!(e, OiJoinFailure::ConflictingRanks { .. }), "{e:?}");
    }

    // Known loss.
    let lossy = clean.markers.clone() + &marker("queue_loss", None, Some(12)) + "\n";
    let (r, e) = fails(Src { data: clean.data.clone(), markers: lossy }, SESSION, at);
    assert_eq!((r.known_loss, e), (12, OiJoinFailure::KnownLoss(12)));
    let trunc = clean.markers.clone() + &marker("ranking_cohort_truncated", Some(serde_json::json!({})), None) + "\n";
    assert_eq!(fails(Src { data: clean.data.clone(), markers: trunc }, SESSION, at).1, OiJoinFailure::KnownLoss(1));

    // Completeness not establishable: unreconciled, two processes, no finish.
    for markers in [
        Src::bracket(n + 5),
        clean.markers.clone() + &marker("writer_started", None, None) + "\n",
        marker("writer_started", None, None) + "\n",
    ] {
        let (r, e) = fails(Src { data: clean.data.clone(), markers }, SESSION, at);
        assert!(!r.completeness_established);
        assert_eq!(e, OiJoinFailure::CompletenessNotEstablished, "{:?}", r.completeness_reasons);
    }

    // Wrong session: the rows are another session's and the binding says so.
    let (r, e) = fails(Src::clean(), "2026-09-30", at);
    assert_eq!((r.session_rows, r.other_session_rows), (0, n));
    assert!(matches!(e, OiJoinFailure::Binding(_)), "{e:?}");

    // Future-computed rank: the window's anchor precedes the computation.
    let (_, e) = fails(Src::clean(), SESSION, ny(9, 59, 59));
    assert!(matches!(e, OiJoinFailure::FutureInformation { .. }), "{e:?}");
}

#[test]
fn a_missing_window_and_identical_duplicates_are_handled_deterministically() {
    let clean = Src::clean();
    let x = clean.run(SESSION);
    // The session extract has no window oiw-1.
    let a = OiArtifact::parse(&x.normalized);
    let exp = OiExpectation { normalized_sha256: &x.binding.normalized_sha256, implementation_sha: IMPL };
    let e = authenticate_oi(&a, &x.binding, &exp, &session_extract(ny(10, 0, 1), "oiw-7"), SESSION).unwrap_err();
    assert!(matches!(e, OiJoinFailure::UnknownWindow(_)), "{e:?}");
    // An identical duplicate line collapses; the engine count still has to
    // reconcile with what is on disk.
    let first = clean.data.lines().next().unwrap();
    let n = clean.data.lines().count() as u64;
    let dup = Src { data: clean.data.clone() + first + "\n", markers: Src::bracket(n + 1) }.run(SESSION);
    assert_eq!((dup.report.duplicate_identical, dup.report.duplicate_conflicting), (1, 0));
    assert_eq!(dup.binding.normalized_rows, x.binding.normalized_rows);
    assert_eq!(join(&dup, ny(10, 0, 1)).unwrap().rows.len() as u64, x.binding.normalized_rows);
    let unreconciled = Src { data: clean.data.clone() + first + "\n", markers: clean.markers.clone() }.run(SESSION);
    assert!(!unreconciled.report.completeness_established, "more lines on disk than the engine emitted");
}

#[test]
fn a_foreign_or_malformed_marker_is_not_silently_ignored() {
    let clean = Src::clean();
    let foreign = clean.markers.replacen(SOURCE_CAPTURE, "measurement", 1);
    let r = Src { data: clean.data.clone(), markers: foreign }.run(SESSION).report;
    assert_eq!(r.foreign_markers, 1);
    assert!(!r.completeness_established);
    let r = Src { data: clean.data.clone(), markers: clean.markers.clone() + "garbage\n" }.run(SESSION).report;
    assert_eq!(r.malformed_markers, 1);
    assert!(!r.completeness_established);
    // Bad inputs are refused outright.
    assert!(super::oi_extract::extract("2026-9-29", &[], &[], IMPL).is_err());
    assert!(super::oi_extract::extract(SESSION, &[], &[], "abc").is_err());
}
