//! Opportunity-native forward outcome capture.
//!
//! A FIFTH research artifact, parallel to `episodes`,
//! `opportunity-intelligence` and `discovery`, and merged into none of them.
//!
//! # Why it exists
//!
//! Episode outcomes attach through episode membership, and membership is not
//! independent of the score. On 2026-09-17, opportunities with
//! `membershipStatus = no_episodes` (~55% of the population, rising to 87% of
//! Friday's top V2 decile) carried a forward excursion **0.00% of the time in
//! every V2 decile**. That is structural non-measurement, not censoring, and
//! it makes any MFE-based ranking comparison a score-selected subsample.
//!
//! Here, every admitted ranking snapshot becomes an anchor and every anchor
//! eventually becomes a row. Whether a row exists depends on nothing but
//! capacity.
//!
//! # What this module is, and is not
//!
//! It is the *wiring*: a writer, and the glue that turns ranking snapshots
//! into anchors and market events into forward prices. The measurement itself
//! lives in `backtest_metrics::opportunity_outcome`, is shared with offline
//! replay, and was validated against an independently written reference on
//! three real-data slices with zero mismatches.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use backtest_metrics::opportunity::OpportunityScoreSnapshot;
use backtest_metrics::opportunity_outcome::{
    AnchorProvenance, AnchorRequest, OpportunityOutcomeCollector, OpportunityOutcomeRow,
    OutcomeHealth, OPPORTUNITY_OUTCOME_VERSION,
};
use chrono::{DateTime, NaiveTime, Utc};
use market_data::ScanEvent;

use crate::research_writer::{Bounds, Naming, ResearchWriter, WriterHealth};

const STEM: &str = "opportunity-outcomes";

/// Queue depth, in records.
///
/// **One whole settlement burst.** `settle_due` releases every anchor whose
/// deadline has passed in one synchronous loop, and because anchors are
/// created a full ranking cohort at a time, a burst is one cohort. The
/// measured peak cohort over the D2-corrected sessions is 4,308
/// (2026-09-18); `OiConfig::max_open_opportunities` bounds it at 16,375. Sized
/// to the engine bound rather than the observed peak, for the same reason the
/// shadow recorder is: the queue must not be the thing that binds first.
const QUEUE_RECORDS: usize = 16_384;

/// Queue bound in bytes.
///
/// Measured: the full 2026-09-17 replay wrote 3,442,365,428 B over 2,540,306
/// rows, a mean of **1,355 B/row**. A full 16,384-record burst is therefore
/// ~22 MB; 64 MiB holds that with room for rows well above the mean, and caps
/// the writer's worst-case footprint at a number that can be stated rather
/// than hoped for.
const QUEUE_BYTES: u64 = 64 * 1024 * 1024;

/// Regular-session close, UTC. Horizons reaching past it are censored
/// `SessionEnded` rather than silently shortened.
const SESSION_CLOSE_UTC: (u32, u32) = (20, 0);

pub type OutcomeCaptureHealth = WriterHealth;

/// Append-only NDJSON writer for opportunity-native outcome rows.
pub struct OutcomeRecorder {
    writer: ResearchWriter,
}

impl OutcomeRecorder {
    /// Starts the writer, or `None` when the directory is unusable. Research
    /// capture degrades to *off*; it never takes the service down.
    pub fn start(dir: PathBuf) -> Option<Self> {
        Self::start_bounded(dir, Bounds { records: QUEUE_RECORDS, bytes: QUEUE_BYTES }, None)
    }

    /// `gate` blocks the writer before it drains, so the queue bound is
    /// *exercisable*. With a live writer a 16,384-deep channel never fills in
    /// a test, and a drop counter no test can reach is indistinguishable from
    /// one that does not work.
    #[cfg(test)]
    pub fn start_inner(
        dir: PathBuf,
        depth: usize,
        gate: Option<Arc<std::sync::Barrier>>,
    ) -> Option<Self> {
        Self::start_bounded(dir, Bounds { records: depth, bytes: u64::MAX / 2 }, gate)
    }

    fn start_bounded(
        dir: PathBuf,
        bounds: Bounds,
        gate: Option<Arc<std::sync::Barrier>>,
    ) -> Option<Self> {
        let naming = Naming { dir, stem: STEM.to_string() };
        ResearchWriter::start_inner(naming, bounds, gate).map(|writer| Self { writer })
    }

    pub fn health(&self) -> &Arc<OutcomeCaptureHealth> {
        self.writer.health()
    }

