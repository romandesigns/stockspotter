// Shared WebSocket protocol contract between the Rust realtime backend and
// every client (web/desktop via apps/client, mobile via apps/mobile).
// This is intentionally just the connection/handshake layer for now — the
// panel-specific message kinds (Ross Cameron / Momentum / Ignition, see
// trading-scanner-architecture.md) get added here once those features land,
// so every client picks up the new types from one place instead of each
// client hand-rolling its own copy of the wire format.

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

export type RealtimeMessage =
  | ClientHello
  | ServerWelcome
  | ServerHelloRejected
  | Ping
  | Pong;
