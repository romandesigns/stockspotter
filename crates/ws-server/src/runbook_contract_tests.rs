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
        let report = CompletenessReport {
            generated_at: chrono::Utc::now(),
            commit: Some("af986b84cd3745f077b48fef912610990b7db725".into()),
            oi_config_fingerprint: Some("oi-cfg-b4f21c8b311a1b99".into()),
            opportunity_intelligence: Some(WriterCapture::default()),
            measurement: Some(WriterCapture::default()),
            discovery: Some(DiscoveryCapture::default()),
            opportunity_engine: Some(EngineCapture::default()),
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
        })
        .unwrap();

        crate::http::completeness_envelope(
            &report,
            Some(settlement),
            serde_json::from_value(retention).unwrap(),
        )
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

        let spec = backtest_metrics::alpha::spec::QualificationSpec::default();
        assert!(
            script.contains(&format!("EXPECTED_SPEC_SHA=\"{}\"", spec.sha256())),
            "the runbook pins contract hash other than {} ({})",
            spec.sha256(),
            spec.version
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
        for tool in ["alpha_qualify", "curl", "python3", "rsync", "sha256sum"] {
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
        let script = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ops/qualify/session.sh"),
        )
        .unwrap();

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
