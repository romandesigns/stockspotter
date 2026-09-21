//! Detection events broadcast to every connected client — the actual
//! payload behind "notifications/alerts", identical for web, desktop,
//! and mobile. Provider/transport-agnostic like `TickerSnapshot` etc.:
//! this module doesn't know about WebSockets, just describes what
//! happened. `crates/ws-server` is what turns these into wire messages
//! and fans them out.
//!
//! `camelCase` field naming matches `packages/shared-types`' existing TS
//! convention (`protocolVersion`, `serverTime`) — one wire contract, not
//! a per-language dialect of it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
fn default_estimated_bands() -> bool { true }

// `Deserialize` (added 2026-09-03 alongside `crates/auto-trader`) is new
// here -- every consumer before that got a `ScanEvent` handed to it
// directly as a Rust value (either off the broadcast channel in-process,
// like `live_signals`, or parsed independently in TypeScript on the two
// frontends), so nothing on the Rust side had ever needed to deserialize
// this type from its own wire JSON. `auto-trader` is the first Rust
// process that receives it as a real WS client and needs to parse it
// back — adding the derive is safe/symmetric for a `#[serde(tag = "type")]`
// enum and changes nothing about how this already-shipped type serializes.
/// How much of a bar's interval this process actually observed.
///
/// Deliberately separate from `is_final`, and the distinction is the whole
/// point. `is_final` means "a provider published this as an official or
/// corrected bar" -- an authority claim. Coverage answers a different
/// question: did we watch the whole interval? Overloading one flag with both
/// would make the two unanswerable independently, and the 2026-09-21 live
/// audit showed they genuinely diverge.
///
/// The witness: DDC's 15:37 UTC minute published a provisional volume of
/// 96,108 against an authoritative 119,482 -- 19.6% missing -- with only
/// ~0.3s of silence before the boundary. 23,374 shares cannot trade in
/// 300ms, so the loss was not a boundary tail. Chart coverage of that bucket
/// had begun mid-minute, and the published bar had no way to say so.
///
/// Time-finality is deliberately NOT represented here. A client can derive
/// it from `timestamp + interval_secs` against its own clock, so putting it
/// on the wire would be redundant. Coverage cannot be derived by anyone
/// except the producer that did or did not observe the interval, which is
/// exactly why it needs a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Coverage {
    /// Not established. The conservative default, and what an older frame
    /// without the field deserialises to. Never assume a bar with unknown
    /// coverage is complete -- that is the assumption this type exists to
    /// prevent.
    #[default]
    Unknown,
    /// Observation began at or before the bucket boundary and has been
    /// continuous since. A provider official/corrected bar is always
    /// complete, because it describes the whole interval by construction.
    Complete,
    /// Observation began after the bucket started, or a known hole exists.
    /// The OHLCV describes the observed portion only, from `observedFrom`
    /// to the bar's last observed trade.
    ///
    /// The window is carried inside the variant rather than as a sibling
    /// field so the two impossible states -- partial with no window,
    /// complete with one -- cannot be constructed at all.
    Partial { observed_from: DateTime<Utc> },
}