    /// Non-blocking. A full queue drops, counts, and persists a `queue_loss`
    /// marker in band -- it never waits, so disk latency cannot reach market
    /// dispatch.
    pub fn record(&self, row: &OpportunityOutcomeRow) {
        self.writer.record(row, row.anchor_at.date_naive());
    }

    pub fn marker(&self, kind: &str, data: Option<serde_json::Value>) {
        self.writer.marker(kind, data);
    }

    pub fn flush(&self, timeout: std::time::Duration) {
        self.writer.flush(timeout);
    }
}

/// Extracts a forward price observation from a market event.
///
/// Deliberately the same rule `measurement::observed_price` applies to
/// episodes, restated here rather than shared because the two subsystems must
/// be able to disagree in future without one silently changing the other:
///
/// * a **finalised** bar's close is a fact about the END of the bar, so it
///   becomes observable one interval after the bar's opening timestamp;
/// * an in-progress bucket rebroadcasts with `timestamp` pinned to
///   `bucket_start`, so using it would assign many prices one artificial
///   instant -- the receipt clock is the only honest answer available.
pub fn forward_price(
    event: &ScanEvent,
    received_at: DateTime<Utc>,
) -> Option<(String, DateTime<Utc>, f64)> {
    match event {
        ScanEvent::IgnitionEvent { symbol, timestamp, price, .. }
        | ScanEvent::ConsolidationEvent { symbol, timestamp, price, .. }
        | ScanEvent::FunnelSignal { symbol, timestamp, price, .. } => {
            Some((symbol.clone(), *timestamp, *price))
        }
        ScanEvent::HaltWarning { symbol, timestamp, current_price, .. } => {
            Some((symbol.clone(), *timestamp, *current_price))
        }
        ScanEvent::BarUpdate {
            symbol, timestamp, close, is_final: true, interval_secs, ..
        } => Some((
            symbol.clone(),
            *timestamp + chrono::Duration::seconds(i64::from(*interval_secs)),
            *close,
        )),
        ScanEvent::BarUpdate { symbol, close, is_final: false, .. } => {
            Some((symbol.clone(), received_at, *close))
        }
        _ => None,
    }
}

/// The collector's accounting, published as atomics so the research health
/// surface can read it while the session is still running.
///
/// Same reasoning as `opportunity_shadow::EngineHealth`: the collector lives
/// inside a tokio task that owns it for the life of the process, so without
/// this its counters would be knowable only at shutdown -- which is exactly
/// when learning that capacity bound is too late to act on.
#[derive(Debug, Default)]
pub struct OutcomeEngineHealth {
    pub outstanding: AtomicU64,
    pub peak_outstanding: AtomicU64,
    pub capacity: AtomicU64,
    pub anchors_created: AtomicU64,
    pub anchors_settled: AtomicU64,
    pub capacity_evictions: AtomicU64,
    pub symbols_tracked: AtomicU64,
}

impl OutcomeEngineHealth {
    pub fn snapshot(&self) -> OutcomeHealth {
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        OutcomeHealth {
            outstanding: g(&self.outstanding) as usize,
            peak_outstanding: g(&self.peak_outstanding) as usize,
            capacity: g(&self.capacity) as usize,
            anchors_created: g(&self.anchors_created),
            anchors_settled: g(&self.anchors_settled),
            capacity_evictions: g(&self.capacity_evictions),
            symbols_tracked: g(&self.symbols_tracked) as usize,
        }
    }
}

/// Owns the collector and the writer, and turns ranking snapshots into anchors.
pub struct OutcomeDriver {
    collector: OpportunityOutcomeCollector,
    recorder: Option<OutcomeRecorder>,
    provenance: Arc<AnchorProvenance>,
    engine_health: Arc<OutcomeEngineHealth>,
}

impl OutcomeDriver {
    pub fn new(
        recorder: Option<OutcomeRecorder>,
        versions: &backtest_metrics::opportunity::OiVersions,
    ) -> Self {
        Self {
            collector: OpportunityOutcomeCollector::new(),
            recorder,
            engine_health: Arc::new(OutcomeEngineHealth::default()),
            // Shared across every anchor: identical for the life of a run, and
            // storing it per anchor cost 284 B each -- 84 MB at capacity.
            provenance: Arc::new(AnchorProvenance {
                measurement_version: OPPORTUNITY_OUTCOME_VERSION.to_string(),
                opportunity_schema: versions.opportunity_schema,
                feature_schema: versions.feature_schema,
                early_quality_model: versions.early_quality_model.clone(),
                continuation_model: versions.continuation_model.clone(),
                ranking: versions.ranking.clone(),
                score_policy: versions.score_policy.clone(),
                config_fingerprint: versions.config_fingerprint.clone(),
            }),
        }
    }

