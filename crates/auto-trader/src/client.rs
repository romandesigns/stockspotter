//! Connects to `ws-server` as a genuine WS client — same handshake
//! browsers/mobile/desktop use, over the internal Docker network
//! (`ws://ws:8787`), not an in-process broadcast subscriber. Modeled
//! directly on `market_data::ws::AlpacaStream`'s connect/read shape,
//! pointed at `ws-server`'s own protocol instead of Alpaca's.
//!
//! The three handshake message shapes (`hello`/`welcome`/`hello_rejected`)
//! are hand-rolled locally rather than depending on `ws-server`'s own
//! `protocol.rs` — deliberate, not an oversight: `ws-server` has no
//! `[lib]` target to depend on, the whole handshake surface is 3 tiny
//! JSON shapes already pinned by `ws-server`'s own tests, and a drift
//! here just fails the handshake loudly (see `connect`'s `bail!`s), not
//! silently. Detection event payloads are NOT duplicated this way —
//! those are real reuse of `market_data::events::ScanEvent` (see
//! `next_event`), the piece with real ongoing logic/drift risk.

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use market_data::ScanEvent;
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use tracing::{info, warn};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

const PROTOCOL_VERSION: u32 = 1;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientHello {
    #[serde(rename = "type")]
    kind: &'static str,
    protocol_version: u32,
    client: &'static str,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum HandshakeResponse {
    #[serde(rename = "welcome")]
    Welcome {},
    #[serde(rename = "hello_rejected")]
    HelloRejected { reason: String },
}

pub struct AutoTraderClient {
    socket: Socket,
}

impl AutoTraderClient {
    /// Connects and completes the hello/welcome handshake in one call —
    /// same "no partially-set-up stream" reasoning as `AlpacaStream::connect`.
    pub async fn connect(ws_url: &str) -> Result<Self> {
        let (mut socket, _) = connect_async(ws_url).await.with_context(|| format!("connecting to {ws_url}"))?;

        let hello = ClientHello { kind: "hello", protocol_version: PROTOCOL_VERSION, client: "auto_trader" };
        socket
            .send(Message::Text(serde_json::to_string(&hello).context("serializing hello")?))
            .await
            .context("sending hello")?;

        let response = read_text(&mut socket).await?.context("stream closed before sending a welcome")?;
        match serde_json::from_str::<HandshakeResponse>(&response) {
            Ok(HandshakeResponse::Welcome {}) => info!("auto-trader: connected and welcomed by ws-server"),
            Ok(HandshakeResponse::HelloRejected { reason }) => bail!("ws-server rejected hello: {reason}"),
            Err(e) => bail!("unexpected handshake response {response:?}: {e}"),
        }

        Ok(Self { socket })
    }

    /// Waits for the next real detection event. `Ok(None)` means the
    /// server closed the connection cleanly — the caller's own loop
    /// decides whether/how to reconnect, same division of responsibility
    /// as `AlpacaStream::next_batch`. Any wire message that isn't a
    /// parseable `ScanEvent` is logged and skipped rather than tearing
    /// down the whole connection — `ws-server` only ever broadcasts
    /// `ScanEvent`s after the handshake, so this should never actually
    /// happen, but a forward-compatible unknown message shouldn't be fatal.
    pub async fn next_event(&mut self) -> Result<Option<ScanEvent>> {
        loop {
            let Some(text) = read_text(&mut self.socket).await? else {
                return Ok(None);
            };
            match serde_json::from_str::<ScanEvent>(&text) {
                Ok(event) => return Ok(Some(event)),
                Err(e) => {
                    warn!(error = %e, raw = %text, "auto-trader: ignoring an unparseable broadcast message");
                }
            }
        }
    }
}

async fn read_text(socket: &mut Socket) -> Result<Option<String>> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => return Ok(Some(text)),
            Some(Ok(Message::Close(_))) | None => return Ok(None),
            Some(Ok(_other)) => continue, // ping/pong/binary frames -- nothing we act on
            Some(Err(e)) => return Err(e).context("reading from ws-server connection"),
        }
    }
}
