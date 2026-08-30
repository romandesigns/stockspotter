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

/// Binds `addr` and accepts connections forever, one task per client.
/// Each task gets its own `broadcast::Receiver` cloned from `events` —
/// same sender, so every client's receiver is fed the identical sequence
/// of messages.
pub async fn run(addr: &str, events: broadcast::Sender<ScanEvent>) -> Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding ws server to {addr}"))?;
    info!(addr, "ws server listening");

    loop {
        let (stream, peer) = listener.accept().await?;
        let events_rx = events.subscribe();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, peer, events_rx).await {
                warn!(%peer, error = %e, "connection ended");
            }
        });
    }
}

async fn handle_connection(
    stream: TcpStream,
    peer: SocketAddr,
    mut events_rx: broadcast::Receiver<ScanEvent>,
) -> Result<()> {
    let mut ws = tokio_tungstenite::accept_async(stream)
        .await
        .context("ws upgrade handshake")?;
    info!(%peer, "client connected, awaiting hello");

    await_hello(&mut ws, peer).await?;

    loop {
        tokio::select! {
            event = events_rx.recv() => {
                match event {
                    Ok(event) => send_json(&mut ws, &event).await?,
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(%peer, skipped, "client lagged behind the broadcast — some events were dropped for it, not altered");
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
async fn await_hello(ws: &mut WebSocketStream<TcpStream>, peer: SocketAddr) -> Result<()> {
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
                            Ok(ClientMessage::Hello { protocol_version, client }) => {
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
                                return Ok(());
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
    ws.send(Message::Text(text)).await.context("sending ws message")?;
    Ok(())
}
