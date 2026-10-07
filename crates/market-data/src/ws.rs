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
    wildcard_requested: Option<std::time::Instant>,
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
                "updatedBars": symbols,
                "lulds": symbols,
                "trades": symbols,
                "quotes": symbols,
                "statuses": symbols,
            });
            socket.send(Message::Text(subscribe.to_string())).await?;
            let sub_resp = read_batch(&mut socket)
                .await?
                .context("stream closed during subscribe")?;
            anyhow::ensure!(sub_resp.iter().any(|m| matches!(m, AlpacaMessage::Subscription { bars, .. }
                if symbols.iter().all(|s| bars.contains(s)))), "Alpaca did not accept initial subscription: {sub_resp:?}");
            info!(?sub_resp, "alpaca ws: subscribed");
        }

        Ok(Self { socket, wildcard_requested: None })
    }

    /// Waits for the next batch of messages. `Ok(None)` means the server
    /// closed the connection cleanly.
    pub async fn next_batch(&mut self) -> Result<Option<Vec<AlpacaMessage>>> {
        let batch = if let Some(start) = self.wildcard_requested {
            let remaining = std::time::Duration::from_secs(10).saturating_sub(start.elapsed());
            tokio::time::timeout(remaining, read_batch(&mut self.socket)).await
                .context("Alpaca wildcard subscription acknowledgement timed out")??
        } else { read_batch(&mut self.socket).await? };
        for msg in batch.iter().flatten() {
            match msg {
                AlpacaMessage::Error { code, msg } => bail!("Alpaca stream error {code}: {msg}"),
                AlpacaMessage::Subscription { trades, statuses, .. } if self.wildcard_requested.is_some() => {
                    if trades.iter().any(|s| s == "*") && statuses.iter().any(|s| s == "*") {
                        self.wildcard_requested = None;
                        info!("Alpaca accepted full-market trades and statuses");
                    }
                }
                _ => {}
            }
        }
        Ok(batch)
    }

    /// Adds `symbols` to an already-open, already-subscribed stream —
    /// Alpaca's protocol supports sending another `subscribe` action at
    /// any time on the same connection, not just once at connect. This
    /// is what lets `live::run_live_scan`'s periodic universe rescan
    /// promote newly-qualifying symbols without reconnecting (which
    /// would drop every other symbol's accumulated tracking state too).
    ///
    /// Deliberately does *not* wait for/consume the `subscription` ack
    /// here — unlike `connect`'s initial subscribe, other real data
    /// messages may already be interleaved on this connection, so trying
    /// to read "the next message" and assume it's the ack would be
    /// wrong. The caller's normal `next_batch` loop sees the ack
    /// eventually (`AlpacaMessage::Subscription`) same as any other
    /// message; nothing currently needs to act on it specifically.
    /// Subscribes to the **entire market's** trade tape plus trading
    /// statuses, via Alpaca's `"*"` wildcard.
    ///
    /// This is what makes the architecture doc's literal requirement
    /// achievable — ignition detection "watches the entire eligible
    /// universe continuously", not a pre-filtered shortlist. Confirmed
    /// accepted by this account's SIP feed (2026-09-06): subscribing
    /// `trades: ["*"]` returns
    /// `{"T":"subscription","trades":["*"],...}` rather than an error.
    ///
    /// **Trades and statuses only, deliberately not quotes.** The quote
    /// tape runs roughly an order of magnitude larger than the trade
    /// tape, and full-market quotes would dominate this process's
    /// budget. What that costs is precise and worth stating: of
    /// ignition's four raw signals, universe-wide coverage gets
    /// trade-frequency spikes and halt-lift resumptions, and loses
    /// spread-tightening and ask-absorption, which need top-of-book.
    /// `detect()` ORs its triggers, so a trade-frequency spike alone
    /// still opens a candidate, and follow-through confirmation is
    /// entirely price-based — so a universe-tier symbol gets a real,
    /// fully-confirmed alert, just from a narrower evidence base than a
    /// funnel-tracked symbol whose quotes are also streaming.
    ///
    /// Statuses are included because they're rare, cheap, and carry the
    /// halt-lift transition — which is one of the signals most worth
    /// having across the whole market rather than a shortlist.
    pub async fn subscribe_all_trades(&mut self) -> Result<()> {
        let subscribe = serde_json::json!({
            "action": "subscribe",
            "trades": ["*"],
            "statuses": ["*"],
        });
        self.socket.send(Message::Text(subscribe.to_string())).await?;
        self.wildcard_requested = Some(std::time::Instant::now());
        info!("alpaca ws: requested FULL-MARKET trade + status subscription");
        Ok(())
    }

    pub async fn subscribe(&mut self, symbols: &[String]) -> Result<()> {
        if symbols.is_empty() {
            return Ok(());
        }
        let msg = serde_json::json!({
            "action": "subscribe",
            "bars": symbols,
                "updatedBars": symbols,
                "lulds": symbols,
            "trades": symbols,
            "quotes": symbols,
            "statuses": symbols,
        });
        self.socket
            .send(Message::Text(msg.to_string()))
            .await
            .context("sending subscribe for additional symbols")?;
        info!(?symbols, "alpaca ws: requested subscribe for additional symbols");
        Ok(())
    }

    /// Drops `symbols` from an already-open stream — the other half of
    /// dynamic promotion/demotion. Same non-blocking ack handling as
    /// `subscribe`.
    pub async fn unsubscribe(&mut self, symbols: &[String]) -> Result<()> {
        if symbols.is_empty() {
            return Ok(());
        }
        let msg = serde_json::json!({
            "action": "unsubscribe",
            "bars": symbols,
                "updatedBars": symbols,
                "lulds": symbols,
            "trades": symbols,
            "quotes": symbols,
            "statuses": symbols,
        });
        self.socket
            .send(Message::Text(msg.to_string()))
            .await
            .context("sending unsubscribe for dropped symbols")?;
        info!(?symbols, "alpaca ws: requested unsubscribe for dropped symbols");
        Ok(())
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
