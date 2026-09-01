import type { RealtimeMessage } from "@stockspotter/shared-types";
export type AppTab = "radar" | "alerts" | "markets" | "watchlist";
export type FeedStatus = "connecting" | "open" | "closed";
export type DetectionEvent = Exclude<RealtimeMessage, { type: "hello" | "welcome" | "hello_rejected" | "ping" | "pong" }>;
export interface Mover { symbol: string; price: number; changePct: number; volume: number; }
export interface MarketReading { symbol: string; name: string; price: number; changePct: number; }
export interface FocusRow { symbol: string; price: number; changePct: number; timestamp: string; detail: string; strong: boolean; }
