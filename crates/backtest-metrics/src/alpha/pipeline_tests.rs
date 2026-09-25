//! End-to-end pipeline tests (§37, §40).
//!
//! The load-bearing one is `an_invalid_session_produces_no_alpha_claim`: the
//! stop gate must prevent a predictive claim from *existing*, not merely from
//! being printed.

use super::*;

use std::path::Path;

use chrono::TimeZone;

use crate::completeness::{
    CompletenessReport, DiscoveryCapture, EngineCapture, WriterCapture,
};
use crate::opportunity::{OiConfig, OpportunityIntelligence};
use market_data::{IgnitionEventKind, ScanEvent};

const DAY: &str = "2026-09-17";
const COMMIT: &str = "af986b84cd3745f077b48fef912610990b7db725";

fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "alpha-pipeline-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(root.join("session/research")).unwrap();
    std::fs::create_dir_all(root.join("session/discovery-audit")).unwrap();
    root
}

fn at(secs: i64) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 17, 13, 30, 0).unwrap() + chrono::Duration::seconds(secs)
}

fn clean_writer(n: u64) -> WriterCapture {
    WriterCapture {
        attempted: n,
        written: n,
        dropped: 0,
        write_errors: 0,
        loss_spans: 0,
        queue_depth: 0,
        queue_peak: 12,
        queue_capacity: 16_384,
        queued_bytes: 0,
        queued_bytes_peak: 4_096,
        queue_capacity_bytes: 96 * 1024 * 1024,
        bytes_written: n * 3_000,
        batches_written: 4,
        last_write: Some(at(23_400)),
        current_file: "research/x.ndjson".into(),
        current_file_bytes: n * 3_000,
        degraded: false,
    }
}

fn health_document(clean: bool) -> CompletenessReport {
    let mut oi = clean_writer(5_000);
    if !clean {
        oi.dropped = 4_000;
        oi.written = 1_000;
        oi.loss_spans = 7;
    }
    CompletenessReport {
        report_schema_version: 1,
        generated_at: at(23_400),
        commit: Some(COMMIT.to_string()),
        oi_config_fingerprint: Some(crate::alpha::spec::EXPECTED_OI_CONFIG_FINGERPRINT.to_string()),
        oi_versions: None,
        outcome_measurement_version: None,
        episode_schema: None,
        signal_context_schema: None,
        opportunity_intelligence: Some(oi),
        measurement: Some(clean_writer(900)),
        discovery: Some(DiscoveryCapture {
            attempted: 40_000,
            written: 40_000,
            queue_lost: 0,
            write_errors: 0,
            sampled_out: 0,
            budget_dropped: 0,
            lost_records_total: 0,
            queue_depth: 0,
            queue_peak: 30,
            queue_capacity: 65_536,
            queued_bytes: 0,
            queued_bytes_peak: 100_000,
            queue_capacity_bytes: 128 * 1024 * 1024,
            bytes_written: 9_000_000,
            batches_written: 80,
            last_write: Some(at(23_400)),
            current_file: "discovery-audit/x.jsonl".into(),
            current_file_bytes: 9_000_000,
            degraded: false,
        }),
        opportunity_engine: Some(EngineCapture {
            open: 100,
            peak: 400,
            capacity: 16_375,
            capacity_evictions: 0,
            eviction_markers_dropped: 0,
            opportunities_opened: 400,
            opportunities_closed: 300,
            cohort_truncations: 0,
            scores_emitted: 5_000,
            ..EngineCapture::default()
        }),
        opportunity_outcomes: None,
        opportunity_outcome_engine: None,
    }
}

