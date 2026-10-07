//! Reading a captured session into an opportunity-level dataset (§10, §12).
//!
//! # What this reads, and what it deliberately does not
//!
//! Two independent captures, and only two:
//!
//! * **Opportunity Intelligence snapshots** — what the platform thought, window
//!   by window. The candidate.
//! * **Discovery capture** — the whole-market ignition stream, which the
//!   platform did not select and cannot have biased. The independent price
//!   series and the reference population both come from here.
//!
//! The measurement capture (`episodes-*.ndjson`) is read for settlement
//! evidence only. Outcomes are computed from the *discovery* price series
//! rather than from measurement's own settled horizons, deliberately: one price
//! source for the candidate and the control means the comparison cannot be an
//! artefact of two pipelines disagreeing, and it keeps the reference label
//! independent of anything the platform decided.
//!
//! # Streaming
//!
//! A full-fidelity session is ~9 GB of snapshots. Nothing here holds a file in
//! memory: every reader streams line by line and accumulates per-opportunity
//! state, which is a few tens of thousands of small records. The outputs are
//! small even when the inputs are not.
//!
//! # Compressed artifacts
//!
//! `export_session.py` writes `.ndjson.gz`. Qualification requires the
//! *uncompressed* files: decompressing here would mean either a new dependency
//! or a hand-rolled DEFLATE, and `gzip -d` plus the export's own `SHA256SUMS`
//! already does the job better. A compressed artifact is detected, reported,
//! and refused rather than silently skipped.

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::alpha::sha256;
use crate::completeness::ArtifactEvidence;
use crate::horizon::PricePoint;
use crate::opportunity::OpportunityScoreSnapshot;

/// The files one captured session is made of.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionArtifacts {
    pub session_date: String,
    pub root: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oi_snapshots: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episodes: Option<PathBuf>,
    pub markers: Vec<PathBuf>,
    pub discovery: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<PathBuf>,
    /// Artifacts found in a compressed form, which qualification refuses.
    pub compressed: Vec<PathBuf>,
}

impl SessionArtifacts {
    /// Finds the artifacts of `session_date` under `root`.
    ///
    /// Looks in `root`, `root/research` and `root/discovery-audit`, which is
    /// the layout both the live `data/` directory and the preserved
    /// instrument-validation artifact use.
    pub fn discover(root: &Path, session_date: &str) -> Self {
        let mut found = Self {
            session_date: session_date.to_string(),
            root: root.to_path_buf(),
            ..Self::default()
        };
        for directory in [root.to_path_buf(), root.join("research"), root.join("discovery-audit")] {
            let Ok(entries) = std::fs::read_dir(&directory) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.contains(session_date) {
                    continue;
                }
                if name.ends_with(".gz") || name.ends_with(".zst") {
                    found.compressed.push(path);
                    continue;
                }
                if name.contains("-markers-") {
                    found.markers.push(path);
                } else if name.starts_with("opportunity-intelligence-") && name.ends_with(".ndjson")
                {
                    found.oi_snapshots = Some(path);
                } else if name.starts_with("episodes-") && name.ends_with(".ndjson") {
                    found.episodes = Some(path);
                } else if name.ends_with(".jsonl") {
                    found.discovery.push(path);
                } else if name.contains("completeness") && name.ends_with(".json") {
                    found.health = Some(path);
                }
            }
        }
        found.markers.sort();
        found.discovery.sort();
        found
    }

    /// Paths that must exist for the session to mean anything.
    pub fn required(&self) -> Vec<String> {
        let mut required = Vec::new();
        if let Some(path) = &self.oi_snapshots {
            required.push(relative(&self.root, path));
        }
        if let Some(path) = &self.episodes {
            required.push(relative(&self.root, path));
        }
        if let Some(first) = self.discovery.first() {
            required.push(relative(&self.root, first));
        }
        required
    }
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// What one line of an artifact turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Parsed,
    Malformed,
}

