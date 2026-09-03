//! Live Alpaca market-data ingestion — the missing piece `fast-funnel`'s
//! doc comments forward-referenced. Connects to Alpaca's realtime bars
//! WebSocket, turns each incoming bar into a `fast_funnel::TickerSnapshot`
//! via `SessionTracker`, seeded from a one-shot REST call for the data the
//! stream itself doesn't carry (prior close, trailing avg volume).
//!
//! Deliberately does *not* depend on any particular runtime loop shape —
//! see `src/bin/scan.rs` for a runnable example that wires this into
//! `fast_funnel::explain`/`run_fast_funnel`.

pub mod alpaca_json;
pub mod assess;
pub mod bar;
pub mod config;
pub mod events;
pub mod float_data;
pub mod history;
pub mod indices;
pub mod live;
pub mod movers;
pub mod qualify;
pub mod rest;
pub mod session;
pub mod trading_session;
pub mod universe;
pub mod ws;

pub use assess::{request_assessment, Assessment, MomentumReading};
pub use bar::{AlpacaMessage, Bar, Quote, Status, Trade};
pub use config::AlpacaConfig;
pub use events::{ConsolidationEventKind, HaltAlertLevel, IgnitionEventKind, ScanEvent};
pub use float_data::fetch_float_shares;
pub use history::fetch_gainers_for_date;
pub use indices::{fetch_markets_today, MarketIndexReading, MARKET_INDEX_PROXIES};
pub use live::{run_live_scan, CatalystRecord, SharedCatalysts};
pub use movers::{spawn_periodic_movers_scan, Mover, SharedTodayMovers, TodayMovers};
pub use qualify::{qualify_shortlist, SymbolQualification};
pub use rest::{fetch_daily_bar_series, fetch_daily_seeds, fetch_daily_seeds_as_of, fetch_recent_minute_bars, DailyBar, DailySeed};
pub use session::SessionTracker;
pub use trading_session::{classify_session, TradingSession};
pub use universe::{fetch_snapshots, fetch_universe, scan_shortlist, QualifiedSymbol};
pub use ws::AlpacaStream;