/// Writes a small but real session: engine-produced snapshots, a discovery
/// price stream, episodes and a health document.
fn write_session(root: &Path, clean_health: bool) -> PathBuf {
    let session = root.join("session");
    let symbols = 24usize;

    // Real snapshots from the real engine.
    let mut engine = OpportunityIntelligence::new(OiConfig::default());
    let mut snapshots = Vec::new();
    for i in 0..symbols {
        let t = at(0);
        engine.observe(
            &ScanEvent::IgnitionEvent {
                symbol: format!("S{i:03}"),
                timestamp: t,
                price: 10.0,
                kind: IgnitionEventKind::FollowThroughConfirmed,
            },
            t,
        );
    }
    for w in 0..4 {
        let t = at(60 + w * 30);
        for i in 0..symbols {
            engine.observe(
                &ScanEvent::MomentumUpdate {
                    symbol: format!("S{i:03}"),
                    timestamp: t,
                    volume_confirmation: 0.6,
                    structure: 0.5,
                    ma_slope: 0.7,
                    wick_rejection: 0.6,
                    overall: 0.8,
                    qualifies: true,
                },
                t,
            );
        }
        if let Some(rows) = engine.rank(t) {
            snapshots.extend(rows);
        }
    }
    let mut text = String::new();
    for snapshot in &snapshots {
        text.push_str(&serde_json::to_string(snapshot).unwrap());
        text.push('\n');
    }
    std::fs::write(
        session.join("research").join(format!("opportunity-intelligence-{DAY}.ndjson")),
        text,
    )
    .unwrap();

    // A discovery price stream, dense past the longest primary horizon.
    let mut discovery = String::new();
    for i in 0..symbols {
        for t in 0..110 {
            let elapsed = t * 10;
            let price = if i < 4 { 10.0 + elapsed as f64 * 0.002 } else { 10.0 + elapsed as f64 * 0.0001 };
            let record = serde_json::json!({
                "schema": 2,
                "recorded_at": at(elapsed).to_rfc3339(),
                "kind": "ignition",
                "data": {
                    "symbol": format!("S{i:03}"),
                    "market_at": at(elapsed).to_rfc3339(),
                    "price": price,
                    "stage": if t == 0 { "confirmed" } else { "candidate" },
                },
            });
            discovery.push_str(&record.to_string());
            discovery.push('\n');
        }
    }
    std::fs::write(
        session.join("discovery-audit").join(format!("{DAY}-1-1-1.jsonl")),
        discovery,
    )
    .unwrap();

    std::fs::write(
        session.join("research").join(format!("episodes-{DAY}.ndjson")),
        "{\"schemaVersion\":1}\n",
    )
    .unwrap();
    // Written in the shape `/research/completeness` actually returns, so the
    // fixture exercises the parser the runbook relies on.
    let captured = CapturedHealth {
        report: health_document(clean_health),
        measurement_pending: Some(MeasurementPending {
            pending: 0,
            pending_peak: 40,
            pending_capacity: 38_400,
            capacity_evictions: 0,
            open_episodes: 0,
        }),
    };
    std::fs::write(
        session.join("research").join(format!("completeness-{DAY}.json")),
        serde_json::to_string_pretty(&captured).unwrap(),
    )
    .unwrap();
    session
}

fn request(root: &Path, session: &Path, tag: &str) -> Request {
    Request {
        session_dir: session.to_path_buf(),
        session_date: DAY.to_string(),
        expected_commit: Some(COMMIT.to_string()),
        expected_oi_config: None,
        expected_spec_sha256: None,
        output_dir: root.join(format!("out-{tag}")),
        spec: QualificationSpec::default(),
    }
}

// ---------------------------------------------------------------------------
// The stop gate
// ---------------------------------------------------------------------------

/// **The load-bearing test.** An invalid session must produce no Alpha claim at
/// all — not a withheld one, not a filtered one. Nothing is computed.
#[test]
fn an_invalid_session_produces_no_alpha_claim() {
    let root = temp_root("invalid");
    let session = write_session(&root, false); // OI dropped 4,000 records
    let result = run(&request(&root, &session, "a")).unwrap();

    assert_eq!(result.session_status, Verdict::Invalid);
    assert_eq!(result.evidence_status, EvidenceStatus::NotEvaluated);
    assert!(result.evaluation.is_none(), "no evaluation may have been computed");
    assert!(result.matrix.rows.is_empty(), "no dimension may carry a number");

    let report =
        std::fs::read_to_string(root.join("out-a").join("FINAL-ALPHA-QUALIFICATION.md")).unwrap();
    assert!(report.contains("SESSION STATUS:** INVALID"));
    assert!(report.contains("no Alpha evaluation was performed"));
    // Nothing that reads as a predictive claim.
    for forbidden in ["enrichment", "QUALIFIES", "top-5 target rate"] {
        assert!(
            !report.contains(forbidden),
            "an invalid session's report must not contain {forbidden:?}"
        );
    }
    // The integrity and completeness reports are still produced.
    assert!(root.join("out-a").join("completeness.json").exists());
    assert!(root.join("out-a").join("integrity.json").exists());
    assert!(!root.join("out-a").join("surfaces.json").exists());
    let _ = std::fs::remove_dir_all(&root);
}

