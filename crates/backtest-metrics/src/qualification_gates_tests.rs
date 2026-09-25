//! Qualification machine gates (`alpha-qualification-v5`, P3 brief §16) and
//! the deploy-day `baselineTruncated` semantics they rely on (brief §17).
//!
//! Every gate has a negative test that starts from a session passing every
//! gate and breaks exactly one thing, so a failure proves the gate reacts to
//! *that* condition. The fields P3 added (`duplicateIdentityRefused`,
//! `lifecycle`, `premarketVolume.*`, the move-v1 disposition tokens) are
//! written by the names the route emits; the fail-closed tests prove a
//! health document from an older build, which lacks them, cannot pass.

use super::*;
use crate::alpha::spec::QualificationSpec;
use crate::context::FeatureCache;
use crate::signals::Strategy;
use chrono::TimeZone;
use chrono_tz::America::New_York;
use market_data::ScanEvent;
use serde_json::{json, Value};

const DAY: &str = "2026-09-29"; // a Tuesday, EDT: the market day opens 08:00Z
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn day() -> NaiveDate {
    DAY.parse().unwrap()
}

fn z(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

/// A New York wall-clock instant, DST-aware.
fn et(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
    New_York
        .with_ymd_and_hms(y, mo, d, h, mi, s)
        .single()
        .unwrap()
        .with_timezone(&Utc)
}

fn pins() -> QualificationPins {
    QualificationSpec::default().pins(Some(COMMIT.to_string()))
}

fn writer(n: u64) -> Value {
    json!({"attempted": n, "written": n, "dropped": 0, "writeErrors": 0, "lossSpans": 0,
           "queuePeak": 10, "queueCapacity": 16384, "degraded": false})
}

/// The `/research/completeness` envelope of a session that passes every gate,
/// including the fields P3 added, where the route puts them.
fn clean_doc() -> Value {
    let p = pins();
    json!({
        "report": {
            "reportSchemaVersion": 1,
            "generatedAt": "2026-09-29T21:00:00Z",
            "commit": COMMIT,
            "oiConfigFingerprint": p.oi_config_fingerprint,
            "oiVersions": {
                "opportunitySchema": p.opportunity_schema,
                "featureSchema": p.feature_schema,
                "regimeClassifier": p.regime_classifier,
                "priceRegime": p.price_regime,
                "earlyQualityModel": p.early_quality_model,
                "continuationModel": p.continuation_model,
                "ranking": p.ranking,
                "scorePolicy": p.score_policy,
                "configFingerprint": p.oi_config_fingerprint,
                "baselinePolicy": p.baseline_policy,
                "lifecycle": p.lifecycle,
            },
            "outcomeMeasurementVersion": p.outcome_measurement_version,
            "episodeSchema": p.episode_schema,
            "signalContextSchema": p.signal_context_schema,
            "opportunityIntelligence": writer(5_000_000),
            "measurement": writer(90_000),
            "opportunityOutcomes": writer(4_000_000),
            "discovery": {"attempted": 40_000_000, "written": 40_000_000, "queueLost": 0,
                          "writeErrors": 0, "sampledOut": 0, "budgetDropped": 0},
            "opportunityEngine": {
                "open": 900, "peak": 4_700, "capacity": 16_375, "capacityEvictions": 0,
                "evictionMarkersDropped": 0, "opportunitiesOpened": 30_000,
                "opportunitiesClosed": 29_100, "cohortTruncations": 0,
                "rankCohortCapacity": 16_375, "earlyCohortTruncations": 0,
                "continuationCohortTruncations": 0, "truncationMarkersDropped": 0,
                "closedByReason": {"setupInactivity": 20_000, "invalidated": 9_000,
                                   "sessionBoundary": 100, "capacityReached": 0, "captureEnded": 0},
                "duplicateIdentityRefused": 0,
                "lifecycle": p.lifecycle,
                "marketDayId": DAY,
            },
            "opportunityOutcomeEngine": {
                "outstanding": 40, "peakOutstanding": 170_000, "capacity": 297_000,
                "anchorsCreated": 4_000_040, "anchorsSettled": 4_000_000, "capacityEvictions": 0,
                "closureNotices": 29_100, "closureAnchorsScanned": 900_000,
                "closureAnchorsMarked": 1_000_010,
                "dispositionCounts": {"stillOpen": 3_000_000, "setupInactivity": 700_000,
                                      "invalidated": 290_000, "sessionBoundary": 10_000,
                                      "capacityReached": 0, "captureEnded": 0},
            },
        },
        // Beside the report, as `http::completeness_envelope` emits it.
        "premarketVolume": {"fetchFailures": 0, "initFailures": 0, "marketDay": DAY,
                            "initializedAt": "2026-09-29T08:00:40Z"},
        "measurementPending": {"pending": 0, "pendingPeak": 40, "pendingCapacity": 38_400,
                               "capacityEvictions": 0, "openEpisodes": 0},
        "retention": {"retentionPending": false, "blockedByProtection": false,
                      "protectedWithoutReceipt": [], "registryErrors": []},
        "discoveryRetention": {"blockedByProtection": false, "registryErrors": []},
        "anyKnownLoss": false,
    })
}

fn designation() -> DesignationRecord {
    let spec = QualificationSpec::default();
    DesignationRecord {
        schema_version: DESIGNATION_SCHEMA_VERSION,
        market_day: day(),
        designated_at: z("2026-09-29T07:10:00Z"), // 03:10 ET
        designated_by: "roman".into(),
        commit: COMMIT.into(),
        oi_config_fingerprint: spec.expected_oi_config_fingerprint.clone().unwrap(),
        spec_version: spec.version.clone(),
        spec_sha256: spec.sha256(),
        process_started_at: z("2026-09-28T21:30:00Z"), // 17:30 ET the day before
        deploy_marker_at: z("2026-09-28T21:29:00Z"),
        container_restart_counts: [("ws".to_string(), 0u64)].into_iter().collect(),
        preflight: "PASS".into(),
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

struct Fx {
    doc: Option<Value>,
    outcome: Outcome,
    artifacts: Vec<ArtifactEvidence>,
    required: Vec<String>,
    baseline: Option<BaselineEvidence>,
    designation: DesignationEvidence,
    protection: ProtectionEvidence,
    pins: QualificationPins,
}

impl Fx {
    fn clean() -> Self {
        let oi = format!("research/opportunity-intelligence-{DAY}.ndjson");
        let outcomes = format!("research/opportunity-outcomes-{DAY}.ndjson");
        Fx {
            doc: Some(clean_doc()),
            outcome: Outcome {
                verdict: Verdict::Valid,
                blocking: vec![],
                missing: vec![],
                notes: vec![],
            },
            artifacts: vec![artifact(&oi, 5_000_000), artifact(&outcomes, 4_000_000)],
            required: vec![oi, outcomes],
            baseline: Some(BaselineEvidence {
                market_day: day(),
                rows_scanned: 5_000_000,
                truncated_rows: 0,
                complete_rows: 4_900_000,
                first_truncated: None,
                read_error: None,
            }),
            designation: DesignationEvidence::Present(designation()),
            protection: ProtectionEvidence::default(),
            pins: pins(),
        }
    }

    fn run(&self) -> GateReport {
        qualification_gates(&GateInputs {
            market_day: day(),
            health: self.doc.as_ref(),
            completeness: &self.outcome,
            artifacts: &self.artifacts,
            required_artifacts: &self.required,
            baseline: self.baseline.as_ref(),
            designation: &self.designation,
            protection: &self.protection,
            pins: &self.pins,
        })
    }

    /// Sets (or, with `None`, removes) one envelope field by dotted path.
    fn set(&mut self, path: &str, value: Option<Value>) -> &mut Self {
        let mut node = self.doc.as_mut().unwrap();
        let keys: Vec<&str> = path.split('.').collect();
        for key in &keys[..keys.len() - 1] {
            node = node
                .get_mut(*key)
                .unwrap_or_else(|| panic!("fixture has no {key} in {path}"));
        }
        let object = node.as_object_mut().unwrap();
        let last = keys[keys.len() - 1];
        match value {
            Some(v) => {
                object.insert(last.to_string(), v);
            }
            None => {
                object.remove(last);
            }
        }
        self
    }

    fn designation_mut(&mut self) -> &mut DesignationRecord {
        match &mut self.designation {
            DesignationEvidence::Present(r) => r,
            _ => panic!("fixture designation is not present"),
        }
    }
}

fn result<'a>(report: &'a GateReport, check: &str) -> &'a GateResult {
    report
        .results
        .iter()
        .find(|r| r.check == check)
        .unwrap_or_else(|| panic!("no result for {check}"))
}

/// The session fails, the named gate is among the failures, and the named
/// check is the one that failed -- with `absent` as stated.
#[track_caller]
fn assert_fails(fx: &Fx, gate: &str, check: &str, absent: bool) {
    let report = fx.run();
    assert!(
        !report.passed(),
        "{gate}/{check}: the session must not pass"
    );
    assert!(
        report.failed_gates().contains(&gate),
        "{gate} not among {:?}",
        report.failed_gates()
    );
    let r = result(&report, check);
    assert!(!r.pass, "{check} passed: {r:?}");
    assert_eq!(r.absent, absent, "{check}: absent flag {r:?}");
    assert_eq!(r.gate, gate);
    let mut folded = fx.outcome.clone();
    report.fold_into(&mut folded);
    assert_ne!(
        folded.verdict,
        Verdict::Valid,
        "a failed gate can never fold to VALID"
    );
}

fn checks_of(gate: &str) -> Vec<&'static str> {
    GATE_TABLE
        .iter()
        .filter(|s| s.gate == gate)
        .map(|s| s.check)
        .collect()
}