/// Streams a file, counting records and malformed lines and detecting a
/// truncated tail.
///
/// A file whose final line lacks its terminator was cut mid-record. That is
/// blocking: the last record is not merely missing, it is *partially present*,
/// and a reader that parsed it would get a silently truncated object.
fn inspect<F>(path: &Path, mut classify: F) -> std::io::Result<(u64, u64, u64, bool)>
where
    F: FnMut(&str) -> LineKind,
{
    let file = std::fs::File::open(path)?;
    let bytes = file.metadata()?.len();
    let mut reader = std::io::BufReader::with_capacity(1 << 20, file);
    let mut records = 0u64;
    let mut malformed = 0u64;
    let mut line = String::new();
    let mut ended_with_newline = true;
    loop {
        line.clear();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            break;
        }
        ended_with_newline = line.ends_with('\n');
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            continue;
        }
        records += 1;
        if classify(trimmed) == LineKind::Malformed {
            malformed += 1;
        }
    }
    Ok((bytes, records, malformed, !ended_with_newline && records > 0))
}

/// Artifact integrity and provenance evidence (§10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrityReport {
    pub artifacts: Vec<ArtifactEvidence>,
    /// SHA-256 of each artifact, so the qualification output can be tied to
    /// exact bytes.
    pub digests: BTreeMap<String, String>,
    /// Compressed artifacts, which qualification refuses to read.
    pub compressed: Vec<String>,
    /// Problems that stop the pipeline before any Alpha claim.
    pub blocking: Vec<String>,
}

/// Inspects every artifact: presence, size, record count, parseability,
/// truncation and digest.
pub fn integrity(artifacts: &SessionArtifacts) -> IntegrityReport {
    let mut report = IntegrityReport {
        artifacts: Vec::new(),
        digests: BTreeMap::new(),
        compressed: artifacts.compressed.iter().map(|p| relative(&artifacts.root, p)).collect(),
        blocking: Vec::new(),
    };

    for path in &report.compressed {
        report.blocking.push(format!(
            "{path} is compressed; decompress it (and verify its SHA256SUMS) before qualifying — \
             qualification reads uncompressed NDJSON only"
        ));
    }

    let mut inspect_one = |path: &Path, kind: &str| {
        let relative_path = relative(&artifacts.root, path);
        let classified = match kind {
            "oi" => inspect(path, |line| {
                match serde_json::from_str::<OpportunityScoreSnapshot>(line) {
                    Ok(_) => LineKind::Parsed,
                    Err(_) => LineKind::Malformed,
                }
            }),
            // Episodes and discovery are validated as JSON rather than against
            // their full types: the qualification does not depend on every
            // field of either, and a schema addition upstream should not read
            // as corruption here.
            _ => inspect(path, |line| match serde_json::from_str::<serde_json::Value>(line) {
                Ok(_) => LineKind::Parsed,
                Err(_) => LineKind::Malformed,
            }),
        };
        match classified {
            Ok((bytes, records, malformed, truncated)) => {
                if let Ok(digest) = sha256::hex_file(path) {
                    report.digests.insert(relative_path.clone(), digest);
                }
                report.artifacts.push(ArtifactEvidence {
                    path: relative_path,
                    present: true,
                    bytes,
                    records,
                    malformed_records: malformed,
                    truncated,
                });
            }
            Err(error) => {
                report.blocking.push(format!("{relative_path}: {error}"));
                report.artifacts.push(ArtifactEvidence {
                    path: relative_path,
                    present: false,
                    bytes: 0,
                    records: 0,
                    malformed_records: 0,
                    truncated: false,
                });
            }
        }
    };

    if let Some(path) = &artifacts.oi_snapshots {
        inspect_one(path, "oi");
    }
    if let Some(path) = &artifacts.episodes {
        inspect_one(path, "episodes");
    }
    for path in &artifacts.markers {
        inspect_one(path, "markers");
    }
    for path in &artifacts.discovery {
        inspect_one(path, "discovery");
    }
    report
}

// ---------------------------------------------------------------------------
// The opportunity-level dataset (§12)
// ---------------------------------------------------------------------------

