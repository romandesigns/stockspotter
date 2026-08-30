// Shared WebSocket protocol contract between the Rust realtime backend
// (crates/ws-server) and every client (web/desktop via apps/client,
// mobile via apps/mobile). Every client picks up these types from one
// place instead of each hand-rolling its own copy of the wire format —
// that's also the actual mechanism behind "every platform sees the exact
// same notifications": crates/ws-server broadcasts one
// `market_data::ScanEvent` per detection, serialized once per connection
// but from the identical Rust struct/serde impl every time, to every
// connected client via the same `tokio::sync::broadcast` channel. There
// is no per-client filtering or variation anywhere on the server.

export const WS_PROTOCOL_VERSION = 1 as const;

/** First message a client sends right after the socket opens. */
export interface ClientHello {
  type: "hello";
  protocolVersion: typeof WS_PROTOCOL_VERSION;
  client: "web" | "desktop" | "mobile";
}

/** Server's reply to a valid ClientHello. */
export interface ServerWelcome {
  type: "welcome";
  protocolVersion: typeof WS_PROTOCOL_VERSION;
  serverTime: string; // ISO 8601
}

/** Sent by the server if a ClientHello's protocolVersion is unsupported. */
export interface ServerHelloRejected {
  type: "hello_rejected";
  reason: string;
}

/** Bidirectional keepalive so clients/server can detect a dead connection. */
export interface Ping {
  type: "ping";
  at: string; // ISO 8601
}

export interface Pong {
  type: "pong";
  at: string; // ISO 8601, echoes the Ping's `at`
}

// ---------------------------------------------------------------------
// Detection events — one per fast_funnel/momentum_scorer/ignition_detector
// signal, broadcast unmodified to every connected client. Mirrors
// crates/market-data/src/events.rs's ScanEvent exactly (field names,
// tag values) — that Rust enum has its own unit tests asserting this
// exact JSON shape, so treat that file as the source of truth if these
// two ever seem to disagree.

/** Ross Cameron panel: a Stage 1/2 fast-funnel verdict for one symbol. */
export interface FunnelSignal {
  type: "funnel_signal";
  symbol: string;
  timestamp: string; // ISO 8601
  price: number;
  gapPct: number;
  sessionVolume: number;
  priceOk: boolean;
  floatOk: boolean;
  relVolOk: boolean;
  gapOk: boolean;
  /** true only when all four of the above are true. */
  passed: boolean;
}

/** Bullish Momentum panel: a per-bar momentum score update for one symbol. */
export interface MomentumUpdate {
  type: "momentum_update";
  symbol: string;
  timestamp: string; // ISO 8601
  volumeConfirmation: number; // 0..1
  structure: number; // 0..1
  maSlope: number; // 0..1
  wickRejection: number; // 0..1
  overall: number; // weighted sum of the four above
  qualifies: boolean; // overall >= momentum_scorer::DEFAULT_QUALIFY_THRESHOLD
}

export type IgnitionEventKind =
  | "candidate_opened"
  | "follow_through_confirmed"
  | "follow_through_rejected";

/** Ignition panel: a tick-level ignition signal opening or resolving. */
export interface IgnitionEvent {
  type: "ignition_event";
  symbol: string;
  timestamp: string; // ISO 8601
  price: number;
  kind: IgnitionEventKind;
}

export type RealtimeMessage =
  | ClientHello
  | ServerWelcome
  | ServerHelloRejected
  | Ping
  | Pong
  | FunnelSignal
  | MomentumUpdate
  | IgnitionEvent;