// ---------------------------------------------------------------------------
// the table and the clean session
// ---------------------------------------------------------------------------

#[test]
fn the_clean_session_passes_every_gate_and_every_row_is_evaluated() {
    let report = Fx::clean().run();
    let failing: Vec<&GateResult> = report.results.iter().filter(|r| !r.pass).collect();
    assert!(failing.is_empty(), "{failing:#?}");
    assert!(report.passed());
    assert!(report.failed_gates().is_empty());
    assert_eq!(report.results.len(), GATE_TABLE.len());
    for (spec, r) in GATE_TABLE.iter().zip(&report.results) {
        assert_eq!(
            (spec.gate, spec.check),
            (r.gate.as_str(), r.check.as_str()),
            "table order"
        );
    }
    assert_eq!(report.market_day_open, z("2026-09-29T08:00:00Z"));
    let mut outcome = Fx::clean().outcome;
    report.fold_into(&mut outcome);
    assert_eq!(outcome.verdict, Verdict::Valid);
}

#[test]
fn every_brief_gate_exists_and_every_check_is_unique() {
    let names = gate_names();
    for required in [
        "completeness-check",
        "writer-loss",
        "capacity-eviction",
        "ranking-truncation",
        "malformed-output",
        "duplicate-identity",
        "lifecycle-contract",
        "baseline-truncation",
        "deployed-before-open",
        "premarket-volume-init",
        "schema-fingerprint",
        "disposition-consistency",
        "designation",
    ] {
        assert!(
            names.contains(&required),
            "gate {required} missing from GATE_TABLE"
        );
    }
    let mut checks: Vec<&str> = GATE_TABLE.iter().map(|s| s.check).collect();
    checks.sort();
    let before = checks.len();
    checks.dedup();
    assert_eq!(
        before,
        checks.len(),
        "a check id appears twice in GATE_TABLE"
    );
    for spec in GATE_TABLE {
        assert!(
            !spec.why.trim().is_empty() && !spec.predicate.trim().is_empty(),
            "{spec:?}"
        );
    }
}

/// The doc table is the human-readable half of the contract; it must name
/// every check the code evaluates, so the two cannot drift.
#[test]
fn the_doc_table_lists_every_check() {
    let doc = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/qualification-v5-gates-2026-09-25.md"),
    )
    .expect("docs/qualification-v5-gates-2026-09-25.md must exist");
    for spec in GATE_TABLE {
        assert!(
            doc.contains(&format!("`{}`", spec.check)) && doc.contains(&format!("`{}`", spec.gate)),
            "doc table does not list {} / {}",
            spec.gate,
            spec.check
        );
    }
}

