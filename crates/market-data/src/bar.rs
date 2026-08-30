//! Wire types for Alpaca's realtime market-data WebSocket. The server
//! always sends a JSON array of these, batched — see `ws.rs`.
//!
//! Reference: <https://docs.alpaca.markets/docs/streaming-market-data>

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Bar {
    #[serde(rename = "S")]
    pub symbol: String,
    #[serde(rename = "o")]
    pub open: f64,
    #[serde(rename = "h")]
    pub high: f64,
    #[serde(rename = "l")]
    pub low: f64,
    #[serde(rename = "c")]
    pub close: f64,
    #[serde(rename = "v")]
    pub volume: u64,
    #[serde(rename = "t")]
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// One message from the batch Alpaca's WS sends. Only what the fast funnel
/// currently needs is modeled (`Bar`) plus enough of the control-channel
/// messages (`Success`/`Error`/`Subscription`) to drive the connect/auth/
/// subscribe handshake in `ws.rs`. Trades/quotes/news/luld frames — needed
/// later for the ignition detector's tick-level signals — fall into
/// `Other` for now rather than being dropped as parse errors.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "T")]
pub enum AlpacaMessage {
    #[serde(rename = "success")]
    Success { msg: String },
    #[serde(rename = "error")]
    Error { code: i32, msg: String },
    #[serde(rename = "subscription")]
    Subscription {
        #[serde(default)]
        trades: Vec<String>,
        #[serde(default)]
        quotes: Vec<String>,
        #[serde(default)]
        bars: Vec<String>,
    },
    #[serde(rename = "b")]
    Bar(Bar),
    #[serde(other)]
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_bar_message_batch() {
        let raw = r#"[{"T":"b","S":"SWVL","o":1.23,"h":1.30,"l":1.20,"c":1.28,"v":15000,"t":"2026-08-28T13:31:00Z"}]"#;
        let batch: Vec<AlpacaMessage> = serde_json::from_str(raw).unwrap();
        assert_eq!(batch.len(), 1);
        match &batch[0] {
            AlpacaMessage::Bar(b) => {
                assert_eq!(b.symbol, "SWVL");
                assert_eq!(b.close, 1.28);
                assert_eq!(b.volume, 15000);
            }
            other => panic!("expected Bar, got {other:?}"),
        }
    }

    #[test]
    fn parses_auth_and_subscription_acks() {
        let raw = r#"[{"T":"success","msg":"authenticated"},{"T":"subscription","bars":["SWVL"]}]"#;
        let batch: Vec<AlpacaMessage> = serde_json::from_str(raw).unwrap();
        assert!(matches!(&batch[0], AlpacaMessage::Success { msg } if msg == "authenticated"));
        assert!(matches!(&batch[1], AlpacaMessage::Subscription { bars, .. } if bars == &["SWVL".to_string()]));
    }

    #[test]
    fn unrecognized_message_types_dont_fail_the_whole_batch() {
        let raw = r#"[{"T":"q","S":"SWVL","bp":1.0,"ap":1.1},{"T":"b","S":"SWVL","o":1,"h":1,"l":1,"c":1,"v":1,"t":"2026-08-28T13:31:00Z"}]"#;
        let batch: Vec<AlpacaMessage> = serde_json::from_str(raw).unwrap();
        assert!(matches!(batch[0], AlpacaMessage::Other));
        assert!(matches!(batch[1], AlpacaMessage::Bar(_)));
    }
}
