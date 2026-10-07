/// The preflight script and the health route must not drift apart (§44).
///
/// `ops/qualify/session.sh preflight` reads a fixed set of JSON paths out of
/// `/research/completeness` before every prospective session, and each one is
/// compared against zero. That comparison has a dangerous failure mode: if a
/// field is renamed, `jget` returns an empty string, the script's
/// `${value:-0}` default turns it into `0`, and the check *passes* while
/// reading nothing at all. A preflight that cannot fail is worse than no
/// preflight, because it is trusted.
///
/// So the paths are extracted from the script itself and resolved against the
/// real envelope. Rename a field in `http.rs` or `completeness.rs` and this
/// test fails, rather than the September-16 failure mode recurring one level
/// up: an instrument that reports health it is not actually measuring.
mod runbook_contract {
    use backtest_metrics::completeness::{
        CompletenessReport, DiscoveryCapture, EngineCapture, WriterCapture,
    };
    use serde_json::Value;

    /// Every path the script reads, scraped from the script rather than retyped.
    ///
    /// Retyping them here would test this file against itself.
    fn paths_referenced_by_the_script() -> Vec<String> {
        let script = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../ops/qualify/session.sh"),
        )
        .expect("ops/qualify/session.sh must exist -- the runbook depends on it");

        let mut found = Vec::new();
        for raw in script.lines() {
            let line = raw.trim();
            if line.starts_with('#') {
                continue;
            }
            // Two shapes appear in the script: `jget some.path` and bare
            // "some.path" entries in the counter and capacity lists.
            if let Some(rest) = line.split("| jget ").nth(1) {
                let candidate = rest.trim().trim_matches('"').trim_matches(')');
                if is_a_document_path(candidate) {
                    found.push(candidate.to_string());
                }
            }
            for token in line.split_whitespace() {
                let candidate = token.trim_matches('"').trim_end_matches('\\');
                if is_a_document_path(candidate) {
                    found.push(candidate.to_string());
                }
            }
        }
        found.sort();
        found.dedup();
        assert!(
            found.len() >= 12,
            "expected the script to read at least a dozen health fields, scraped {found:?}"
        );
        found
    }

    /// A dotted path of JSON-ish identifiers, and nothing else.
    fn is_a_document_path(token: &str) -> bool {
        let mut parts = token.split('.');
        let head = parts.next().unwrap_or_default();
        if !matches!(head, "report" | "measurementPending" | "retention") {
            return false;
        }
        token.split('.').count() >= 2
            && token
                .split('.')
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric()))
    }

    fn resolve<'a>(doc: &'a Value, path: &str) -> Option<&'a Value> {
        let mut node = doc;
        for key in path.split('.') {
            node = node.get(key)?;
        }
        Some(node)
    }

    /// A fully-populated envelope. Every optional branch is present, because the
    /// question here is whether the *names* exist, not whether a given capture
    /// happened to be running.
    fn envelope() -> Value {
        let config = backtest_metrics::opportunity::OiConfig::default();
        let versions = crate::research_health::ReportVersions::for_config(&config);
        let report = CompletenessReport {
            report_schema_version: crate::research_health::REPORT_SCHEMA_VERSION,
            generated_at: chrono::Utc::now(),
            commit: Some("af986b84cd3745f077b48fef912610990b7db725".into()),
            oi_config_fingerprint: Some(config.fingerprint()),
            oi_versions: Some(versions.oi.clone()),
            outcome_measurement_version: Some(versions.outcome_measurement_version.clone()),
            episode_schema: Some(versions.episode_schema),
            signal_context_schema: Some(versions.signal_context_schema),
            opportunity_intelligence: Some(WriterCapture::default()),
            measurement: Some(WriterCapture::default()),
            discovery: Some(DiscoveryCapture::default()),
            opportunity_engine: Some(EngineCapture::default()),
            opportunity_outcomes: None,
            opportunity_outcome_engine: None,
        };
        let settlement = serde_json::json!({
            "pending": 0,
            "pendingPeak": 40,
            "pendingCapacity": 38_400,
            "capacityEvictions": 0,
            "openEpisodes": 0,
        });
        let retention = serde_json::to_value(crate::research_retention::RetentionSnapshot {
            ceiling_bytes: 64 << 30,
            dir_bytes: 0,
            sessions_present: 1,
            sessions_deleted: 0,
            bytes_reclaimed: 0,
            deleted_without_export: 0,
            retention_pending: false,
            last_sweep: None,
            last_deleted: String::new(),
            blocked_by_protection: false,
            bytes_over_ceiling: 0,
            protected_sessions: 0,
            protected_bytes: 0,
            deleted_protected_with_receipt: 0,
            protected_without_receipt: Vec::new(),
            protected_retained: Vec::new(),
            registry_errors: Vec::new(),
        })
        .unwrap();

        crate::http::completeness_envelope(
            &report,
            Some(settlement),
            serde_json::from_value(retention).unwrap(),
            Some(market_data::discovery_audit::DiscoveryRetention::default()),
            Some(market_data::PremarketVolumeHealth::default()),
        )
    }

    /// D7 (2026-09-25): the premarket-volume health block is present, additive,
    /// and its failure counters are plain numbers a gate can compare to zero.
    #[test]
    fn premarket_volume_health_is_reported_with_numeric_failure_counters() {
        let doc = envelope();
        for path in [
            "premarketVolume.marketDay",
            "premarketVolume.initializedAt",
            "premarketVolume.lastSuccessfulFetchAt",
            "premarketVolume.bySource.snapshotDailyBarCurrent",
            "premarketVolume.bySource.minuteBarsSinceOpen",
            "premarketVolume.bySource.unknown",
            "premarketVolume.survivorsDeferred",
            "premarketVolume.requestsThisMinute",
        ] {
            assert!(resolve(&doc, path).is_some(), "missing {path}");
        }
        for path in ["premarketVolume.fetchFailures", "premarketVolume.initFailures"] {
            assert!(resolve(&doc, path).is_some_and(|v| v.is_u64()), "{path} must be a number");
        }
    }

    #[test]
    fn every_field_the_preflight_reads_exists_in_the_health_route() {
        let doc = envelope();
        let mut missing = Vec::new();
        for path in paths_referenced_by_the_script() {
            if resolve(&doc, &path).is_none() {
                missing.push(path);
            }
        }
        assert!(
            missing.is_empty(),
            "ops/qualify/session.sh reads {missing:?}, which /research/completeness does not \
             emit. The script's `${{value:-0}}` default would read each of these as zero and \
             the preflight check would silently pass. Fix the script or restore the field."
        );
    }

    /// The loss and capacity counters must be *numbers*, not strings or objects.
    ///
    /// The script compares them to the literal `0`. A field that serialized as
    /// `"0"` or `{"count":0}` would compare unequal and fail every preflight; one
    /// that serialized as a bool would compare equal for the wrong reason.
    #[test]
    fn every_counter_the_preflight_compares_to_zero_is_numeric() {
        let doc = envelope();
        for path in paths_referenced_by_the_script() {
            let Some(value) = resolve(&doc, &path) else { continue };
            let is_counter = path.contains("dropped")
                || path.contains("Dropped")
                || path.contains("Errors")
                || path.contains("lossSpans")
                || path.contains("queueLost")
                || path.contains("Evictions")
                || path.contains("Truncations")
                || path.ends_with("pending");
            if is_counter {
                assert!(
                    value.is_number(),
                    "{path} is compared against 0 by the preflight but serializes as {value}"
                );
            }
        }
    }

    /// The three frozen identities the script hard-codes must match the code.
    ///
    /// The script cannot import Rust constants, so it carries literals. This is
    /// the test that keeps those literals honest -- without it, a contract change
    /// would leave the preflight verifying yesterday's identity.
    #[test]
    fn the_scripts_frozen_identities_match_the_frozen_code() {
        let script = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ops/qualify/session.sh"),
        )
        .unwrap();

        let fingerprint = backtest_metrics::opportunity::OiConfig::default().fingerprint();
        assert!(
            script.contains(&format!("EXPECTED_OI_CONFIG=\"{fingerprint}\"")),
            "the runbook pins an OI fingerprint that is no longer the deployed one ({fingerprint})"
        );

        // D4 moved this pin (opportunity-outcome-v1 -> v2). The preflight
        // compares it against the live `report.outcomeMeasurementVersion`, so
        // a build writing v1 rows -- whose dispositions are all unknown --
        // cannot be mistaken for one writing v2.
        let outcome = backtest_metrics::opportunity_outcome::OPPORTUNITY_OUTCOME_VERSION;
        assert!(
            script.contains(&format!("EXPECTED_OUTCOME_VERSION=\"{outcome}\"")),
            "the runbook pins an outcome measurement version other than {outcome}"
        );

        let spec = backtest_metrics::alpha::spec::QualificationSpec::default();
        assert!(
            script.contains(&format!("EXPECTED_SPEC_SHA=\"{}\"", spec.sha256())),
            "the runbook pins contract hash other than {} ({})",
            spec.sha256(),
            spec.version
        );
    }

    /// The live report carries every version the qualification contract pins,
    /// not only the fingerprint -- read from the running build's own constants,
    /// and from `OiVersions` whole, so a version field added there later
    /// reaches the route without an edit here.
    #[test]
    fn the_live_report_carries_every_pinned_version() {
        let health = crate::research_health::ResearchHealth::default();
        let before = health.report();
        assert_eq!(before.oi_versions, None, "unset stays absent, never a default");
        let config = backtest_metrics::opportunity::OiConfig::default();
        health.set_versions(crate::research_health::ReportVersions::for_config(&config));
        let report = health.report();
        assert_eq!(report.report_schema_version, crate::research_health::REPORT_SCHEMA_VERSION);
        assert_eq!(report.oi_versions, Some(config.versions()));
        assert_eq!(
            report.outcome_measurement_version.as_deref(),
            Some(backtest_metrics::opportunity_outcome::OPPORTUNITY_OUTCOME_VERSION)
        );
        assert_eq!(report.episode_schema, Some(backtest_metrics::episode::EPISODE_SCHEMA_VERSION));
        assert_eq!(
            report.signal_context_schema,
            Some(backtest_metrics::context::SIGNAL_CONTEXT_SCHEMA_VERSION)
        );
        let spec = backtest_metrics::alpha::spec::QualificationSpec::default();
        let v = report.oi_versions.unwrap();
        assert_eq!(v.opportunity_schema, spec.expected_opportunity_schema);
        assert_eq!(v.feature_schema, spec.expected_feature_schema);
        assert_eq!(Some(v.config_fingerprint), spec.expected_oi_config_fingerprint);
        assert_eq!(
            report.outcome_measurement_version.unwrap(),
            spec.expected_outcome_measurement_version
        );
    }

    /// A missing external tool must be reported, never abort the preflight.
    ///
    /// `session.sh` runs under `set -euo pipefail`, so an unguarded call to a
    /// binary that is not installed exits 127 and kills the script *mid-run* --
    /// after it has already printed a column of `ok` lines and before it prints
    /// any verdict. An operator skimming that output reads an abort as a pass,
    /// which is precisely the failure the preflight exists to prevent.
    ///
    /// This happened for real: `alpha_qualify` lives on the analysis host, not
    /// the capture host, so the first live preflight aborted at exit 127 with
    /// nineteen `ok` lines above it and no verdict below.
    #[test]
    fn a_missing_external_tool_is_reported_rather_than_aborting_the_preflight() {
        let script = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ops/qualify/session.sh"),
        )
        .unwrap();

        assert!(
            script.contains("set -euo pipefail"),
            "the guard below only matters under `set -e`; if that changed, revisit this test"
        );

        // Every binary the script does not itself provide. `need` aborts with a
        // named message (that is a deliberate hard requirement); `command -v`
        // lets the caller decide. Either is fine -- a bare call is not.
        for tool in ["alpha_qualify", "curl", "python3", "rsync", "sha256sum", "docker"] {
            let guarded = script.contains(&format!("need {tool}"))
                || script.contains(&format!("command -v {tool}"));
            assert!(
                guarded,
                "ops/qualify/session.sh calls {tool:?} without `need` or `command -v`. Under \
                 `set -euo pipefail` a missing binary exits 127 and aborts the preflight after \
                 its `ok` lines and before its verdict, which reads as a pass."
            );
        }

        // And the contract check specifically must not be the thing that kills
        // the run, since it is the last check and therefore the easiest to
        // mistake for a completed one.
        let contract = script
            .split("--- the contract")
            .nth(1)
            .expect("the preflight must still have a contract check");
        assert!(
            contract.contains("command -v alpha_qualify"),
            "the contract check must tolerate alpha_qualify being absent on a capture host"
        );
    }

    /// The automation must contain no path that changes production (§45).
    ///
    /// A property of the file, not a convention: there is nothing here to
    /// disable, because the verbs are absent.
    #[test]
    fn the_automation_cannot_deploy_tune_or_trade() {
        // The readiness evaluator is part of the automation, so it is held to
        // the same rule as the script that calls it.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ops/qualify");
        let script = std::fs::read_to_string(root.join("session.sh")).unwrap()
            + &std::fs::read_to_string(root.join("preflight_gates.py")).unwrap();

        // Comments say what the script will not do; the code must agree, so the
        // prose is stripped before looking.
        let code: String = script
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");

        for forbidden in [
            "deploy.sh",
            "docker compose",
            "docker-compose",
            "systemctl",
            "git push",
            "git pull",
            "git checkout",
            "git merge",
            "--threshold",
            "auto-trader",
            "auto_trader",
        ] {
            assert!(
                !code.contains(forbidden),
                "ops/qualify/session.sh contains {forbidden:?}: the qualification automation must \
                 never change production, deploy, tune a threshold, or touch Auto-Trader"
            );
        }
    }

}

