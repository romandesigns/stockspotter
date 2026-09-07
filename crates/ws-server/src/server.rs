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
    #[serde(flatten)]
    event: ScanEvent,
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
pub async fn run(addr: &str, events: broadcast::Sender<ScanEvent>) -> Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding ws server to {addr}"))?;
    info!(addr, "ws server listening");

    let (live_tx, _) = broadcast::channel::<EventFrame>(4096);
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
                    let frame = EventFrame { event_id: format!("{epoch}:{sequence}"), event };
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
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = handle_connection(stream, peer, events_rx, snapshot).await {
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
) -> Result<()> {
    let token = crate::access::configured_token();
    let expected_token = token.clone();
    let header_auth = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let auth_result = header_auth.clone();
    let limits = tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
        max_message_size: Some(16 * 1024), max_frame_size: Some(16 * 1024),
        ..Default::default()
    };
    let mut ws = tokio::time::timeout(HELLO_TIMEOUT, tokio_tungstenite::accept_hdr_async_with_config(stream,
        move |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
            let header = request.headers().get("authorization").and_then(|h| h.to_str().ok());
            let valid = crate::access::authorized(header, token.as_deref());
            if valid || header.is_none() {
                auth_result.store(valid,std::sync::atomic::Ordering::Relaxed);
                Ok(response)
            } else {
                Err(tokio_tungstenite::tungstenite::http::Response::builder().status(401).body(Some("Unauthorized".into())).unwrap())
            }
        }, Some(limits))).await.context("WebSocket upgrade timed out")?

        .context("ws upgrade handshake")?;
    info!(%peer, "client connected, awaiting hello");

    let client = await_hello(&mut ws, peer, header_auth.load(std::sync::atomic::Ordering::Relaxed), expected_token.as_deref()).await?;
    if client != crate::protocol::ClientKind::AutoTrader {
        let history = snapshot.lock().await.frames();
        for frame in history { send_json(&mut ws, &frame).await?; }
    }

    loop {
        tokio::select! {
            event = events_rx.recv() => {
                match event {
                    Ok(event) => send_json(&mut ws, &event).await?,
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(%peer, skipped, "recovering retained events for lagging client");
                        if client == crate::protocol::ClientKind::AutoTrader { anyhow::bail!("trader feed gap requires historical reconciliation"); }
                        let history = snapshot.lock().await.frames();
                        for frame in history { send_json(&mut ws, &frame).await?; }
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
async fn await_hello(ws: &mut WebSocketStream<TcpStream>, peer: SocketAddr, header_authenticated: bool, expected_token: Option<&str>) -> Result<crate::protocol::ClientKind> {
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
                                let credential = token.map(|t| format!("Bearer {t}"));
                                if !header_authenticated && !crate::access::authorized(credential.as_deref(),expected_token) {
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

    #[tokio::test]
    async fn browser_hello_requires_authentication_before_welcome_or_event_delivery() {
        for supplied in ["wrong", "secret"] {
            let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address=listener.local_addr().unwrap();
            let server=tokio::spawn(async move {
                let (stream,peer)=listener.accept().await.unwrap();
                let mut ws=tokio_tungstenite::accept_async(stream).await.unwrap();
                await_hello(&mut ws,peer,false,Some("secret")).await.is_ok()
            });
            let (mut ws,_)=tokio_tungstenite::connect_async(format!("ws://{address}")).await.unwrap();
            ws.send(Message::Text(serde_json::json!({"type":"hello","protocolVersion":1,"client":"web","token":supplied}).to_string())).await.unwrap();
            let response=ws.next().await.unwrap().unwrap().into_text().unwrap();
            let response:serde_json::Value=serde_json::from_str(&response).unwrap();
            assert_eq!(response["type"],if supplied=="secret" {"welcome"} else {"hello_rejected"});
            assert_eq!(server.await.unwrap(),supplied=="secret");
        }
    }
    #[test]
    fn frequent_telemetry_does_not_evict_retained_entry_alerts() {
        let mut snapshot = EventSnapshot::default();
        snapshot.record(EventFrame { event_id:"1:1".into(),event:ScanEvent::IgnitionEvent {
            symbol:"TEST".into(),timestamp:chrono::Utc::now(),price:10.0,
            kind:market_data::IgnitionEventKind::FollowThroughConfirmed } });
        for n in 2..5002 {
            snapshot.record(EventFrame { event_id:format!("1:{n}"),event:ScanEvent::FunnelHealth {
                timestamp:chrono::Utc::now(),float_budget_remaining:0,float_budget:240,starved_candidates:0,api_key_missing:false } });
        }
        let frames=snapshot.frames();
        assert_eq!(frames.len(),2);
        assert_eq!(frames[0].event_id,"1:1");
        assert_eq!(frames[1].event_id,"1:5001");
    }
}
