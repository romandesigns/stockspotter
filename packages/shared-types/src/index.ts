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

export type ConsolidationEventKind =
  | "surge_detected"
  | "consolidation_confirmed"
  | "entry_triggered";

/**
 * Which of the two parallel ConsolidationBreakoutMonitor configs produced
 * a given ConsolidationEvent — a real, user-facing distinction, not an
 * internal-only tag. "micropullback" (added 2026-09-03, the live YQ/UFG/
 * PPBT finding) is the SAME surge -> consolidation -> breakout pattern,
 * tuned to catch a genuine single-candle micropullback the original
 * 2-candle-minimum config structurally can't — see live.rs's own doc
 * comment on why. It fires faster and on thinner evidence, so clients
 * must label it distinctly rather than rendering it identically to the
 * slower, already-validated "consolidation_breakout" signal.
 */
export type ConsolidationStrategy = "consolidation_breakout" | "micropullback";

/**
 * Post-Ignition Consolidation Breakout — not its own panel per the doc's
 * Panels list, shown as an extra condition/tag inside the Ignition panel
 * (same treatment as the flat-base gate).
 */
export interface ConsolidationEvent {
  type: "consolidation_event";
  symbol: string;
  timestamp: string; // ISO 8601
  price: number;
  kind: ConsolidationEventKind;
  strategy: ConsolidationStrategy;
}

export type HaltAlertLevel = "calm" | "amber" | "red";

/**
 * Halt Early-Warning panel: a live proximity-to-halt reading for one
 * symbol. Sent on every trade for a tracked symbol (not edge-triggered
 * like the others) — a proximity gauge needs the current value
 * continuously, not just transitions.
 */
export interface HaltWarning {
  type: "halt_warning";
  symbol: string;
  timestamp: string; // ISO 8601
  referencePrice: number;
  currentPrice: number;
  bandWidthDollars: number;
  bandDoubled: boolean;
  proximityRatio: number; // 0..1+, >=1 means price is at/past the halt band
  relativeVolume: number | null;
  level: HaltAlertLevel;
}

/**
 * Super Chart panel: one raw OHLCV bar for a tracked symbol, straight from
 * Alpaca with no funnel/scoring transformation applied -- FunnelSignal's
 * price/gapPct are derived values for the scanner panels, not what a
 * candlestick chart needs. Sent on every bar for every tracked symbol
 * (not edge-triggered), alongside FunnelSignal.
 *
 * `intervalSecs` (2026-09-03, real sub-minute multi-view support): `60`
 * for the existing 1-minute stream, `30` for the new live-only sub-minute
 * stream (no historical backfill below 1 minute -- confirmed live against
 * Alpaca's own API, `timeframe=30Sec` is rejected outright). **Every
 * consumer of this event must filter/route on `intervalSecs` before
 * merging into its own bars array** -- a 1-minute and a 30-second bar for
 * the same symbol are otherwise structurally indistinguishable, and
 * interleaving them into one array corrupts whichever timeframe is
 * currently displayed.
 */
export interface BarUpdate {
  type: "bar_update";
  symbol: string;
  timestamp: string; // ISO 8601
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number;
  intervalSecs: number;
}

/**
 * Catalysts panel: news catalyst tags for a symbol, from the Python
 * qualitative layer. Fired once per symbol at promotion time, not
 * per-trade/per-bar like the others — catalysts don't change tick-by-tick.
 */
export interface CatalystUpdate {
  type: "catalyst_update";
  symbol: string;
  timestamp: string; // ISO 8601
  catalystTags: string[];
  headlineCount: number;
  mostRecentHeadline: string | null;
}

export type RealtimeMessage =
  | ClientHello
  | ServerWelcome
  | ServerHelloRejected
  | Ping
  | Pong
  | FunnelSignal
  | MomentumUpdate
  | IgnitionEvent
  | ConsolidationEvent
  | HaltWarning
  | BarUpdate
  | CatalystUpdate;