#[test]
fn the_contract_lists_every_gate_and_refuses_to_drop_one() {
    let spec = QualificationSpec::default();
    let names: Vec<String> = gate_names().into_iter().map(str::to_string).collect();
    assert_eq!(spec.qualification_gates, names);
    spec.validate().unwrap();
    let mut dropped = spec.clone();
    dropped
        .qualification_gates
        .retain(|g| g != "baseline-truncation");
    assert!(
        dropped.validate().is_err(),
        "a contract without a gate must be refused"
    );
    assert_ne!(
        dropped.sha256(),
        spec.sha256(),
        "and dropping one must move the hash"
    );
}

// ---------------------------------------------------------------------------
// no narrative override
// ---------------------------------------------------------------------------

/// The verdict is recomputed from the results; it cannot pass by omission,
/// by duplication, or by an empty report.
#[test]
fn the_verdict_cannot_pass_by_omission() {
    let clean = Fx::clean().run();
    let mut missing_row = clean.clone();
    missing_row
        .results
        .retain(|r| r.check != "rows.baselineTruncated");
    assert!(!missing_row.passed(), "a skipped check must not pass");
    let mut empty = clean.clone();
    empty.results.clear();
    assert!(!empty.passed());
    let mut doubled = clean.clone();
    let extra = doubled.results[3].clone();
    doubled.results.push(extra);
    assert!(
        !doubled.passed(),
        "a duplicated result is not a clean report"
    );
    let mut absent_but_pass = clean.clone();
    absent_but_pass.results[5].absent = true;
    assert!(!absent_but_pass.passed(), "absent evidence is never a pass");
}

/// No flag, environment variable or argument can force a PASS. Asserted on
/// the sources, because a bypass that exists is eventually used.
#[test]
fn nothing_can_force_a_pass() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let read = |p: &str| std::fs::read_to_string(root.join(p)).unwrap();
    let completeness = read("src/completeness.rs");
    let gates = completeness
        .split("Qualification v5 machine gates")
        .nth(1)
        .expect("the gate section")
        .split("#[cfg(test)]")
        .next()
        .unwrap();
    let pipeline = read("src/alpha/pipeline.rs");
    let cli = read("src/bin/alpha_qualify.rs");
    for (name, source) in [
        ("completeness gates", gates),
        ("pipeline", &pipeline[..]),
        ("alpha_qualify", &cli[..]),
    ] {
        let code: String = source
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in [
            "env::var",
            "var_os(",
            "--force",
            "--override",
            "--skip",
            "--allow",
            "--accept",
            "force_pass",
            "override",
        ] {
            assert!(
                !code.contains(forbidden),
                "{name} contains {forbidden:?}: the verdict must have no override"
            );
        }
    }
    // The CLI accepts exactly these options; a new one needs this test edited,
    // which is the point.
    let options: Vec<&str> = cli
        .lines()
        .filter_map(|l| l.trim().strip_prefix('"'))
        .filter(|l| l.starts_with("--"))
        .filter_map(|l| l.split('"').next())
        .collect();
    let mut sorted = options.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted,
        vec![
            "--expected-commit",
            "--expected-oi-config",
            "--expected-spec-sha256",
            "--help",
            "--output",
            "--print-spec",
            "--session",
            "--session-date"
        ],
    );
    // And the inputs struct has no switch: its fields are all evidence.
    let inputs = gates
        .split("pub struct GateInputs")
        .nth(1)
        .unwrap()
        .split('}')
        .next()
        .unwrap();
    assert!(
        !inputs.contains("bool"),
        "GateInputs must carry evidence, never a mode flag"
    );
}

// ---------------------------------------------------------------------------
// negative tests, one gate at a time
// ---------------------------------------------------------------------------

#[test]
fn completeness_check_rejects_a_non_valid_check_verdict() {
    for verdict in [Verdict::Invalid, Verdict::Indeterminate] {
        let mut fx = Fx::clean();
        fx.outcome.verdict = verdict;
        assert_fails(&fx, "completeness-check", "completeness.verdict", false);
    }
}

/// Every present-and-zero counter: a non-zero value fails, an absent one fails
/// closed, and a non-numeric one fails.
#[test]
fn every_zero_counter_gate_rejects_nonzero_absent_and_malformed() {
    let zero_checks: Vec<&GateSpec> = GATE_TABLE
        .iter()
        .filter(|s| s.predicate == "present and == 0")
        .collect();
    assert!(zero_checks.len() >= 20, "{}", zero_checks.len());
    for spec in zero_checks {
        let mut fx = Fx::clean();
        fx.set(spec.check, Some(json!(1)));
        assert_fails(&fx, spec.gate, spec.check, false);

        let mut fx = Fx::clean();
        fx.set(spec.check, None);
        assert_fails(&fx, spec.gate, spec.check, true);

        let mut fx = Fx::clean();
        fx.set(spec.check, Some(json!("0")));
        assert_fails(&fx, spec.gate, spec.check, false);
    }
}

#[test]
fn writer_loss_names_every_writer() {
    let checks = checks_of("writer-loss");
    for w in [
        "opportunityIntelligence",
        "measurement",
        "opportunityOutcomes",
    ] {
        for c in ["dropped", "writeErrors", "lossSpans"] {
            assert!(
                checks.contains(&format!("report.{w}.{c}").as_str()),
                "{w}.{c}"
            );
        }
    }
    // A writer that was not running at all is absent, not clean.
    let mut fx = Fx::clean();
    fx.set("report.opportunityOutcomes", Some(Value::Null));
    assert_fails(
        &fx,
        "writer-loss",
        "report.opportunityOutcomes.dropped",
        true,
    );
}

