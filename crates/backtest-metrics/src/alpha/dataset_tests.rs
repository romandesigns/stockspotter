//! Tests for artifact ingest and opportunity-level assembly.
//!
//! Built on synthetic captures written to a temp directory, so the fixtures
//! have known ground truth and the tests stay hermetic. The September-16
//! artifact is read-only evidence and is never touched by a test.

use super::*;

use chrono::TimeZone;

use crate::alpha::labels;
use crate::opportunity::{
    OiConfig, OpportunityIntelligence, OpportunityScoreSnapshot,
};
use market_data::{IgnitionEventKind, ScanEvent};

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "alpha-dataset-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(dir.join("research")).unwrap();
    std::fs::create_dir_all(dir.join("discovery-audit")).unwrap();
    dir
}

const DAY: &str = "2026-09-17";

fn at(h: u32, m: u32, s: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 17, h, m, s).unwrap()
}

/// Real snapshots, produced by the real engine, so the fixture exercises the
/// actual schema rather than a hand-built approximation of it.
fn real_snapshots(symbols: usize, windows: usize) -> Vec<OpportunityScoreSnapshot> {
    let mut engine = OpportunityIntelligence::new(OiConfig::default());
    let mut out = Vec::new();
    let confirmed = |symbol: &str, t: DateTime<Utc>, price: f64| ScanEvent::IgnitionEvent {
        symbol: symbol.into(),
        timestamp: t,
        price,
        kind: IgnitionEventKind::FollowThroughConfirmed,
    };
    let momentum = |symbol: &str, t: DateTime<Utc>| ScanEvent::MomentumUpdate {
        symbol: symbol.into(),
        timestamp: t,
        volume_confirmation: 0.6,
        structure: 0.5,
        ma_slope: 0.7,
        wick_rejection: 0.6,
        overall: 0.75,
        qualifies: true,
    };
    for i in 0..symbols {
        let t = at(13, 30, 0);
        engine.observe(&confirmed(&format!("S{i:04}"), t, 10.0 + i as f64 * 0.01), t);
    }
    for i in 0..symbols {
        let t = at(13, 30, 1);
        engine.observe(&momentum(&format!("S{i:04}"), t), t);
    }
    for w in 0..windows {
        let t = at(13, 31, 0) + chrono::Duration::seconds(w as i64 * 30);
        for i in 0..symbols {
            engine.observe(&momentum(&format!("S{i:04}"), t), t);
        }
        if let Some(rows) = engine.rank(t) {
            out.extend(rows);
        }
    }
    out
}

fn write_snapshots(dir: &Path, snapshots: &[OpportunityScoreSnapshot]) -> PathBuf {
    let path = dir.join("research").join(format!("opportunity-intelligence-{DAY}.ndjson"));
    let mut text = String::new();
    for snapshot in snapshots {
        text.push_str(&serde_json::to_string(snapshot).unwrap());
        text.push('\n');
    }
    std::fs::write(&path, text).unwrap();
    path
}

fn write_discovery(dir: &Path, rows: &[(&str, DateTime<Utc>, f64, &str)]) -> PathBuf {
    let path = dir.join("discovery-audit").join(format!("{DAY}-1-1-1.jsonl"));
    let mut text = String::new();
    for (symbol, at, price, stage) in rows {
        let record = serde_json::json!({
            "schema": 2,
            "recorded_at": at.to_rfc3339(),
            "kind": "ignition",
            "lost_records": 0,
            "sampled_out": 0,
            "queue_loss": serde_json::Value::Null,
            "data": {"symbol": symbol, "market_at": at.to_rfc3339(), "price": price, "stage": stage},
        });
        text.push_str(&record.to_string());
        text.push('\n');
    }
    std::fs::write(&path, text).unwrap();
    path
}

// ---------------------------------------------------------------------------
// Discovery and integrity
// ---------------------------------------------------------------------------

