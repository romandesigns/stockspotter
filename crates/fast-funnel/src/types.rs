//! Shared data types for the fast funnel.
//!
//! `TickerSnapshot` is intentionally provider-agnostic: it's what both the
//! live Alpaca client and (later) the replay/backtest engine feed into the
//! filters. Same struct in, same filter functions run — that's what keeps
//! live and backtest on one code path per the architecture doc.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TickerSnapshot {
    pub symbol: String,
    /// Latest traded price, in dollars.
    pub price: f64,
    /// Free-float share count, if known. `None` means "unknown" and such
    /// tickers are excluded from Stage 1 rather than assumed to pass —
    /// see the float-data TODO in `alpaca.rs`.
    pub float_shares: Option<u64>,
    /// Trailing average daily volume (shares), used as the denominator for
    /// relative volume.
    pub avg_daily_volume: u64,
    /// Cumulative **extended-session** volume for the current market day:
    /// every share printed since 04:00 America/New_York, premarket
    /// included, through the last completed minute (or the snapshot
    /// instant). `None` means *unknown* -- not zero -- and relative volume
    /// is then incomputable, so Stage 2 fails closed exactly as it does on
    /// unknown float.
    ///
    /// Why an `Option` and not a `u64` with 0 for unknown (2026-09-25, D7
    /// of docs/measurement-correctness-contract-2026-09-25.md): premarket,
    /// Alpaca's snapshot `dailyBar` is still *yesterday's* bar until the
    /// ~09:31 ET roll, and this field used to be filled from it
    /// unconditionally. Yesterday's runner then cleared the 5x relative
    /// volume gate all premarket on yesterday's shares while today's
    /// premarket runner could not. A plain number cannot say "I don't know
    /// today's volume yet", so every consumer silently used the wrong day.
    /// An `Option` makes each consumer decide.
    ///
    /// There is deliberately no separate regular-session (09:30-onward)
    /// figure here. If one is ever needed it gets its own field
    /// (`regular_session_volume`), never an overload of this one.
    pub session_volume: Option<u64>,
    /// Where `session_volume` came from. Serialized under the contract's
    /// own name, `sessionVolumeSource`, because discovery's
    /// `scan_completed.selection_inputs` persists this struct and the
    /// contract names that key; the tape must self-describe which rule
    /// produced each number.
    #[serde(rename = "sessionVolumeSource")]
    pub session_volume_source: SessionVolumeSource,
    /// Premarket/intraday gap vs. prior close, as a percentage (e.g. 12.5 = +12.5%).
    pub gap_pct: f64,
}

/// Provenance of `TickerSnapshot::session_volume`.
///
/// Invariant: `Unknown` if and only if `session_volume` is `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionVolumeSource {
    /// The provider's snapshot daily bar, and that bar belongs to the
    /// current market day (after the ~09:31 ET roll it already includes
    /// premarket volume).
    SnapshotDailyBarCurrent,
    /// Summed 1-minute bars since the market-day open (04:00 ET): the
    /// REST premarket resolver, the stream `SessionTracker` (backfilled
    /// from 04:00 ET) and the replay engine all produce this.
    MinuteBarsSinceOpen,
    /// Not known. `session_volume` is `None`.
    Unknown,
}

impl TickerSnapshot {
    /// Relative volume: session volume vs. trailing average. `None` if the
    /// average is zero (can't divide) or the ticker has no volume history yet.
    /// Also `None` when the session volume itself is unknown -- that is
    /// the fail-closed path for a premarket symbol whose snapshot daily
    /// bar is still yesterday's.
    pub fn relative_volume(&self) -> Option<f64> {
        if self.avg_daily_volume == 0 {
            return None;
        }
        Some(self.session_volume? as f64 / self.avg_daily_volume as f64)
    }
}

/// Thresholds for both funnel stages, pulled out into one struct so they can
/// be tuned/backtested without touching filter logic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FilterThresholds {
    // Stage 1 — static
    pub min_price: f64,
    pub max_price: f64,
    pub max_float_shares: u64,

    // Stage 2 — dynamic
    pub min_relative_volume: f64,
    pub min_gap_pct: f64,
}

impl Default for FilterThresholds {
    /// Defaults from docs/trading-scanner-architecture-part-3.md's
    /// explicit "Price Floor Decision": $0.25, not $1.50 or $1 (an
    /// earlier doc revision had briefly moved it to $1 — part-3
    /// deliberately reverts that, reasoning that sub-$1.50 signals are
    /// less reliable but "not willing to fully exclude the low end",
    /// especially for the low-float flat-base ignition pattern which
    /// specifically targets the ~$0.15-$0.25 range). Float < 20M, rel
    /// volume >= 5x, gap >= 10% are unchanged from section 4.1.
    fn default() -> Self {
        Self {
            min_price: 0.25,
            max_price: 20.0,
            max_float_shares: 20_000_000,
            min_relative_volume: 5.0,
            min_gap_pct: 10.0,
        }
    }
}
