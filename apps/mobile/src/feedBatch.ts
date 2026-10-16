// Batched application of realtime-feed messages (2026-10-16 chart
// diagnosis, root cause #1). In regular hours the socket delivers ~230+
// messages a second, and the feed used to run one setState per message
// -- every one re-rendering the whole App tree. useRealtimeFeed now
// queues data messages and applies them here, once per flush.
//
// Two pure steps, kept apart so the tests can prove the second one alone
// is what defines the state:
//
// 1. coalesceFeedBatch drops messages that a LATER message in the same
//    batch makes irrelevant -- earlier forming previews of the same
//    candle, earlier halt readings and latest-per-symbol updates for the
//    same symbol. Nothing that is a history (alert lists, ignition and
//    micropullback lists, finished bars) is ever dropped.
// 2. applyFeedBatch folds the batch into the previous state with exactly
//    the per-message rules the hook used to apply one setState at a
//    time. Fields the batch does not touch keep their identity, so memos
//    keyed on (say) momentumBySymbol do not recompute on a bars-only
//    batch.
//
// The property the tests pin: applyFeedBatch(s, coalesceFeedBatch(b))
// equals applyFeedBatch(s, b) for every field. Coalescing is purely a
// cost saving; it can never change what the screen shows.
//
// No React or React Native imports: bun:test runs this directly.
import { reconcileBars } from "@stockspotter/shared-types";
import type { BarUpdate, CatalystUpdate, ConsolidationEvent, FunnelSignal, HaltWarning, IgnitionEvent, MomentumUpdate, RealtimeMessage } from "@stockspotter/shared-types";
import type { DetectionEvent } from "./types";

export const MAX_EVENTS = 500;
/** See useRealtimeFeed.ts's header comments for why these two caps are
 * small and why they are separate lists, not views of `events`. */
export const MAX_MICROPULLBACK_EVENTS = 50;
export const MAX_IGNITION_CONFIRMED_EVENTS = 100;
/** ~8.3 hours of 1-minute bars; same cap as apps/client. */
export const MAX_BARS_PER_SYMBOL = 500;
/** Halt readings kept: one per symbol, newest first. */
export const MAX_HALT_SYMBOLS = 200;

export interface FeedData {
  events: DetectionEvent[];
  barsBySymbol: Map<string, BarUpdate[]>;
  subMinuteBarsBySymbol: Map<string, BarUpdate[]>;
  momentumBySymbol: Map<string, MomentumUpdate>;
  catalystsBySymbol: Map<string, CatalystUpdate>;
  funnelBySymbol: Map<string, FunnelSignal>;
  micropullbackEvents: ConsolidationEvent[];
  ignitionConfirmedEvents: IgnitionEvent[];
}

export function emptyFeedData(): FeedData {
  return {
    events: [],
    barsBySymbol: new Map(),
    subMinuteBarsBySymbol: new Map(),
    momentumBySymbol: new Map(),
    catalystsBySymbol: new Map(),
    funnelBySymbol: new Map(),
    micropullbackEvents: [],
    ignitionConfirmedEvents: [],
  };
}

/** Message types the feed handles out of band (immediately, never
 * queued) or never stores. Anything reaching applyFeedBatch with one of
 * these is ignored rather than pushed into `events`. */
const CONTROL_TYPES = new Set<string>(["hello", "welcome", "hello_rejected", "stream_lagged", "ping", "pong", "feed_status", "subscribe_chart"]);

/** Which of the two candle streams a bar belongs to. 30 s bars are their
 * own series (see BarUpdate.intervalSecs); everything else is 1-minute. */
function isSubMinute(bar: BarUpdate): boolean {
  return bar.intervalSecs === 30;
}

/** Key under which a later message makes an earlier one redundant, or
 * null when the message must always be kept. */
function supersedeKey(message: RealtimeMessage): string | null {
  switch (message.type) {
    // Only forming previews are ever dropped, and only by a later bar for
    // the very same candle (symbol + stream + bucket start), forming or
    // final. A forming bar of an OLDER bucket is kept even when a newer
    // bucket follows it: until that minute's final bar arrives, the
    // preview is the only data the chart has for it. A final bar is
    // never dropped; reconcileBars keeps it ahead of any later preview.
    case "bar_update":
      return `bar|${message.symbol}|${isSubMinute(message) ? 30 : 60}|${Date.parse(message.timestamp)}`;
    // The events reducer keeps exactly one halt reading per symbol, the
    // newest; earlier ones in the batch would be replaced anyway.
    case "halt_warning":
      return `halt|${message.symbol}`;
    // Latest-per-symbol maps: Map.set in order leaves only the last.
    case "momentum_update":
      return `momentum|${message.symbol}`;
    case "funnel_signal":
      return `funnel|${message.symbol}`;
    case "catalyst_update":
      return `catalyst|${message.symbol}`;
    default:
      return null;
  }
}

/**
 * Drop messages a later message in the same batch supersedes; keep the
 * rest in arrival order. See this file's header for the guarantee.
 */
