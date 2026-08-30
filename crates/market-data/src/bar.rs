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

/// A single trade print — feeds the ignition detector's trade-frequency
/// signal (`ignition_detector::detect::trade_frequency_ratio`).
#[derive(Debug, Clone, Deserialize)]
pub struct Trade {
    #[serde(rename = "S")]
    pub symbol: String,
    #[serde(rename = "p")]
    pub price: f64,
    #[serde(rename = "s")]
    pub size: u64,
    #[serde(rename = "t")]
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// A trading status update (halts, resumptions) — feeds the ignition
/// detector's halt-lift signal. Alpaca's exact status-code set isn't
/// fully enumerated in their public docs; the confirmed one is "H" for
/// Halted (see `ignition_detector::monitor`'s `is_halted`).
#[derive(Debug, Clone, Deserialize)]
pub struct Status {
    #[serde(rename = "S")]
    pub symbol: String,
    #[serde(rename = "sc")]
    pub status_code: String,
    #[serde(rename = "t")]
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// A top-of-book quote update — feeds the ignition detector's spread and
/// ask-absorption signals.
#[derive(Debug, Clone, Deserialize)]
pub struct Quote {
    #[serde(rename = "S")]
    pub symbol: String,
    #[serde(rename = "bp")]
    pub bid_price: f64,
    #[serde(rename = "bs")]
    pub bid_size: u64,
    #[serde(rename = "ap")]
    pub ask_price: f64,
    // "as" is a Rust keyword, hence the rename.
    #[serde(rename = "as")]
    pub ask_size: u64,
    #[serde(rename = "t")]
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// One message from the batch Alpaca's WS sends. Models `Bar`/`Trade`/
/// `Quote`/`Status` (what the fast funnel, momentum scorer, and ignition
/// detector need — `Status` feeds the ignition detector's halt-lift
/// signal) plus enough of the control-channel messages (`Success`/
/// `Error`/`Subscription`) to drive the connect/auth/subscribe handshake
/// in `ws.rs`. News/LULD frames still fall into `Other` — nothing
/// consumes them yet.
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
        #[serde(default)]
        statuses: Vec<String>,
    },
    #[serde(rename = "b")]
    Bar(Bar),
    #[serde(rename = "t")]
    Trade(Trade),
    #[serde(rename = "q")]
    Quote(Quote),
    #[serde(rename = "s")]
    Status(Status),
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
    fn parses_a_trade_message() {
        let raw = r#"[{"T":"t","S":"SWVL","i":123,"x":"V","p":1.285,"s":200,"c":["@"],"t":"2026-08-28T13:31:05Z","z":"C"}]"#;
        let batch: Vec<AlpacaMessage> = serde_json::from_str(raw).unwrap();
        match &batch[0] {
            AlpacaMessage::Trade(t) => {
                assert_eq!(t.symbol, "SWVL");
                assert_eq!(t.price, 1.285);
                assert_eq!(t.size, 200);
            }
            other => panic!("expected Trade, got {other:?}"),
        }
    }

    #[test]
    fn parses_a_quote_message_including_the_as_field() {
        // "as" (ask size) is a Rust keyword — this specifically confirms
        // the #[serde(rename = "as")] mapping actually works, not just
        // that the struct compiles.
        let raw = r#"[{"T":"q","S":"SWVL","bx":"V","bp":1.28,"bs":3,"ax":"V","ap":1.29,"as":5,"t":"2026-08-28T13:31:05Z","c":["R"],"z":"C"}]"#;
        let batch: Vec<AlpacaMessage> = serde_json::from_str(raw).unwrap();
        match &batch[0] {
            AlpacaMessage::Quote(q) => {
                assert_eq!(q.symbol, "SWVL");
                assert_eq!(q.bid_price, 1.28);
                assert_eq!(q.bid_size, 3);
                assert_eq!(q.ask_price, 1.29);
                assert_eq!(q.ask_size, 5);
            }
            other => panic!("expected Quote, got {other:?}"),
        }
    }

    #[test]
    fn parses_a_status_message() {
        let raw = r#"[{"T":"s","S":"SWVL","sc":"H","sm":"Trading Halt","rc":"T12","rm":"Trading Halted; News Pending","t":"2026-08-28T13:31:00Z","z":"C"}]"#;
        let batch: Vec<AlpacaMessage> = serde_json::from_str(raw).unwrap();
        match &batch[0] {
            AlpacaMessage::Status(s) => {
                assert_eq!(s.symbol, "SWVL");
                assert_eq!(s.status_code, "H");
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[test]
    fn unrecognized_message_types_dont_fail_the_whole_batch() {
        // "q" then "s" were this test's stand-ins before Quote/Status got
        // modeled — "l" (LULD bands) is next in line, still genuinely
        // unhandled.
        let raw = r#"[{"T":"l","S":"SWVL","u":1.30,"d":1.20,"t":"2026-08-28T13:31:00Z"},{"T":"b","S":"SWVL","o":1,"h":1,"l":1,"c":1,"v":1,"t":"2026-08-28T13:31:00Z"}]"#;
        let batch: Vec<AlpacaMessage> = serde_json::from_str(raw).unwrap();
        assert!(matches!(batch[0], AlpacaMessage::Other));
        assert!(matches!(batch[1], AlpacaMessage::Bar(_)));
    }
}
