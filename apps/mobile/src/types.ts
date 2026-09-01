import type { RealtimeMessage } from "@stockspotter/shared-types";
export type AppTab = "radar" | "alerts" | "markets" | "watchlist";
export type FeedStatus = "connecting" | "open" | "closed";
export type DetectionEvent = Exclude<RealtimeMessage, { type: "hello" | "welcome" | "hello_rejected" | "ping" | "pong" }>;
export interface Mover { symbol: string; price: number; changePct: number; volume: number; }
export interface MarketReading { symbol: string; name: string; price: number; changePct: number; }
export interface FocusRow { symbol: string; price: number; changePct: number; timestamp: string; detail: string; strong: boolean; }
/** Same shape as apps/client/src/lib/derive.ts's CandleBar -- unix
 * seconds, raw OHLCV, matching ws-server's own BarOut wire shape
 * (both /bars/:symbol and /replay/bars/:symbol return this directly). */
export interface CandleBar { time: number; open: number; high: number; low: number; close: number; volume: number; }