/// One opportunity, collapsed from every ranking snapshot that mentioned it.
///
/// The analytical unit. Repeated snapshots contribute to the *timings* below
/// and never to a count — §27's requirement, made structural by there being no
/// per-window row in the dataset at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpportunityRow {
    pub opportunity_id: String,
    pub symbol: String,
    pub session_date: String,
    /// Derived as `timestamp - opportunityAgeSecs` on the first snapshot.
    pub opened_at: DateTime<Utc>,
    pub first_ranked_at: DateTime<Utc>,
    /// The window it first appeared in. Same-window controls are built by
    /// grouping on this, which is what makes them contemporaneous.
    pub first_window_id: String,
    pub first_price: f64,
    /// Windows this opportunity appeared in. Reported, never a denominator.
    pub windows: u32,
    /// Rank held in the opportunity's **first** ranking window.
    ///
    /// The cohort-membership key, not `best_*`. Membership decided by the best
    /// rank an opportunity ever held would use later windows to classify an
    /// earlier instant, which is look-ahead: the analysis would be choosing,
    /// after the fact, the moment each opportunity looked strongest.
    pub first_window_early_rank: Option<usize>,
    pub first_window_continuation_rank: Option<usize>,
    /// Best rank ever held. Reported, never a membership key.
    pub best_early_rank: Option<usize>,
    pub best_continuation_rank: Option<usize>,
    /// First instant at which it held each top-k early-quality rank.
    pub first_top_k_early: BTreeMap<usize, DateTime<Utc>>,
    pub first_top_k_continuation: BTreeMap<usize, DateTime<Utc>>,
    /// Whether the score was available at all, distinct from ranking badly.
    pub early_quality_available: bool,
    pub continuation_available: bool,
    /// Segmentation keys, taken from the first snapshot so they are causal.
    pub regime: String,
    pub price_band: Option<usize>,
    pub detectors: Vec<String>,
    pub confluence_count: usize,
    pub move_before_detection_pct: Option<f64>,
    /// Cohort size of the window this opportunity first entered, for the
    /// percentile surfaces.
    pub first_window_cohort_size: usize,
}

/// The whole session, at opportunity granularity.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpportunityDataset {
    pub rows: Vec<OpportunityRow>,
    /// Ranking windows seen, for the minimum-evidence check.
    pub windows: usize,
    /// Raw snapshot rows read, so collapse is visible rather than implied.
    pub raw_rows: u64,
    pub malformed_rows: u64,
    /// Per-window cohort sizes: the larger of the two surfaces' cohorts.
    /// Kept for the window count and for readers that predate the
    /// per-surface maps below; **not** a percentile denominator any more.
    pub window_cohort_sizes: BTreeMap<String, usize>,
    /// Per-window EarlyQuality cohort size -- the denominator for an
    /// EarlyQuality percentile surface in that window.
    ///
    /// Added with D6 (2026-09-25). Percentile thresholds used to be computed
    /// on `max(earlyCohortSize, continuationCohortSize)` for BOTH surfaces, so
    /// the smaller surface's top-p% was taken over the other surface's N --
    /// whether or not truncation occurred. Each surface is ranked
    /// independently, so each has its own N.
    #[serde(default)]
    pub window_early_cohort_sizes: BTreeMap<String, usize>,
    /// Per-window Continuation cohort size; same contract.
    #[serde(default)]
    pub window_continuation_cohort_sizes: BTreeMap<String, usize>,
}