#[test]
fn capacity_eviction_covers_engine_outcomes_and_measurement() {
    let checks = checks_of("capacity-eviction");
    for c in [
        "report.opportunityEngine.capacityEvictions",
        "report.opportunityOutcomeEngine.capacityEvictions",
        "measurementPending.capacityEvictions",
    ] {
        assert!(checks.contains(&c), "{c}");
    }
    let mut fx = Fx::clean();
    fx.set("measurementPending", Some(Value::Null));
    assert_fails(
        &fx,
        "capacity-eviction",
        "measurementPending.capacityEvictions",
        true,
    );
}

#[test]
fn ranking_truncation_rejects_a_rank_bound_below_open_capacity() {
    let mut fx = Fx::clean();
    fx.set(
        "report.opportunityEngine.rankCohortCapacity",
        Some(json!(4_096)),
    );
    assert_fails(
        &fx,
        "ranking-truncation",
        "report.opportunityEngine.rankCohortCapacity",
        false,
    );

    let mut fx = Fx::clean();
    fx.set("report.opportunityEngine.rankCohortCapacity", None);
    assert_fails(
        &fx,
        "ranking-truncation",
        "report.opportunityEngine.rankCohortCapacity",
        true,
    );

    let mut fx = Fx::clean();
    fx.set("report.opportunityEngine.capacity", Some(json!(0)));
    assert_fails(
        &fx,
        "ranking-truncation",
        "report.opportunityEngine.rankCohortCapacity",
        false,
    );

    // Equal is the D6 design: the bound *is* open capacity.
    let mut fx = Fx::clean();
    fx.set(
        "report.opportunityEngine.rankCohortCapacity",
        Some(json!(16_375)),
    );
    assert!(fx.run().passed());
}

#[test]
fn malformed_output_rejects_malformed_truncated_missing_and_empty() {
    let mut fx = Fx::clean();
    fx.artifacts[0].malformed_records = 1;
    assert_fails(&fx, "malformed-output", "artifacts.malformedRecords", false);

    let mut fx = Fx::clean();
    fx.artifacts[1].truncated = true;
    assert_fails(&fx, "malformed-output", "artifacts.truncated", false);

    let mut fx = Fx::clean();
    fx.artifacts.remove(1);
    assert_fails(&fx, "malformed-output", "artifacts.required", false);

    let mut fx = Fx::clean();
    fx.artifacts[0].records = 0;
    assert_fails(&fx, "malformed-output", "artifacts.required", false);

    let mut fx = Fx::clean();
    fx.artifacts[0].present = false;
    assert_fails(&fx, "malformed-output", "artifacts.required", false);

    let mut fx = Fx::clean();
    fx.artifacts.clear();
    assert_fails(&fx, "malformed-output", "artifacts.malformedRecords", true);
}

#[test]
fn duplicate_identity_fails_on_any_refusal_and_when_uncounted() {
    let mut fx = Fx::clean();
    fx.set(
        "report.opportunityEngine.duplicateIdentityRefused",
        Some(json!(1)),
    );
    assert_fails(
        &fx,
        "duplicate-identity",
        "report.opportunityEngine.duplicateIdentityRefused",
        false,
    );
    let mut fx = Fx::clean();
    fx.set("report.opportunityEngine.duplicateIdentityRefused", None);
    assert_fails(
        &fx,
        "duplicate-identity",
        "report.opportunityEngine.duplicateIdentityRefused",
        true,
    );
}

#[test]
fn lifecycle_contract_rejects_the_old_lifecycle_and_an_unreported_one() {
    for path in [
        "report.opportunityEngine.lifecycle",
        "report.oiVersions.lifecycle",
    ] {
        let mut fx = Fx::clean();
        fx.set(
            path,
            Some(json!("opportunity-lifecycle-symbol-activity-v1")),
        );
        assert_fails(&fx, "lifecycle-contract", path, false);
        let mut fx = Fx::clean();
        fx.set(path, None);
        assert_fails(&fx, "lifecycle-contract", path, true);
    }
}

#[test]
fn baseline_truncation_rejects_truncated_rows_and_unproven_completeness() {
    let mut fx = Fx::clean();
    fx.baseline.as_mut().unwrap().truncated_rows = 1;
    assert_fails(&fx, "baseline-truncation", "rows.baselineTruncated", false);

    let mut fx = Fx::clean();
    fx.baseline.as_mut().unwrap().complete_rows = 0;
    assert_fails(&fx, "baseline-truncation", "rows.baselineComplete", true);

    let mut fx = Fx::clean();
    fx.baseline = None;
    assert_fails(&fx, "baseline-truncation", "rows.baselineTruncated", true);

    let mut fx = Fx::clean();
    fx.baseline.as_mut().unwrap().read_error = Some("disk".into());
    assert_fails(&fx, "baseline-truncation", "rows.baselineTruncated", true);
}

#[test]
fn deployed_before_open_rejects_a_start_at_or_after_the_open() {
    let open = z("2026-09-29T08:00:00Z");
    // Strictly before: a process started *at* 04:00:00 ET has not observed
    // 04:00:00 itself, so it cannot vouch for the whole day.
    for started in [
        open,
        open + chrono::Duration::minutes(1),
        et(2026, 9, 29, 8, 0, 0),
        et(2026, 9, 29, 11, 0, 0),
    ] {
        let mut fx = Fx::clean();
        fx.designation_mut().process_started_at = started;
        assert_fails(
            &fx,
            "deployed-before-open",
            "designation.processStartedAt",
            false,
        );
    }
    let mut fx = Fx::clean();
    fx.designation_mut().deploy_marker_at = et(2026, 9, 29, 9, 45, 0);
    assert_fails(
        &fx,
        "deployed-before-open",
        "designation.deployMarkerAt",
        false,
    );

    let mut fx = Fx::clean();
    fx.designation = DesignationEvidence::Missing("x".into());
    assert_fails(
        &fx,
        "deployed-before-open",
        "designation.processStartedAt",
        true,
    );
}

