//! Wire protocol — mirrors `packages/shared-types/src/index.ts` exactly
//! (same `type` tags, same `camelCase` field names, same protocol
//! version number). One contract, two languages: if this drifts from
//! that file, clients silently stop understanding the server.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ClientKind {
    Web,
    Desktop,
    Mobile,
    /// The auto-trader service (`crates/auto-trader`) — a backend process,
    /// not a browser/mobile client, but it speaks the exact same hello/
    /// welcome handshake and receives the exact same broadcast, so it's a
    /// `ClientKind` like any other rather than a special-cased connection
    /// type. Named explicitly (not lumped in as "desktop") so this
    /// server's own connection logs say what actually connected.
    #[serde(rename = "auto_trader")]
    AutoTrader,
}

/// Messages a client can send: the `hello`/`ping` handshake layer that
/// `shared-types` defines, plus two per-connection requests,
/// `subscribe_order_flow` and `subscribe_chart` (see `server.rs`). Everything else on this
/// socket is still the same broadcast for every client.
///
/// `subscribe_order_flow` is deliberately NOT added to
/// `@stockspotter/shared-types`: it is a web-only opt-in research
/// protocol, and the mobile app's own copy of the handshake types is
/// behind the Android advisory gate. The web client's decoder and types
/// live in `apps/client/src/lib/orderFlow.ts`, and no existing shared
/// message is altered or shadowed by it.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    #[serde(rename = "hello", rename_all = "camelCase")]
    Hello {
        #[serde(default)]
        token: Option<String>,
        #[serde(default)]
        experimental_strategies: bool,
        #[serde(default)]
        experimental_only: bool,
        protocol_version: u32,
        client: ClientKind,
    },
    #[serde(rename = "ping")]
    Ping { at: String },
    /// "Push me order-flow summaries for exactly these symbols", replacing
    /// whatever this connection asked for before. An empty list stops it.
    /// A server that predates this ignores the message (it fails to parse
    /// there and is dropped), and the client keeps polling over HTTP.
    #[serde(rename = "subscribe_order_flow")]
    SubscribeOrderFlow {
        #[serde(default)]
        symbols: Vec<String>,
    },
    /// `ClientSubscribeChart` in shared-types: "build live candles for
    /// exactly these symbols and send them to me", replacing this
    /// connection's previous list; empty clears it. Symbols are uppercased,
    /// invalid ones dropped, and at most `CHART_SUBSCRIPTION_LIMIT` (8) kept.
    /// The frames are ordinary `bar_update`s (60s and 30s, forming and final)
    /// for symbols the shared broadcast does not already carry.
    #[serde(rename = "subscribe_chart")]
    SubscribeChart {
        #[serde(default)]
        symbols: Vec<String>,
    },
}

/// `ServerFeedStatus`'s `state`.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FeedStateWire {
    Up,
    Down,
}