/// Streams the OI capture into an opportunity-level dataset.
///
/// One pass, constant memory in the file size. Percentile surfaces need the
/// window's cohort size, which every snapshot already carries, so no second
/// pass over the file is required.
pub fn read_opportunities(path: &Path, top_k: &[usize]) -> std::io::Result<OpportunityDataset> {
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::with_capacity(1 << 20, file);
    let mut dataset = OpportunityDataset::default();
    let mut by_id: BTreeMap<String, OpportunityRow> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut line = String::new();

    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            continue;
        }
        dataset.raw_rows += 1;
        let Ok(snapshot) = serde_json::from_str::<OpportunityScoreSnapshot>(trimmed) else {
            dataset.malformed_rows += 1;
            continue;
        };
        let cohort = snapshot.early_cohort_size.max(snapshot.continuation_cohort_size);
        for (map, size) in [
            (&mut dataset.window_cohort_sizes, cohort),
            (&mut dataset.window_early_cohort_sizes, snapshot.early_cohort_size),
            (&mut dataset.window_continuation_cohort_sizes, snapshot.continuation_cohort_size),
        ] {
            map.entry(snapshot.window_id.clone())
                .and_modify(|existing| *existing = (*existing).max(size))
                .or_insert(size);
        }

        let entry = by_id.entry(snapshot.opportunity_id.clone()).or_insert_with(|| {
            order.push(snapshot.opportunity_id.clone());
            OpportunityRow {
                opportunity_id: snapshot.opportunity_id.clone(),
                symbol: snapshot.symbol.clone(),
                session_date: snapshot.session_date.clone(),
                opened_at: snapshot.timestamp
                    - chrono::Duration::seconds(snapshot.opportunity_age_secs),
                first_ranked_at: snapshot.timestamp,
                first_window_id: snapshot.window_id.clone(),
                first_price: snapshot.current_price,
                windows: 0,
                first_window_early_rank: snapshot.early_quality_rank,
                first_window_continuation_rank: snapshot.continuation_rank,
                best_early_rank: None,
                best_continuation_rank: None,
                first_top_k_early: BTreeMap::new(),
                first_top_k_continuation: BTreeMap::new(),
                early_quality_available: false,
                continuation_available: false,
                regime: serde_json::to_value(snapshot.regime)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_else(|| "unclassified".to_string()),
                price_band: snapshot.price_regime.as_ref().map(|p| p.band),
                detectors: snapshot.detectors_seen.clone(),
                confluence_count: snapshot.confluence_count,
                move_before_detection_pct: snapshot.move_before_detection_pct,
                first_window_cohort_size: cohort,
            }
        });

        entry.windows += 1;
        if snapshot.early_quality.value.is_some() {
            entry.early_quality_available = true;
        }
        if snapshot.continuation.value.is_some() {
            entry.continuation_available = true;
        }
        if let Some(rank) = snapshot.early_quality_rank {
            entry.best_early_rank =
                Some(entry.best_early_rank.map_or(rank, |best| best.min(rank)));
            for &k in top_k {
                if rank <= k {
                    entry.first_top_k_early.entry(k).or_insert(snapshot.timestamp);
                }
            }
        }
        if let Some(rank) = snapshot.continuation_rank {
            entry.best_continuation_rank =
                Some(entry.best_continuation_rank.map_or(rank, |best| best.min(rank)));
            for &k in top_k {
                if rank <= k {
                    entry.first_top_k_continuation.entry(k).or_insert(snapshot.timestamp);
                }
            }
        }
        // Detector confluence can only grow, and the latest set is the fullest
        // description of what saw this opportunity.
        if snapshot.confluence_count > entry.confluence_count {
            entry.confluence_count = snapshot.confluence_count;
            entry.detectors = snapshot.detectors_seen.clone();
        }
    }

    dataset.windows = dataset.window_cohort_sizes.len();
    dataset.rows = order.into_iter().filter_map(|id| by_id.remove(&id)).collect();
    Ok(dataset)
}

// ---------------------------------------------------------------------------
// The independent price series (§13)
// ---------------------------------------------------------------------------

/// Per-symbol prices, and which symbols the detectors produced an event for.
#[derive(Debug, Clone, Default)]
pub struct DiscoveryView {
    pub series: BTreeMap<String, Vec<PricePoint>>,
    /// Symbols with at least one ignition print of any stage. Visible to the
    /// platform.
    pub visible: std::collections::BTreeSet<String>,
    /// Symbols with a *confirmed* ignition — the detector stage.
    pub detected: std::collections::BTreeSet<String>,
    pub records: u64,
}

