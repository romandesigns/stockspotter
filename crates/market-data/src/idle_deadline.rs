//! The live scan's idle deadline: when a connected-but-silent market stream
//! counts as dead.
//!
//! # The defect this replaces
//!
//! `run_live_scan` wrapped `stream.next_batch()` in a RELATIVE
//! `tokio::time::timeout(IDLE_TIMEOUT, ..)` inside its `select!`, so the
//! timeout was recreated on every loop iteration -- including iterations
//! woken by a control branch rather than by the stream. Those branches (the
//! 15s discovery-audit tick, the 15s universe rescan result, the 60s halt
//! watch, the 60s universe eviction, mover seeds, catalysts) all fire far
//! more often than the 600s timeout, so the deadline was postponed forever
//! while the upstream sat open and silent. A transport that stays connected
//! and sends nothing is exactly what the timeout exists to catch, and it was
//! the one case it could not.
//!
//! Extracted by hand from `257e5c5` (the chart-port branch): this deadline,
//! its tests, and the `timeout_at` wiring only. Nothing else from that port.
//!
//! # The invariant
//!
//! The deadline is absolute and advances on **market data** and on nothing
//! else ([`is_market_data`]). Control ticks are inert. So are the stream's
//! own control frames: a subscription acknowledgement follows every
//! `subscribe` a rescan sends when it promotes symbols, and counting them
//! would recreate the defect through a different door. WebSocket pings never
//! reach here at all (`ws::read_batch` skips them).
//!
//! # When the market is closed
//!
//! Silence is normal from 20:00 to 04:00 ET and all weekend: with nothing
//! trading there is nothing to send. Reconnecting every `IDLE_TIMEOUT` then
//! would rebuild every detector and re-fetch every seed six times an hour
//! for no reason, and would make a real reconnect indistinguishable from
//! routine churn in the log. So an expiry during
//! [`TradingSession::Overnight`] -- which `classify_session` also returns
//! for the whole of a weekend or NYSE holiday -- re-arms instead of
//! reconnecting. The cost is stated: a transport that dies overnight is
//! detected at most one `IDLE_TIMEOUT` after 04:00 ET, before the premarket
//! has produced anything worth missing. From 04:00 to 20:00 ET on a trading
//! day an expiry always reconnects.

use std::time::Duration;

use tokio::time::Instant;

use crate::trading_session::TradingSession;
use crate::AlpacaMessage;

/// What an expired deadline means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IdleExpiry {
    /// Silent for a whole timeout while data is expected: treat the
    /// connection as dead and take the existing reconnect path.
    Reconnect,
    /// Silent while the market is closed. Expected; the deadline has been
    /// re-armed and the loop carries on.
    MarketClosed,
}

/// Absolute idle deadline for the market stream.
#[derive(Debug, Clone, Copy)]
pub(crate) struct IdleDeadline {
    at: Instant,
    timeout: Duration,
}

impl IdleDeadline {
    pub(crate) fn new(now: Instant, timeout: Duration) -> Self {
        Self {
            at: now + timeout,
            timeout,
        }
    }

    pub(crate) fn at(&self) -> Instant {
        self.at
    }

    /// A batch arrived. Advances the deadline iff it carries market data;
    /// returns whether it did.
    pub(crate) fn on_batch(&mut self, batch: &[AlpacaMessage], now: Instant) -> bool {
        let data = batch.iter().any(is_market_data);
        if data {
            self.at = now + self.timeout;
        }
        data
    }

    /// A control branch fired. Deliberately does nothing, and exists so each
    /// call site reads as a decision rather than an omission -- an omission
    /// is what the relative timeout amounted to.
    pub(crate) fn on_control_tick(self) {}

    /// The deadline passed with no market data. `session` is the trading
    /// session at `now`; see the module doc for why it decides the answer.
    pub(crate) fn on_expiry(&mut self, now: Instant, session: TradingSession) -> IdleExpiry {
        match session {
            TradingSession::Overnight => {
                self.at = now + self.timeout;
                IdleExpiry::MarketClosed
            }
            TradingSession::Premarket | TradingSession::Regular | TradingSession::AfterHours => {
                IdleExpiry::Reconnect
            }
        }
    }
}

