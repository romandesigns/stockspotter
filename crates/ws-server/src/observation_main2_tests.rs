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
        process_id: None, // legacy captures predate process identity
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
        super::oi_extract::extract_legacy_process_close(session, &[("opportunity-intelligence-2026-09-29.ndjson", self.data.as_bytes())], &[("opportunity-intelligence-markers-2026-09-29.ndjson", self.markers.as_bytes())], IMPL)
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

/// Legacy artifacts are research-only: the Step-4 join refuses every one,
/// however clean, because only session-bounded (v2) evidence may qualify.
fn refused_as_legacy(x: &Extraction) {
    assert_eq!(x.binding.extraction_contract, LEGACY_EXTRACTION_CONTRACT);
    match join(x, ny(10, 0, 1)) {
        Err(OiJoinFailure::Binding(m)) => assert!(m.contains("contract"), "{m}"),
        other => panic!("legacy evidence must be refused, got {other:?}"),
    }
}

#[test]
fn legacy_mode_still_extracts_a_clean_process_bracket_but_the_join_refuses_it() {
    let snaps = snapshots();
    assert!(snaps.len() >= 3);
    assert!(snaps.iter().all(|s| s.session_date == SESSION && s.window_id == "oiw-1"), "the engine's own identities");
    let x = Src::clean().run(SESSION);
    assert!(x.report.completeness_established, "{:?}", x.report.completeness_reasons);
    assert_eq!((x.report.session_rows, x.report.known_loss, x.report.reconciled), (snaps.len() as u64, 0, true));
    assert_eq!(x.binding.sources[0].sha256, super::prereg::sha256_hex(Src::clean().data.as_bytes()));
    let text = String::from_utf8(x.normalized.clone()).unwrap();
    assert!(text.contains(LEGACY_EXTRACTION_CONTRACT), "the artifact labels itself legacy");
    refused_as_legacy(&x);
}

#[test]
fn legacy_extraction_is_deterministic_and_verifiable_from_its_sources() {
    let src = Src::clean();
    let (a, b) = (src.run(SESSION), src.run(SESSION));
    assert_eq!(a.normalized, b.normalized, "byte-identical rerun");
    assert_eq!(a.binding, b.binding);
    let data = [("opportunity-intelligence-2026-09-29.ndjson", src.data.as_bytes())];
    let markers = [("opportunity-intelligence-markers-2026-09-29.ndjson", src.markers.as_bytes())];
    verify_legacy_extraction(&a.binding, &a.normalized, &data, &markers).unwrap();
    let mut other = src.data.clone();
    other.push_str(&src.data.lines().next().unwrap().replace("\"oiw-1\"", "\"oiw-9\""));
    other.push('\n');
    let err = verify_legacy_extraction(&a.binding, &a.normalized, &[(data[0].0, other.as_bytes())], &markers).unwrap_err();
    assert!(err.contains("binding"), "{err}");
}

#[test]
fn legacy_mode_reports_every_source_defect() {
    let clean = Src::clean();
    let first = clean.data.lines().next().unwrap().to_string();
    let n = clean.data.lines().count() as u64;
    let r = Src { data: clean.data.clone() + "{not json\n", markers: Src::bracket(n + 1) }.run(SESSION).report;
    assert_eq!(r.malformed_rows, 1);
    let mut v: serde_json::Value = serde_json::from_str(&first).unwrap();
    v["schemaVersion"] = serde_json::json!(2);
    let r = Src { data: clean.data.clone() + &v.to_string() + "\n", markers: Src::bracket(n + 1) }.run(SESSION).report;
    assert_eq!(r.unsupported_schema_rows, 1);
    let mut v: serde_json::Value = serde_json::from_str(&first).unwrap();
    v["earlyQualityRank"] = serde_json::json!(99);
    let r = Src { data: clean.data.clone() + &v.to_string() + "\n", markers: Src::bracket(n + 1) }.run(SESSION).report;
    assert_eq!(r.duplicate_conflicting, 1);
    let lossy = clean.markers.clone() + &marker("queue_loss", None, Some(12)) + "\n";
    let r = Src { data: clean.data.clone(), markers: lossy }.run(SESSION).report;
    assert_eq!(r.known_loss, 12);
    for markers in [
        Src::bracket(n + 5),
        clean.markers.clone() + &marker("writer_started", None, None) + "\n",
        marker("writer_started", None, None) + "\n",
    ] {
        assert!(!Src { data: clean.data.clone(), markers }.run(SESSION).report.completeness_established);
    }
    let foreign = clean.markers.replacen(SOURCE_CAPTURE, "measurement", 1);
    assert_eq!(Src { data: clean.data.clone(), markers: foreign }.run(SESSION).report.foreign_markers, 1);
    assert!(super::oi_extract::extract_legacy_process_close("2026-9-29", &[], &[], IMPL).is_err());
}
