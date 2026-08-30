//! Replay/backtest engine — architecture doc section 7. See `replay.rs`
//! for the actual "same code path, historical data" implementation.

pub mod historical;
pub mod replay;

pub use historical::{fetch_historical_bars, fetch_historical_quotes, fetch_historical_trades};
pub use replay::{replay_symbol, BarEvent, IgnitionEvent, IgnitionEventKind, ReplayResult};
