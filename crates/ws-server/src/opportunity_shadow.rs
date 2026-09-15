//! Opportunity Intelligence shadow persistence — an **independent consumer** of
//! the already-broadcast `ScanEvent` stream.
//!
//! Deliberately a sibling of `measurement.rs`, not an extension of it:
//!
//! * It subscribes to the same `broadcast` channel and receives the same events
//!   *after* they have been dispatched. It cannot reorder, suppress, delay or
//!   mutate anything a client or the auto-trader sees.
//! * Nothing it produces is read by a detector, by client ordering, or by
//!   `auto_trader`. There is no path from this module back into production --
//!   the only writer is an append-only research file.
//! * It is bounded in the same three places the measurement collector had to be
//!   (open state, queue depth, write path) and it counts its own drops. The
//!   `PendingCapacityReached` incident is the precedent: an unbounded or
//!   silently-saturating research subsystem is worse than none.
//!
//! Writes happen on a dedicated thread behind a bounded `sync_channel`, and the
//! market-facing side only ever `try_send`s, so disk latency can never reach
//! dispatch.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::Arc;

use backtest_metrics::opportunity::{
    OiConfig, OpportunityIntelligence, OpportunityScoreSnapshot,
};
use chrono::{DateTime, Utc};
use market_data::ScanEvent;
use tracing::{info, warn};

/// Bounded, matching `discovery_audit` and `measurement`. A backlog means the
/// writer cannot keep up, and the correct response is to drop and count rather
/// than grow memory in a realtime process.
const QUEUE_DEPTH: usize = 64;

#[derive(Debug)]
enum Record {
    Snapshot(Box<OpportunityScoreSnapshot>),
    Flush(std::sync::mpsc::Sender<()>),
}

/// Counters describing shadow-capture completeness. Non-zero values are
/// findings, not noise.
#[derive(Debug, Default)]
pub struct ShadowHealth {
    /// Snapshots discarded because the writer could not keep up.
    pub dropped: AtomicU64,
    /// Snapshots the writer accepted but failed to persist.
    pub write_errors: AtomicU64,
    pub snapshots_written: AtomicU64,
}

impl ShadowHealth {
    pub fn is_degraded(&self) -> bool {
        self.dropped.load(Ordering::Relaxed) > 0 || self.write_errors.load(Ordering::Relaxed) > 0
    }
}

/// Append-only NDJSON writer for shadow scoring decisions.
pub struct ShadowRecorder {
    tx: SyncSender<Record>,
    health: Arc<ShadowHealth>,
}

impl ShadowRecorder {
    /// Starts the writer, or returns `None` when the directory is unusable --
    /// research capture must degrade to *off*, never take the service down.
    pub fn start(dir: PathBuf) -> Option<Self> {
        Self::start_inner(dir, QUEUE_DEPTH, None)
    }

    /// `gate`, when supplied, is waited on before the writer drains anything.
    ///
    /// It exists so the queue bound is *exercisable* rather than merely
    /// asserted: with a live writer thread a 64-deep channel never fills in a
    /// test, and a drop counter no test can reach is indistinguishable from a
    /// drop counter that does not work. That is the measurement lesson applied
    /// to the measurement code itself.
    fn start_inner(
        dir: PathBuf,
        depth: usize,
        gate: Option<Arc<std::sync::Barrier>>,
    ) -> Option<Self> {
        if let Err(error) = std::fs::create_dir_all(&dir) {
            warn!(%error, ?dir, "opportunity-intelligence capture disabled: cannot create directory");
            return None;
        }
        let (tx, rx) = sync_channel::<Record>(depth);
        let health = Arc::new(ShadowHealth::default());
        let writer_health = health.clone();
        let logged_dir = dir.clone();
        std::thread::spawn(move || {
            if let Some(gate) = gate {
                gate.wait();
            }
            for record in rx {
                match record {
                    Record::Snapshot(snapshot) => {
                        let path = dir.join(format!(
                            "opportunity-intelligence-{}.ndjson",
                            snapshot.timestamp.date_naive()
                        ));
                        match append_json(&path, &*snapshot) {
                            Ok(()) => {
                                writer_health.snapshots_written.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(error) => {
                                let n =
                                    writer_health.write_errors.fetch_add(1, Ordering::Relaxed) + 1;
                                if n.is_power_of_two() {
                                    warn!(%error, write_errors = n,
                                        "opportunity-intelligence write failed");
                                }
                            }
                        }
                    }
                    Record::Flush(reply) => {
                        let _ = reply.send(());
                    }
                }
            }
        });
        info!(path = %logged_dir.display(), "opportunity-intelligence shadow capture enabled");
        Some(Self { tx, health })
    }

    pub fn health(&self) -> &ShadowHealth {
        &self.health
    }

    /// Non-blocking. A full queue drops and counts; it never waits, so disk
    /// latency cannot reach market dispatch.
    pub fn record(&self, snapshot: OpportunityScoreSnapshot) {
        if self.tx.try_send(Record::Snapshot(Box::new(snapshot))).is_err() {
            let n = self.health.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_power_of_two() {
                warn!(dropped = n, "opportunity-intelligence queue full; snapshots dropped");
            }
        }
    }

    /// Bounded drain, for shutdown only.
    pub fn flush(&self, timeout: std::time::Duration) {
        let (tx, rx) = std::sync::mpsc::channel();
        if self.tx.send(Record::Flush(tx)).is_ok() {
            let _ = rx.recv_timeout(timeout);
        }
    }
}

fn append_json<T: serde::Serialize>(path: &std::path::Path, value: &T) -> anyhow::Result<()> {
    use std::io::Write;
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&line)?;
    Ok(())
}

