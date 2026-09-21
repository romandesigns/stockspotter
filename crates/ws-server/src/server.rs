//! The actual "every platform gets the same notifications" guarantee:
//! one `tokio::sync::broadcast` channel, fed by `market_data::run_live_scan`,
//! and every connected client (web/desktop/mobile — indistinguishable
//! from here on out) subscribes to that exact same channel. A
//! `broadcast::Receiver` delivers every message sent after it
//! subscribed, in order, identically to every other receiver — there is
//! no per-client filtering or customization anywhere in this file. A
//! client that falls behind (slow network, backgrounded app) can lag and
//! miss some events (`RecvError::Lagged`), but never receives a
//! *different* event than any other client would have at that point.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use market_data::ScanEvent;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use tracing::{info, warn};

use crate::protocol::{ClientMessage, HandshakeMessage, PROTOCOL_VERSION};

const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct EventFrame {
    event_id: String,
    /// When THIS SERVER PREPARED THIS FRAME FOR TRANSMISSION to one client.
    ///
    /// Not a market timestamp, and the distinction is the entire point. Every
    /// event already carries a `timestamp` describing when something happened
    /// in the market; that field answers "how old is this information". This
    /// one answers "how long did we take to deliver it", which nothing else
    /// could answer.
    ///
    /// The 2026-09-21 chart audit needed exactly this and did not have it.
    /// It could measure exchange timestamp -> backend (p50 13.1ms, p99
    /// 82.7ms) from the discovery-audit records, but publication -> client
    /// was unmeasurable, and the attempt to infer it from event timestamps
    /// produced nonsense: `momentum_update` implied a p50 "latency" of 23
    /// minutes and `halt_warning` a maximum of 6.9 hours, because those
    /// fields are market-semantic and are re-broadcast long after the moment
    /// they describe.
    ///
    /// Stamped PER CLIENT immediately before serialization, not once at
    /// broadcast: the question is when this client's copy went out, and a
    /// snapshot frame replayed to a reconnecting client an hour later must
    /// carry that later instant, not the original one. The shared broadcast
    /// copy and the retained snapshot therefore hold `None`, and
    /// `skip_serializing_if` keeps the field absent there -- so this is
    /// additive and backward compatible: existing clients see an unchanged
    /// envelope.
    ///
    /// UTC wall clock, microsecond precision. No monotonicity is implied or
    /// available: it is comparable against a client clock only as well as the
    /// two clocks are synchronised, which is why any measurement using it has
    /// to state its clock-offset assumption.
    #[serde(skip_serializing_if = "Option::is_none")]
    sent_at: Option<String>,
    #[serde(flatten)]
    event: ScanEvent,
}

impl EventFrame {
    /// Stamp a copy for transmission. Cheap by construction -- one clock read
    /// and one RFC3339 format per frame per client, no JSON round-trip --
    /// because this runs on the fanout hot path at hundreds of messages a
    /// second.
    fn stamped(mut self) -> Self {
        self.sent_at = Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true));
        self
    }
}

#[derive(Default)]
struct EventSnapshot {
    alerts: std::collections::VecDeque<EventFrame>,
    latest: std::collections::BTreeMap<String, EventFrame>,
}

impl EventSnapshot {
    fn record(&mut self, frame: EventFrame) {
        if matches!(&frame.event, ScanEvent::IgnitionEvent { .. } | ScanEvent::ConsolidationEvent { .. } | ScanEvent::FunnelSignal { passed: true, .. }) {
            self.alerts.push_back(frame);
            while self.alerts.len() > 2000 { self.alerts.pop_front(); }
        } else if let Ok(value) = serde_json::to_value(&frame.event) {
            let key = format!("{}:{}:{}", value["type"], value["symbol"], value["intervalSecs"]);
            self.latest.insert(key, frame);
            while self.latest.len() > 5000 { self.latest.pop_first(); }
        }
    }
    fn frames(&self) -> Vec<EventFrame> {
        let mut frames: Vec<_> = self.alerts.iter().chain(self.latest.values()).cloned().collect();
        frames.sort_by_key(|f| f.event_id.rsplit(':').next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0));
        frames
    }
}

