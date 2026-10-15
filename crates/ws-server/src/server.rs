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
//!
//! One deliberate exception: order-flow summaries. A connection that has
//! already said hello may name the few symbols it has charts open for
//! (`subscribe_order_flow`) and is then sent an `order_flow` frame for
//! each every `ORDER_FLOW_PUSH_INTERVAL`. That is per-connection by
//! nature -- a summary is only ever read for the symbol on screen, and
//! broadcasting every tracked symbol's flow to everyone is what the HTTP
//! endpoint was built to avoid. It does not go through the broadcast
//! channel, is not a `ScanEvent`, carries no `eventId`, and is never
//! retained or replayed, so the guarantee above -- and the observation
//! layer that depends on the `ScanEvent` stream -- are untouched by it.
//!
//! Three things keep it from being a cost the running app pays by default:
//!
//! - only an authenticated, welcomed connection can subscribe at all
//!   (`await_hello` ignores the message), and never the auto-trader;
//! - with `ORDER_FLOW_ENABLED` unset the server answers a subscription
//!   once, saying the feature is off, and then sends nothing further. It
//!   does not stream a frame per symbol every two seconds forever telling
//!   the client the same thing;
//! - each frame is the cached lite read (`order_flow_store::lite_read`),
//!   so a fan-out of connections does not rebuild the chart arrays per
//!   connection, nor serialize them.

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

/// How often a subscribed connection is sent each symbol's order flow.
/// The HTTP poll this replaces runs every 5 seconds because it shares a
/// request budget with the rest of the app; a push has no such limit, and
/// this is set by bytes instead: a lite summary is about 3 KB.
const ORDER_FLOW_PUSH_INTERVAL: Duration = Duration::from_secs(2);
/// A client shows at most four charts. The rest of a longer list is
/// ignored rather than rejected.
const MAX_ORDER_FLOW_SYMBOLS: usize = 4;

/// What a connection may subscribe to: a short list of plausible tickers.
/// The symbols only ever become map lookups, but they are also echoed back
/// in every frame, so junk is dropped here.
fn order_flow_symbols(requested: Vec<String>) -> Vec<String> {
    let mut symbols: Vec<String> = Vec::new();
    for symbol in requested {
        let ok = !symbol.is_empty() && symbol.len() <= 12 && symbol.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '.');
        if ok && !symbols.contains(&symbol) {
            symbols.push(symbol);
        }
        if symbols.len() == MAX_ORDER_FLOW_SYMBOLS {
            break;
        }
    }
    symbols
}

/// One pushed frame: the body `GET /orderflow/:symbol` returns, tagged so
/// a client can route it. Always the lite shape -- the chart arrays are
/// large and change slowly, and stay an occasional HTTP fetch.
fn order_flow_frame(symbol: &str, now: chrono::DateTime<chrono::Utc>) -> serde_json::Value {
    let mut frame = crate::order_flow_wire::body(symbol, market_data::order_flow_store::lite_read(symbol, now), crate::order_flow_wire::Detail::Lite);
    if let Some(object) = frame.as_object_mut() {
        object.insert("type".into(), serde_json::Value::String("order_flow".into()));
    }
    frame
}

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
pub async fn run_with_strategies(addr: &str, events: broadcast::Sender<ScanEvent>, auth: std::sync::Arc<crate::access::AuthLimiter>, strategies: crate::experimental_strategies::Hub) -> Result<()> {
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
                    let frame = EventFrame { event_id: format!("{epoch}:{sequence}"), event };
                    history.lock().await.record(frame.clone());
                    let _ = output.send(frame);
                }
                Err(broadcast::error::RecvError::Lagged(n)) => warn!(n, "event history collector lagged; upstream events unavailable"),
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    let connections = std::sync::Arc::new(tokio::sync::Semaphore::new(if strategies.enabled() {96} else {64}));
    let primary_connections = std::sync::Arc::new(tokio::sync::Semaphore::new(64));
    let research_connections = std::sync::Arc::new(tokio::sync::Semaphore::new(32));
    loop {
        let (stream, peer) = listener.accept().await?;
        let Ok(permit) = connections.clone().try_acquire_owned() else { continue; };
        let events_rx = live_tx.subscribe();
        let snapshot = snapshot.clone();
        let auth = auth.clone();
        let strategies = strategies.clone();
        let research_connections = research_connections.clone();
        let primary_connections = primary_connections.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, peer, events_rx, snapshot, auth, strategies, permit, primary_connections, research_connections).await {
                warn!(%peer, error = %e, "connection ended");
            }
        });
    }
}

