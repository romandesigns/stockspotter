// Fixture data whose TIMESTAMPS carry their own symbol identity: each
// symbol owns a disjoint block of the time axis, so any bar array that
// reached the chart engine can be attributed back to the symbol it was
// fetched/received for -- without the engine ever being told a symbol.
// That is what makes "no old symbol data after A->B" (F5) an assertion
// about the actual engine input rather than about source text.
//
// mergeBars() keys by time, so disjoint blocks also mean a stale
// symbol's history can never be silently absorbed by the new symbol's
// live bars: if it leaks, it survives the merge and is visible.

import type { BarUpdate } from "@stockspotter/shared-types";

/** Every fixture timestamp has to sit on the UTC minute AND 5-minute
 * grid: the runner asserts 5m bucket alignment and half-minute
 * boundaries on what actually reached the engine, and a base that is on
 * neither grid makes those assertions read the wrong thing. Derived by
 * flooring a real epoch second onto the grid rather than typed by hand. */
const ALIGNMENT = 300;
const ANCHOR = Math.floor(1_700_000_000 / ALIGNMENT) * ALIGNMENT;

/** Whole multiple of ALIGNMENT (so every block base stays on the grid)
 * and far wider than any offset below, so blocks never overlap. */
export const SYMBOL_BLOCK_SIZE = 1_000 * ALIGNMENT;

/** One disjoint block each, in order. DDD/EEE/FFF exist so a four-slot
 * multi-view test can fill every slot and still push a fifth, entirely
 * unrelated symbol. */
const SYMBOL_ORDER = ["AAA", "BBB", "CCC", "DDD", "EEE", "FFF"];

export const SYMBOL_BLOCKS: Record<string, number> = Object.fromEntries(
  SYMBOL_ORDER.map((symbol, i) => [symbol, ANCHOR + i * SYMBOL_BLOCK_SIZE]),
);

/** Historical (REST backfill) bars sit at the bottom of the block. */
const HISTORY_OFFSET = 0;
/** Live 1-minute bars start at +61 minutes -- deliberately NOT on the
 * 5-minute grid, so a 5m-resampled payload is distinguishable from a
 * raw 1m one by its bucket alignment alone. */
const LIVE_MINUTE_OFFSET = 3660;
/** Live 30s bars start on a half-minute boundary. Note the series then
 * ALTERNATES `time % 60` between 30 and 0, as any real 30s series does
 * -- what identifies it is the starting boundary plus the 30s step, not
 * a per-bar `% 60 === 30`. */
const LIVE_SUBMINUTE_OFFSET = 7230;

export interface FixtureCandle {
  time: number;
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number;
}

export function symbolOfTime(time: number): string {
  for (const [symbol, base] of Object.entries(SYMBOL_BLOCKS)) {
    if (time >= base && time < base + SYMBOL_BLOCK_SIZE) return symbol;
  }
  return `unattributed:${time}`;
}

export function symbolsOfBars(bars: { time: number }[]): string[] {
  return [...new Set(bars.map((b) => symbolOfTime(b.time)))].sort();
}

function isoOf(timeSec: number): string {
  return new Date(timeSec * 1000).toISOString();
}

export function liveBarTime(symbol: string, intervalSecs: 30 | 60, index: number): number {
  const base = SYMBOL_BLOCKS[symbol];
  if (base === undefined) throw new Error(`unknown fixture symbol ${symbol}`);
  return intervalSecs === 30 ? base + LIVE_SUBMINUTE_OFFSET + index * 30 : base + LIVE_MINUTE_OFFSET + index * 60;
}

/**
 * One wire-shaped BarUpdate. `close` can be overridden to replay the
 * SAME bucket with a corrected value -- the real per-tick case
 * reconcileBars exists for (a still-forming bucket updated in place, or
 * a provider correction), which must still reach the engine after the
 * F8 memoization change.
 */
export function makeLiveBar(
  symbol: string,
  intervalSecs: 30 | 60,
  index: number,
  overrides: { close?: number; isFinal?: boolean } = {},
): BarUpdate {
  const time = liveBarTime(symbol, intervalSecs, index);
  const close = overrides.close ?? 10 + index;
  return {
    type: "bar_update",
    symbol,
    timestamp: isoOf(time),
    open: close - 0.5,
    high: close + 1,
    low: close - 1,
    close,
    volume: 100 + index,
    intervalSecs,
    isFinal: overrides.isFinal ?? false,
  };
}

/** What the /bars REST endpoint hands back: plain CandleBar rows. */
export function makeHistoryBars(symbol: string, count: number): FixtureCandle[] {
  const base = SYMBOL_BLOCKS[symbol];
  if (base === undefined) throw new Error(`unknown fixture symbol ${symbol}`);
  return Array.from({ length: count }, (_, i) => {
    const close = 5 + i;
    return { time: base + HISTORY_OFFSET + i * 60, open: close - 0.5, high: close + 1, low: close - 1, close, volume: 50 + i };
  });
}