/// Binds `addr` and accepts connections forever, one task per client.
/// Each task gets its own `broadcast::Receiver` cloned from `events` —
/// same sender, so every client's receiver is fed the identical sequence
/// of messages.
pub async fn run(addr: &str, events: broadcast::Sender<ScanEvent>, auth: std::sync::Arc<crate::access::AuthLimiter>) -> Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding ws server to {addr}"))?;
    info!(addr, "ws server listening");

    // The channel client sockets actually read from. Sized by the same
    // constant as the upstream one in `main` -- raising only that one would
    // have done nothing for clients, since a slow socket lags *here*.
    let (live_tx, _) = broadcast::channel::<EventFrame>(crate::BROADCAST_CAPACITY);
    let snapshot = std::sync::Arc::new(tokio::sync::Mutex::new(EventSnapshot::default()));
    let mut source = events.subscribe();
    let history = snapshot.clone();
    let output = live_tx.clone();
    let epoch = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
    tokio::spawn(async move {
        let mut sequence = 0u64;
        loop {
            match source.recv().await {
                Ok(event) => {
                    sequence += 1;
                    let frame = EventFrame { event_id: format!("{epoch}:{sequence}"), sent_at: None, event };
                    history.lock().await.record(frame.clone());
                    let _ = output.send(frame);
                }
                Err(broadcast::error::RecvError::Lagged(n)) => warn!(n, "event history collector lagged; upstream events unavailable"),
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    let connections = std::sync::Arc::new(tokio::sync::Semaphore::new(64));
    loop {
        let (stream, peer) = listener.accept().await?;
        let Ok(permit) = connections.clone().try_acquire_owned() else { continue; };
        let events_rx = live_tx.subscribe();
        let snapshot = snapshot.clone();
        let auth = auth.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = handle_connection(stream, peer, events_rx, snapshot, auth).await {
                warn!(%peer, error = %e, "connection ended");
            }
        });
    }
}

async fn handle_connection(
    stream: TcpStream,
    peer: SocketAddr,
    mut events_rx: broadcast::Receiver<EventFrame>,
    snapshot: std::sync::Arc<tokio::sync::Mutex<EventSnapshot>>,
    auth: std::sync::Arc<crate::access::AuthLimiter>,
) -> Result<()> {
    let token = crate::access::configured_token();
    let expected_token = token.clone();
    let header_auth = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let auth_result = header_auth.clone();
    // Resolved inside the upgrade callback, which is the only place the
    // request headers are visible, and read back out afterwards.
    let client_ip = std::sync::Arc::new(std::sync::Mutex::new(peer.ip()));
    let ip_slot = client_ip.clone();
    let upgrade_auth = auth.clone();
    let limits = tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
        max_message_size: Some(16 * 1024), max_frame_size: Some(16 * 1024),
        ..Default::default()
    };
    let mut ws = tokio::time::timeout(HELLO_TIMEOUT, tokio_tungstenite::accept_hdr_async_with_config(stream,
        move |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
            let ip = crate::access::effective_client_ip(peer.ip(), request.headers());
            *ip_slot.lock().unwrap_or_else(|p| p.into_inner()) = ip;
            // Reject a throttled source at the upgrade itself -- cheaper than
            // letting it establish a socket only to fail the hello, and it is
            // the reconnect-and-retry loop that made this path abusable.
            if upgrade_auth.retry_after(ip).is_some() {
                return Err(tokio_tungstenite::tungstenite::http::Response::builder().status(429).body(Some("Too Many Requests".into())).unwrap());
            }
            let header = request.headers().get("authorization").and_then(|h| h.to_str().ok());
            let valid = crate::access::authorized(header, token.as_ref());
            if valid || header.is_none() {
                auth_result.store(valid,std::sync::atomic::Ordering::Relaxed);
                Ok(response)
            } else {
                upgrade_auth.record_failure(ip);
                Err(tokio_tungstenite::tungstenite::http::Response::builder().status(401).body(Some("Unauthorized".into())).unwrap())
            }
        }, Some(limits))).await.context("WebSocket upgrade timed out")?

        .context("ws upgrade handshake")?;
    info!(%peer, "client connected, awaiting hello");

    let client_ip = *client_ip.lock().unwrap_or_else(|p| p.into_inner());
    let client = await_hello(&mut ws, peer, header_auth.load(std::sync::atomic::Ordering::Relaxed), expected_token.as_ref(), &auth, client_ip).await?;
    if client != crate::protocol::ClientKind::AutoTrader {
        let history = snapshot.lock().await.frames();
        for frame in history { send_json(&mut ws, &frame.stamped()).await?; }
    }

    loop {
        tokio::select! {
            event = events_rx.recv() => {
                match event {
                    Ok(event) => send_json(&mut ws, &event.stamped()).await?,
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(%peer, skipped, "recovering retained events for lagging client");
                        if client == crate::protocol::ClientKind::AutoTrader { anyhow::bail!("trader feed gap requires historical reconciliation"); }
                        // Tell the client it lost data before resending the
                        // snapshot. The resend restores current state, but it
                        // cannot restore an event that came and went inside
                        // the gap -- so a client that was never told would
                        // render a complete-looking picture with holes in it,
                        // and a missed trading signal would be
                        // indistinguishable from one that never fired.
                        // Exactly one notification per lag occurrence.
                        send_json(&mut ws, &HandshakeMessage::StreamLagged { missed_events: skipped }).await?;
                        let history = snapshot.lock().await.frames();
                        for frame in history { send_json(&mut ws, &frame.stamped()).await?; }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        info!(%peer, "broadcast channel closed, ending connection");
                        return Ok(());
                    }
                }
            }
            msg = ws.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(ClientMessage::Ping { at }) = serde_json::from_str::<ClientMessage>(&text) {
                            send_json(&mut ws, &HandshakeMessage::Pong { at }).await?;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        info!(%peer, "client disconnected");
                        return Ok(());
                    }
                    Some(Err(e)) => return Err(e).context("ws read error"),
                    _ => {}
                }
            }
        }
    }
}