#[test]
fn premarket_volume_init_rejects_failures_stale_days_and_bad_timestamps() {
    let mut fx = Fx::clean();
    fx.set("premarketVolume.fetchFailures", Some(json!(3)));
    assert_fails(
        &fx,
        "premarket-volume-init",
        "premarketVolume.fetchFailures",
        false,
    );

    let mut fx = Fx::clean();
    fx.set("premarketVolume.marketDay", Some(json!("2026-09-28")));
    assert_fails(
        &fx,
        "premarket-volume-init",
        "premarketVolume.marketDay",
        false,
    );

    // 03:59 ET on the designated date is still the previous market day.
    let mut fx = Fx::clean();
    fx.set(
        "premarketVolume.initializedAt",
        Some(json!("2026-09-29T07:59:00Z")),
    );
    assert_fails(
        &fx,
        "premarket-volume-init",
        "premarketVolume.initializedAt",
        false,
    );

    let mut fx = Fx::clean();
    fx.set("premarketVolume.initializedAt", Some(json!("09:30 ET")));
    assert_fails(
        &fx,
        "premarket-volume-init",
        "premarketVolume.initializedAt",
        false,
    );

    for path in ["premarketVolume.marketDay", "premarketVolume.initializedAt"] {
        let mut fx = Fx::clean();
        fx.set(path, None);
        assert_fails(&fx, "premarket-volume-init", path, true);
    }
}

/// `premarketVolume` is read where the route emits it -- beside `retention`,
/// not inside `report` -- and only there. A block anywhere else (for example
/// nested under `report` by a hand-edited document) is not the route's block
/// and reads as absent; so does a `null` block (no universe scan ran).
#[test]
fn premarket_volume_is_read_only_where_the_route_emits_it() {
    let mut fx = Fx::clean();
    assert!(fx.run().passed());
    let block = fx.doc.as_ref().unwrap()["premarketVolume"].clone();
    fx.set("premarketVolume", None);
    fx.set("report.premarketVolume", Some(block));
    assert_fails(
        &fx,
        "premarket-volume-init",
        "premarketVolume.fetchFailures",
        true,
    );

    let mut fx = Fx::clean();
    fx.set("premarketVolume", Some(Value::Null));
    assert_fails(
        &fx,
        "premarket-volume-init",
        "premarketVolume.fetchFailures",
        true,
    );
}

#[test]
fn schema_fingerprint_rejects_every_pinned_identity_mismatch() {
    for check in checks_of("schema-fingerprint") {
        let original = result(&Fx::clean().run(), check).observed.clone();
        let wrong = match &original {
            Value::Number(n) => json!(n.as_u64().unwrap() + 1),
            Value::String(s) => json!(format!("{s}-other")),
            other => panic!("{check}: unexpected {other}"),
        };
        let mut fx = Fx::clean();
        fx.set(check, Some(wrong));
        assert_fails(&fx, "schema-fingerprint", check, false);

        let mut fx = Fx::clean();
        fx.set(check, None);
        assert_fails(&fx, "schema-fingerprint", check, true);
    }
    // No expected commit at all (none requested, no designation) is absent.
    let mut fx = Fx::clean();
    fx.pins.commit = None;
    fx.designation = DesignationEvidence::Missing("x".into());
    assert_fails(&fx, "schema-fingerprint", "report.commit", true);
    // ...but the designation's commit is the fallback when none is requested.
    let mut fx = Fx::clean();
    fx.pins.commit = None;
    assert!(result(&fx.run(), "report.commit").pass);
}

#[test]
fn disposition_consistency_rejects_every_reconciliation_failure() {
    const C: &str = "report.opportunityOutcomeEngine.dispositionCounts";
    // Sum != anchorsSettled.
    let mut fx = Fx::clean();
    fx.set(
        "report.opportunityOutcomeEngine.anchorsSettled",
        Some(json!(4_000_001)),
    );
    assert_fails(&fx, "disposition-consistency", C, false);
    // A non-numeric token.
    let mut fx = Fx::clean();
    fx.set(&format!("{C}.stillOpen"), Some(json!("3000000")));
    assert_fails(&fx, "disposition-consistency", C, false);
    // Absent counts.
    let mut fx = Fx::clean();
    fx.set(C, None);
    assert_fails(&fx, "disposition-consistency", C, true);

    const M: &str = "report.opportunityOutcomeEngine.closureAnchorsMarked";
    // closed = 1,000,000 settled non-still_open; outstanding = 40.
    let mut fx = Fx::clean();
    fx.set(M, Some(json!(1_000_041)));
    assert_fails(&fx, "disposition-consistency", M, false);
    let mut fx = Fx::clean();
    fx.set(M, Some(json!(999_999)));
    assert_fails(&fx, "disposition-consistency", M, false);
    // The brief's plain form -- marked > settled -- is the outstanding == 0 case.
    let mut fx = Fx::clean();
    fx.set(
        "report.opportunityOutcomeEngine.outstanding",
        Some(json!(0)),
    );
    fx.set(M, Some(json!(1_000_001)));
    assert_fails(&fx, "disposition-consistency", M, false);
    // Both bounds are inclusive.
    for ok in [1_000_000, 1_000_040] {
        let mut fx = Fx::clean();
        fx.set(M, Some(json!(ok)));
        assert!(fx.run().passed(), "{ok}");
    }

    // v1 tokens present.
    let path = format!("{C}.inactivity");
    let mut fx = Fx::clean();
    fx.set(&path, Some(json!(5)));
    fx.set(
        "report.opportunityOutcomeEngine.anchorsSettled",
        Some(json!(4_000_005)),
    );
    assert_fails(&fx, "disposition-consistency", &path, false);
    let mut fx = Fx::clean();
    fx.set(
        "report.opportunityEngine.closedByReason.inactivity",
        Some(json!(2)),
    );
    assert_fails(
        &fx,
        "disposition-consistency",
        "report.opportunityEngine.closedByReason.inactivity",
        false,
    );
    // ...but a zero legacy field (kept for deserialisation) is fine.
    let mut fx = Fx::clean();
    fx.set(&path, Some(json!(0)));
    assert!(fx.run().passed());

    // move-v1 tokens absent.
    for token in ["setupInactivity", "invalidated"] {
        let p = format!("{C}.{token}");
        let mut fx = Fx::clean();
        fx.set(&p, None);
        assert_fails(&fx, "disposition-consistency", &p, true);
    }
}