/// The designated-session readiness gates (P3 §18): `ops/qualify/preflight_gates.py`
/// evaluated against the real health envelope, run for real.
///
/// The Python is executed rather than re-implemented here, because the thing
/// that must not drift is the code the operator runs. Every negative case
/// starts from facts and a health document that PASS and changes exactly one
/// thing.
mod readiness_contract {
    use serde_json::{json, Value};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const DAY: &str = "2026-09-29"; // opens 2026-09-29T08:00:00Z (EDT)
    const COMMIT: &str = "af986b84cd3745f077b48fef912610990b7db725";

    fn ops() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ops/qualify")
    }

    fn gates_source() -> String {
        std::fs::read_to_string(ops().join("preflight_gates.py"))
            .expect("ops/qualify/preflight_gates.py must exist")
    }

    fn script() -> String {
        std::fs::read_to_string(ops().join("session.sh")).unwrap()
    }

    /// A Python >= 3.9 with the America/New_York zone, as on the VPS. Required,
    /// not optional: a skipped test is a silent pass.
    fn python() -> Command {
        for candidate in ["python3", "python"] {
            let probe = Command::new(candidate)
                .args([
                    "-c",
                    "import sys, zoneinfo; assert sys.version_info >= (3, 9); \
                     zoneinfo.ZoneInfo('America/New_York')",
                ])
                .output();
            if probe.is_ok_and(|o| o.status.success()) {
                return Command::new(candidate);
            }
        }
        panic!("python >= 3.9 with tz data is required to test ops/qualify/preflight_gates.py");
    }

    /// Every quoted health path the evaluator reads.
    fn paths_read_by_the_gates() -> Vec<String> {
        let mut out = Vec::new();
        for piece in gates_source().split('"').skip(1).step_by(2) {
            let head = piece.split('.').next().unwrap_or_default();
            let shaped = piece.split('.').count() >= 2
                && piece.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric()));
            if (shaped
                && matches!(
                    head,
                    "report" | "retention" | "discoveryRetention" | "measurementPending" | "premarketVolume"
                ))
                || piece == "anyKnownLoss"
            {
                out.push(piece.to_string());
            }
        }
        out.sort();
        out.dedup();
        assert!(out.len() >= 25, "scraped too few paths: {out:?}");
        out
    }

    /// `PROVISIONAL` as declared: `set()` (empty) or `{"a.b", ...}`.
    fn provisional() -> Vec<String> {
        let source = gates_source();
        let rest = source
            .split("\nPROVISIONAL = ")
            .nth(1)
            .expect("preflight_gates.py must declare PROVISIONAL");
        if rest.starts_with("set()") {
            return Vec::new();
        }
        let block = rest
            .strip_prefix('{')
            .and_then(|r| r.split('}').next())
            .expect("PROVISIONAL must be `set()` or a set literal");
        let mut out: Vec<String> =
            block.split('"').skip(1).step_by(2).map(str::to_string).collect();
        out.sort();
        out
    }

    /// The route's envelope with every optional capture present, exactly as
    /// this build emits it: the engine block comes from the real
    /// `EngineHealth` -> `research_health::engine_capture` mapping, and the
    /// premarket block is a real `PremarketVolumeHealth`, at 03:00 ET on
    /// `DAY` (so both still describe the previous market day, 2026-09-28).
    fn this_builds_envelope() -> Value {
        use backtest_metrics::completeness::{
            CompletenessReport, DiscoveryCapture, WriterCapture,
        };
        let config = backtest_metrics::opportunity::OiConfig::default();
        let versions = crate::research_health::ReportVersions::for_config(&config);
        let writer = WriterCapture {
            attempted: 10,
            written: 10,
            last_write: Some("2026-09-28T23:59:30Z".parse().unwrap()),
            ..WriterCapture::default()
        };
        let engine = {
            use std::sync::atomic::Ordering::Relaxed;
            let health = crate::opportunity_shadow::EngineHealth::default();
            health.capacity.store(16_375, Relaxed);
            health.rank_cohort_capacity.store(16_375, Relaxed);
            health.lifecycle.set(config.lifecycle.version().to_string()).unwrap();
            let mut engine = crate::research_health::engine_capture(&health);
            // `engine_capture` stamps the market day of the wall clock it runs
            // on; the fixture's clock is 03:00 ET on DAY. Pinned rather than
            // left to today's date so the `timezone` check is deterministic.
            assert!(engine.market_day_id.is_some(), "the route emits marketDayId");
            engine.market_day_id = Some("2026-09-28".into());
            engine
        };
        let premarket = market_data::PremarketVolumeHealth {
            market_day: Some("2026-09-28".parse().unwrap()),
            initialized_at: Some("2026-09-28T08:00:30Z".parse().unwrap()),
            last_scan_at: Some("2026-09-28T23:59:00Z".parse().unwrap()),
            last_successful_fetch_at: Some("2026-09-28T13:29:00Z".parse().unwrap()),
            survivors_needing_volume: 40,
            survivors_resolved: 40,
            requests_this_market_day: 90,
            cached_symbols: 40,
            ..Default::default()
        };
        let report = CompletenessReport {
            report_schema_version: crate::research_health::REPORT_SCHEMA_VERSION,
            generated_at: "2026-09-29T07:00:00Z".parse().unwrap(),
            commit: Some(COMMIT.into()),
            oi_config_fingerprint: Some(config.fingerprint()),
            oi_versions: Some(versions.oi.clone()),
            outcome_measurement_version: Some(versions.outcome_measurement_version.clone()),
            episode_schema: Some(versions.episode_schema),
            signal_context_schema: Some(versions.signal_context_schema),
            opportunity_intelligence: Some(writer.clone()),
            measurement: Some(writer.clone()),
            discovery: Some(DiscoveryCapture { attempted: 10, written: 10, ..Default::default() }),
            opportunity_engine: Some(engine),
            opportunity_outcomes: Some(writer),
            opportunity_outcome_engine: Some(Default::default()),
        };
        let retention = crate::research_retention::RetentionSnapshot {
            ceiling_bytes: 64 << 30,
            dir_bytes: 0,
            sessions_present: 1,
            sessions_deleted: 0,
            bytes_reclaimed: 0,
            deleted_without_export: 0,
            retention_pending: false,
            last_sweep: None,
            last_deleted: String::new(),
            blocked_by_protection: false,
            bytes_over_ceiling: 0,
            protected_sessions: 0,
            protected_bytes: 0,
            deleted_protected_with_receipt: 0,
            protected_without_receipt: Vec::new(),
            protected_retained: Vec::new(),
            registry_errors: Vec::new(),
        };
        crate::http::completeness_envelope(
            &report,
            Some(json!({"pending": 0, "pendingPeak": 0, "pendingCapacity": 38_400,
                        "capacityEvictions": 0, "openEpisodes": 0})),
            Some(retention),
            Some(market_data::discovery_audit::DiscoveryRetention::default()),
            Some(premarket),
        )
    }

    /// Fields P3 added, removed from this build's envelope to model the health
    /// document an older build wrote. `premarketVolume` is removed as a key,
    /// which is not the same as `null` (a build that emits it, before its
    /// first universe scan).
    const P3_FIELDS: [&str; 5] = [
        "report.opportunityEngine.duplicateIdentityRefused",
        "report.opportunityEngine.lifecycle",
        "report.opportunityEngine.marketDayId",
        "report.oiVersions.lifecycle",
        "premarketVolume",
    ];

    fn remove(doc: &mut Value, path: &str) {
        let keys: Vec<&str> = path.split('.').collect();
        let mut node = doc;
        for key in &keys[..keys.len() - 1] {
            node = &mut node[*key];
        }
        node.as_object_mut().unwrap().remove(keys[keys.len() - 1]);
    }

    /// Facts of a host ready for DAY: 03:00 ET that morning, ws up since the
    /// previous afternoon, clean checkout, marker older than the open.
    fn clean_facts() -> Value {
        let spec = backtest_metrics::alpha::spec::QualificationSpec::default();
        let expected = json!({
            "oiConfig": backtest_metrics::opportunity::OiConfig::default().fingerprint(),
            "outcomeVersion": backtest_metrics::opportunity_outcome::OPPORTUNITY_OUTCOME_VERSION,
            "opportunitySchema": backtest_metrics::opportunity::OPPORTUNITY_SCHEMA_VERSION,
            "featureSchema": backtest_metrics::opportunity::OI_FEATURE_SCHEMA_VERSION,
            "signalContextSchema": backtest_metrics::context::SIGNAL_CONTEXT_SCHEMA_VERSION,
            "episodeSchema": backtest_metrics::episode::EPISODE_SCHEMA_VERSION,
            "baselinePolicy": backtest_metrics::context::BASELINE_POLICY,
            "lifecycle": spec.expected_lifecycle,
            "specSha": spec.sha256(),
            "specVersion": spec.version,
        });
        json!({
            "marketDay": DAY,
            "now": "2026-09-29T07:00:00Z",
            "head": COMMIT,
            "deployedCommit": COMMIT,
            "deployMarkerMtime": "1790630000", // 2026-09-28T21:13:20Z
            "dirtyEntries": 0,
            "behindUpstream": 0,
            "freeGb": 120,
            "minFreeGb": 40,
            "researchDirsPresent": true,
            "captureService": "ws",
            "containers": [
                {"name": "stockspotter-vps-ws-1", "service": "ws", "restartCount": 0,
                 "startedAt": "2026-09-28T21:30:00.395010709Z", "running": true},
                {"name": "stockspotter-vps-web-1", "service": "web", "restartCount": 0,
                 "startedAt": "2026-09-28T21:30:01Z", "running": true},
            ],
            "specSha": spec.sha256(),
            "expected": expected,
        })
    }

    struct Run {
        code: i32,
        stdout: String,
        result: Value,
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "readiness-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run_with(health: &Value, facts: &Value, env: &[(&str, &str)]) -> Run {
        let dir = scratch("check");
        std::fs::write(dir.join("health.json"), health.to_string()).unwrap();
        std::fs::write(dir.join("facts.json"), facts.to_string()).unwrap();
        let mut cmd = python();
        cmd.arg(ops().join("preflight_gates.py"))
            .arg("check")
            .arg("--health")
            .arg(dir.join("health.json"))
            .arg("--facts")
            .arg(dir.join("facts.json"))
            .arg("--out")
            .arg(dir.join("out.json"));
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        let result: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("out.json")).unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        Run {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            result,
        }
    }

    fn run(health: &Value, facts: &Value) -> Run {
        run_with(health, facts, &[])
    }

    fn check<'a>(run: &'a Run, name: &str) -> &'a Value {
        run.result["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["check"] == name)
            .unwrap_or_else(|| panic!("no check {name} in {}", run.stdout))
    }

    #[track_caller]
    fn assert_fails_on(run: &Run, name: &str) {
        assert_eq!(run.code, 1, "must exit 1:\n{}", run.stdout);
        assert_eq!(run.result["preflight"], "FAIL");
        assert_eq!(check(run, name)["pass"], false, "{name} should fail:\n{}", run.stdout);
    }

    fn set(doc: &mut Value, path: &str, value: Value) {
        let mut node = doc;
        let keys: Vec<&str> = path.split('.').collect();
        for key in &keys[..keys.len() - 1] {
            node = &mut node[*key];
        }
        node[keys[keys.len() - 1]] = value;
    }

    // -----------------------------------------------------------------------

    /// Nothing is provisional since the P3 integration: every gate reads a
    /// path this build emits. A future gate that reads ahead of its field
    /// must declare it, and the equality above then keeps the list honest.
    #[test]
    fn no_readiness_path_is_provisional() {
        assert_eq!(provisional(), Vec::<String>::new());
    }

    #[test]
    fn every_path_the_readiness_gates_read_exists_or_is_declared_provisional() {
        let doc = this_builds_envelope();
        let resolve = |path: &str| -> bool {
            let mut node = &doc;
            for key in path.split('.') {
                match node.get(key) {
                    Some(next) => node = next,
                    None => return false,
                }
            }
            true
        };
        let missing: Vec<String> =
            paths_read_by_the_gates().into_iter().filter(|p| !resolve(p)).collect();
        assert_eq!(
            missing,
            provisional(),
            "PROVISIONAL in preflight_gates.py must be exactly the fields this build does not \
             emit: a field missing and undeclared is a typo the gate would read as absent forever; \
             a declared field that now exists must be removed from PROVISIONAL"
        );
    }

    #[test]
    fn a_ready_host_passes_and_the_result_is_machine_readable() {
        let r = run(&this_builds_envelope(), &clean_facts());
        assert_eq!(r.code, 0, "{}", r.stdout);
        assert_eq!(r.result["preflight"], "PASS");
        assert_eq!(r.result["marketDay"], DAY);
        for c in r.result["checks"].as_array().unwrap() {
            for key in ["check", "pass", "absent", "observed", "expected"] {
                assert!(c.get(key).is_some(), "{key} missing from {c}");
            }
        }
        assert!(r.stdout.contains("READINESS PASS"));
    }

    /// The checks that read P3's fields pass on what this build emits -- read,
    /// not defaulted: each is present, and each observed value is the real one.
    #[test]
    fn this_builds_envelope_passes_every_presence_check() {
        let r = run(&this_builds_envelope(), &clean_facts());
        assert_eq!(r.code, 0, "{}", r.stdout);
        for name in ["duplicate-identity", "lifecycle-versions", "lifecycle-engine", "premarket-volume-reporting", "timezone"] {
            let c = check(&r, name);
            assert_eq!((c["pass"].as_bool(), c["absent"].as_bool()), (Some(true), Some(false)), "{name}: {c}");
        }
        let lifecycle = backtest_metrics::opportunity::LIFECYCLE_MOVE_V1_VERSION;
        assert_eq!(check(&r, "lifecycle-engine")["observed"], lifecycle);
        assert_eq!(check(&r, "lifecycle-versions")["observed"], lifecycle);
        assert_eq!(check(&r, "timezone")["observed"], "2026-09-28");
    }

    /// An older build's health lacks those fields, so it cannot pass -- each
    /// reads as ABSENT, never as zero.
    #[test]
    fn an_older_builds_health_fails_closed() {
        let mut health = this_builds_envelope();
        for path in P3_FIELDS {
            remove(&mut health, path);
        }
        let r = run(&health, &clean_facts());
        assert_eq!(r.code, 1, "{}", r.stdout);
        for name in ["duplicate-identity", "lifecycle-versions", "lifecycle-engine", "premarket-volume-reporting", "timezone"] {
            let c = check(&r, name);
            assert_eq!((c["pass"].as_bool(), c["absent"].as_bool()), (Some(false), Some(true)), "{name}: {c}");
        }
    }

    /// Pre-open, the premarket block can only describe an earlier market day
    /// (its counters reset at 04:00 ET of DAY, after the preflight). So the
    /// preflight checks the instrument is reporting and well-formed; the
    /// designated day's `fetchFailures == 0` is the post-session gate's job.
    #[test]
    fn the_premarket_preflight_checks_the_instrument_not_a_day_it_cannot_see() {
        // No universe scan yet in this process: emitted as null, and passes.
        let mut health = this_builds_envelope();
        health["premarketVolume"] = Value::Null;
        let r = run(&health, &clean_facts());
        assert_eq!(r.code, 0, "{}", r.stdout);

        // The previous market day's failures are shown, not gated: they
        // cannot be cleared before the open and say nothing about DAY.
        let mut health = this_builds_envelope();
        set(&mut health, "premarketVolume.fetchFailures", json!(2));
        let r = run(&health, &clean_facts());
        assert_eq!(r.code, 0, "{}", r.stdout);
        assert_eq!(check(&r, "premarket-volume-reporting")["observed"]["fetchFailures"], 2);
    }

    #[test]
    fn every_fact_the_brief_lists_is_a_failing_gate() {
        type Mutation = fn(&mut Value, &mut Value);
        let cases: Vec<(&str, Mutation)> = vec![
            ("running-commit", |_, f| f["deployedCommit"] = json!("0".repeat(40))),
            ("running-commit", |h, _| set(h, "report.commit", json!("f".repeat(40)))),
            ("deploy-marker-before-open", |_, f| f["deployMarkerMtime"] = json!("1790668800")), // 08:00:00Z
            ("deploy-marker-before-open", |_, f| f["deployMarkerMtime"] = json!("")),
            ("worktree-clean", |_, f| f["dirtyEntries"] = json!(1)),
            ("worktree-clean", |_, f| f["dirtyEntries"] = Value::Null),
            ("no-pending-deploy", |_, f| f["behindUpstream"] = json!(2)),
            ("no-pending-deploy", |_, f| f["behindUpstream"] = Value::Null),
            ("spec-sha", |_, f| f["specSha"] = json!("0".repeat(64))),
            ("containers-present", |_, f| f["containers"][0]["running"] = json!(false)),
            ("containers-present", |_, f| f["captureService"] = json!("nope")),
            ("container-restarts", |_, f| f["containers"][1]["restartCount"] = json!(1)),
            ("container-restarts", |_, f| f["containers"] = json!([])),
            ("capture-started-before-open", |_, f| f["containers"][0]["startedAt"] = json!("2026-09-29T08:00:00Z")),
            ("capture-started-before-open", |_, f| f["containers"][0]["startedAt"] = json!("2026-09-29T11:00:00Z")),
            // Started before the open but has observed nothing since.
            ("observation-started-before-open", |h, _| set(h, "report.opportunityIntelligence.lastWrite", json!("2026-09-28T20:00:00Z"))),
            ("observation-started-before-open", |h, _| set(h, "report.opportunityIntelligence.lastWrite", Value::Null)),
            ("before-observation-boundary", |_, f| f["now"] = json!("2026-09-29T08:00:00Z")),
            ("before-observation-boundary", |_, f| f["now"] = json!("2026-09-29T13:30:00Z")),
            ("disk", |_, f| f["freeGb"] = json!(39)),
            ("disk", |_, f| f["freeGb"] = Value::Null),
            ("research-dirs", |_, f| f["researchDirsPresent"] = json!(false)),
            ("rank-capacity", |h, _| set(h, "report.opportunityEngine.rankCohortCapacity", json!(4_096))),
            ("timezone", |h, _| set(h, "report.opportunityEngine.marketDayId", json!("2026-09-29"))),
            ("fingerprint", |h, _| set(h, "report.oiConfigFingerprint", json!("oi-cfg-b4f21c8b311a1b99"))),
            ("opportunity-schema", |h, _| set(h, "report.oiVersions.opportunitySchema", json!(1))),
            ("feature-schema", |h, _| set(h, "report.oiVersions.featureSchema", json!(2))),
            ("signal-context-schema", |h, _| set(h, "report.signalContextSchema", json!(1))),
            ("episode-schema", |h, _| set(h, "report.episodeSchema", json!(1))),
            ("outcome-version", |h, _| set(h, "report.outcomeMeasurementVersion", json!("opportunity-outcome-v1"))),
            ("baseline-policy", |h, _| set(h, "report.oiVersions.baselinePolicy", Value::Null)),
            ("lifecycle-versions", |h, _| set(h, "report.oiVersions.lifecycle", json!("opportunity-lifecycle-symbol-activity-v1"))),
            ("lifecycle-engine", |h, _| set(h, "report.opportunityEngine.lifecycle", json!("opportunity-lifecycle-symbol-activity-v1"))),
            ("any-known-loss", |h, _| h["anyKnownLoss"] = json!(true)),
            ("outcomes-dropped", |h, _| set(h, "report.opportunityOutcomes.dropped", json!(3))),
            ("outcome-capacity-evictions", |h, _| set(h, "report.opportunityOutcomeEngine.capacityEvictions", json!(1))),
            ("duplicate-identity", |h, _| set(h, "report.opportunityEngine.duplicateIdentityRefused", json!(1))),
            ("oi-writer-healthy", |h, _| set(h, "report.opportunityIntelligence.degraded", json!(true))),
            ("discovery-writer-healthy", |h, _| set(h, "report.discovery.degraded", json!(true))),
            ("retention-protected-without-receipt", |h, _| set(h, "retention.protectedWithoutReceipt", json!(["2026-09-21"]))),
            ("retention-registry-errors", |h, _| set(h, "retention.registryErrors", json!(["x.json: bad"]))),
            ("retention-blocked", |h, _| set(h, "retention.blockedByProtection", json!(true))),
            ("discovery-retention-blocked", |h, _| set(h, "discoveryRetention.blockedByProtection", json!(true))),
            ("discovery-retention-registry-errors", |h, _| set(h, "discoveryRetention.registryErrors", json!(["bad"]))),
            ("premarket-volume-reporting", |h, _| remove(h, "premarketVolume")),
            ("premarket-volume-reporting", |h, _| set(h, "premarketVolume.fetchFailures", json!("0"))),
            ("premarket-volume-reporting", |h, _| remove(h, "premarketVolume.fetchFailures")),
            // A block already claiming the designated day before its open.
            ("premarket-volume-reporting", |h, _| set(h, "premarketVolume.marketDay", json!(DAY))),
            ("premarket-volume-reporting", |h, _| set(h, "premarketVolume.marketDay", Value::Null)),
            ("health-read", |h, _| *h = json!("not a document")),
        ];
        for (name, mutate) in cases {
            let (mut health, mut facts) = (this_builds_envelope(), clean_facts());
            mutate(&mut health, &mut facts);
            assert_fails_on(&run(&health, &facts), name);
        }
    }

    /// The EST open is 09:00Z: a capture started at 08:30Z is before it.
    #[test]
    fn the_open_is_dst_aware() {
        let (mut health, mut facts) = (this_builds_envelope(), clean_facts());
        set(&mut health, "premarketVolume.marketDay", json!("2026-01-12"));
        facts["marketDay"] = json!("2026-01-13");
        facts["now"] = json!("2026-01-13T08:45:00Z");
        facts["deployMarkerMtime"] = json!("1768250000"); // 2026-01-12T20:33:20Z
        facts["containers"][0]["startedAt"] = json!("2026-01-13T08:30:00Z");
        facts["containers"][1]["startedAt"] = json!("2026-01-13T08:30:00Z");
        set(&mut health, "report.opportunityIntelligence.lastWrite", json!("2026-01-13T08:40:00Z"));
        set(&mut health, "report.opportunityEngine.marketDayId", json!("2026-01-12"));
        let r = run(&health, &facts);
        assert_eq!(r.code, 0, "{}", r.stdout);
        facts["now"] = json!("2026-01-13T09:00:00Z");
        assert_fails_on(&run(&health, &facts), "before-observation-boundary");
    }

    /// No environment variable turns a FAIL into a PASS.
    #[test]
    fn no_environment_variable_can_force_a_pass() {
        let mut facts = clean_facts();
        facts["dirtyEntries"] = json!(1);
        let r = run_with(
            &this_builds_envelope(),
            &facts,
            &[("FORCE", "1"), ("PREFLIGHT_FORCE", "1"), ("FORCE_PASS", "1"), ("SKIP_PREFLIGHT", "1"), ("OVERRIDE", "1")],
        );
        assert_fails_on(&r, "worktree-clean");
        let code: String = gates_source()
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        // `facts` reads FACT_* to *assemble* evidence; nothing else reads the
        // environment, and no argument can mark a check passed.
        for forbidden in ["--force", "--override", "--skip", "FORCE", "OVERRIDE", "SKIP_"] {
            assert!(!code.contains(forbidden), "preflight_gates.py contains {forbidden:?}");
        }
        assert!(!script().contains("PREFLIGHT_FORCE") && !script().contains("--force"));
    }

    /// The pins the script carries equal the code, identity by identity.
    #[test]
    fn the_readiness_pins_match_the_frozen_code() {
        let script = script();
        let spec = backtest_metrics::alpha::spec::QualificationSpec::default();
        for (name, value) in [
            ("EXPECTED_OPPORTUNITY_SCHEMA", backtest_metrics::opportunity::OPPORTUNITY_SCHEMA_VERSION.to_string()),
            ("EXPECTED_FEATURE_SCHEMA", backtest_metrics::opportunity::OI_FEATURE_SCHEMA_VERSION.to_string()),
            ("EXPECTED_SIGNAL_CONTEXT_SCHEMA", backtest_metrics::context::SIGNAL_CONTEXT_SCHEMA_VERSION.to_string()),
            ("EXPECTED_EPISODE_SCHEMA", backtest_metrics::episode::EPISODE_SCHEMA_VERSION.to_string()),
            ("EXPECTED_BASELINE_POLICY", backtest_metrics::context::BASELINE_POLICY.to_string()),
            // D5's own constant, not the spec's copy of it: the script, the
            // spec and the engine must all name the lifecycle the engine runs.
            ("EXPECTED_LIFECYCLE", backtest_metrics::opportunity::LIFECYCLE_MOVE_V1_VERSION.to_string()),
            ("EXPECTED_SPEC_VERSION", spec.version.clone()),
        ] {
            assert!(
                script.contains(&format!("{name}=\"{value}\"")),
                "session.sh must pin {name}=\"{value}\" (the code's value)"
            );
        }
        let lifecycle = backtest_metrics::opportunity::LIFECYCLE_MOVE_V1_VERSION;
        assert_eq!(spec.expected_lifecycle, lifecycle);
        assert_eq!(backtest_metrics::opportunity::OiConfig::default().versions().lifecycle, lifecycle);
        // Each pin reaches the evaluator.
        for name in ["EXPECTED_OPPORTUNITY_SCHEMA", "EXPECTED_LIFECYCLE", "EXPECTED_BASELINE_POLICY", "EXPECTED_SPEC_SHA"] {
            assert!(script.contains(&format!("FACT_{name}=\"${name}\"")), "{name} is not passed to the gates");
        }
    }

    /// `designation` and `protection` produce exactly the records the
    /// qualifier and the retention registry read, and `designation` refuses a
    /// failing preflight.
    #[test]
    fn the_designation_step_writes_what_the_qualifier_and_retention_read() {
        let dir = scratch("designate");
        std::fs::write(dir.join("health.json"), this_builds_envelope().to_string()).unwrap();
        std::fs::write(dir.join("facts.json"), clean_facts().to_string()).unwrap();
        let gates = ops().join("preflight_gates.py");

        let out = python()
            .arg(&gates)
            .args(["designation", "--by", "roman"])
            .arg("--health")
            .arg(dir.join("health.json"))
            .arg("--facts")
            .arg(dir.join("facts.json"))
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let record: backtest_metrics::completeness::DesignationRecord =
            serde_json::from_slice(&out.stdout).expect("the qualifier's DesignationRecord shape");
        let spec = backtest_metrics::alpha::spec::QualificationSpec::default();
        assert_eq!(record.market_day.to_string(), DAY);
        assert_eq!(record.preflight, "PASS");
        assert_eq!(record.commit, COMMIT);
        assert_eq!(record.spec_sha256, spec.sha256());
        assert_eq!(record.spec_version, spec.version);
        assert_eq!(record.process_started_at, "2026-09-28T21:30:00.395010Z".parse::<chrono::DateTime<chrono::Utc>>().unwrap());
        let open = market_data::trading_session::market_day_open(record.market_day);
        assert!(record.designated_at < open && record.process_started_at < open && record.deploy_marker_at < open);

        let out = python()
            .arg(&gates)
            .args(["protection", "--by", "roman", "--reason", "P3 designated session"])
            .arg("--facts")
            .arg(dir.join("facts.json"))
            .output()
            .unwrap();
        assert!(out.status.success());
        let registry = dir.join("research/.retention/protected");
        std::fs::create_dir_all(&registry).unwrap();
        std::fs::write(registry.join(format!("{DAY}.json")), &out.stdout).unwrap();
        let day: chrono::NaiveDate = DAY.parse().unwrap();
        assert!(matches!(
            market_data::retention_registry::ProtectionIndex::load(&dir.join("research")).of(day),
            market_data::retention_registry::Protection::Protected {
                class: market_data::retention_registry::ProtectionClass::Designated,
                ..
            }
        ));
        let verify = python()
            .arg(&gates)
            .args(["verify-protection", "--day", DAY, "--file"])
            .arg(registry.join(format!("{DAY}.json")))
            .output()
            .unwrap();
        assert!(verify.status.success());

        // A failing preflight yields no record.
        let mut facts = clean_facts();
        facts["now"] = json!("2026-09-29T08:00:01Z");
        std::fs::write(dir.join("facts.json"), facts.to_string()).unwrap();
        let out = python()
            .arg(&gates)
            .args(["designation", "--by", "roman"])
            .arg("--health")
            .arg(dir.join("health.json"))
            .arg("--facts")
            .arg(dir.join("facts.json"))
            .output()
            .unwrap();
        assert!(!out.status.success(), "a designation after the open must be refused");
        assert!(out.stdout.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The script wires the step: designate runs the preflight first, never
    /// rewrites a designation, and qualify takes its commit from the record.
    #[test]
    fn the_script_designates_before_data_and_qualifies_against_the_record() {
        let script = script();
        let designate = script.split("cmd_designate() {").nth(1).expect("cmd_designate").split("\n}\n").next().unwrap();
        let preflight_at = designate.find("cmd_preflight \"$day\"").expect("designate runs the preflight");
        let first_write = designate.find("mv ").expect("designate writes by rename");
        assert!(preflight_at < first_write, "the preflight must run before anything is written");
        assert!(designate.contains("never rewritten"));
        assert!(designate.contains("designations/${day}.json"));
        let qualify = script.split("cmd_qualify() {").nth(1).unwrap().split("\n}\n").next().unwrap();
        assert!(qualify.contains("designations/${day}.json"));
        assert!(qualify.contains("--expected-commit \"$expected_commit\""));
        assert!(!qualify.contains("completeness-${day}.json"), "the expected commit must not come from the capture itself");
        assert!(script.contains("designate) shift; cmd_designate"));
    }
}