/// Drives `OpportunityIntelligence` from a live event stream and persists the
/// ranking snapshots it produces.
///
/// The engine itself is shared with offline replay (`replay_stream` below), so
/// live and replay cannot drift apart: there is exactly one implementation of
/// the research logic.
pub struct ShadowDriver {
    engine: OpportunityIntelligence,
    recorder: Option<ShadowRecorder>,
}

impl ShadowDriver {
    pub fn new(config: OiConfig, recorder: Option<ShadowRecorder>) -> Self {
        Self { engine: OpportunityIntelligence::new(config), recorder }
    }

    #[cfg(test)]
    pub fn engine(&self) -> &OpportunityIntelligence {
        &self.engine
    }

    /// Capture completeness, or `None` when capture is off. Exposed so a
    /// caller can distinguish "no records" from "records lost", which is the
    /// distinction the measurement milestone had to add retroactively.
    #[cfg(test)]
    pub fn capture_health(&self) -> Option<&ShadowHealth> {
        self.recorder.as_ref().map(|r| r.health())
    }

    /// Folds one already-broadcast event in. Returns the snapshots produced, so
    /// a caller (or a test) can inspect them without reading the file.
    pub fn observe(
        &mut self,
        event: &ScanEvent,
        received_at: DateTime<Utc>,
    ) -> Vec<OpportunityScoreSnapshot> {
        // Closed opportunities are not persisted here: the shadow log records
        // *scoring decisions*, and outcomes are joined later by the existing
        // measurement system rather than duplicated into a second schema.
        let _closed = self.engine.observe(event, received_at);
        let snapshots = self.engine.rank(received_at).unwrap_or_default();
        if let Some(recorder) = &self.recorder {
            for snapshot in &snapshots {
                recorder.record(snapshot.clone());
            }
        }
        snapshots
    }

    pub fn finish(&mut self, at: DateTime<Utc>) {
        let _ = self.engine.finish(at);
        if let Some(recorder) = &self.recorder {
            recorder.flush(std::time::Duration::from_secs(5));
            let health = recorder.health();
            let h = self.engine.health();
            if health.is_degraded() {
                warn!(
                    dropped = health.dropped.load(Ordering::Relaxed),
                    write_errors = health.write_errors.load(Ordering::Relaxed),
                    "opportunity-intelligence capture finished with gaps"
                );
            }
            // Always reported, pass or fail -- saturation must be establishable
            // without inferring it from the data afterwards.
            info!(
                peak_open = h.peak_open_opportunities,
                capacity_evictions = h.capacity_evictions,
                cohort_truncations = h.cohort_truncations,
                opportunities_opened = h.opportunities_opened,
                scores_emitted = h.scores_emitted,
                "opportunity-intelligence shadow summary"
            );
        }
    }
}

#[cfg(test)]
#[path = "opportunity_shadow_tests.rs"]
mod tests;
