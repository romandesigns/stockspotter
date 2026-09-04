import type { RealtimeMessage } from "@stockspotter/shared-types";
export type AppTab = "radar" | "alerts" | "markets" | "watchlist" | "autotrader";
export type FeedStatus = "connecting" | "open" | "closed";
export type DetectionEvent = Exclude<RealtimeMessage, { type: "hello" | "welcome" | "hello_rejected" | "ping" | "pong" }>;
export type TradingSession = "premarket" | "regular" | "after_hours" | "overnight";
/** `session` is which trading session produced this reading -- ws-server
 * keeps a rolling 24h "best observed" value per symbol (market_data::
 * movers), not just the current live snapshot, so an earlier session's
 * real mover stays on the list after it's no longer live-leading. `null`
 * for the historical date-lookup path (/movers/gainers?date=), which
 * only has daily-bar resolution and genuinely can't classify a session --
 * render nothing rather than a fabricated label. */
export interface Mover { symbol: string; price: number; changePct: number; volume: number; session: TradingSession | null; }
export interface MarketReading { symbol: string; name: string; price: number; changePct: number; }
export interface FocusRow { symbol: string; price: number; changePct: number; timestamp: string; detail: string; strong: boolean; }
/** One row per *saved* symbol, regardless of whether it currently has a
 * live Focus signal -- price/changePct/timestamp are nullable because a
 * saved symbol genuinely might have none of that right now (e.g. saved
 * from Top Gainers hours ago, no longer trading actively). */
export interface WatchlistRow { symbol: string; price: number | null; changePct: number | null; timestamp: string | null; detail: string; strong: boolean; }
/** Same shape as apps/client/src/lib/derive.ts's CandleBar -- unix
 * seconds, raw OHLCV, matching ws-server's own BarOut wire shape
 * (both /bars/:symbol and /replay/bars/:symbol return this directly). */
export interface CandleBar { time: number; open: number; high: number; low: number; close: number; volume: number; }

/** Auto-trader monitoring (2026-09-04) -- mirrors
 * apps/client/src/lib/useAutoTrader.ts's own TS shape exactly, both hand-
 * typed against ws-server's /auto-trader/status the same way every other
 * REST response in this codebase is (no shared-types entry -- that file
 * is WS-protocol-only, see its own header comment). */
// "momentum_deteriorated"/"halt_risk_too_high" (2026-09-04) -- the
// engine now exits early on real momentum breakdown (overall < 0.4, the
// same "critical" tier MomentumScoreRow.tsx already uses) and skips an
// entry when the symbol's latest known halt-proximity level is Amber or
// Red, real added risk a plain momentum reading doesn't capture.
export type ExitReason = "target_hit" | "stop_hit" | "timeout" | "momentum_deteriorated";
export type SkipReason =
  | "momentum_gate_failed"
  | "outside_regular_hours"
  | "max_concurrent_positions"
  | "already_entered_today"
  | "zero_quantity"
  | "halt_risk_too_high";
export interface OpenPosition { symbol: string; entryPrice: number; qty: number; enteredAt: string; targetPrice: number; stopPrice: number; }
export type JournalEntry =
  | { type: "entered"; symbol: string; entryPrice: number; qty: number; positionSizeUsd: number; targetPrice: number; stopPrice: number; enteredAt: string; momentumOverall: number; momentumVolumeConfirmation: number; catalystTags: string[] }
  | { type: "exited"; symbol: string; exitPrice: number; exitReason: ExitReason; pnlUsd: number; pnlPct: number; qty: number; enteredAt: string; exitedAt: string }
  | { type: "skipped"; symbol: string; reason: SkipReason; at: string; detail: string }
  // The trailing stop ratcheting up -- a real, visible "something
  // happened" line, not a win or a loss. Only emitted on an actual
  // increase, not every bar.
  | { type: "stop_adjusted"; symbol: string; previousStopPrice: number; newStopPrice: number; triggerPrice: number; at: string };
export interface AutoTraderStatus { trades: number; wins: number; losses: number; cumulativePnlUsd: number; openPositions: OpenPosition[]; recentEntries: JournalEntry[]; }