/// Blocks until the client sends a valid `hello`, replying `welcome`, or
/// bails after `HELLO_TIMEOUT` / a protocol-version mismatch / an early
/// disconnect. Nothing from the broadcast channel is sent to this
/// connection before this returns — every client says hello first, same
/// as the documented protocol.
async fn await_hello(ws: &mut WebSocketStream<TcpStream>, peer: SocketAddr, header_authenticated: bool, expected_token: Option<&crate::access::ApiToken>, auth: &crate::access::AuthLimiter, client_ip: std::net::IpAddr) -> Result<crate::protocol::ClientKind> {
    let deadline = tokio::time::sleep(HELLO_TIMEOUT);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            _ = &mut deadline => {
                anyhow::bail!("client did not send hello within {HELLO_TIMEOUT:?}");
            }
            msg = ws.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ClientMessage>(&text) {
                            Ok(ClientMessage::Hello { protocol_version, client, token }) => {
                                // A source that has already spent its allowance is refused
                                // outright, so reconnecting cannot buy more guesses. The reason
                                // is deliberately coarse: it says "not now", never anything
                                // about how close a credential came.
                                if auth.retry_after(client_ip).is_some() {
                                    send_json(ws,&HandshakeMessage::HelloRejected { reason:"Too many authentication attempts".into() }).await?;
                                    anyhow::bail!("throttled WebSocket hello");
                                }
                                let credential = token.map(|t| format!("Bearer {t}"));
                                if !header_authenticated && !crate::access::authorized(credential.as_deref(),expected_token) {
                                    auth.record_failure(client_ip);
                                    send_json(ws,&HandshakeMessage::HelloRejected { reason:"Unauthorized".into() }).await?;
                                    anyhow::bail!("unauthorized WebSocket hello");
                                }
                                if protocol_version != PROTOCOL_VERSION {
                                    let reason = format!(
                                        "unsupported protocol version {protocol_version}, server is on {PROTOCOL_VERSION}"
                                    );
                                    send_json(ws, &HandshakeMessage::HelloRejected { reason: reason.clone() }).await?;
                                    anyhow::bail!(reason);
                                }
                                send_json(ws, &HandshakeMessage::Welcome {
                                    protocol_version: PROTOCOL_VERSION,
                                    server_time: chrono::Utc::now().to_rfc3339(),
                                }).await?;
                                info!(%peer, ?client, "hello accepted");
                                return Ok(client);
                            }
                            Ok(ClientMessage::Ping { .. }) => continue, // out of order, harmless
                            Err(e) => {
                                warn!(%peer, error = %e, "unparseable message while awaiting hello");
                                continue;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        anyhow::bail!("client disconnected before hello");
                    }
                    Some(Err(e)) => return Err(e).context("ws read error while awaiting hello"),
                    _ => continue, // ping/pong/binary control frames
                }
            }
        }
    }
}