#[test]
fn designation_rejects_every_defect() {
    let mut fx = Fx::clean();
    fx.designation = DesignationEvidence::Missing("research/.retention/designations/x.json".into());
    assert_fails(&fx, "designation", "designation.record", true);

    let mut fx = Fx::clean();
    fx.designation = DesignationEvidence::Malformed("expected value".into());
    assert_fails(&fx, "designation", "designation.record", false);

    type Mutation = fn(&mut DesignationRecord);
    let cases: [(&str, Mutation); 9] = [
        ("designation.record", |r| {
            r.market_day = "2026-09-30".parse().unwrap()
        }),
        ("designation.record", |r| r.preflight = "FAIL".into()),
        ("designation.record", |r| r.schema_version = 2),
        ("designation.record", |r| r.designated_by = " ".into()),
        ("designation.designatedAt", |r| {
            r.designated_at = "2026-09-29T08:00:00Z".parse().unwrap()
        }),
        ("designation.specSha256", |r| r.spec_sha256 = "0".repeat(64)),
        ("designation.specVersion", |r| {
            r.spec_version = "alpha-qualification-v3".into()
        }),
        ("designation.commit", |r| r.commit = "f".repeat(40)),
        ("designation.oiConfigFingerprint", |r| {
            r.oi_config_fingerprint = "oi-cfg-b4f21c8b311a1b99".into()
        }),
    ];
    for (check, mutate) in cases {
        let mut fx = Fx::clean();
        mutate(fx.designation_mut());
        assert_fails(&fx, "designation", check, false);
    }

    let mut fx = Fx::clean();
    fx.protection.research = Some("research: no protection record".into());
    assert_fails(&fx, "designation", "protection.research", false);
    let mut fx = Fx::clean();
    fx.protection.discovery = Some("discovery-audit: protected as Forensic, not designated".into());
    assert_fails(&fx, "designation", "protection.discovery", false);
}

/// The fields P3 added, removed from the passing envelope to model the health
/// document a build **before** P3 wrote. Nothing else differs.
const P3_FIELDS: [&str; 8] = [
    "report.opportunityEngine.duplicateIdentityRefused",
    "report.opportunityEngine.lifecycle",
    "report.opportunityEngine.marketDayId",
    "report.oiVersions.lifecycle",
    "report.opportunityOutcomeEngine.dispositionCounts.setupInactivity",
    "report.opportunityOutcomeEngine.dispositionCounts.invalidated",
    "report.opportunityEngine.closedByReason.setupInactivity",
    "premarketVolume",
];

/// A health document an **older build** wrote -- raw JSON without the fields
/// P3 added -- cannot pass: every such field reads as absent, never as zero.
///
/// Modelled as JSON, deliberately. This build's typed structs always
/// serialize these fields (and `#[serde(default)]` lets them *parse* an old
/// document as zero), so building the old document through them would test a
/// document no older build ever wrote. The gates read the raw capture, and an
/// older build's raw capture simply lacks the keys.
#[test]
fn a_build_without_the_new_counters_fails_closed() {
    let mut fx = Fx::clean();
    for path in P3_FIELDS {
        fx.set(path, None);
    }
    let out = fx.run();
    assert!(!out.passed());
    for check in [
        "report.opportunityEngine.duplicateIdentityRefused",
        "report.opportunityEngine.lifecycle",
        "report.oiVersions.lifecycle",
        "premarketVolume.fetchFailures",
        "premarketVolume.marketDay",
        "premarketVolume.initializedAt",
        "report.opportunityOutcomeEngine.dispositionCounts.setupInactivity",
        "report.opportunityOutcomeEngine.dispositionCounts.invalidated",
    ] {
        let r = result(&out, check);
        assert!(!r.pass && r.absent, "{check} must fail closed: {r:?}");
    }
    // A bare report (as the qualifier accepts) can never carry the
    // envelope-level blocks, so it cannot pass either.
    let mut fx = Fx::clean();
    fx.doc = Some(fx.doc.as_ref().unwrap()["report"].clone());
    let out = fx.run();
    assert!(!out.passed());
    for check in [
        "premarketVolume.fetchFailures",
        "measurementPending.capacityEvictions",
    ] {
        let r = result(&out, check);
        assert!(
            !r.pass && r.absent,
            "{check} must fail closed on a bare report: {r:?}"
        );
    }
    // No health document at all: every health-derived check is absent.
    let mut fx = Fx::clean();
    fx.doc = None;
    let out = fx.run();
    assert!(!out.passed());
    assert!(result(&out, "report.opportunityIntelligence.dropped").absent);
}

/// Older health documents still parse into the typed counters (a replay or
/// report reader must not choke on them); the gates above never trust the
/// resulting zeros.
#[test]
fn older_disposition_and_close_counters_still_parse() {
    let counts: crate::opportunity_outcome::DispositionCounts =
        serde_json::from_value(json!({"stillOpen": 3, "inactivity": 1})).unwrap();
    assert_eq!(
        (
            counts.still_open,
            counts.setup_inactivity,
            counts.invalidated
        ),
        (3, 0, 0)
    );
    let closed: crate::opportunity::ClosedByReason =
        serde_json::from_value(json!({"inactivity": 2})).unwrap();
    assert_eq!(
        (
            closed.inactivity,
            closed.setup_inactivity,
            closed.session_boundary
        ),
        (2, 0, 0)
    );
}

#[test]
fn folding_keeps_the_three_valued_verdict() {
    // Absent-only failures: INDETERMINATE.
    let mut fx = Fx::clean();
    fx.set("report.opportunityEngine.duplicateIdentityRefused", None);
    let mut outcome = fx.outcome.clone();
    fx.run().fold_into(&mut outcome);
    assert_eq!(outcome.verdict, Verdict::Indeterminate);
    assert!(outcome
        .missing
        .iter()
        .any(|m| m.contains("duplicateIdentityRefused")));

    // Any positive violation: INVALID, and it dominates.
    fx.set("report.opportunityEngine.capacityEvictions", Some(json!(1)));
    let mut outcome = fx.outcome.clone();
    fx.run().fold_into(&mut outcome);
    assert_eq!(outcome.verdict, Verdict::Invalid);
    assert!(outcome
        .blocking
        .iter()
        .any(|m| m.contains("capacityEvictions")));
}

