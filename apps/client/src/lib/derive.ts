// Turns the raw, newest-first event stream from useRealtimeFeed into
// what each panel actually wants to render. Kept separate from the
// panels themselves so the "what counts as a signal" logic is testable
// in one place, same reasoning as the Rust side's
// backtest_metrics::signals module.

import type {
  BarUpdate,
  CatalystUpdate,
  ConsolidationEvent,
  HaltWarning,
  IgnitionEvent,
  MomentumUpdate,
} from "@stockspotter/shared-types";
import type { DetectionEvent } from "./useRealtimeFeed";
import { FACTOR_GOOD_THRESHOLD } from "./momentumLabel";

/** lightweight-charts' `Time` type for intraday data is a plain Unix
 * timestamp in *seconds* (its `UTCTimestamp` — deliberately not importing
 * the library type here so this file, and its tests, stay independent of
 * the chart library itself; SuperChart.tsx does the one-line cast at its
 * boundary instead). */
export interface CandleBar {
  time: number;
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number;
}

/**
 * `barsBySymbol` (from useRealtimeFeed) stores raw wire `BarUpdate`s in
 * arrival order already deduped/capped per symbol — this just reshapes
 * one symbol's list into what the chart wants: numeric seconds instead of
 * an ISO string, and guards against a non-increasing timestamp (a
 * reconnect replaying an already-seen bar, or two bars landing with the
 * same wall-clock second) since lightweight-charts requires strictly
 * ascending time per series and throws if that's violated.
 */
export function toChartBars(bars: BarUpdate[]): CandleBar[] {
  const result: CandleBar[] = [];
  for (const b of bars) {
    const time = Math.floor(new Date(b.timestamp).getTime() / 1000);
    const prev = result[result.length - 1];
    if (prev && time <= prev.time) continue;
    result.push({ time, open: b.open, high: b.high, low: b.low, close: b.close, volume: b.volume });
  }
  return result;
}

/** Symbols currently being tracked, for a chart symbol-picker — anything
 * with at least one bar counts as "has chart data", sorted alphabetically
 * (no "most active" ranking yet, deliberately simple until there's a real
 * UI to hang that on). */
export function listChartableSymbols(barsBySymbol: Map<string, BarUpdate[]>): string[] {
  return [...barsBySymbol.keys()].sort();
}

/**
 * Merges a REST-fetched historical backfill with the live-accumulated
 * bars for the same symbol — without this, a freshly-selected symbol
 * only shows whatever's arrived over the live feed since ws-server
 * started tracking it this session (often just a handful of bars),
 * nothing like the Artifact prototype's own pre-fetched full-session
 * demo data. `live` wins on a timestamp collision (the live tick is more
 * current than a REST snapshot fetched moments earlier), and the result
 * is sorted ascending — required by lightweight-charts, and not
 * guaranteed here since `historical` and `live` arrive from two
 * independent sources on their own schedules.
 */
export function mergeBars(historical: CandleBar[], live: CandleBar[]): CandleBar[] {
  const byTime = new Map<number, CandleBar>();
  for (const b of historical) byTime.set(b.time, b);
  for (const b of live) byTime.set(b.time, b);
  return [...byTime.values()].sort((a, b) => a.time - b.time);
}

// filterFunnelSignals/deriveConfirmedMomentum used to live here, deriving
// Gap & Go's feed and Bullish Momentum's qualify-edge feed from the
// generic `events` list on every render. Removed 2026-09-03: both signal
// types are rare enough (relative to halt_warning's per-trade volume)
// that they'd get evicted from the shared MAX_EVENTS ring buffer within
// seconds of real trading activity -- confirmed live (Roman: "It's not
// displaying any stocks... when it does, they just last for a few
// seconds"). Same root cause bar_update/catalyst_update were already
// fixed for. Real fix moved into useRealtimeFeed.ts itself:
// funnelSignals is now its own dedicated, capped list (populated as
// funnel_signal messages arrive, immune to unrelated event volume) and
// momentumConfirmations is edge-detected incrementally as
// momentum_update messages arrive (via momentumQualifiedRef), not
// re-derived from history on every render. Both are returned directly
// from the hook now; App.tsx no longer computes them here.