async fn send_json<T: serde::Serialize>(ws: &mut WebSocketStream<TcpStream>, value: &T) -> Result<()> {
    let text = serde_json::to_string(value).context("serializing outgoing message")?;
    tokio::time::timeout(Duration::from_secs(10), ws.send(Message::Text(text))).await.context("slow WebSocket client")??;
    Ok(())
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    use std::net::IpAddr;
    use std::sync::Arc;
    use crate::access::AuthLimiter;

    /// Drives one hello against a throwaway listener and returns the server's
    /// reply plus whether `await_hello` accepted it.
    async fn attempt_hello(
        auth: Arc<AuthLimiter>,
        client_ip: IpAddr,
        supplied: &str,
    ) -> (serde_json::Value, bool) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let expected = crate::access::ApiToken::new("secret").unwrap();
            await_hello(&mut ws, peer, false, Some(&expected), &auth, client_ip).await.is_ok()
        });
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{address}")).await.unwrap();
        ws.send(Message::Text(serde_json::json!({"type":"hello","protocolVersion":1,"client":"web","token":supplied}).to_string())).await.unwrap();
        let raw = ws.next().await.unwrap().unwrap().into_text().unwrap();
        let response: serde_json::Value = serde_json::from_str(&raw).unwrap();
        (response, server.await.unwrap())
    }

    #[tokio::test]
    async fn browser_hello_requires_authentication_before_welcome_or_event_delivery() {
        for supplied in ["wrong", "secret"] {
            let auth = Arc::new(AuthLimiter::new());
            let (response, accepted) = attempt_hello(auth, "203.0.113.5".parse().unwrap(), supplied).await;
            assert_eq!(response["type"], if supplied == "secret" { "welcome" } else { "hello_rejected" });
            assert_eq!(accepted, supplied == "secret");
        }
    }

    #[tokio::test]
    async fn repeated_failed_hellos_exhaust_that_source_ip_and_then_are_refused() {
        // The audited weakness: upgrade cheaply, send a bad token, get
        // rejected, reconnect forever. Each rejection now costs the source.
        let auth = Arc::new(AuthLimiter::with_policy(std::time::Duration::from_secs(60), 3));
        let attacker: IpAddr = "203.0.113.5".parse().unwrap();

        for n in 0..3 {
            let (response, accepted) = attempt_hello(auth.clone(), attacker, "wrong").await;
            assert_eq!(response["type"], "hello_rejected", "attempt {n}");
            assert_eq!(response["reason"], "Unauthorized", "attempt {n}");
            assert!(!accepted);
        }

        let (response, accepted) = attempt_hello(auth.clone(), attacker, "wrong").await;
        assert_eq!(response["type"], "hello_rejected");
        assert_eq!(response["reason"], "Too many authentication attempts");
        assert!(!accepted);

        // Even the correct credential is refused while the window stands --
        // reconnecting must not buy more guesses.
        let (response, accepted) = attempt_hello(auth, attacker, "secret").await;
        assert_eq!(response["reason"], "Too many authentication attempts");
        assert!(!accepted);
    }

    #[tokio::test]
    async fn one_throttled_source_does_not_affect_another_client() {
        let auth = Arc::new(AuthLimiter::with_policy(std::time::Duration::from_secs(60), 2));
        let attacker: IpAddr = "203.0.113.5".parse().unwrap();
        let bystander: IpAddr = "198.51.100.7".parse().unwrap();

        for _ in 0..4 {
            attempt_hello(auth.clone(), attacker, "wrong").await;
        }
        let (throttled, _) = attempt_hello(auth.clone(), attacker, "wrong").await;
        assert_eq!(throttled["reason"], "Too many authentication attempts");

        let (response, accepted) = attempt_hello(auth, bystander, "secret").await;
        assert_eq!(response["type"], "welcome", "an unrelated client must still connect");
        assert!(accepted);
    }

    #[tokio::test]
    async fn a_rejection_reveals_nothing_about_the_expected_credential() {
        let auth = Arc::new(AuthLimiter::new());
        let (response, _) = attempt_hello(auth, "203.0.113.5".parse().unwrap(), "secre").await;
        let text = response.to_string();
        assert!(!text.contains("secret"), "reason must not echo or hint at the token");
        assert_eq!(response["reason"], "Unauthorized");
    }
    #[test]
    fn frequent_telemetry_does_not_evict_retained_entry_alerts() {
        let mut snapshot = EventSnapshot::default();
        snapshot.record(EventFrame { event_id:"1:1".into(),sent_at: None, event:ScanEvent::IgnitionEvent {
            symbol:"TEST".into(),timestamp:chrono::Utc::now(),price:10.0,
            kind:market_data::IgnitionEventKind::FollowThroughConfirmed } });
        for n in 2..5002 {
            snapshot.record(EventFrame { event_id:format!("1:{n}"),sent_at: None, event:ScanEvent::FunnelHealth {
                timestamp:chrono::Utc::now(),float_budget_remaining:0,float_budget:240,starved_candidates:0,api_key_missing:false } });
        }
        let frames=snapshot.frames();
        assert_eq!(frames.len(),2);
        assert_eq!(frames[0].event_id,"1:1");
        assert_eq!(frames[1].event_id,"1:5001");
    }

    #[test]
    fn characterize_chart_reconnect_history_loss_and_late_snapshot_rewind() {
        // Audit characterization, not a claim that snapshot replay repairs history.
        let mut snapshot = EventSnapshot::default();
        for (seq,second) in [(1,0),(2,30),(3,60),(4,30)] {
            snapshot.record(EventFrame {event_id:format!("1:{seq}"),sent_at: None, event:ScanEvent::BarUpdate { coverage: market_data::events::Coverage::Unknown,
                symbol:"AUDIT".into(),timestamp:chrono::DateTime::from_timestamp(1_789_718_400+second,0).unwrap(),
                open:10.,high:12.,low:9.,close:11.,volume:5,interval_secs:30,is_final:false,
            }});
        }
        let frames=snapshot.frames();
        assert_eq!(frames.len(),1,"only latest arrival per symbol/interval is retained");
        assert_eq!(frames[0].event_id,"1:4","late correction replaces newer bucket in snapshot");
    }

    #[tokio::test]
    async fn characterize_upstream_broadcast_overflow() {
        let (tx,mut rx)=tokio::sync::broadcast::channel::<u64>(4);
        for n in 0..10 {tx.send(n).unwrap();}
        assert!(matches!(rx.recv().await,Err(tokio::sync::broadcast::error::RecvError::Lagged(6))));
        assert_eq!(rx.recv().await.unwrap(),6);
        // Collector IDs are assigned after recv; its existing warn-only Lagged arm
        // cannot communicate these six missing inputs to client sequence tracking.
    }
}

