//! Non-blocking tap of trading-interruption statuses (halt / pause /
//! quotation-only / resume) for ws-server's panel-detection recorder
//! (2026-10-16).
//!
//! Separate from `status_tap` on purpose: that one is a single-consumer
//! seam owned by the observation layer (installed only when observation is
//! on), and `halt_log` is off in production (`HALT_LOG_ENABLED`) and only
//! answers per-symbol queries. This carries only the statuses that change
//! whether a symbol is trading, already classified, so the consumer never
//! needs the plan tables.
//!
//! * **`ScanEvent` is untouched** -- the client wire contract cannot change.
//! * **Never blocks the feed.** `offer` is `try_send`; a full channel is a
//!   counted drop.
//! * **Inert until installed.** With no consumer, `offer` checks one
//!   `OnceLock` and returns before classifying anything.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use ignition_detector::{classify_status, TradingStatus};
use tokio::sync::mpsc;

use crate::bar::Status;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltTransitionKind {
    /// Halt, LULD pause or quotation-only period.
    Halted,
    Resumed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HaltStatus {
    pub symbol: String,
    pub kind: HaltTransitionKind,
    pub status_code: String,
    pub reason_code: Option<String>,
    pub reason_message: Option<String>,
    /// Exchange timestamp of the status.
    pub market_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
}

struct Tap {
    tx: mpsc::Sender<HaltStatus>,
    dropped: AtomicU64,
}

static TAP: OnceLock<Tap> = OnceLock::new();

/// Installs the tap and returns its receiver. A second call returns `None`.
pub fn install(capacity: usize) -> Option<mpsc::Receiver<HaltStatus>> {
    let (tx, rx) = mpsc::channel(capacity.max(1));
    TAP.set(Tap { tx, dropped: AtomicU64::new(0) }).ok()?;
    Some(rx)
}

/// Maps a classified status to a transition kind; `None` for statuses that
/// do not change whether the symbol trades (indications, imbalances, band
/// updates, unknown codes).
pub fn transition_kind(class: TradingStatus) -> Option<HaltTransitionKind> {
    match class {
        TradingStatus::Halt | TradingStatus::Pause | TradingStatus::NonTradable => Some(HaltTransitionKind::Halted),
        TradingStatus::Resume => Some(HaltTransitionKind::Resumed),
        _ => None,
    }
}

/// Offers one received status. Non-blocking; a no-op unless installed.
pub fn offer(status: &Status, received_at: DateTime<Utc>) {
    let Some(tap) = TAP.get() else { return };
    let Some(kind) = transition_kind(classify_status(status.tape.as_deref(), &status.status_code)) else {
        return;
    };
    let message = HaltStatus {
        symbol: status.symbol.clone(),
        kind,
        status_code: status.status_code.clone(),
        reason_code: status.reason_code.clone(),
        reason_message: status.reason_message.clone(),
        market_at: status.timestamp,
        received_at,
    };
    if tap.tx.try_send(message).is_err() {
        tap.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

/// Statuses dropped because the consumer's channel was full or closed.
pub fn dropped() -> u64 {
    TAP.get().map_or(0, |t| t.dropped.load(Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_interruption_classes_map_to_a_transition() {
        assert_eq!(transition_kind(TradingStatus::Halt), Some(HaltTransitionKind::Halted));
        assert_eq!(transition_kind(TradingStatus::Pause), Some(HaltTransitionKind::Halted));
        assert_eq!(transition_kind(TradingStatus::NonTradable), Some(HaltTransitionKind::Halted));
        assert_eq!(transition_kind(TradingStatus::Resume), Some(HaltTransitionKind::Resumed));
        assert_eq!(transition_kind(TradingStatus::Informational), None);
        assert_eq!(transition_kind(TradingStatus::Unknown), None);
    }
}