impl Coverage {
    /// Lets the field be omitted from the wire when nothing is known, which
    /// is what keeps this change additive for existing clients.
    pub fn is_unknown(&self) -> bool {
        matches!(self, Coverage::Unknown)
    }
    pub fn is_complete(&self) -> bool {
        matches!(self, Coverage::Complete)
    }
    pub fn is_partial(&self) -> bool {
        matches!(self, Coverage::Partial { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ScanEvent {
    #[serde(rename = "funnel_signal", rename_all = "camelCase")]
    FunnelSignal {
        symbol: String,
        timestamp: DateTime<Utc>,
        price: f64,
        gap_pct: f64,
        session_volume: u64,
        price_ok: bool,
        float_ok: bool,
        rel_vol_ok: bool,
        gap_ok: bool,
        passed: bool,
    },
    #[serde(rename = "momentum_update", rename_all = "camelCase")]
    MomentumUpdate {
        symbol: String,
        timestamp: DateTime<Utc>,
        volume_confirmation: f64,
        structure: f64,
        ma_slope: f64,
        wick_rejection: f64,
        overall: f64,
        qualifies: bool,
    },
    #[serde(rename = "ignition_event", rename_all = "camelCase")]
    IgnitionEvent {
        symbol: String,
        timestamp: DateTime<Utc>,
        price: f64,
        kind: IgnitionEventKind,
    },
    /// Post-Ignition Consolidation Breakout — per the doc's own Panels
    /// list this isn't a separate panel, it's an extra condition inside
    /// the Ignition panel (same treatment as the flat-base gate), so it
    /// shares this event's symbol/timestamp/price shape rather than
    /// getting its own top-level type. `strategy` distinguishes which of
    /// the two parallel `ConsolidationBreakoutMonitor` configs a symbol
    /// runs live.rs now produced this from (added 2026-09-03, the
    /// "micropullback" real-data finding) — same underlying pattern
    /// (surge -> consolidation -> breakout), tuned to catch a genuine
    /// single-candle micropullback the original 2-candle-minimum config
    /// structurally can't (see live.rs's own doc comment on why). Kept
    /// as one event shape with a tag rather than a second event type so
    /// clients don't need a whole parallel handling path for what's
    /// fundamentally the same signal at a different sensitivity.
    #[serde(rename = "consolidation_event", rename_all = "camelCase")]
    ConsolidationEvent {
        symbol: String,
        timestamp: DateTime<Utc>,
        price: f64,
        kind: ConsolidationEventKind,
        strategy: ConsolidationStrategy,
    },
    /// Health of the Stage-1 float-lookup budget, emitted once per
    /// universe rescan.
    ///
    /// Exists because the funnel's failure mode is invisible without it
    /// (2026-09-06): unknown float fails Stage 1 closed, so an exhausted
    /// FMP quota, a missing API key, and a genuinely quiet market all
    /// render as the same empty Gap & Go panel. `starvedCandidates > 0`
    /// is the precise "stocks cleared Stage 2 but we couldn't afford to
    /// check their float" condition — the panel is blind, not empty.
    #[serde(rename = "funnel_health", rename_all = "camelCase")]
    FunnelHealth {
        timestamp: DateTime<Utc>,
        /// FMP requests still available today, of `budget`.
        float_budget_remaining: u32,
        float_budget: u32,
        /// Stage-2 survivors this scan that went unchecked for lack of
        /// budget. Zero on a healthy scan.
        starved_candidates: usize,
        /// No `FMP_API_KEY` configured at all — same symptom, different
        /// cause, and a different fix for whoever is reading the panel.
        api_key_missing: bool,
    },
    /// Halt Early-Warning panel: a live proximity-to-halt reading for one
    /// symbol — sent on every trade for a symbol currently being tracked
    /// (not edge-triggered like the others), since a UI proximity gauge
    /// needs the current value continuously, not just transitions.
    #[serde(rename = "halt_warning", rename_all = "camelCase")]
    HaltWarning {
        #[serde(default = "default_estimated_bands")]
        estimated_bands: bool,
        symbol: String,
        timestamp: DateTime<Utc>,
        reference_price: f64,
        current_price: f64,
        band_width_dollars: f64,
        band_doubled: bool,
        proximity_ratio: f64,
        relative_volume: Option<f64>,
        level: HaltAlertLevel,
        /// False outside 9:30-16:00 ET on a weekday, when LULD bands
        /// aren't in force at all and `level` is pinned to `Calm`
        /// regardless of `proximity_ratio` (see
        /// `halt_detector::bands::luld_in_effect`). Sent to the client so
        /// the Halt panel can say "outside LULD hours" instead of
        /// silently showing every premarket gapper as calm.
        luld_in_effect: bool,
    },
    /// Super Chart panel: one raw OHLCV bar for a tracked symbol, straight
    /// from Alpaca's own bar (see `bar.rs`) with no funnel/scoring
    /// transformation applied — `FunnelSignal`'s `price`/`gapPct` etc. are
    /// derived values for the scanner panels, not what a candlestick chart
    /// needs to render. Sent alongside `FunnelSignal` on every bar for
    /// every tracked symbol (not edge-triggered) since a chart needs every
    /// bar, not just qualifying ones.
    ///
    /// `interval_secs` (added 2026-09-03, the real sub-minute multi-view
    /// finding) distinguishes which bucket width produced this bar — `60`
    /// for the existing 1-minute stream (Alpaca's own official bar, and
    /// the live-tick estimate for the still-forming minute), `30` for the
    /// new live-only sub-minute stream (`live::SUB_MINUTE_BUCKET_SECS`).
    /// Without this a client can't safely tell the two apart just from
    /// `timestamp`/OHLCV alone, and interleaving them into one array would
    /// corrupt whichever timeframe is currently displayed — every
    /// consumer of this event MUST filter on `interval_secs` before
    /// merging into its own bars array, not just the ones that care about
    /// sub-minute data.
    #[serde(rename = "bar_update", rename_all = "camelCase")]
    BarUpdate {
        #[serde(default)]
        is_final: bool,
        /// See `Coverage`. Defaults to `Unknown` so a frame from a server
        /// that predates this field deserialises conservatively rather than
        /// silently claiming completeness.
        #[serde(default, skip_serializing_if = "Coverage::is_unknown")]
        coverage: Coverage,
        symbol: String,
        timestamp: DateTime<Utc>,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        volume: u64,
        interval_secs: u32,
    },
    /// Catalysts panel: news catalyst tags for a symbol, from the Python
    /// qualitative layer (doc section 4.4). Fired once per symbol at
    /// promotion time (see `live.rs`) — catalysts don't change tick-by-
    /// tick the way price does, so this isn't a per-trade/per-bar event
    /// like the others.
    #[serde(rename = "catalyst_update", rename_all = "camelCase")]
    CatalystUpdate {
        symbol: String,
        /// When *this process* received the catalyst lookup -- observation
        /// time, not publication time. Causality is judged against this: we
        /// cannot have known a headline before we fetched it, so attaching a
        /// catalyst to a signal is only sound when this value precedes it.
        timestamp: DateTime<Utc>,
        catalyst_tags: Vec<String>,
        headline_count: u32,
        most_recent_headline: Option<String>,
        /// Publication time of the newest underlying headline, straight from
        /// the provider (Alpaca `created_at`). Distinct from `timestamp` and
        /// strictly less useful for causality -- but it is the only way to
        /// tell fresh news from a tag driven by a three-week-old headline,
        /// because the upstream lookup requests the 10 most recent items with
        /// no time window at all. Optional: absent when the symbol had no
        /// news, or on records written before this field existed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        most_recent_published_at: Option<DateTime<Utc>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IgnitionEventKind {
    CandidateOpened,
    FollowThroughConfirmed,
    FollowThroughRejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsolidationEventKind {
    SurgeDetected,
    ConsolidationConfirmed,
    EntryTriggered,
}

/// Which of the two parallel `ConsolidationBreakoutMonitor` configs
/// produced a given `ConsolidationEvent` — see that event's own doc
/// comment. Real, user-facing distinction (not an internal-only tag):
/// clients label these differently so a genuine "act within seconds"
/// micropullback entry doesn't read as identical to the slower,
/// already-validated consolidation-breakout signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsolidationStrategy {
    ConsolidationBreakout,
    Micropullback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HaltAlertLevel {
    Calm,
    Amber,
    Red,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts() -> DateTime<Utc> {
        Utc.timestamp_opt(1_787_000_000, 0).unwrap()
    }

    #[test]
    fn funnel_signal_serializes_with_camel_case_fields() {
        // Regression: rename_all on the enum itself only renames variant
        // tags, not fields within struct variants — an earlier version
        // of this enum had it there instead of per-variant, and every
        // field silently came out snake_case (protocolVersion-style
        // fields like gapPct/sessionVolume/priceOk would all have been
        // wrong) until ws-server's own protocol tests caught the
        // identical mistake there and this got fixed too.
        let event = ScanEvent::FunnelSignal {
            symbol: "SWVL".to_string(),
            timestamp: ts(),
            price: 3.12,
            gap_pct: 12.5,
            session_volume: 100_000,
            price_ok: true,
            float_ok: true,
            rel_vol_ok: true,
            gap_ok: true,
            passed: true,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"funnel_signal""#));
        assert!(json.contains(r#""gapPct":12.5"#));
        assert!(json.contains(r#""sessionVolume":100000"#));
        assert!(json.contains(r#""priceOk":true"#));
        assert!(!json.contains("gap_pct"));
        assert!(!json.contains("session_volume"));
    }

    #[test]
    fn momentum_update_serializes_with_camel_case_fields() {
        let event = ScanEvent::MomentumUpdate {
            symbol: "SWVL".to_string(),
            timestamp: ts(),
            volume_confirmation: 0.9,
            structure: 0.8,
            ma_slope: 0.7,
            wick_rejection: 0.6,
            overall: 0.85,
            qualifies: true,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"momentum_update""#));
        assert!(json.contains(r#""volumeConfirmation":0.9"#));
        assert!(json.contains(r#""maSlope":0.7"#));
        assert!(json.contains(r#""wickRejection":0.6"#));
        assert!(!json.contains("volume_confirmation"));
    }

    #[test]
    fn ignition_event_serializes_with_snake_case_kind() {
        let event = ScanEvent::IgnitionEvent {
            symbol: "SWVL".to_string(),
            timestamp: ts(),
            price: 3.12,
            kind: IgnitionEventKind::FollowThroughConfirmed,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"ignition_event""#));
        assert!(json.contains(r#""kind":"follow_through_confirmed""#));
    }

    #[test]
    fn consolidation_event_serializes_with_snake_case_kind() {
        let event = ScanEvent::ConsolidationEvent {
            symbol: "SWVL".to_string(),
            timestamp: ts(),
            price: 3.12,
            kind: ConsolidationEventKind::EntryTriggered,
            strategy: ConsolidationStrategy::ConsolidationBreakout,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"consolidation_event""#));
        assert!(json.contains(r#""kind":"entry_triggered""#));
        assert!(json.contains(r#""strategy":"consolidation_breakout""#));
    }

    #[test]
    fn consolidation_event_serializes_micropullback_strategy_distinctly() {
        let event = ScanEvent::ConsolidationEvent {
            symbol: "SWVL".to_string(),
            timestamp: ts(),
            price: 3.12,
            kind: ConsolidationEventKind::EntryTriggered,
            strategy: ConsolidationStrategy::Micropullback,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""strategy":"micropullback""#));
    }

    #[test]
    fn halt_warning_serializes_with_camel_case_fields_and_lowercase_level() {
        let event = ScanEvent::HaltWarning {
            symbol: "SWVL".to_string(),
            timestamp: ts(),
            reference_price: 3.00,
            current_price: 3.20,
            band_width_dollars: 0.60,
            band_doubled: false,
            proximity_ratio: 0.33,
            relative_volume: Some(2.5),
            level: HaltAlertLevel::Amber,
            luld_in_effect: true, estimated_bands: true,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"halt_warning""#));
        assert!(json.contains(r#""referencePrice":3.0"#));
        assert!(json.contains(r#""bandWidthDollars":0.6"#));
        assert!(json.contains(r#""proximityRatio":0.33"#));
        assert!(json.contains(r#""level":"amber""#));
        assert!(json.contains(r#""luldInEffect":true"#));
        assert!(!json.contains("reference_price"));
    }

    #[test]
    fn bar_update_serializes_with_camel_case_fields_and_raw_ohlcv() {
        let event = ScanEvent::BarUpdate { coverage: Coverage::Unknown,
            symbol: "SWVL".to_string(),
            timestamp: ts(),
            open: 3.10,
            high: 3.25,
            low: 3.05,
            close: 3.20,
            volume: 45_000,
            is_final: true, interval_secs: 60,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"bar_update""#));
        assert!(json.contains(r#""open":3.1"#));
        assert!(json.contains(r#""high":3.25"#));
        assert!(json.contains(r#""low":3.05"#));
        assert!(json.contains(r#""close":3.2"#));
        assert!(json.contains(r#""volume":45000"#));
        assert!(json.contains(r#""intervalSecs":60"#));
    }

    #[test]
    fn bar_update_distinguishes_sub_minute_bars_via_interval_secs() {
        // Real correctness requirement (2026-09-03): a client can't tell
        // a 1-minute bar from a 30-second one just from timestamp/OHLCV
        // alone -- interval_secs is what every consumer must filter on
        // before merging into its own bars array (see BarUpdate's own
        // doc comment).
        let event = ScanEvent::BarUpdate { coverage: Coverage::Unknown,
            symbol: "SWVL".to_string(),
            timestamp: ts(),
            open: 3.10,
            high: 3.25,
            low: 3.05,
            close: 3.20,
            volume: 45_000,
            is_final: true, interval_secs: 30,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""intervalSecs":30"#));
    }

    #[test]
    fn consolidation_event_round_trips_through_deserialize() {
        // Real correctness requirement for `crates/auto-trader` (2026-09-03,
        // the first Rust-side consumer that parses ScanEvent JSON back out
        // instead of only ever constructing/serializing it): confirms the
        // new `Deserialize` derive actually reads the exact wire shape
        // `ws-server` broadcasts, not just that it compiles.
        let original = ScanEvent::ConsolidationEvent {
            symbol: "SWVL".to_string(),
            timestamp: ts(),
            price: 3.12,
            kind: ConsolidationEventKind::EntryTriggered,
            strategy: ConsolidationStrategy::Micropullback,
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: ScanEvent = serde_json::from_str(&json).unwrap();
        match parsed {
            ScanEvent::ConsolidationEvent { symbol, price, kind, strategy, .. } => {
                assert_eq!(symbol, "SWVL");
                assert_eq!(price, 3.12);
                assert_eq!(kind, ConsolidationEventKind::EntryTriggered);
                assert_eq!(strategy, ConsolidationStrategy::Micropullback);
            }
            other => panic!("expected ConsolidationEvent, got {other:?}"),
        }
    }

    #[test]
    fn bar_update_round_trips_through_deserialize() {
        let original = ScanEvent::BarUpdate { coverage: Coverage::Unknown,
            symbol: "SWVL".to_string(),
            timestamp: ts(),
            open: 3.10,
            high: 3.25,
            low: 3.05,
            close: 3.20,
            volume: 45_000,
            is_final: true, interval_secs: 60,
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: ScanEvent = serde_json::from_str(&json).unwrap();
        match parsed {
            ScanEvent::BarUpdate { close, interval_secs, .. } => {
                assert_eq!(close, 3.20);
                assert_eq!(interval_secs, 60);
            }
            other => panic!("expected BarUpdate, got {other:?}"),
        }
    }

    #[test]
    fn catalyst_update_serializes_with_camel_case_fields() {
        let event = ScanEvent::CatalystUpdate {
            symbol: "SWVL".to_string(),
            timestamp: ts(),
            catalyst_tags: vec!["offering_dilution".to_string()],
            headline_count: 3,
            most_recent_headline: Some("SWVL announces registered direct offering".to_string()),
            most_recent_published_at: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"catalyst_update""#));
        assert!(json.contains(r#""catalystTags":["offering_dilution"]"#));
        assert!(json.contains(r#""headlineCount":3"#));
        assert!(json.contains(r#""mostRecentHeadline":"SWVL announces registered direct offering""#));
        assert!(!json.contains("catalyst_tags"));
        assert!(!json.contains("headline_count"));
        assert!(
            !json.contains("mostRecentPublishedAt"),
            "an absent publication time must be omitted, not sent as null"
        );
    }

    #[test]
    fn catalyst_publication_time_is_carried_when_known() {
        let event = ScanEvent::CatalystUpdate {
            symbol: "SWVL".to_string(),
            timestamp: ts(),
            catalyst_tags: vec!["earnings".to_string()],
            headline_count: 1,
            most_recent_headline: None,
            most_recent_published_at: Some(ts()),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""mostRecentPublishedAt""#));
    }

    #[test]
    fn a_catalyst_record_without_a_publication_time_still_parses() {
        // Every catalyst record written before this field existed -- including
        // everything already on the VPS -- must keep loading.
        let legacy = r#"{"type":"catalyst_update","symbol":"SWVL",
            "timestamp":"2026-08-30T20:00:00Z","catalystTags":["earnings"],
            "headlineCount":2,"mostRecentHeadline":null}"#;
        let parsed: ScanEvent = serde_json::from_str(legacy).expect("legacy record must parse");
        match parsed {
            ScanEvent::CatalystUpdate { most_recent_published_at, headline_count, .. } => {
                assert_eq!(most_recent_published_at, None);
                assert_eq!(headline_count, 2);
            }
            other => panic!("expected CatalystUpdate, got {other:?}"),
        }
    }
}


