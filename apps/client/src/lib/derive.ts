// Turns the raw, newest-first event stream from useRealtimeFeed into
// what each panel actually wants to render. Kept separate from the
// panels themselves so the "what counts as a signal" logic is testable
// in one place, same reasoning as the Rust side's
// backtest_metrics::signals module.

import type {
  ConsolidationEvent,
  FunnelSignal,
  HaltWarning,
  IgnitionEvent,
  MomentumUpdate,
} from "@stockspotter/shared-types";
import type { DetectionEvent } from "./useRealtimeFeed";

export function filterFunnelSignals(events: DetectionEvent[]): FunnelSignal[] {
  return events.filter((e): e is FunnelSignal => e.type === "funnel_signal");
}

/**
 * The wire feed sends a MomentumUpdate on every bar for every tracked
 * symbol (not edge-triggered server-side) — a "Confirmed Bullish
 * Momentum" panel showing every one of those would just be noise (one
 * new row per symbol per minute regardless of whether anything
 * happened). Mirrors backtest_metrics::signals' edge-triggering exactly:
 * only a qualifies=false -> true transition counts as a new row.
 * `events` is newest-first, so this walks it in chronological order to
 * detect transitions correctly, then re-reverses for display.
 */
export function deriveConfirmedMomentum(events: DetectionEvent[]): MomentumUpdate[] {
  const updates = events.filter((e): e is MomentumUpdate => e.type === "momentum_update");
  const chronological = [...updates].reverse();
  const wasQualified = new Map<string, boolean>();
  const confirmations: MomentumUpdate[] = [];
  for (const u of chronological) {
    const prev = wasQualified.get(u.symbol) ?? false;
    if (u.qualifies && !prev) confirmations.push(u);
    wasQualified.set(u.symbol, u.qualifies);
  }
  return confirmations.reverse();
}

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