#[cfg(test)]
mod sent_at_tests {
    use super::*;
    use market_data::ScanEvent;

    fn frame() -> EventFrame {
        EventFrame {
            event_id: "1:1".into(),
            sent_at: None,
            event: ScanEvent::BarUpdate { coverage: market_data::events::Coverage::Unknown,
                symbol: "DDC".into(),
                timestamp: "2026-09-21T15:37:00Z".parse().unwrap(),
                open: 10.0, high: 11.0, low: 9.0, close: 10.5,
                volume: 96_108, interval_secs: 60, is_final: false,
            },
        }
    }

    /// Backward compatibility: an unstamped frame serialises exactly as it did
    /// before the field existed, so an older client sees no change.
    #[test]
    fn an_unstamped_frame_omits_the_field_entirely() {
        let v = serde_json::to_value(frame()).unwrap();
        assert!(v.get("sentAt").is_none(), "sentAt must be absent, not null: {v}");
        assert_eq!(v["eventId"], "1:1");
        assert_eq!(v["type"], "bar_update");
    }

    /// The field appears only once stamped, and is camelCase like the rest of
    /// the envelope.
    #[test]
    fn stamping_adds_a_parseable_utc_instant() {
        let v = serde_json::to_value(frame().stamped()).unwrap();
        let sent = v["sentAt"].as_str().expect("sentAt present after stamping");
        let parsed = chrono::DateTime::parse_from_rfc3339(sent).expect("sentAt is RFC3339");
        // Stamped now, so it must be close to now and not in the future.
        let skew = (chrono::Utc::now() - parsed.with_timezone(&chrono::Utc)).num_seconds();
        assert!((0..5).contains(&skew), "sentAt should be ~now, skew was {skew}s");
        // Microsecond precision, so sub-millisecond deltas are expressible.
        assert!(sent.contains('.'), "expected fractional seconds in {sent}");
    }

