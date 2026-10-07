//! Non-blocking tap of SIP trading-status messages for the observation layer.
//!
//! The live loop already receives full-market trading statuses (halts,
//! resumptions, with reason codes) and has only ever logged them. The
//! consumer-received observation layer needs them as *evidence*: an outcome
//! horizon may rely on halt knowledge only where the session's status record
//! is provably complete. This module is the seam between the two.
//!
//! * **`ScanEvent` is untouched.** Statuses travel on their own channel, so
//!   the client wire contract cannot change by accident.
//! * **Never blocks the feed.** `offer` is `try_send`: a full channel costs a
//!   *counted* drop, and any drop makes the run's status evidence incomplete.
//! * **Inert until installed.** With no observer, `offer` checks one
//!   `OnceLock` and returns. Connection state is tracked regardless, so an
//!   observer installed after the stream connected still learns whether the
//!   full-market status subscription is live.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Utc};
use tokio::sync::mpsc;

use crate::bar::Status;

/// One trading-status message as received.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusMessage {
    pub symbol: String,
    pub status_code: String,
    pub status_message: Option<String>,
    pub reason_code: Option<String>,
    pub reason_message: Option<String>,
    pub tape: Option<String>,
    /// Exchange timestamp of the status (`t`).
    pub market_at: DateTime<Utc>,
    /// When this process received it.
    pub received_at: DateTime<Utc>,
}

/// What the tap delivers.
#[derive(Debug, Clone, PartialEq)]
pub enum StatusTapEvent {
    Status(StatusMessage),
    /// A feed connection started. `full_market` is true when the full-market
    /// (`"*"`) status subscription was accepted on it.
    StreamStarted { connection: u64, full_market: bool, at: DateTime<Utc> },
    /// That connection ended (idle timeout, error, session rebuild).
    StreamEnded { connection: u64, at: DateTime<Utc> },
}

/// The connection state at a moment, for a run that starts mid-connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamState {
    pub connection: u64,
    pub full_market: bool,
    pub since: DateTime<Utc>,
}

struct Tap {
    tx: Mutex<Option<mpsc::Sender<StatusTapEvent>>>,
    offered: AtomicU64,
    dropped: AtomicU64,
}

static TAP: OnceLock<Tap> = OnceLock::new();
static STATE: Mutex<Option<StreamState>> = Mutex::new(None);
static CONNECTIONS: AtomicU64 = AtomicU64::new(0);

/// Installs the tap and returns its receiver. Called once, by the observation
/// layer, only when observation is enabled. A second call returns `None`.
pub fn install(capacity: usize) -> Option<mpsc::Receiver<StatusTapEvent>> {
    let (tx, rx) = mpsc::channel(capacity.max(1));
    let tap = Tap { tx: Mutex::new(Some(tx)), offered: AtomicU64::new(0), dropped: AtomicU64::new(0) };
    TAP.set(tap).ok()?;
    Some(rx)
}

fn send(event: StatusTapEvent) {
    let Some(tap) = TAP.get() else { return };
    tap.offered.fetch_add(1, Ordering::Relaxed);
    let guard = tap.tx.lock().unwrap_or_else(|e| e.into_inner());
    let delivered = match guard.as_ref() {
        Some(tx) => tx.try_send(event).is_ok(),
        None => false,
    };
    if !delivered {
        tap.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

/// Offers one received status. Non-blocking.
pub fn offer_status(status: &Status, received_at: DateTime<Utc>) {
    if TAP.get().is_none() {
        return;
    }
    send(StatusTapEvent::Status(StatusMessage {
        symbol: status.symbol.clone(),
        status_code: status.status_code.clone(),
        status_message: status.status_message.clone(),
        reason_code: status.reason_code.clone(),
        reason_message: status.reason_message.clone(),
        tape: status.tape.clone(),
        market_at: status.timestamp,
        received_at,
    }));
}

/// Records that a feed connection started; returns its id.
pub fn stream_started(full_market: bool, at: DateTime<Utc>) -> u64 {
    let connection = CONNECTIONS.fetch_add(1, Ordering::Relaxed) + 1;
    *STATE.lock().unwrap_or_else(|e| e.into_inner()) = Some(StreamState { connection, full_market, since: at });
    send(StatusTapEvent::StreamStarted { connection, full_market, at });
    connection
}

/// Records that the current feed connection ended.
pub fn stream_ended(at: DateTime<Utc>) {
    let previous = STATE.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(state) = previous {
        send(StatusTapEvent::StreamEnded { connection: state.connection, at });
    }
}

/// The live connection, if any.
pub fn current_state() -> Option<StreamState> {
    *STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// `(offered, dropped)` since install. Zero before install.
pub fn counters() -> (u64, u64) {
    TAP.get()
        .map(|t| (t.offered.load(Ordering::Relaxed), t.dropped.load(Ordering::Relaxed)))
        .unwrap_or((0, 0))
}