#[test]
fn the_gate_report_round_trips_and_is_machine_readable() {
    let report = Fx::clean().run();
    let text = serde_json::to_string(&report).unwrap();
    let back: GateReport = serde_json::from_str(&text).unwrap();
    assert_eq!(back, report);
    let v: Value = serde_json::from_str(&text).unwrap();
    let first = &v["results"][0];
    for key in ["gate", "check", "pass", "absent", "observed", "expected"] {
        assert!(first.get(key).is_some(), "{key}");
    }
}

// ---------------------------------------------------------------------------
// brief §17: deploy-day baselineTruncated
// ---------------------------------------------------------------------------
//
// The rule (D3, `context.rs`): `baselineTruncated = observationStartedAt >
// market_day_open(marketDay)`, where `observationStartedAt` is the **data
// timestamp of the first event the cache ever folded** -- not the process
// start. So:
//
// * first event strictly before 04:00:00 ET of the market day -> complete;
// * first event **exactly at** 04:00:00 ET -> complete: `>` is strict, and a
//   cache whose first event is the open itself has seen the whole day;
// * anything after -> truncated, down to one second;
// * a process started before 04:00 whose feed is silent until after 04:00 is
//   **truncated** too. The designation preflight therefore requires the
//   capture process to be up while the previous market day's feed is still
//   live (before 20:00 ET), not merely before 04:00.

fn bar(symbol: &str, t: DateTime<Utc>, close: f64) -> ScanEvent {
    ScanEvent::BarUpdate {
        is_final: true,
        symbol: symbol.into(),
        timestamp: t,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1_000,
        interval_secs: 60,
    }
}

/// A cache whose first event is at `first`.
fn cache_starting_at(first: DateTime<Utc>) -> FeatureCache {
    let mut cache = FeatureCache::new();
    cache.observe(&bar("FIRST", first, 1.0));
    cache
}

/// One OI-row-shaped line: the real `SignalContext` under `features`.
fn row(cache: &mut FeatureCache, symbol: &str, at: DateTime<Utc>, price: f64) -> String {
    cache.observe(&bar(symbol, at, price));
    let features = cache.snapshot(symbol, Strategy::IgnitionDetector, at, at, price);
    json!({
        "schemaVersion": 3,
        "timestamp": at,
        "opportunityId": format!("{symbol}:{}:{}", at.date_naive(), at.timestamp_millis() % 86_400_000),
        "symbol": symbol,
        "features": features,
    })
    .to_string()
}

fn truncated(first: DateTime<Utc>, at: DateTime<Utc>) -> Option<bool> {
    let mut cache = cache_starting_at(first);
    cache.observe(&bar("AAA", at, 2.0));
    cache
        .snapshot("AAA", Strategy::IgnitionDetector, at, at, 2.0)
        .pre_detection
        .and_then(|p| p.baseline_truncated)
}

fn scan(lines: &[String]) -> BaselineEvidence {
    let text = lines.join("\n") + "\n";
    scan_baseline_reader(std::io::Cursor::new(text.into_bytes()), day())
}

#[test]
fn deploy_day_boundary_in_edt() {
    let row_at = et(2026, 9, 29, 9, 45, 0);
    let cases = [
        (
            "process up the previous afternoon",
            et(2026, 9, 28, 15, 0, 0),
            false,
        ),
        ("first event 03:30 ET", et(2026, 9, 29, 3, 30, 0), false),
        ("first event 03:59:59 ET", et(2026, 9, 29, 3, 59, 59), false),
        (
            "first event exactly 04:00:00 ET",
            et(2026, 9, 29, 4, 0, 0),
            false,
        ),
        ("first event 04:00:01 ET", et(2026, 9, 29, 4, 0, 1), true),
        ("first event 04:01 ET", et(2026, 9, 29, 4, 1, 0), true),
        ("first event 08:00 ET", et(2026, 9, 29, 8, 0, 0), true),
    ];
    for (what, first, expected) in cases {
        assert_eq!(truncated(first, row_at), Some(expected), "{what}");
    }
    assert_eq!(
        et(2026, 9, 29, 4, 0, 0),
        z("2026-09-29T08:00:00Z"),
        "EDT open is 08:00Z"
    );
}

#[test]
fn deploy_day_boundary_in_est() {
    // 2026-01-13, EST: the market day opens at 09:00Z.
    let row_at = et(2026, 1, 13, 10, 0, 0);
    assert_eq!(truncated(z("2026-01-13T08:59:59Z"), row_at), Some(false));
    assert_eq!(
        truncated(z("2026-01-13T09:00:00Z"), row_at),
        Some(false),
        "exactly 04:00 EST"
    );
    assert_eq!(truncated(z("2026-01-13T09:00:01Z"), row_at), Some(true));
    // 08:00Z is 03:00 EST, before the open -- the EDT rule applied in winter
    // would wrongly call this truncated.
    assert_eq!(truncated(z("2026-01-13T08:00:00Z"), row_at), Some(false));
}

#[test]
fn a_process_started_before_0400_on_a_silent_feed_is_still_truncated() {
    // Nothing exists to observe between 20:00 and 04:00 ET, so a process
    // started at 03:00 whose first event is the 04:00:30 funnel signal began
    // observing *after* the open. The flag follows observation, not uptime.
    assert_eq!(
        truncated(et(2026, 9, 29, 4, 0, 30), et(2026, 9, 29, 9, 45, 0)),
        Some(true)
    );
}