/// Whether a stream message is market data -- evidence the feed is alive --
/// rather than a control frame.
///
/// Data: trades, quotes, bars, updated bars, LULD bands, trading statuses.
/// Not data: `success` (auth), `subscription` (the ack a rescan's
/// `subscribe` produces), `error` (`next_batch` bails on it before this is
/// consulted) and anything unrecognised.
pub(crate) fn is_market_data(msg: &AlpacaMessage) -> bool {
    match msg {
        AlpacaMessage::Trade(_)
        | AlpacaMessage::Quote(_)
        | AlpacaMessage::Bar(_)
        | AlpacaMessage::UpdatedBar(_)
        | AlpacaMessage::Luld { .. }
        | AlpacaMessage::Status(_) => true,
        AlpacaMessage::Success { .. }
        | AlpacaMessage::Error { .. }
        | AlpacaMessage::Subscription { .. }
        | AlpacaMessage::Other => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(600);

    fn parse(json: &str) -> Vec<AlpacaMessage> {
        serde_json::from_str(json).unwrap()
    }

    fn trade() -> Vec<AlpacaMessage> {
        parse(r#"[{"T":"t","S":"AAA","p":1.5,"s":100,"t":"2026-09-23T13:30:00Z","c":["@"]}]"#)
    }

    fn subscription_ack() -> Vec<AlpacaMessage> {
        parse(
            r#"[{"T":"subscription","trades":["*"],"quotes":[],"bars":["AAA"],"statuses":["*"]}]"#,
        )
    }

    /// Reproduces the original failure: control ticks postponing the deadline
    /// forever while the upstream sits connected and silent. Under the relative
    /// timeout each of these wakeups pushed it out by another full 600s.
    #[test]
    fn control_ticks_cannot_postpone_the_deadline() {
        let start = Instant::now();
        let d = IdleDeadline::new(start, TIMEOUT);
        let original = d.at();
        // Audit and rescan every 15s, halt watch and eviction every 60s:
        // hundreds of wakeups inside one window, which is the real shape.
        for _ in 0..10_000 {
            d.on_control_tick();
        }
        assert_eq!(d.at(), original);
        assert_eq!(d.at(), start + TIMEOUT);
    }

    /// The other half: market data does move it, from the later instant. A
    /// test that only asserted immovability could be satisfied by a deadline
    /// that never advances, which would kill a healthy stream every 600s.
    #[test]
    fn market_data_advances_the_deadline() {
        let start = Instant::now();
        let mut d = IdleDeadline::new(start, TIMEOUT);
        let t1 = start + Duration::from_secs(120);
        assert!(d.on_batch(&trade(), t1));
        assert_eq!(d.at(), t1 + TIMEOUT);
        let t2 = start + Duration::from_secs(300);
        assert!(d.on_batch(&trade(), t2));
        assert_eq!(d.at(), t2 + TIMEOUT);
    }

    /// Stream control frames are not data: a rescan's `subscribe` produces an
    /// ack as often as every 15s, which would otherwise keep a dead feed "alive" exactly
    /// as the control ticks did.
    #[test]
    fn control_frames_do_not_advance_the_deadline() {
        let start = Instant::now();
        let mut d = IdleDeadline::new(start, TIMEOUT);
        let later = start + Duration::from_secs(300);
        assert!(!d.on_batch(&subscription_ack(), later));
        assert!(!d.on_batch(&parse(r#"[{"T":"success","msg":"authenticated"}]"#), later));
        assert!(
            !d.on_batch(&parse(r#"[{"T":"n","S":"AAA"}]"#), later),
            "unknown type"
        );
        assert!(!d.on_batch(&[], later));
        assert_eq!(d.at(), start + TIMEOUT);
    }

    /// One data message in a mixed batch is enough.
    #[test]
    fn a_mixed_batch_counts_as_data() {
        let start = Instant::now();
        let mut d = IdleDeadline::new(start, TIMEOUT);
        let mut batch = subscription_ack();
        batch.extend(trade());
        let later = start + Duration::from_secs(10);
        assert!(d.on_batch(&batch, later));
        assert_eq!(d.at(), later + TIMEOUT);
    }

    #[test]
    fn every_data_kind_counts() {
        for json in [
            r#"[{"T":"q","S":"AAA","bp":1.0,"bs":1,"ap":1.1,"as":1,"t":"2026-09-23T13:30:00Z"}]"#,
            r#"[{"T":"b","S":"AAA","o":1,"h":1,"l":1,"c":1,"v":1,"t":"2026-09-23T13:30:00Z"}]"#,
            r#"[{"T":"u","S":"AAA","o":1,"h":1,"l":1,"c":1,"v":1,"t":"2026-09-23T13:30:00Z"}]"#,
            r#"[{"T":"l","S":"AAA","u":1.1,"d":0.9,"t":"2026-09-23T13:30:00Z"}]"#,
            r#"[{"T":"s","S":"AAA","sc":"H","t":"2026-09-23T13:30:00Z"}]"#,
        ] {
            assert!(parse(json).iter().all(is_market_data), "{json}");
        }
        assert!(trade().iter().all(is_market_data));
    }

    /// A silent-but-connected stream reaches its deadline on schedule, and
    /// while data is expected that means reconnect.
    #[test]
    fn silence_during_trading_hours_reconnects() {
        let start = Instant::now();
        let mut d = IdleDeadline::new(start, TIMEOUT);
        d.on_batch(&trade(), start);
        for _ in 0..1_200 {
            d.on_control_tick();
            d.on_batch(&subscription_ack(), start + Duration::from_secs(5));
        }
        assert_eq!(
            d.at(),
            start + TIMEOUT,
            "deadline drifted under control pressure"
        );
        for session in [
            TradingSession::Premarket,
            TradingSession::Regular,
            TradingSession::AfterHours,
        ] {
            assert_eq!(
                d.on_expiry(d.at(), session),
                IdleExpiry::Reconnect,
                "{session:?}"
            );
        }
    }

    /// Silence while the market is closed (overnight, weekends, holidays) is
    /// expected: re-arm rather than tear everything down every 600s.
    #[test]
    fn silence_while_closed_rearms_instead_of_reconnecting() {
        let start = Instant::now();
        let mut d = IdleDeadline::new(start, TIMEOUT);
        let expired_at = d.at();
        assert_eq!(
            d.on_expiry(expired_at, TradingSession::Overnight),
            IdleExpiry::MarketClosed
        );
        assert_eq!(d.at(), expired_at + TIMEOUT, "re-armed one full timeout on");
        // Still silent when the premarket opens: the next expiry, at most one
        // timeout into it, reconnects.
        let next = d.at();
        assert_eq!(
            d.on_expiry(next, TradingSession::Premarket),
            IdleExpiry::Reconnect
        );
    }
}