export function coalesceFeedBatch(messages: readonly RealtimeMessage[]): RealtimeMessage[] {
  const seen = new Set<string>();
  const keep: boolean[] = new Array(messages.length);
  for (let i = messages.length - 1; i >= 0; i--) {
    const message = messages[i];
    const key = supersedeKey(message);
    if (key === null) { keep[i] = true; continue; }
    const superseded = seen.has(key);
    seen.add(key);
    // A final bar survives even when superseded: a later final for the
    // same candle would replace it identically, but a later forming one
    // would not, and keeping finals unconditionally is the simple rule.
    keep[i] = !superseded || (message.type === "bar_update" && message.isFinal === true);
  }
  return messages.filter((_, i) => keep[i]);
}

/**
 * Fold one batch into the previous state, applying each message with the
 * same rule the per-message setState calls used. Returns `prev` itself
 * when nothing changed.
 */
export function applyFeedBatch(prev: FeedData, batch: readonly RealtimeMessage[]): FeedData {
  if (batch.length === 0) return prev;
  let bars: Map<string, BarUpdate[]> | null = null;
  let subBars: Map<string, BarUpdate[]> | null = null;
  let momentum: Map<string, MomentumUpdate> | null = null;
  let funnel: Map<string, FunnelSignal> | null = null;
  let catalysts: Map<string, CatalystUpdate> | null = null;
  let micropullback: ConsolidationEvent[] | null = null;
  let ignitions: IgnitionEvent[] | null = null;
  // `events` is two lists glued together: non-halt alerts (newest first,
  // capped) followed by halt readings (one per symbol, newest first).
  // Split once per batch instead of once per message.
  let events: { alerts: DetectionEvent[]; halts: HaltWarning[] } | null = null;

  for (const message of batch) {
    if (CONTROL_TYPES.has(message.type)) continue;
    if (message.type === "bar_update") {
      const sub = isSubMinute(message);
      const target = sub ? (subBars ??= new Map(prev.subMinuteBarsBySymbol)) : (bars ??= new Map(prev.barsBySymbol));
      const existing = target.get(message.symbol) ?? [];
      const next = reconcileBars(existing, message, MAX_BARS_PER_SYMBOL);
      if (next !== existing) target.set(message.symbol, next);
      continue;
    }
    if (message.type === "momentum_update") { (momentum ??= new Map(prev.momentumBySymbol)).set(message.symbol, message); continue; }
    if (message.type === "funnel_signal") { (funnel ??= new Map(prev.funnelBySymbol)).set(message.symbol, message); continue; }
    if (message.type === "catalyst_update") { (catalysts ??= new Map(prev.catalystsBySymbol)).set(message.symbol, message); continue; }
    // Additional dedicated consumers; both still fall through into the
    // shared `events` list below, exactly as before.
    if (message.type === "ignition_event" && message.kind === "follow_through_confirmed") {
      ignitions = [message, ...(ignitions ?? prev.ignitionConfirmedEvents)].slice(0, MAX_IGNITION_CONFIRMED_EVENTS);
    }
    if (message.type === "consolidation_event" && message.kind === "entry_triggered" && message.strategy === "micropullback") {
      micropullback = [message, ...(micropullback ?? prev.micropullbackEvents)].slice(0, MAX_MICROPULLBACK_EVENTS);
    }
    events ??= {
      alerts: prev.events.filter((e) => e.type !== "halt_warning"),
      halts: prev.events.filter((e): e is HaltWarning => e.type === "halt_warning"),
    };
    const event = message as DetectionEvent;
    if (event.type === "halt_warning") {
      events.halts = [event, ...events.halts.filter((h) => h.symbol !== event.symbol).slice(0, MAX_HALT_SYMBOLS - 1)];
    } else {
      events.alerts = [event, ...events.alerts].slice(0, MAX_EVENTS);
    }
  }

  const changed = bars || subBars || momentum || funnel || catalysts || micropullback || ignitions || events;
  if (!changed) return prev;
  return {
    events: events ? [...events.alerts, ...events.halts] : prev.events,
    barsBySymbol: bars ?? prev.barsBySymbol,
    subMinuteBarsBySymbol: subBars ?? prev.subMinuteBarsBySymbol,
    momentumBySymbol: momentum ?? prev.momentumBySymbol,
    catalystsBySymbol: catalysts ?? prev.catalystsBySymbol,
    funnelBySymbol: funnel ?? prev.funnelBySymbol,
    micropullbackEvents: micropullback ?? prev.micropullbackEvents,
    ignitionConfirmedEvents: ignitions ?? prev.ignitionConfirmedEvents,
  };
}

/** Does this message carry a market timestamp the connection status
 * should track? Same exclusions as before: funnel_health and
 * catalyst_update stamp observation time, not market time. */
export function marketTimestampMs(message: RealtimeMessage): number | null {
  if (!("timestamp" in message) || message.type === "funnel_health" || message.type === "catalyst_update") return null;
  const at = Date.parse(message.timestamp);
  return Number.isFinite(at) ? at : null;
}