#[test]
fn an_intraday_restart_truncates_only_the_rows_after_it() {
    let mut before = cache_starting_at(et(2026, 9, 28, 15, 0, 0));
    let mut lines = vec![
        row(&mut before, "AAA", et(2026, 9, 29, 9, 45, 0), 2.0),
        row(&mut before, "BBB", et(2026, 9, 29, 10, 15, 0), 3.0),
    ];
    // Restart at 11:00 ET: a brand-new cache.
    let mut after = FeatureCache::new();
    lines.push(row(&mut after, "AAA", et(2026, 9, 29, 11, 0, 0), 2.2));
    lines.push(row(&mut after, "CCC", et(2026, 9, 29, 11, 30, 0), 4.0));
    let evidence = scan(&lines);
    assert_eq!(evidence.rows_scanned, 4);
    assert_eq!(evidence.truncated_rows, 2);
    assert_eq!(evidence.complete_rows, 2);
    assert!(evidence
        .first_truncated
        .as_deref()
        .unwrap()
        .starts_with("AAA:"));
}

#[test]
fn a_feed_reconnect_without_a_restart_does_not_truncate() {
    let mut cache = cache_starting_at(et(2026, 9, 28, 15, 0, 0));
    let mut lines = vec![row(&mut cache, "AAA", et(2026, 9, 29, 9, 0, 0), 2.0)];
    // 90 minutes of silence (a stream error, the idle timeout), then the same
    // process resumes: the cache is untouched.
    lines.push(row(&mut cache, "AAA", et(2026, 9, 29, 10, 30, 0), 2.4));
    lines.push(row(&mut cache, "DDD", et(2026, 9, 29, 10, 31, 0), 7.0));
    let evidence = scan(&lines);
    assert_eq!((evidence.truncated_rows, evidence.complete_rows), (0, 3));
}

#[test]
fn the_previous_market_days_truncated_tail_does_not_fail_the_designated_day() {
    // The UTC file for 09-29 opens with 00:00-08:00Z = 20:00-04:00 ET of market
    // day 09-28. A process restarted at 21:00 ET on 09-28 writes truncated rows
    // for 09-28 there -- and is complete for 09-29.
    let mut cache = cache_starting_at(et(2026, 9, 28, 21, 0, 0));
    let tail = row(&mut cache, "AAA", z("2026-09-29T02:00:00Z"), 2.0);
    assert!(tail.contains("\"baselineTruncated\":true"), "{tail}");
    let today = row(&mut cache, "AAA", et(2026, 9, 29, 9, 45, 0), 2.1);
    let evidence = scan(&[tail, today]);
    assert_eq!((evidence.truncated_rows, evidence.complete_rows), (0, 1));
}

#[test]
fn an_unreadable_line_claiming_truncation_counts_as_truncated() {
    let evidence = scan(&["{\"baselineTruncated\":true, oops".to_string()]);
    assert_eq!(evidence.truncated_rows, 1);
}

/// End to end: a session deployed at 04:01 ET is rejected by the gates,
/// through both the rows and the designation record.
#[test]
fn the_gates_reject_a_deploy_day_session() {
    let mut cache = cache_starting_at(et(2026, 9, 29, 4, 1, 0));
    let lines: Vec<String> = (0..5)
        .map(|i| {
            row(
                &mut cache,
                &format!("S{i}"),
                et(2026, 9, 29, 9, 45 + i, 0),
                2.0,
            )
        })
        .collect();
    let dir = std::env::temp_dir().join(format!("gates-deploy-day-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("opportunity-intelligence-{DAY}.ndjson"));
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    let evidence = scan_baseline(&path, day());
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(evidence.truncated_rows, 5);

    let mut fx = Fx::clean();
    fx.baseline = Some(evidence);
    fx.designation_mut().process_started_at = et(2026, 9, 29, 4, 1, 0);
    let report = fx.run();
    assert!(!report.passed());
    assert_eq!(
        report.failed_gates(),
        vec!["baseline-truncation", "deployed-before-open"],
        "exactly the deploy-day gates"
    );

    // The same session started the evening before passes both.
    let mut cache = cache_starting_at(et(2026, 9, 28, 17, 0, 0));
    let lines: Vec<String> = (0..5)
        .map(|i| {
            row(
                &mut cache,
                &format!("S{i}"),
                et(2026, 9, 29, 9, 45 + i, 0),
                2.0,
            )
        })
        .collect();
    let mut fx = Fx::clean();
    fx.baseline = Some(scan(&lines));
    assert!(fx.run().passed());
}

#[test]
fn a_missing_oi_file_is_absent_not_clean() {
    let evidence = scan_baseline(std::path::Path::new("/definitely/not/here.ndjson"), day());
    assert!(evidence.read_error.is_some());
    let mut fx = Fx::clean();
    fx.baseline = Some(evidence);
    assert_fails(&fx, "baseline-truncation", "rows.baselineTruncated", true);
}

// ---------------------------------------------------------------------------
// designation and protection loading
// ---------------------------------------------------------------------------

#[test]
fn designation_and_protection_load_from_the_exported_session() {
    let root = std::env::temp_dir().join(format!("gates-designation-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    assert!(matches!(
        load_designation(&root, day()),
        DesignationEvidence::Missing(_)
    ));
    let p = ProtectionEvidence::load(&root, day());
    assert!(
        p.research.is_some() && p.discovery.is_some(),
        "no registry is not designated"
    );

    let path = designation_path(&root, day());
    assert!(path.ends_with(format!("research/.retention/designations/{DAY}.json")));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec(&designation()).unwrap()).unwrap();
    assert_eq!(
        load_designation(&root, day()),
        DesignationEvidence::Present(designation())
    );
    std::fs::write(&path, b"{\"schemaVersion\":1}").unwrap();
    assert!(matches!(
        load_designation(&root, day()),
        DesignationEvidence::Malformed(_)
    ));

    for (dir, class) in [("research", "designated"), ("discovery-audit", "forensic")] {
        let protected = root.join(dir).join(".retention/protected");
        std::fs::create_dir_all(&protected).unwrap();
        std::fs::write(
            protected.join(format!("{DAY}.json")),
            json!({"schemaVersion": 1, "date": DAY, "class": class, "reason": "P3 test",
                   "protectedBy": "test", "protectedAt": "2026-09-29T07:00:00Z"})
            .to_string(),
        )
        .unwrap();
    }
    let p = ProtectionEvidence::load(&root, day());
    assert_eq!(p.research, None);
    assert!(p.discovery.as_deref().unwrap().contains("Forensic"));
    let _ = std::fs::remove_dir_all(&root);
}
