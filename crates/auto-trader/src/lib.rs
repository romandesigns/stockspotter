//! Bin+lib shape (standard, low-risk Cargo pattern) — added so
//! `crates/ws-server` can depend on this crate and reuse
//! `journal::JournalEntry` directly for its own `/auto-trader/status`
//! endpoint, rather than hand-duplicating a multi-field type with real
//! drift risk (unlike the small, deliberate duplication of the tiny WS
//! handshake shapes in `client.rs` — a journal entry has far more fields
//! where a hand-copy could silently drift).

pub mod client;
pub mod config;
pub mod engine;
pub mod journal;