async fn handle_connection(
    stream: TcpStream,
    peer: SocketAddr,
    events_rx: broadcast::Receiver<EventFrame>,
    snapshot: std::sync::Arc<tokio::sync::Mutex<EventSnapshot>>,
    auth: std::sync::Arc<crate::access::AuthLimiter>,
    strategies: crate::experimental_strategies::Hub,
    admission_permit: tokio::sync::OwnedSemaphorePermit,
    primary_connections: std::sync::Arc<tokio::sync::Semaphore>,
    research_connections: std::sync::Arc<tokio::sync::Semaphore>,
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
    let (client, wants_strategies, experimental_only) = await_hello(&mut ws, peer, header_auth.load(std::sync::atomic::Ordering::Relaxed), expected_token.as_ref(), &auth, client_ip).await?;
    // Experimental outputs never enter ScanEvent/EventFrame or the auto-trader.
    if experimental_only {
        if !strategies.enabled() {
            for message in strategies.snapshot() {send_json(&mut ws,&message).await?;}
            send_json(&mut ws,&HandshakeMessage::HelloRejected {reason:"Experimental strategies disabled".into()}).await?;
            return Ok(());
        }
        if !experimental_delivery_allowed(client, wants_strategies, strategies.enabled()) {
            send_json(&mut ws,&HandshakeMessage::HelloRejected {reason:"Unsupported experimental client".into()}).await?;
            anyhow::bail!("unsupported experimental client");
        }
        drop(admission_permit);
        drop(events_rx);
        let Ok(_research_permit)=research_connections.try_acquire_owned() else {
            send_json(&mut ws,&HandshakeMessage::HelloRejected {reason:"Experimental connection capacity reached".into()}).await?;
            return Ok(());
        };
        return handle_strategy_connection(&mut ws,&strategies).await;
    }
    // Flags off: keep the original 64-slot accept/connection behavior exactly.
    // Flags on: bounded handshake admission is separate from 64 primary and
    // 32 research active connections, so research clients cannot occupy primary slots.
    let mut admission_permit=Some(admission_permit);
    let _primary_permit=if strategies.enabled() {
        admission_permit.take();
        let Ok(permit)=primary_connections.try_acquire_owned() else {
            send_json(&mut ws,&HandshakeMessage::HelloRejected {reason:"Connection capacity reached".into()}).await?;
            return Ok(());
        };
        Some(permit)
    } else {None};
    // The admission/primary permits above stay alive in this scope for as
    // long as the connection is served.
    serve_client(ws, peer, client, events_rx, snapshot).await
}