    /// The two timestamps answer different questions and must not be
    /// conflated. This is the failure mode that made the 2026-09-21 audit's
    /// first latency reading wrong.
    #[test]
    fn sent_at_does_not_overwrite_or_equal_the_market_timestamp() {
        let v = serde_json::to_value(frame().stamped()).unwrap();
        assert_eq!(v["timestamp"], "2026-09-21T15:37:00Z", "market timestamp preserved");
        assert_ne!(v["sentAt"], v["timestamp"]);
        // Market time is in the past relative to transmission.
        let market = chrono::DateTime::parse_from_rfc3339(v["timestamp"].as_str().unwrap()).unwrap();
        let sent = chrono::DateTime::parse_from_rfc3339(v["sentAt"].as_str().unwrap()).unwrap();
        assert!(sent > market);
    }

    /// Re-transmission gets a NEW stamp. A snapshot frame replayed to a
    /// reconnecting client describes this delivery, not the original one --
    /// otherwise a reconnect would report hours of apparent latency, which is
    /// precisely the artefact this field exists to avoid.
    #[test]
    fn each_transmission_is_stamped_independently() {
        let base = frame();
        let first = base.clone().stamped();
        std::thread::sleep(std::time::Duration::from_millis(3));
        let second = base.clone().stamped();
        assert!(first.sent_at.is_some() && second.sent_at.is_some());
        assert_ne!(first.sent_at, second.sent_at);
        // And the stored/broadcast original is untouched by either.
        assert!(base.sent_at.is_none());
    }
}
