//! Post-Ignition Consolidation Breakout — architecture doc part-3's entry
//! strategy: rather than chasing the peak of a surge or waiting for a
//! full pullback (which may never come on low-float names), watch for a
//! brief consolidation after an initial surge and enter on breakout from
//! that range.
//!
//! **Isolation**: genuinely independent, not just claimed — the doc's
//! own Strategy Isolation principle says no detection system may read,
//! gate, or depend on another's state, and this crate honors that
//! literally. It does **not** depend on `ignition-detector` at all
//! (`replay-engine`/`market-data` appear only as `[dev-dependencies]`,
//! used solely by `examples/`, the same idiom `halt-detector` established
//! first) — the "initial surge" this strategy watches for is detected
//! from the same raw OHLCV candle stream every other strategy also
//! reads, independently, in `surge::detect_surge`. In practice a real
//! surge usually also trips the ignition detector's tick-level trigger
//! around the same time; that overlap is expected and useful (per the
//! doc: "a ticker can legitimately appear in multiple panels at once"),
//! not evidence of a hidden dependency between the two.
//!
//! Three modules, same "pure functions + stateful monitor" split as
//! every other detector in this workspace:
//! - `surge`: is there a real surge in the recent candle history?
//! - `consolidation`: is one candle a valid consolidation candle
//!   (volume contraction, range tightening, holding support), and does a
//!   candle count as a breakout of the consolidation range?
//! - `monitor`: `ConsolidationBreakoutMonitor`, the per-symbol state
//!   machine a live loop or replay actually calls `on_candle` on.
//!
//! **First real backtest (2026-08-31, `backtest-metrics --bin
//! tune_broad`, the same 27-session broad set momentum's threshold was
//! revised against):** the surge phase fires plenty on real data (56
//! surges at the current defaults across the 27 sessions, up to 136 at a
//! looser variant), but the doc's full multi-stage pattern (surge ->
//! several genuinely-contracting-and-tight consolidation candles ->
//! confirmed -> held breakout close) is a narrow filter — only 2
//! consolidations ever confirmed at the defaults, producing just 1 real
//! entry signal across all 27 sessions (9 confirmed / 3 entries at the
//! loosest variant tried). That's nowhere near enough signals to compute
//! a trustworthy hit rate either way — this is explicitly a "too rare to
//! evaluate yet" finding, not a "doesn't work" one, and deliberately
//! *not* used to retune `SurgeThresholds`/`ConsolidationThresholds` off
//! a sample of 1-3 (that would repeat the exact mistake
//! `momentum_scorer::DEFAULT_QUALIFY_THRESHOLD`'s original 0.90 made).
//! Confirmed working correctly on individual sessions via
//! `examples/replay_consolidation.rs` (NCRA 2026-07-29: 8 surges, 1
//! confirmed consolidation, 1 real breakout entry at a plausible price).
//! Revisit once an even broader session set (or a longer per-symbol
//! history) produces enough real entries to actually judge.

pub mod candle;
pub mod consolidation;
pub mod monitor;
pub mod surge;

pub use candle::Candle;
pub use consolidation::{breakout_triggered, is_valid_consolidation_candle, support_level, ConsolidationThresholds};
pub use monitor::{ConsolidationBreakoutConfig, ConsolidationBreakoutEvent, ConsolidationBreakoutMonitor};
pub use surge::{detect_surge, SurgeInfo, SurgeThresholds};
