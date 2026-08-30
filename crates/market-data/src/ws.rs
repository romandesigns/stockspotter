//! Connects to Alpaca's realtime bars WebSocket, drives the connect/auth/
//! subscribe handshake, and yields parsed message batches. Deliberately
//! thin — reconnect/backoff logic belongs to the caller (the future scan
//! loop that runs continuously in prod), not baked in here.

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use tracing::{info, warn};

use crate::bar::AlpacaMessage;
use crate::config::AlpacaConfig;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct AlpacaStream {
    socket: Socket,
}

impl AlpacaStream {
    /// Connects, authenticates, and subscribes to bar updates for
    /// `symbols` in one call — a partially-set-up stream isn't a state
    /// worth exposing to callers.
    pub async fn connect(cfg: &AlpacaConfig, symbols: &[String]) -> Result<Self> {
        let (mut socket, _) = connect_async(&cfg.market_ws)
            .await
            .with_context(|| format!("connecting to {}", cfg.market_ws))?;

        // Alpaca sends an unprompted "connected" success message first.
        let greeting = read_batch(&mut socket)
            .await?
            .context("stream closed before sending a connect ack")?;
        info!(?greeting, "alpaca ws: connect ack");

        let auth = serde_json::json!({
            "action": "auth",
            "key": cfg.api_key.clone(),
            "secret": cfg.api_secret.clone(),
        });
        socket.send(Message::Text(auth.to_string())).await?;
        let auth_resp = read_batch(&mut socket)
            .await?
            .context("stream closed during auth")?;
        let authenticated = auth_resp
            .iter()
            .any(|m| matches!(m, AlpacaMessage::Success { msg } if msg == "authenticated"));
        if !authenticated {
            // Send a real WS close frame instead of just dropping the
            // socket — an abrupt TCP drop may leave Alpaca's server
            // thinking this connection is still open until its own
            // keepalive timeout fires, which would make the *next*
            // connect attempt fail with the exact same "connection limit
            // exceeded" error even though nothing else is actually
            // connected. Empirically this was worth doing: repeated
            // failed attempts here were plausible self-inflicted phantom
            // connections, not necessarily a real external conflict.
            let _ = socket.close(None).await;
            bail!("alpaca ws auth failed, response: {auth_resp:?}");
        }
        info!("alpaca ws: authenticated");

        if !symbols.is_empty() {
            // Bars feed the fast funnel + momentum scorer; trades/quotes
            // feed the ignition detector's tick-level signals. All three
            // in one subscribe call, same connection.
            let subscribe = serde_json::json!({
                "action": "subscribe",
                "bars": symbols,
                "trades": symbols,
                "quotes": symbols,
                "statuses": symbols,
            });
            socket.send(Message::Text(subscribe.to_string())).await?;
            let sub_resp = read_batch(&mut socket)
                .await?
                .context("stream closed during subscribe")?;
            info!(?sub_resp, "alpaca ws: subscribed");
        }

        Ok(Self { socket })
    }

    /// Waits for the next batch of messages. `Ok(None)` means the server
    /// closed the connection cleanly.
    pub async fn next_batch(&mut self) -> Result<Option<Vec<AlpacaMessage>>> {
        read_batch(&mut self.socket).await
    }
}

async fn read_batch(socket: &mut Socket) -> Result<Option<Vec<AlpacaMessage>>> {
    loop {
        match socket.next().await {
            None => return Ok(None),
            Some(Err(e)) => return Err(e).context("alpaca ws read error"),
            Some(Ok(Message::Text(txt))) => {
                let batch: Vec<AlpacaMessage> = serde_json::from_str(&txt)
                    .with_context(|| format!("parsing alpaca ws message: {txt}"))?;
                return Ok(Some(batch));
            }
            Some(Ok(Message::Close(frame))) => {
                warn!(?frame, "alpaca ws: server closed connection");
                return Ok(None);
            }
            // Ping/Pong/Binary frames carry nothing we need; tungstenite
            // answers pings automatically.
            Some(Ok(_)) => continue,
        }
    }
}