/// An indeterminate session — provenance absent — stops in the same way.
#[test]
fn an_indeterminate_session_also_stops_before_evaluation() {
    let root = temp_root("indeterminate");
    let session = write_session(&root, true);
    // Remove the health document: provenance cannot be established.
    std::fs::remove_file(session.join("research").join(format!("completeness-{DAY}.json")))
        .unwrap();

    let result = run(&request(&root, &session, "b")).unwrap();
    assert_eq!(result.session_status, Verdict::Indeterminate);
    assert_eq!(result.evidence_status, EvidenceStatus::NotEvaluated);
    assert!(result.evaluation.is_none());
    let _ = std::fs::remove_dir_all(&root);
}

/// A commit that does not match the expectation is blocking, not a note.
#[test]
fn a_commit_mismatch_stops_the_pipeline() {
    let root = temp_root("commit");
    let session = write_session(&root, true);
    let mut request = request(&root, &session, "c");
    request.expected_commit = Some("0000000000000000000000000000000000000000".to_string());

    let result = run(&request).unwrap();
    assert_eq!(result.session_status, Verdict::Invalid);
    assert!(result.completeness.blocking.iter().any(|b| b.contains("commit mismatch")));
    assert!(result.evaluation.is_none());
    let _ = std::fs::remove_dir_all(&root);
}

/// The OI configuration fingerprint is checked the same way the commit is.
///
/// A session captured under a different configuration is not a session about
/// this contract's subject. The contract binds one fingerprint
/// (`oi-cfg-73ccdbaf661996ed` since D5, provisional); a capture carrying another one describes a
/// different engine, and comparing the two would silently attribute one
/// configuration's behaviour to another.
#[test]
fn an_oi_config_mismatch_stops_the_pipeline() {
    let root = temp_root("oicfg");
    let session = write_session(&root, true);
    let mut mismatched = request(&root, &session, "cfg");
    mismatched.expected_oi_config = Some("oi-cfg-0000000000000000".to_string());

    let result = run(&mismatched).unwrap();
    assert_eq!(result.session_status, Verdict::Invalid);
    assert!(
        result.completeness.blocking.iter().any(|b| b.contains("OI config mismatch")),
        "the mismatch must be named, not merely counted: {:?}",
        result.completeness.blocking
    );
    assert!(result.evaluation.is_none(), "a mismatched configuration yields no Alpha claim");

    // And the default -- no override -- agrees with the capture, so the same
    // fixture passes its gate. Without this half, the test above would also
    // pass if the check rejected everything.
    let ok = run(&request(&root, &session, "cfg-ok")).unwrap();
    assert_eq!(ok.session_status, Verdict::Valid);
    let _ = std::fs::remove_dir_all(&root);
}

/// A contract that moved after the session opened refuses to run at all (§29).
///
/// This is the mechanical half of session immutability. The operator records
/// the hash before the market opens and passes it back afterwards; if any
/// threshold, control or blocking flag changed in between, the hash differs
/// and there is no report. The failure is an `Err` -- not a verdict -- because
/// a contract mismatch cannot be reconciled by looking at the session, so
/// producing any document about it would be producing a document whose
/// authority is unearned.
#[test]
fn a_specification_mismatch_fails_loudly_before_anything_is_read() {
    let root = temp_root("specmm");
    let session = write_session(&root, true);
    let mut stale_request = request(&root, &session, "mm");
    let stale = "0".repeat(64);
    stale_request.expected_spec_sha256 = Some(stale.clone());

    let error = run(&stale_request).expect_err("a moved contract must refuse to run");
    let message = error.to_string();
    assert!(message.contains(&stale), "the recorded hash must be named: {message}");
    assert!(
        message.contains(&QualificationSpec::default().sha256()),
        "the build's own hash must be named too, so the difference is inspectable: {message}"
    );
    // Loudly means loudly: no output directory is left behind to be mistaken
    // for a result.
    assert!(
        !root.join("out-mm").exists(),
        "a refused run must not leave a partial report"
    );

    // The matching hash runs normally -- proving the gate discriminates.
    let mut good = stale_request.clone();
    good.expected_spec_sha256 = Some(QualificationSpec::default().sha256());
    good.output_dir = root.join("out-mm-ok");
    let result = run(&good).expect("the recorded contract must be accepted");
    assert_eq!(result.spec_sha256, QualificationSpec::default().sha256());

    // Case is not identity theatre: a hash is a hash however it was pasted.
    let mut upper = stale_request.clone();
    upper.expected_spec_sha256 = Some(QualificationSpec::default().sha256().to_uppercase());
    upper.output_dir = root.join("out-mm-upper");
    assert!(run(&upper).is_ok(), "an upper-cased hash is the same hash");
    let _ = std::fs::remove_dir_all(&root);
}

