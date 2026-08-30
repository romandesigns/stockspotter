//! Bullish momentum scorer — architecture doc section 4.2. Pure,
//! synchronous, provider-agnostic (same philosophy as `fast_funnel`): runs
//! identically whether fed by a live Alpaca bar stream or a replay engine.

pub mod candle;
pub mod scorer;

pub use candle::{Candle, RollingWindow};
pub use scorer::{score, MomentumScore, MomentumWeights, DEFAULT_QUALIFY_THRESHOLD};
