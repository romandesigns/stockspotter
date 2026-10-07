//! Tests for the report generator.
//!
//! The contract carries reporting obligations, and a duty that can be dropped
//! while writing is not a duty — so the report is checked against them rather
//! than trusted to include them.

use super::*;

use crate::alpha::spec::QualificationSpec;

#[test]
fn the_contracts_reporting_obligations_are_checked_not_assumed() {
    let spec = QualificationSpec::default();
    let request = Request {
        session_dir: std::path::PathBuf::from("session"),
        session_date: "2026-09-17".to_string(),
        expected_commit: None,
        expected_oi_config: None,
        expected_spec_sha256: None,
        output_dir: std::path::PathBuf::from("out"),
        spec,
    };
    // A report that says none of the required things must be caught.
    let missing = missing_requirements(&request, "nothing useful here");
    assert!(
        missing.contains(&"R-two-interpretations".to_string()),
        "the obligation most likely to be dropped must be detected: {missing:?}"
    );
    assert!(missing.contains(&"R-detection-coverage".to_string()));
    assert!(missing.contains(&"R-runner-stage-ladder".to_string()));
    assert!(missing.contains(&"R-secondary-direction".to_string()));
}

#[test]
fn a_classification_line_reports_a_share() {
    let line = classification_line(Classification::RankedEarly, 25, 100);
    assert!(line.starts_with('F'));
    assert!(line.contains("25"));
    assert!(line.contains("25.0%"));
    assert_eq!(classification_line(Classification::Unknown, 0, 0), "UNKNOWN — 0 (0.0%)");
}