/// Handshake/keepalive messages the server sends. Detection events
/// (`market_data::ScanEvent`) are sent as their own already-tagged JSON
/// directly — no need to wrap them in a second enum here, their `type`
/// tags (`funnel_signal`/`momentum_update`/`ignition_event`) don't
/// collide with any of these.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum HandshakeMessage {
    #[serde(rename = "welcome", rename_all = "camelCase")]
    Welcome {
        protocol_version: u32,
        server_time: String,
    },
    #[serde(rename = "hello_rejected")]
    HelloRejected { reason: String },
    #[serde(rename = "pong")]
    Pong { at: String },
    /// This connection's receiver fell behind the broadcast channel and the
    /// server dropped `missed_events` events for it before it caught up.
    ///
    /// Sent once per lag occurrence, and never as a substitute for data: the
    /// dropped events are gone and are not replayed. The server does follow
    /// this with a resend of its retained snapshot (latest-per-key plus
    /// recent alerts), so current state recovers -- but any event that came
    /// and went inside the gap is genuinely lost, and a client that silently
    /// assumed otherwise would be showing a confident, incomplete picture.
    #[serde(rename = "stream_lagged", rename_all = "camelCase")]
    StreamLagged { missed_events: u64 },
    /// `ServerFeedStatus` in shared-types: the server's own upstream
    /// market-data stream went down or came back. Sent on every change and
    /// once straight after `welcome` (and again after a lag recovery). While
    /// down no data is arriving at all; after the next up a client marks the
    /// gap and refetches the history it shows.
    #[serde(rename = "feed_status")]
    FeedStatus {
        state: FeedStateWire,
        since: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

impl HandshakeMessage {
    pub fn feed_status(state: &market_data::client_feed::FeedState) -> Self {
        HandshakeMessage::FeedStatus {
            state: if state.up { FeedStateWire::Up } else { FeedStateWire::Down },
            since: state.since.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            reason: state.reason.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_client_hello() {
        let raw = r#"{"type":"hello","protocolVersion":1,"client":"web"}"#;
        let msg: ClientMessage = serde_json::from_str(raw).unwrap();
        match msg {
            ClientMessage::Hello { protocol_version, client, .. } => {
                assert_eq!(protocol_version, 1);
                assert_eq!(client, ClientKind::Web);
            }
            other => panic!("expected Hello, got {other:?}"),
        }
    }

    #[test]
    fn parses_desktop_and_mobile_client_kinds() {
        let desktop: ClientMessage =
            serde_json::from_str(r#"{"type":"hello","protocolVersion":1,"client":"desktop"}"#).unwrap();
        let mobile: ClientMessage =
            serde_json::from_str(r#"{"type":"hello","protocolVersion":1,"client":"mobile"}"#).unwrap();
        assert!(matches!(desktop, ClientMessage::Hello { client: ClientKind::Desktop, .. }));
        assert!(matches!(mobile, ClientMessage::Hello { client: ClientKind::Mobile, .. }));
    }

    #[test]
    fn parses_the_auto_trader_client_kind() {
        let msg: ClientMessage =
            serde_json::from_str(r#"{"type":"hello","protocolVersion":1,"client":"auto_trader"}"#).unwrap();
        assert!(matches!(msg, ClientMessage::Hello { client: ClientKind::AutoTrader, .. }));
    }

    #[test]
    fn parses_a_ping() {
        let raw = r#"{"type":"ping","at":"2026-08-30T20:00:00Z"}"#;
        let msg: ClientMessage = serde_json::from_str(raw).unwrap();
        assert!(matches!(msg, ClientMessage::Ping { at } if at == "2026-08-30T20:00:00Z"));
    }

    #[test]
    fn parses_an_order_flow_subscription_including_an_empty_one() {
        let msg: ClientMessage = serde_json::from_str(r#"{"type":"subscribe_order_flow","symbols":["AAA","BBB"]}"#).unwrap();
        assert!(matches!(msg, ClientMessage::SubscribeOrderFlow { symbols } if symbols == ["AAA", "BBB"]));
        let stop: ClientMessage = serde_json::from_str(r#"{"type":"subscribe_order_flow"}"#).unwrap();
        assert!(matches!(stop, ClientMessage::SubscribeOrderFlow { symbols } if symbols.is_empty()));
    }

    #[test]
    fn parses_a_chart_subscription_including_an_empty_one() {
        let msg: ClientMessage = serde_json::from_str(r#"{"type":"subscribe_chart","symbols":["aapl","BRK.B"]}"#).unwrap();
        assert!(matches!(msg, ClientMessage::SubscribeChart { symbols } if symbols == ["aapl", "BRK.B"]));
        let stop: ClientMessage = serde_json::from_str(r#"{"type":"subscribe_chart","symbols":[]}"#).unwrap();
        assert!(matches!(stop, ClientMessage::SubscribeChart { symbols } if symbols.is_empty()));
    }

    #[test]
    fn feed_status_serializes_as_shared_types_server_feed_status() {
        use chrono::TimeZone;
        let since = chrono::Utc.with_ymd_and_hms(2026, 10, 15, 16, 57, 3).unwrap();
        let down = market_data::client_feed::FeedState { up: false, since, reason: Some("no market data for 60s".into()) };
        assert_eq!(
            serde_json::to_string(&HandshakeMessage::feed_status(&down)).unwrap(),
            r#"{"type":"feed_status","state":"down","since":"2026-10-15T16:57:03.000Z","reason":"no market data for 60s"}"#
        );
        let up = market_data::client_feed::FeedState { up: true, since, reason: None };
        assert_eq!(
            serde_json::to_string(&HandshakeMessage::feed_status(&up)).unwrap(),
            r#"{"type":"feed_status","state":"up","since":"2026-10-15T16:57:03.000Z"}"#,
            "no reason key at all when there is none"
        );
    }

    #[test]
    fn the_existing_hello_shape_is_unchanged_by_the_new_message() {
        // An older client's hello, and one with the experimental flags,
        // both still parse exactly as before.
        let plain: ClientMessage = serde_json::from_str(r#"{"type":"hello","protocolVersion":1,"client":"web"}"#).unwrap();
        assert!(matches!(plain, ClientMessage::Hello { experimental_strategies: false, experimental_only: false, .. }));
        let flagged: ClientMessage = serde_json::from_str(r#"{"type":"hello","protocolVersion":1,"client":"web","experimentalStrategies":true}"#).unwrap();
        assert!(matches!(flagged, ClientMessage::Hello { experimental_strategies: true, .. }));
    }

    #[test]
    fn welcome_serializes_with_shared_types_field_names() {
        let welcome = HandshakeMessage::Welcome {
            protocol_version: 1,
            server_time: "2026-08-30T20:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&welcome).unwrap();
        assert_eq!(
            json,
            r#"{"type":"welcome","protocolVersion":1,"serverTime":"2026-08-30T20:00:00Z"}"#
        );
    }

    #[test]
    fn stream_lagged_serializes_with_shared_types_field_names() {
        let lagged = HandshakeMessage::StreamLagged { missed_events: 42 };
        let json = serde_json::to_string(&lagged).unwrap();
        assert_eq!(json, r#"{"type":"stream_lagged","missedEvents":42}"#);
    }

    #[test]
    fn hello_rejected_serializes_with_expected_shape() {
        let rejected = HandshakeMessage::HelloRejected {
            reason: "unsupported protocol version".to_string(),
        };
        let json = serde_json::to_string(&rejected).unwrap();
        assert_eq!(json, r#"{"type":"hello_rejected","reason":"unsupported protocol version"}"#);
    }
}