#[test]
fn artifacts_are_discovered_in_the_expected_layout() {
    let dir = temp_dir("discover");
    write_snapshots(&dir, &real_snapshots(3, 2));
    std::fs::write(
        dir.join("research").join(format!("episodes-{DAY}.ndjson")),
        "{\"schemaVersion\":1}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("research").join(format!("opportunity-intelligence-markers-{DAY}.ndjson")),
        "{\"kind\":\"writer_started\"}\n",
    )
    .unwrap();
    write_discovery(&dir, &[("AAA", at(13, 30, 0), 10.0, "confirmed")]);

    let artifacts = SessionArtifacts::discover(&dir, DAY);
    assert!(artifacts.oi_snapshots.is_some());
    assert!(artifacts.episodes.is_some());
    assert_eq!(artifacts.markers.len(), 1, "markers must not be mistaken for the data file");
    assert_eq!(artifacts.discovery.len(), 1);
    assert!(artifacts.compressed.is_empty());
    assert_eq!(artifacts.required().len(), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The marker stream and the data stream share a prefix. Confusing them would
/// make every session look corrupt, because markers are not snapshots.
#[test]
fn the_marker_stream_is_never_read_as_the_data_stream() {
    let dir = temp_dir("markers");
    let snapshots = real_snapshots(3, 2);
    let data = write_snapshots(&dir, &snapshots);
    std::fs::write(
        dir.join("research").join(format!("opportunity-intelligence-markers-{DAY}.ndjson")),
        "{\"schemaVersion\":1,\"kind\":\"queue_loss\",\"capture\":\"opportunity-intelligence\"}\n",
    )
    .unwrap();

    let artifacts = SessionArtifacts::discover(&dir, DAY);
    assert_eq!(artifacts.oi_snapshots.as_deref(), Some(data.as_path()));
    let report = integrity(&artifacts);
    assert!(report.blocking.is_empty(), "{:?}", report.blocking);
    // The marker file parses as JSON, so it contributes no malformed records.
    assert_eq!(report.artifacts.iter().map(|a| a.malformed_records).sum::<u64>(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_compressed_artifact_is_refused_rather_than_silently_skipped() {
    let dir = temp_dir("gz");
    write_snapshots(&dir, &real_snapshots(2, 1));
    std::fs::write(
        dir.join("research").join(format!("episodes-{DAY}.ndjson.gz")),
        [0x1fu8, 0x8b, 0x08, 0x00],
    )
    .unwrap();

    let artifacts = SessionArtifacts::discover(&dir, DAY);
    assert_eq!(artifacts.compressed.len(), 1);
    let report = integrity(&artifacts);
    assert_eq!(report.blocking.len(), 1);
    assert!(report.blocking[0].contains("decompress it"), "{}", report.blocking[0]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_malformed_record_is_counted_and_a_truncated_tail_is_detected() {
    let dir = temp_dir("malformed");
    let snapshots = real_snapshots(3, 2);
    let path = write_snapshots(&dir, &snapshots);
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("{not json at all}\n");
    // A final line with no terminator: the capture was cut mid-record.
    text.push_str("{\"schemaVersion\":1,\"partial\":");
    std::fs::write(&path, text).unwrap();

    let artifacts = SessionArtifacts::discover(&dir, DAY);
    let report = integrity(&artifacts);
    let evidence = report.artifacts.iter().find(|a| a.path.contains("opportunity")).unwrap();
    assert_eq!(evidence.malformed_records, 2, "both bad lines must be counted");
    assert!(evidence.truncated, "a final line without its terminator is a truncated capture");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn every_artifact_gets_a_digest_tying_the_result_to_exact_bytes() {
    let dir = temp_dir("digest");
    let path = write_snapshots(&dir, &real_snapshots(2, 1));
    let artifacts = SessionArtifacts::discover(&dir, DAY);
    let report = integrity(&artifacts);

    let key = report.digests.keys().next().unwrap().clone();
    assert_eq!(report.digests[&key], sha256::hex_file(&path).unwrap());
    assert_eq!(report.digests[&key].len(), 64);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Opportunity-level assembly
// ---------------------------------------------------------------------------

/// The property §12 and §27 both turn on: many windows, one row per
/// opportunity.
#[test]
fn many_ranking_windows_collapse_to_one_row_per_opportunity() {
    let dir = temp_dir("collapse");
    let snapshots = real_snapshots(6, 8);
    assert!(snapshots.len() > 30, "the fixture must produce real volume");
    let path = write_snapshots(&dir, &snapshots);

    let dataset = read_opportunities(&path, &[5, 10]).unwrap();
    assert_eq!(dataset.rows.len(), 6, "six symbols, six opportunities, however many windows");
    assert_eq!(dataset.raw_rows, snapshots.len() as u64);
    assert!(
        dataset.raw_rows > dataset.rows.len() as u64,
        "the collapse must be visible: {} raw rows to {} opportunities",
        dataset.raw_rows,
        dataset.rows.len()
    );
    assert!(dataset.windows >= 2);
    for row in &dataset.rows {
        assert!(row.windows > 1, "each opportunity was ranked in several windows");
        assert_eq!(row.session_date, DAY);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_top_k_entry_records_the_first_instant_not_the_best_one() {
    let dir = temp_dir("firstk");
    let snapshots = real_snapshots(4, 6);
    let path = write_snapshots(&dir, &snapshots);
    let dataset = read_opportunities(&path, &[1, 5]).unwrap();

    for row in &dataset.rows {
        if let Some(first) = row.first_top_k_early.get(&5) {
            // The earliest snapshot in which this opportunity held rank <= 5.
            let earliest = snapshots
                .iter()
                .filter(|s| {
                    s.opportunity_id == row.opportunity_id
                        && s.early_quality_rank.is_some_and(|r| r <= 5)
                })
                .map(|s| s.timestamp)
                .min()
                .unwrap();
            assert_eq!(*first, earliest, "{} entered at its first qualifying window", row.symbol);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn score_availability_is_distinct_from_ranking_badly() {
    let dir = temp_dir("available");
    let path = write_snapshots(&dir, &real_snapshots(4, 4));
    let dataset = read_opportunities(&path, &[5]).unwrap();
    // The fixture supplies momentum, so early quality is computable.
    assert!(
        dataset.rows.iter().any(|r| r.early_quality_available),
        "the fixture must produce at least one scoreable opportunity"
    );
    for row in &dataset.rows {
        if !row.early_quality_available {
            assert!(
                row.best_early_rank.is_none(),
                "an unscoreable opportunity cannot hold a rank"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn opened_at_is_derived_from_the_snapshots_own_age() {
    let dir = temp_dir("opened");
    let snapshots = real_snapshots(3, 3);
    let path = write_snapshots(&dir, &snapshots);
    let dataset = read_opportunities(&path, &[5]).unwrap();
    for row in &dataset.rows {
        let first = snapshots
            .iter()
            .filter(|s| s.opportunity_id == row.opportunity_id)
            .min_by_key(|s| s.timestamp)
            .unwrap();
        assert_eq!(
            row.opened_at,
            first.timestamp - chrono::Duration::seconds(first.opportunity_age_secs)
        );
        assert!(row.opened_at <= row.first_ranked_at);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn window_cohort_sizes_are_captured_for_percentile_surfaces() {
    let dir = temp_dir("cohorts");
    let path = write_snapshots(&dir, &real_snapshots(7, 4));
    let dataset = read_opportunities(&path, &[5]).unwrap();
    assert_eq!(dataset.window_cohort_sizes.len(), dataset.windows);
    for size in dataset.window_cohort_sizes.values() {
        assert!(*size > 0 && *size <= 7);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// The independent price series
// ---------------------------------------------------------------------------

#[test]
fn discovery_yields_a_price_series_and_the_two_stage_sets() {
    let dir = temp_dir("series");
    write_discovery(
        &dir,
        &[
            ("AAA", at(13, 30, 0), 10.00, "confirmed"),
            ("AAA", at(13, 31, 0), 10.30, "candidate"),
            ("BBB", at(13, 30, 0), 5.00, "candidate"),
            ("BBB", at(13, 32, 0), 5.10, "rejected"),
        ],
    );
    let artifacts = SessionArtifacts::discover(&dir, DAY);
    let view = read_discovery(&artifacts.discovery, DAY).unwrap();

    assert_eq!(view.records, 4);
    assert_eq!(view.visible.len(), 2, "both symbols were visible");
    assert_eq!(view.detected.len(), 1, "only AAA produced a confirmed ignition");
    assert!(view.detected.contains("AAA"));
    assert_eq!(view.series["AAA"].len(), 2);
    assert!(view.series["AAA"][0].0 < view.series["AAA"][1].0, "series must be time-ordered");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn records_from_another_session_date_are_excluded() {
    let dir = temp_dir("otherday");
    let other = Utc.with_ymd_and_hms(2026, 9, 16, 14, 0, 0).unwrap();
    write_discovery(&dir, &[("AAA", at(13, 30, 0), 10.0, "confirmed")]);
    // Append a record from the previous session into the same segment.
    let path = dir.join("discovery-audit").join(format!("{DAY}-1-1-1.jsonl"));
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(
        &serde_json::json!({
            "schema": 2, "recorded_at": other.to_rfc3339(), "kind": "ignition",
            "data": {"symbol": "ZZZ", "market_at": other.to_rfc3339(), "price": 3.0, "stage": "confirmed"},
        })
        .to_string(),
    );
    text.push('\n');
    std::fs::write(&path, text).unwrap();

    let view = read_discovery(&[path], DAY).unwrap();
    assert_eq!(view.records, 1, "only the session under evaluation may contribute");
    assert!(!view.visible.contains("ZZZ"));
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Forward measurement
// ---------------------------------------------------------------------------

#[test]
fn a_forward_window_measures_the_crossing_and_both_excursions() {
    let series: Vec<PricePoint> =
        (0..20).map(|i| (at(14, 0, 0) + chrono::Duration::seconds(i * 10), 10.0 + i as f64 * 0.02)).collect();
    let (reached, mfe, mae, crossing) =
        forward(&series, at(14, 0, 0), 10.0, 300, 2.0, 120).unwrap();
    assert!(reached, "10.00 -> 10.38 clears +2%");
    assert!(mfe > 2.0);
    assert!(mae <= 0.0);
    assert!(crossing.is_some());
}

/// A sparse forward window cannot say the target was missed.
#[test]
fn a_gap_in_the_forward_window_yields_unknown_not_a_miss() {
    let series = vec![(at(14, 0, 0), 10.0), (at(14, 9, 0), 10.05)];
    assert_eq!(
        forward(&series, at(14, 0, 0), 10.0, 600, 2.0, 120),
        None,
        "a nine-minute gap cannot establish that the target was not reached"
    );
}

/// But a gap *after* a confirmed crossing does not un-cross it.
#[test]
fn a_gap_after_the_crossing_does_not_make_it_unknown() {
    let series = vec![(at(14, 0, 0), 10.0), (at(14, 0, 30), 10.50), (at(14, 9, 0), 10.0)];
    let (reached, _, _, crossing) = forward(&series, at(14, 0, 0), 10.0, 600, 2.0, 120).unwrap();
    assert!(reached);
    assert_eq!(crossing, Some(at(14, 0, 30)));
}

#[test]
fn an_empty_forward_window_is_unknown() {
    assert_eq!(forward(&[], at(14, 0, 0), 10.0, 300, 2.0, 120), None);
    let series = vec![(at(15, 0, 0), 10.0)];
    assert_eq!(
        forward(&series, at(14, 0, 0), 10.0, 300, 2.0, 120),
        None,
        "prices outside the horizon do not answer the question"
    );
}

#[test]
fn the_reference_label_and_the_forward_window_agree_on_censoring() {
    // Both refuse to convert a gap into a negative; this pins them together so
    // one cannot drift from the other.
    let sparse = vec![(at(13, 30, 0), 10.0), (at(17, 0, 0), 10.05)];
    let label = labels::label("AAA", session_date_from(DAY).unwrap(), &sparse, &labels::ReferenceLabelSpec::default());
    assert_eq!(label.is_opportunity(0), None);
    assert_eq!(forward(&sparse, at(13, 30, 0), 10.0, 900, 2.0, 120), None);
}

#[test]
fn a_session_date_is_parsed_from_a_path() {
    assert_eq!(
        session_date_from("opportunity-intelligence-2026-09-17.ndjson"),
        Some(NaiveDate::from_ymd_opt(2026, 9, 17).unwrap())
    );
    assert_eq!(
        session_date_from("session-001-2026-09-16"),
        Some(NaiveDate::from_ymd_opt(2026, 9, 16).unwrap()),
        "a leading numeric prefix must not shift the match window"
    );
    assert_eq!(session_date_from("no-date-here"), None);
    assert_eq!(session_date_from(""), None);
    // A negative year is never a session date, however willing chrono is.
    assert_eq!(session_date_from("x-2026-09-16"), Some(NaiveDate::from_ymd_opt(2026, 9, 16).unwrap()));
    assert_eq!(session_date_from("20260917"), None, "an undelimited date is not the format");
}