/// Everything after an accepted hello: the retained snapshot, then the
/// live stream until either side goes away. Split out of
/// `handle_connection` so the per-connection order-flow subscription can
/// be driven in a test without the process-wide access token and the
/// admission semaphores that the upgrade and hello read.
async fn serve_client(
    mut ws: WebSocketStream<TcpStream>,
    peer: SocketAddr,
    client: crate::protocol::ClientKind,
    mut events_rx: broadcast::Receiver<EventFrame>,
    snapshot: std::sync::Arc<tokio::sync::Mutex<EventSnapshot>>,
) -> Result<()> {
    if client != crate::protocol::ClientKind::AutoTrader {
        let history = snapshot.lock().await.frames();
        for frame in history { send_json(&mut ws, &frame).await?; }
    }

    // Order-flow symbols this connection has charts open for. Empty
    // unless it asked, and the tick below does not run while it is empty
    // or while the store is off -- a server with the feature disabled
    // answers the subscription once and then stays quiet.
    let mut flow_symbols: Vec<String> = Vec::new();
    let mut flow_tick = tokio::time::interval(ORDER_FLOW_PUSH_INTERVAL);
    // A connection stalled behind a slow socket should resume at the
    // normal cadence, not fire every tick it missed.
    flow_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = flow_tick.tick(), if !flow_symbols.is_empty() && market_data::order_flow_store::enabled() => {
                let now = chrono::Utc::now();
                for symbol in &flow_symbols {
                    send_json(&mut ws, &order_flow_frame(symbol, now)).await?;
                }
            }
            event = events_rx.recv() => {
                match event {
                    Ok(event) => send_json(&mut ws, &event).await?,
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
                        match serde_json::from_str::<ClientMessage>(&text) {
                            Ok(ClientMessage::Ping { at }) => send_json(&mut ws, &HandshakeMessage::Pong { at }).await?,
                            // The trader acts on ScanEvents alone; it has
                            // no business being sent research data.
                            Ok(ClientMessage::SubscribeOrderFlow { symbols }) if client != crate::protocol::ClientKind::AutoTrader => {
                                flow_symbols = order_flow_symbols(symbols);
                                // Answered straight away rather than after
                                // up to a full interval: this one frame per
                                // symbol is also what tells a client whether
                                // the server pushes at all, and whether the
                                // feature is on. With the feature off it is
                                // the ONLY thing sent -- the recurring tick
                                // above does not run.
                                let now = chrono::Utc::now();
                                for symbol in &flow_symbols {
                                    send_json(&mut ws, &order_flow_frame(symbol, now)).await?;
                                }
                                flow_tick.reset();
                            }
                            _ => {}
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

/// A distinct authenticated socket: research backpressure cannot close the
/// ordinary EventFrame socket or deliver experimental outputs to its consumers.
async fn handle_strategy_connection(ws: &mut WebSocketStream<TcpStream>, hub: &crate::experimental_strategies::Hub) -> Result<()> {
    use crate::experimental_strategies::Message as StrategyMessage;
    let mut rx=hub.output.subscribe();
    for message in hub.snapshot() {send_json(ws,&message).await?;}
    loop {tokio::select! {
        event=rx.recv()=>match event {
            Ok(message)=>send_json(ws,&message).await?,
            Err(broadcast::error::RecvError::Lagged(missed))=>{
                hub.record_output_lag(missed);
                send_json(ws,&serde_json::json!({"type":"experimental_strategy_gap"})).await?;
                // Never amplify a lag by resending a hundred historical rows.
                for message in hub.snapshot() {if matches!(message,StrategyMessage::Status(_)) {send_json(ws,&message).await?;}}
            }
            Err(broadcast::error::RecvError::Closed)=>return Ok(()),
        },
        message=ws.next()=>match message {
            Some(Ok(Message::Text(text)))=>{
                if let Ok(ClientMessage::Ping {at})=serde_json::from_str::<ClientMessage>(&text) {send_json(ws,&HandshakeMessage::Pong {at}).await?;}
            }
            Some(Ok(Message::Close(_)))|None=>return Ok(()),
            Some(Err(error))=>return Err(error).context("experimental ws read"),
            _=>{},
        }
    }}
}

fn experimental_delivery_allowed(client: crate::protocol::ClientKind, opted_in: bool, enabled: bool) -> bool {
    opted_in && enabled && matches!(client, crate::protocol::ClientKind::Web | crate::protocol::ClientKind::Desktop)
}

/// Blocks until the client sends a valid `hello`, replying `welcome`, or
/// bails after `HELLO_TIMEOUT` / a protocol-version mismatch / an early
/// disconnect. Nothing from the broadcast channel is sent to this
/// connection before this returns — every client says hello first, same
/// as the documented protocol.
async fn await_hello(ws: &mut WebSocketStream<TcpStream>, peer: SocketAddr, header_authenticated: bool, expected_token: Option<&crate::access::ApiToken>, auth: &crate::access::AuthLimiter, client_ip: std::net::IpAddr) -> Result<(crate::protocol::ClientKind,bool,bool)> {
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
                            Ok(ClientMessage::Hello { protocol_version, client, token, experimental_strategies, experimental_only }) => {
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
                                return Ok((client,experimental_strategies,experimental_only));
                            }
                            Ok(ClientMessage::Ping { .. }) => continue, // out of order, harmless
                            // Nothing is delivered before hello, including
                            // this: an unauthenticated socket cannot
                            // subscribe to anything.
                            Ok(ClientMessage::SubscribeOrderFlow { .. }) => continue,
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

    #[test]
    fn experimental_delivery_requires_capability_and_excludes_traders_and_native() {
        use crate::protocol::ClientKind::*;
        for client in [Web,Desktop,Mobile,AutoTrader] {
            assert!(!experimental_delivery_allowed(client,false,true));
            assert!(!experimental_delivery_allowed(client,true,false));
            assert_eq!(experimental_delivery_allowed(client,true,true),matches!(client,Web|Desktop));
        }
        let old: ClientMessage=serde_json::from_str(r#"{"type":"hello","protocolVersion":1,"client":"web"}"#).unwrap();
        assert!(matches!(old,ClientMessage::Hello {experimental_strategies:false,..}));
        let new: ClientMessage=serde_json::from_str(r#"{"type":"hello","protocolVersion":1,"client":"web","experimentalStrategies":true}"#).unwrap();
        assert!(matches!(new,ClientMessage::Hello {experimental_strategies:true,..}));
    }

    #[test]
    fn an_order_flow_subscription_is_cut_down_to_a_few_plausible_tickers() {
        let requested = ["AAA", "BRK.B", "AAA", "aaa", "", "TOOLONGTICKER1", "A B", "CCC", "DDD", "EEE"].map(String::from).to_vec();
        assert_eq!(order_flow_symbols(requested), vec!["AAA", "BRK.B", "CCC", "DDD"]);
        assert!(order_flow_symbols(Vec::new()).is_empty());
        assert_eq!(order_flow_symbols(vec!["A".into(); 10]), vec!["A"]);
    }

    #[test]
    fn a_pushed_order_flow_frame_is_tagged_and_is_not_a_scan_event() {
        // ORDER_FLOW_ENABLED is unset under test, so this is the shape a
        // client sees from a server with the feature off: tagged, no
        // summary, and nothing a ScanEvent consumer would mistake for one.
        let frame = order_flow_frame("AAA", chrono::Utc::now());
        assert_eq!(frame["type"], "order_flow");
        assert_eq!(frame["symbol"], "AAA");
        assert_eq!(frame["enabled"], false);
        assert!(frame["summary"].is_null());
        assert!(frame.get("eventId").is_none());
        assert!(serde_json::from_value::<ScanEvent>(frame).is_err());
    }

    /// One welcomed connection served by `serve_client`, with the handshake
    /// and admission layers left out -- those have their own tests above.
    async fn serve_one(client: crate::protocol::ClientKind) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (events, _) = broadcast::channel::<EventFrame>(16);
        let rx = events.subscribe();
        tokio::spawn(async move {
            // The sender has to outlive the connection: dropping it closes
            // the channel, which ends `serve_client` straight away.
            let _events = events;
            let (stream, peer) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let snapshot = Arc::new(tokio::sync::Mutex::new(EventSnapshot::default()));
            let _ = serve_client(ws, peer, client, rx, snapshot).await;
        });
        tokio_tungstenite::connect_async(format!("ws://{address}")).await.unwrap().0
    }

    #[tokio::test]
    async fn a_subscription_is_answered_once_and_an_empty_list_stops_it() {
        let mut ws = serve_one(crate::protocol::ClientKind::Web).await;
        ws.send(Message::Text(r#"{"type":"subscribe_order_flow","symbols":["AAA","BBB"]}"#.into())).await.unwrap();
        for expected in ["AAA", "BBB"] {
            let frame: serde_json::Value = serde_json::from_str(&ws.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
            assert_eq!((frame["type"].as_str(), frame["symbol"].as_str()), (Some("order_flow"), Some(expected)));
        }
        // ORDER_FLOW_ENABLED is unset here, which is production's own
        // default: the recurring tick must not run, so a subscribed
        // connection gets nothing further rather than a frame per symbol
        // every two seconds saying the feature is off.
        let quiet = tokio::time::timeout(ORDER_FLOW_PUSH_INTERVAL * 2, ws.next()).await;
        assert!(quiet.is_err(), "a disabled server must not stream frames");

        ws.send(Message::Text(r#"{"type":"subscribe_order_flow","symbols":[]}"#.into())).await.unwrap();
        ws.send(Message::Text(r#"{"type":"ping","at":"now"}"#.into())).await.unwrap();
        let pong: serde_json::Value = serde_json::from_str(&ws.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
        assert_eq!(pong["type"], "pong");
        let quiet = tokio::time::timeout(ORDER_FLOW_PUSH_INTERVAL * 2, ws.next()).await;
        assert!(quiet.is_err(), "no order flow after unsubscribing");
    }

    #[tokio::test]
    async fn the_auto_trader_is_never_sent_research_data() {
        let mut ws = serve_one(crate::protocol::ClientKind::AutoTrader).await;
        ws.send(Message::Text(r#"{"type":"subscribe_order_flow","symbols":["AAA"]}"#.into())).await.unwrap();
        ws.send(Message::Text(r#"{"type":"ping","at":"now"}"#.into())).await.unwrap();
        // The pong answers the ping sent after the subscription, so
        // nothing may precede it.
        let first: serde_json::Value = serde_json::from_str(&ws.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
        assert_eq!(first["type"], "pong");
        let quiet = tokio::time::timeout(ORDER_FLOW_PUSH_INTERVAL * 2, ws.next()).await;
        assert!(quiet.is_err());
    }

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
        snapshot.record(EventFrame { event_id:"1:1".into(),event:ScanEvent::IgnitionEvent {
            symbol:"TEST".into(),timestamp:chrono::Utc::now(),price:10.0,
            kind:market_data::IgnitionEventKind::FollowThroughConfirmed } });
        for n in 2..5002 {
            snapshot.record(EventFrame { event_id:format!("1:{n}"),event:ScanEvent::FunnelHealth {
                baseline_unknown_candidates:0, timestamp:chrono::Utc::now(),float_budget_remaining:0,float_budget:240,starved_candidates:0,api_key_missing:false } });
        }
        let frames=snapshot.frames();
        assert_eq!(frames.len(),2);
        assert_eq!(frames[0].event_id,"1:1");
        assert_eq!(frames[1].event_id,"1:5001");
    }
}
