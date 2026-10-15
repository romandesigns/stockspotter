// "What changed" feed for the Shortlist view: the handful of list changes
// worth a glance between two /shortlist polls -- a name entering the top
// 3, price crossing its premarket high, the halt band tightening, a name
// dropping off. Pure, so the rules are unit tested (shortlistChanges.test.ts).
//
// Display only: no sound, no push, no OS notification. These are changes
// in where to look, not entry signals.

import type { ShortlistCandidate, ShortlistHalt, ShortlistResponse } from "@stockspotter/shared-types";

/** How many top-ranked names count as "the top" for an `entered` change. */
export const TOP_N = 3;
/** Most changes kept per market day (newest first). */
export const CHANGE_HISTORY_CAP = 50;

interface ChangeBase {
  symbol: string;
  /** The poll that showed the change -- `next.generatedAt`. */
  at: string;
}

export type ShortlistChange =
  | (ChangeBase & { kind: "entered"; rank: number })
  | (ChangeBase & { kind: "crossedPremarketHigh"; premarketHigh: number; price: number })
  | (ChangeBase & { kind: "haltEscalated"; from: ShortlistHalt["level"]; to: ShortlistHalt["level"] })
  | (ChangeBase & { kind: "left" });

const HALT_SEVERITY: Record<ShortlistHalt["level"], number> = { calm: 0, amber: 1, red: 2 };

function topSymbols(body: ShortlistResponse): Set<string> {
  return new Set([...body.candidates].sort((a, b) => a.rank - b.rank).slice(0, TOP_N).map((c) => c.symbol));
}

/** Changes from `prev` to `next`, in a fixed order per poll: entered,
 * crossed premarket high, halt escalated, left. A null `prev` (the first
 * poll of the session) yields nothing -- there is nothing to compare. */
export function diffShortlist(prev: ShortlistResponse | null, next: ShortlistResponse): ShortlistChange[] {
  if (!prev) return [];
  const at = next.generatedAt;
  const before = new Map<string, ShortlistCandidate>(prev.candidates.map((c) => [c.symbol, c]));
  const nextSymbols = new Set(next.candidates.map((c) => c.symbol));
  const prevTop = topSymbols(prev);
  const nextTop = topSymbols(next);
  const ordered = [...next.candidates].sort((a, b) => a.rank - b.rank);

  const entered: ShortlistChange[] = [];
  const crossed: ShortlistChange[] = [];
  const escalated: ShortlistChange[] = [];
  for (const c of ordered) {
    if (nextTop.has(c.symbol) && !prevTop.has(c.symbol)) entered.push({ kind: "entered", symbol: c.symbol, at, rank: c.rank });

    const p = before.get(c.symbol);
    if (!p) continue;
    const level = c.levels.premarketHigh;
    if (level !== undefined && p.levels.premarketHigh !== undefined && Number.isFinite(level) && p.price <= level && level < c.price) {
      crossed.push({ kind: "crossedPremarketHigh", symbol: c.symbol, at, premarketHigh: level, price: c.price });
    }
    // Only while LULD is in effect: outside 9:30-16:00 no halt can trigger,
    // so a band reading then is not worth a line.
    if (p.halt && c.halt && c.halt.luldInEffect && HALT_SEVERITY[c.halt.level] > HALT_SEVERITY[p.halt.level]) {
      escalated.push({ kind: "haltEscalated", symbol: c.symbol, at, from: p.halt.level, to: c.halt.level });
    }
  }

  const left: ShortlistChange[] = prev.candidates
    .filter((c) => !nextSymbols.has(c.symbol))
    .sort((a, b) => a.rank - b.rank)
    .map((c) => ({ kind: "left", symbol: c.symbol, at }));

  return [...entered, ...crossed, ...escalated, ...left];
}

export interface ChangeHistory {
  marketDay: string | null;
  /** Newest first, at most CHANGE_HISTORY_CAP. */
  changes: ShortlistChange[];
}

export const EMPTY_HISTORY: ChangeHistory = { marketDay: null, changes: [] };

/** Folds one poll into the history. A new market day starts a fresh
 * history and is never diffed against yesterday's list. */
export function appendChanges(history: ChangeHistory, prev: ShortlistResponse | null, next: ShortlistResponse, cap = CHANGE_HISTORY_CAP): ChangeHistory {
  const sameDay = prev !== null && history.marketDay === next.marketDay && prev.marketDay === next.marketDay;
  const base = sameDay ? history.changes : [];
  const fresh = diffShortlist(sameDay ? prev : null, next);
  if (sameDay && fresh.length === 0) return history;
  return { marketDay: next.marketDay, changes: [...fresh, ...base].slice(0, cap) };
}

/** One short line per change, for the feed. */
export function describeChange(change: ShortlistChange): string {
  switch (change.kind) {
    case "entered":
      return `entered the top ${TOP_N} at #${change.rank}`;
    case "crossedPremarketHigh":
      return `crossed its premarket high ${change.premarketHigh.toFixed(change.premarketHigh < 1 ? 4 : 2)}`;
    case "haltEscalated":
      return `halt band ${change.from} → ${change.to}`;
    case "left":
      return "left the list";
  }
}
