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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IgnitionEventKind {
    CandidateOpened,
    FollowThroughConfirmed,
    FollowThroughRejected,
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
}