/// Streams discovery segments into a price series and the two stage sets.
///
/// Only `ignition` records are read. They are the ones that carry a per-symbol
/// price at a market timestamp, and they are the stream the OI engine itself
/// consumes — so the reference population is built from the same evidence the
/// platform saw, without using anything the platform *chose*.
pub fn read_discovery(paths: &[PathBuf], session_date: &str) -> std::io::Result<DiscoveryView> {
    let mut view = DiscoveryView::default();
    for path in paths {
        let file = std::fs::File::open(path)?;
        let mut reader = std::io::BufReader::with_capacity(1 << 20, file);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            let trimmed = line.trim_end_matches(['\n', '\r']);
            if trimmed.is_empty() {
                continue;
            }
            let Ok(record) = serde_json::from_str::<serde_json::Value>(trimmed) else { continue };
            if record["kind"].as_str() != Some("ignition") {
                continue;
            }
            let data = &record["data"];
            let Some(symbol) = data["symbol"].as_str() else { continue };
            let Some(price) = data["price"].as_f64() else { continue };
            let stamp = data["market_at"].as_str().or_else(|| record["recorded_at"].as_str());
            let Some(stamp) = stamp else { continue };
            let Ok(at) = stamp.parse::<DateTime<Utc>>() else { continue };
            if at.date_naive().to_string() != session_date {
                continue;
            }
            view.records += 1;
            view.visible.insert(symbol.to_string());
            if data["stage"].as_str() == Some("confirmed") {
                view.detected.insert(symbol.to_string());
            }
            if price.is_finite() && price > 0.0 {
                view.series.entry(symbol.to_string()).or_default().push((at, price));
            }
        }
    }
    for series in view.series.values_mut() {
        series.sort_by_key(|(at, _)| *at);
    }
    Ok(view)
}

/// The forward excursion available from `from`, over `horizon_secs`.
///
/// Returns `(reached_target, mfe_pct, mae_pct, first_crossing)`. `None` for the
/// whole tuple when the series is too sparse to answer — the same censoring
/// discipline the reference label applies.
pub fn forward(
    series: &[PricePoint],
    from: DateTime<Utc>,
    price: f64,
    horizon_secs: i64,
    target_pct: f64,
    max_gap_secs: i64,
) -> Option<(bool, f64, f64, Option<DateTime<Utc>>)> {
    if !price.is_finite() || price <= 0.0 {
        return None;
    }
    let until = from + chrono::Duration::seconds(horizon_secs);
    let window: Vec<PricePoint> =
        series.iter().copied().filter(|(at, _)| *at >= from && *at <= until).collect();
    if window.is_empty() {
        return None;
    }
    let threshold = price * (1.0 + target_pct / 100.0);
    let mut high = price;
    let mut low = price;
    let mut crossing = None;
    let mut previous = from;
    let mut largest_gap = 0i64;
    for (at, observed) in &window {
        largest_gap = largest_gap.max((*at - previous).num_seconds().max(0));
        previous = *at;
        high = high.max(*observed);
        low = low.min(*observed);
        if crossing.is_none() && *observed >= threshold {
            crossing = Some(*at);
        }
    }
    // A gap wider than the tolerance before the question is answered means the
    // answer is unknown, not "no".
    if crossing.is_none() {
        largest_gap = largest_gap.max((until - previous).num_seconds().max(0));
        if largest_gap > max_gap_secs {
            return None;
        }
    }
    Some((
        crossing.is_some(),
        (high - price) / price * 100.0,
        (low - price) / price * 100.0,
        crossing,
    ))
}

/// Session date parsed from a directory or file name, where it can be found.
pub fn session_date_from(text: &str) -> Option<NaiveDate> {
    let bytes = text.as_bytes();
    // The shape is checked before parsing, because `%Y` accepts a leading
    // minus: `session-001-2026-09-16` otherwise yields the window `-2026-09-1`
    // and parses as the year -2026. A wrong session date would silently
    // evaluate the wrong day.
    let shaped = |window: &[u8]| {
        window.len() == 10
            && window[..4].iter().all(u8::is_ascii_digit)
            && window[4] == b'-'
            && window[5..7].iter().all(u8::is_ascii_digit)
            && window[7] == b'-'
            && window[8..10].iter().all(u8::is_ascii_digit)
    };
    for start in 0..bytes.len().saturating_sub(9) {
        let window = &bytes[start..start + 10];
        if !shaped(window) {
            continue;
        }
        if let Ok(date) = NaiveDate::parse_from_str(&text[start..start + 10], "%Y-%m-%d") {
            return Some(date);
        }
    }
    None
}

#[cfg(test)]
#[path = "dataset_tests.rs"]
mod tests;