export type IgnitionFeedItem =
  | { source: "ignition"; event: IgnitionEvent }
  | { source: "consolidation"; event: ConsolidationEvent };

/**
 * Ignition panel (doc Panels #3) folds in the consolidation-breakout
 * entry strategy as an extra tag rather than a separate panel, per the
 * doc's own Panels list (only 4 panels total, consolidation-breakout
 * isn't one of them) — same treatment the flat-base gate already gets
 * (an extra condition inside this panel, not a new one).
 */
export function deriveIgnitionFeed(events: DetectionEvent[]): IgnitionFeedItem[] {
  return events
    .filter((e): e is IgnitionEvent | ConsolidationEvent => e.type === "ignition_event" || e.type === "consolidation_event")
    .map((e) => (e.type === "ignition_event" ? { source: "ignition" as const, event: e } : { source: "consolidation" as const, event: e }));
}

// Super Chart's momentum panel needs the *latest* reading for whichever
// one symbol is currently selected. Originally implemented here as a
// derive-from-`events` scan (same pattern as everything else in this
// file) -- but momentum_update fires once per bar, the same low
// frequency as bar_update, and confirmed live that halt_warning's
// per-trade frequency (2000+/min vs. ~14/min) floods it out of the
// shared, capped `events` list within seconds. Same fix shape as
// bar_update: useRealtimeFeed now maintains its own `momentumBySymbol`
// map (latest-only, not history) fed directly off the wire, so
// ChartPanel reads that map directly instead of a derive function here.

/** Halt panel shows one live-updating card per symbol, not a scrolling
 * feed (per the doc's UI concept) — `events` is newest-first, so the
 * first HaltWarning seen per symbol is already its latest reading. */
export function deriveLatestHaltBySymbol(events: DetectionEvent[]): HaltWarning[] {
  const seen = new Set<string>();
  const latest: HaltWarning[] = [];
  for (const e of events) {
    if (e.type !== "halt_warning") continue;
    if (seen.has(e.symbol)) continue;
    seen.add(e.symbol);
    latest.push(e);
  }
  return latest.sort((a, b) => b.proximityRatio - a.proximityRatio);
}

/** Catalysts panel's row order -- most recently looked-up symbol first,
 * out of useRealtimeFeed's own latest-per-symbol catalystsBySymbol map
 * (see that map's doc comment on why catalysts need their own map rather
 * than the generic capped `events` list). */
export function catalystRows(catalystsBySymbol: Map<string, CatalystUpdate>): CatalystUpdate[] {
  return Array.from(catalystsBySymbol.values()).sort(
    (a, b) => new Date(b.timestamp).getTime() - new Date(a.timestamp).getTime(),
  );
}

export type CatalystConfirmation = "confirmed" | "unconfirmed" | "pending";

/**
 * Real-time catalyst-confirmation read -- per Roman's ask ("quantify...
 * which of them is actually fueling bullish momentum" rather than just
 * displaying the tag). A catalyst only counts as "confirmed" when the
 * SAME real momentum_scorer reading this app already computes
 * everywhere else (SuperChart's own momentum panel, buildFocusRows'
 * `qualifies` gate) says both the overall score clears the qualify bar
 * AND volume specifically backs it -- a catalyst with no real buying
 * pressure behind it is exactly the "narrative without confirmation"
 * case this app's whole gap-and-go philosophy already treats
 * skeptically everywhere else (momentum_scorer weights volume heaviest
 * of its 4 factors for the same reason). Deliberately NOT a new
 * threshold: FACTOR_GOOD_THRESHOLD (0.6) is the real qualify-gate value
 * already reused per-factor throughout momentumLabel.ts, not a second
 * number invented for this.
 *
 * "pending" (no momentum reading yet, e.g. a symbol just promoted) is
 * kept distinct from "unconfirmed" (a real reading that doesn't clear
 * the bar) -- collapsing the two would read as "this catalyst failed"
 * for a symbol that's simply too new to judge yet.
 */
export function catalystConfirmation(momentum: MomentumUpdate | undefined): CatalystConfirmation {
  if (!momentum) return "pending";
  return momentum.overall >= FACTOR_GOOD_THRESHOLD && momentum.volumeConfirmation >= FACTOR_GOOD_THRESHOLD
    ? "confirmed"
    : "unconfirmed";
}
