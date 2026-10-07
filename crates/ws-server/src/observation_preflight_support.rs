//! Small shared fixtures for the Step 4B test modules.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, Utc};

use super::*;

/// A one-candidate window scored at `price`.
pub fn window1(id: &str, sym: &str, opened_at: DateTime<Utc>, price: f64, at: DateTime<Utc>) -> WindowInput {
    let oid = format!("{sym}:1");
    WindowInput {
        window_id: id.into(),
        processing_started_at: at,
        rank_completed_at: at,
        processing_started_mono: None,
        rank_completed_mono: None,
        open: vec![OpenCandidate { opportunity_id: oid.clone(), symbol: sym.into(), opened_at }],
        scored: BTreeSet::from([oid.clone()]),
        engine_prices: BTreeMap::from([(oid, price)]),
        cohort_truncated: false,
    }
}

/// A permissive live configuration rooted at `root`.
pub fn config(root: &Path) -> ObserverConfig {
    ObserverConfig {
        root: root.to_path_buf(),
        namespace: "main".into(),
        pid: 7,
        capture_max_bytes: u64::MAX / 2,
        capture_warn_permille: CAPTURE_WARN_PERMILLE,
        rotate_bytes: DEFAULT_ROTATE_BYTES,
        queue_records: PROPOSED_QUEUE_RECORDS,
        queue_bytes: PROPOSED_QUEUE_BYTES,
        overhead: OverheadLimits { stop_window_micros: u64::MAX, stop_duty_ppm: u64::MAX, ..OverheadLimits::default() },
        identity: RunIdentity::default(),
    }
}
