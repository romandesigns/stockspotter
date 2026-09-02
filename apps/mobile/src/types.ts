import type { RealtimeMessage } from "@stockspotter/shared-types";
export type AppTab = "radar" | "alerts" | "markets" | "watchlist";
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
