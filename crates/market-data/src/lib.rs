//! Live Alpaca market-data ingestion — the missing piece `fast-funnel`'s
//! doc comments forward-referenced. Connects to Alpaca's realtime bars
//! WebSocket, turns each incoming bar into a `fast_funnel::TickerSnapshot`
//! via `SessionTracker`, seeded from a one-shot REST call for the data the
//! stream itself doesn't carry (prior close, trailing avg volume).
//!
//! Deliberately does *not* depend on any particular runtime loop shape —
//! see `src/bin/scan.rs` for a runnable example that wires this into
//! `fast_funnel::explain`/`run_fast_funnel`.

pub mod bar;
pub mod config;
pub mod rest;
pub mod session;
pub mod ws;

pub use bar::{AlpacaMessage, Bar};
pub use config::AlpacaConfig;
pub use rest::{fetch_daily_seeds, DailySeed};
pub use session::SessionTracker;
pub use ws::AlpacaStream;