    /// Test-only: production reads the same counters through the health
    /// surface, which holds the `Arc` directly rather than reaching through
    /// the driver.
    #[cfg(test)]
    pub fn health(&self) -> OutcomeHealth {
        self.collector.health()
    }

    /// Shared handle onto the collector's accounting, for the health surface.
    pub fn engine_health(&self) -> &Arc<OutcomeEngineHealth> {
        &self.engine_health
    }

    /// Republishes the collector's counters into the shared atomics.
    ///
    /// Seven relaxed stores, on a path that already folded a price into every
    /// anchor for the symbol.
    fn publish_health(&self) {
        let h = self.collector.health();
        let s = &self.engine_health;
        s.outstanding.store(h.outstanding as u64, Ordering::Relaxed);
        s.peak_outstanding.store(h.peak_outstanding as u64, Ordering::Relaxed);
        s.capacity.store(h.capacity as u64, Ordering::Relaxed);
        s.anchors_created.store(h.anchors_created, Ordering::Relaxed);
        s.anchors_settled.store(h.anchors_settled, Ordering::Relaxed);
        s.capacity_evictions.store(h.capacity_evictions, Ordering::Relaxed);
        s.symbols_tracked.store(h.symbols_tracked as u64, Ordering::Relaxed);
    }

    /// Test-only, same reason.
    #[cfg(test)]
    pub fn capture_health(&self) -> Option<&Arc<OutcomeCaptureHealth>> {
        self.recorder.as_ref().map(|r| r.health())
    }

    /// Publishes one forward price into every anchor for that symbol.
    ///
    /// Runs on EVERY market event, before ranking, and independently of
    /// whether any opportunity for the symbol is still open. That
    /// independence is the whole point: an anchor keeps measuring after its
    /// opportunity is closed by inactivity, by a session boundary, or by
    /// capacity eviction.
    pub fn observe_price(&mut self, event: &ScanEvent, received_at: DateTime<Utc>) {
        if let Some((symbol, at, price)) = forward_price(event, received_at) {
            self.collector.observe_price(&symbol, at, price);
        }
    }

    /// Creates one anchor per ranking snapshot, then settles whatever is due.
    ///
    /// **Admission consults nothing but capacity.** `AnchorRequest` carries no
    /// score, no rank, no episode and no detector, so there is no path by
    /// which any of them could influence whether a row exists.
    pub fn anchor_and_settle(
        &mut self,
        snapshots: &[OpportunityScoreSnapshot],
        now: DateTime<Utc>,
    ) {
        for snapshot in snapshots {
            let session_end = session_close(snapshot.timestamp);
            let evicted = self.collector.anchor(AnchorRequest {
                opportunity_id: snapshot.opportunity_id.clone(),
                window_id: snapshot.window_id.clone(),
                symbol: snapshot.symbol.clone(),
                session_date: snapshot.session_date.clone(),
                anchor_at: snapshot.timestamp,
                signal_price: snapshot.current_price,
                opened_at: snapshot.opened_at,
                session_end,
                provenance: Arc::clone(&self.provenance),
            });
            // A capacity eviction is a measurement failure to report, never a
            // row to drop.
            self.emit(evicted);
        }
        let settled = self.collector.settle_due(now);
        self.emit(settled);
        self.publish_health();
    }

    /// Settles everything outstanding as `CaptureEnded` -- censored, never
    /// dropped -- and drains the writer.
    pub fn finish(&mut self, at: DateTime<Utc>) {
        let rows = self.collector.finish(at);
        self.emit(rows);
        self.publish_health();
        if let Some(recorder) = &self.recorder {
            recorder.marker(
                "outcome_capture_finished",
                serde_json::to_value(self.collector.health()).ok(),
            );
            recorder.flush(std::time::Duration::from_secs(5));
        }
    }

    fn emit(&self, rows: Vec<OpportunityOutcomeRow>) {
        if let Some(recorder) = &self.recorder {
            for row in &rows {
                recorder.record(row);
            }
        }
    }
}

/// Regular-session close for the day an anchor belongs to.
fn session_close(at: DateTime<Utc>) -> DateTime<Utc> {
    let (h, m) = SESSION_CLOSE_UTC;
    at.date_naive()
        .and_time(NaiveTime::from_hms_opt(h, m, 0).expect("20:00 is a valid time"))
        .and_utc()
}

#[cfg(test)]
#[path = "opportunity_outcomes_tests.rs"]
mod tests;
