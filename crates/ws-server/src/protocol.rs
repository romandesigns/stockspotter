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
}

/// Messages a client can send. Only `hello` and `ping` exist so far, per
/// the connection/handshake layer `shared-types` currently defines —
/// clients don't have anything else to say to the server yet (this is a
/// broadcast-only feed for now, not a request/response API).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    #[serde(rename = "hello", rename_all = "camelCase")]
    Hello {
        protocol_version: u32,
        client: ClientKind,
    },
    #[serde(rename = "ping")]
    Ping { at: String },
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_client_hello() {
        let raw = r#"{"type":"hello","protocolVersion":1,"client":"web"}"#;
        let msg: ClientMessage = serde_json::from_str(raw).unwrap();
        match msg {
            ClientMessage::Hello { protocol_version, client } => {
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
    fn parses_a_ping() {
        let raw = r#"{"type":"ping","at":"2026-08-30T20:00:00Z"}"#;
        let msg: ClientMessage = serde_json::from_str(raw).unwrap();
        assert!(matches!(msg, ClientMessage::Ping { at } if at == "2026-08-30T20:00:00Z"));
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
    fn hello_rejected_serializes_with_expected_shape() {
        let rejected = HandshakeMessage::HelloRejected {
            reason: "unsupported protocol version".to_string(),
        };
        let json = serde_json::to_string(&rejected).unwrap();
        assert_eq!(json, r#"{"type":"hello_rejected","reason":"unsupported protocol version"}"#);
    }
}
