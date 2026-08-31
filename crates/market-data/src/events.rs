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
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
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
    /// getting its own top-level type.
    #[serde(rename = "consolidation_event", rename_all = "camelCase")]
    ConsolidationEvent {
        symbol: String,
        timestamp: DateTime<Utc>,
        price: f64,
        kind: ConsolidationEventKind,
    },
    /// Halt Early-Warning panel: a live proximity-to-halt reading for one
    /// symbol — sent on every trade for a symbol currently being tracked
    /// (not edge-triggered like the others), since a UI proximity gauge
    /// needs the current value continuously, not just transitions.
    #[serde(rename = "halt_warning", rename_all = "camelCase")]
    HaltWarning {
        symbol: String,
        timestamp: DateTime<Utc>,
        reference_price: f64,
        current_price: f64,
        band_width_dollars: f64,
        band_doubled: bool,
        proximity_ratio: f64,
        relative_volume: Option<f64>,
        level: HaltAlertLevel,
    },
    /// Super Chart panel: one raw OHLCV bar for a tracked symbol, straight
    /// from Alpaca's own bar (see `bar.rs`) with no funnel/scoring
    /// transformation applied — `FunnelSignal`'s `price`/`gapPct` etc. are
    /// derived values for the scanner panels, not what a candlestick chart
    /// needs to render. Sent alongside `FunnelSignal` on every bar for
    /// every tracked symbol (not edge-triggered) since a chart needs every
    /// bar, not just qualifying ones.
    #[serde(rename = "bar_update", rename_all = "camelCase")]
    BarUpdate {
        symbol: String,
        timestamp: DateTime<Utc>,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        volume: u64,
    },
    /// Catalysts panel: news catalyst tags for a symbol, from the Python
    /// qualitative layer (doc section 4.4). Fired once per symbol at
    /// promotion time (see `live.rs`) — catalysts don't change tick-by-
    /// tick the way price does, so this isn't a per-trade/per-bar event
    /// like the others.
    #[serde(rename = "catalyst_update", rename_all = "camelCase")]
    CatalystUpdate {
        symbol: String,
        timestamp: DateTime<Utc>,
        catalyst_tags: Vec<String>,
        headline_count: u32,
        most_recent_headline: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IgnitionEventKind {
    CandidateOpened,
    FollowThroughConfirmed,
    FollowThroughRejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsolidationEventKind {
    SurgeDetected,
    ConsolidationConfirmed,
    EntryTriggered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"consolidation_event""#));
        assert!(json.contains(r#""kind":"entry_triggered""#));
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
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"halt_warning""#));
        assert!(json.contains(r#""referencePrice":3.0"#));
        assert!(json.contains(r#""bandWidthDollars":0.6"#));
        assert!(json.contains(r#""proximityRatio":0.33"#));
        assert!(json.contains(r#""level":"amber""#));
        assert!(!json.contains("reference_price"));
    }

    #[test]
    fn bar_update_serializes_with_camel_case_fields_and_raw_ohlcv() {
        let event = ScanEvent::BarUpdate {
            symbol: "SWVL".to_string(),
            timestamp: ts(),
            open: 3.10,
            high: 3.25,
            low: 3.05,
            close: 3.20,
            volume: 45_000,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"bar_update""#));
        assert!(json.contains(r#""open":3.1"#));
        assert!(json.contains(r#""high":3.25"#));
        assert!(json.contains(r#""low":3.05"#));
        assert!(json.contains(r#""close":3.2"#));
        assert!(json.contains(r#""volume":45000"#));
    }

    #[test]
    fn catalyst_update_serializes_with_camel_case_fields() {
        let event = ScanEvent::CatalystUpdate {
            symbol: "SWVL".to_string(),
            timestamp: ts(),
            catalyst_tags: vec!["offering_dilution".to_string()],
            headline_count: 3,
            most_recent_headline: Some("SWVL announces registered direct offering".to_string()),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"catalyst_update""#));
        assert!(json.contains(r#""catalystTags":["offering_dilution"]"#));
        assert!(json.contains(r#""headlineCount":3"#));
        assert!(json.contains(r#""mostRecentHeadline":"SWVL announces registered direct offering""#));
        assert!(!json.contains("catalyst_tags"));
        assert!(!json.contains("headline_count"));
    }
}