/// A valid session proceeds and produces a full evaluation.
#[test]
fn a_valid_session_is_evaluated() {
    let root = temp_root("valid");
    let session = write_session(&root, true);
    let result = run(&request(&root, &session, "d")).unwrap();

    assert_eq!(result.session_status, Verdict::Valid, "{:?}", result.completeness);
    assert!(result.evaluation.is_some(), "a valid session must be evaluated");
    // This fixture is far too small to satisfy the contract's evidence floors,
    // so the honest answer is INSUFFICIENT_EVIDENCE -- never a failure.
    assert_eq!(result.evidence_status, EvidenceStatus::InsufficientEvidence);
    assert!(!result.matrix.evidence_shortfalls.is_empty());
    // Individual criteria may still be *reported* as failing on the little data
    // there was, and that is honest -- but the shortfall outranks them, so the
    // programme-level answer is "not enough evidence", never "V1 failed".
    // That precedence is the property under test.
    assert_ne!(
        result.evidence_status,
        EvidenceStatus::DoesNotQualify,
        "a session too thin to judge must never be reported as V1 failing"
    );
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// Immutability and outputs
// ---------------------------------------------------------------------------

#[test]
fn an_existing_output_directory_is_refused() {
    let root = temp_root("exists");
    let session = write_session(&root, true);
    let request = request(&root, &session, "e");
    std::fs::create_dir_all(&request.output_dir).unwrap();

    match run(&request) {
        Err(Error::OutputExists(path)) => assert_eq!(path, request.output_dir),
        other => panic!("expected a refusal, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// Source artifacts must be byte-identical afterwards.
#[test]
fn the_pipeline_never_modifies_a_source_artifact() {
    let root = temp_root("readonly");
    let session = write_session(&root, true);

    let digest_all = |dir: &Path| -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for sub in ["research", "discovery-audit"] {
            for entry in std::fs::read_dir(dir.join(sub)).unwrap().flatten() {
                out.insert(
                    entry.file_name().to_string_lossy().to_string(),
                    sha256::hex_file(&entry.path()).unwrap(),
                );
            }
        }
        out
    };
    let before = digest_all(&session);
    run(&request(&root, &session, "f")).unwrap();
    let after = digest_all(&session);
    assert_eq!(before, after, "the pipeline must not write to its inputs");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_output_directory_carries_every_required_artifact() {
    let root = temp_root("outputs");
    let session = write_session(&root, true);
    run(&request(&root, &session, "g")).unwrap();
    let out = root.join("out-g");

    for name in [
        "manifest.json",
        "qualification-spec.json",
        "qualification-spec.sha256",
        "completeness.json",
        "integrity.json",
        "qualification.json",
        "opportunities.ndjson",
        "reference-opportunities.ndjson",
        "stage-ladder.ndjson",
        "surfaces.json",
        "segmentation.json",
        "secondary-direction.json",
        "recall.json",
        "FINAL-ALPHA-QUALIFICATION.md",
        "SHA256SUMS",
    ] {
        assert!(out.join(name).exists(), "{name} is missing from the output directory");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// `SHA256SUMS` must actually verify, in the form `sha256sum -c` reads.
#[test]
fn the_checksum_file_verifies_against_the_outputs_it_names() {
    let root = temp_root("sums");
    let session = write_session(&root, true);
    run(&request(&root, &session, "h")).unwrap();
    let out = root.join("out-h");

    let sums = std::fs::read_to_string(out.join("SHA256SUMS")).unwrap();
    let mut checked = 0;
    for line in sums.lines() {
        let (digest, name) = line.split_once("  ").expect("two-space separator, as sha256sum writes");
        assert_eq!(digest.len(), 64);
        assert_eq!(
            sha256::hex_file(&out.join(name)).unwrap(),
            digest,
            "{name} does not match its recorded digest"
        );
        checked += 1;
    }
    // Every file except SHA256SUMS itself, which is written last and over the
    // others.
    assert!(checked >= 14, "only {checked} files checksummed");
    assert!(
        !sums.contains("SHA256SUMS"),
        "the checksum file must not attempt to checksum itself"
    );
    // The spec's own sidecar digest must agree with the spec file.
    let sidecar = std::fs::read_to_string(out.join("qualification-spec.sha256")).unwrap();
    assert!(sidecar.starts_with(&sha256::hex_file(&out.join("qualification-spec.json")).unwrap()));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_report_quotes_the_exact_specification_hash() {
    let root = temp_root("spechash");
    let session = write_session(&root, true);
    let request = request(&root, &session, "i");
    let expected = request.spec.sha256();
    run(&request).unwrap();

    let report =
        std::fs::read_to_string(root.join("out-i").join("FINAL-ALPHA-QUALIFICATION.md")).unwrap();
    assert!(
        report.contains(&expected),
        "the report must identify the exact contract it applied"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Identical input must give an identical result, byte for byte, apart from
/// the generation timestamp.
#[test]
fn a_complete_run_is_deterministic() {
    let root = temp_root("determinism");
    let session = write_session(&root, true);
    let first = run(&request(&root, &session, "j1")).unwrap();
    let second = run(&request(&root, &session, "j2")).unwrap();

    assert_eq!(first.session_status, second.session_status);
    assert_eq!(first.evidence_status, second.evidence_status);
    assert_eq!(first.matrix, second.matrix);
    assert_eq!(first.integrity.digests, second.integrity.digests);
    assert_eq!(
        first.evaluation.as_ref().map(|e| &e.results),
        second.evaluation.as_ref().map(|e| &e.results)
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_unbound_specification_is_refused_before_anything_is_read() {
    let root = temp_root("unbound");
    let session = write_session(&root, true);
    let mut request = request(&root, &session, "k");
    request.spec.expected_oi_config_fingerprint = None;

    match run(&request) {
        Err(Error::SpecInvalid(reason)) => assert!(reason.contains("bound to an OI configuration")),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(!request.output_dir.exists(), "nothing may be written for an invalid contract");
    let _ = std::fs::remove_dir_all(&root);
}

/// The route's own response shape must parse, and so must a bare report.
#[test]
fn both_health_document_shapes_are_accepted() {
    let full = CapturedHealth {
        report: health_document(true),
        measurement_pending: Some(MeasurementPending {
            pending: 3,
            pending_peak: 40,
            pending_capacity: 38_400,
            capacity_evictions: 0,
            open_episodes: 1,
        }),
    };
    let text = serde_json::to_string(&full).unwrap();
    let parsed = parse_health(&text).unwrap();
    assert_eq!(parsed.measurement_pending.unwrap().pending, 3);

    // A bare report carries no settlement, which must stay distinguishable
    // from "everything settled".
    let bare = serde_json::to_string(&health_document(true)).unwrap();
    let parsed = parse_health(&bare).unwrap();
    assert!(
        parsed.measurement_pending.is_none(),
        "a bare report must not be read as evidence that settlement completed"
    );
    assert!(parse_health("not json").is_none());
}

/// Unsettled episodes are blocking: their horizons are censored by the capture
/// rather than by the market.
#[test]
fn an_unsettled_session_is_invalid() {
    let root = temp_root("unsettled");
    let session = write_session(&root, true);
    let captured = CapturedHealth {
        report: health_document(true),
        measurement_pending: Some(MeasurementPending {
            pending: 17,
            pending_peak: 900,
            pending_capacity: 38_400,
            capacity_evictions: 0,
            open_episodes: 17,
        }),
    };
    std::fs::write(
        session.join("research").join(format!("completeness-{DAY}.json")),
        serde_json::to_string_pretty(&captured).unwrap(),
    )
    .unwrap();

    let result = run(&request(&root, &session, "l")).unwrap();
    assert_eq!(result.session_status, Verdict::Invalid);
    assert!(result.completeness.blocking.iter().any(|b| b.contains("settlement incomplete")));
    assert!(result.evaluation.is_none());
    let _ = std::fs::remove_dir_all(&root);
}
