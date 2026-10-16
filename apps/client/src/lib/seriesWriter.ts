/** Lightweight Charts 4.1 permits update() only at/after its latest time.
 * Keep complete replacement for corrections, eviction, reset and timeframe changes.
 * Callers supply immutable, sorted arrays, as the indicator functions already do.
 *
 * `unchangedBefore` (optional, a time in the series' own unit) is the
 * caller's promise that every point earlier than it is identical to the
 * last write -- superChartEngine.ts works it out once from the bars
 * rather than every one of its twelve series re-comparing its whole
 * history on each live tick. It only skips that comparison; it never
 * makes a write that the full check would not have made, and it is
 * ignored when it does not cover all but the last previous point.
 */
export function createSeriesWriter<T extends { time: unknown }>(series: {
  setData: (data: T[]) => void;
  update: (item: T) => void;
}) {
  let previous: T[] = [];
  let initialized = false;
  const equal = (a: T, b: T) => {
    const keys = Object.keys(a) as (keyof T)[];
    return keys.length === Object.keys(b).length && keys.every((key) => a[key] === b[key]);
  };
  return (data: T[], unchangedBefore?: number) => {
    let tailOnly = previous.length > 0 && data.length >= previous.length;
    if (tailOnly) {
      const last = previous.length - 1;
      // Trust the hint only if it vouches for everything before the last
      // previous point; otherwise compare as before.
      const trusted = unchangedBefore !== undefined && (last === 0 || (previous[last - 1].time as number) < unchangedBefore);
      if (!trusted) {
        for (let i = 0; i < last; i++) {
          if (!equal(previous[i], data[i])) { tailOnly = false; break; }
        }
      }
      if (previous.at(-1)!.time !== data[last]?.time) tailOnly = false;
    }
    if (tailOnly) {
      for (let i = previous.length - 1; i < data.length; i++) {
        if (!previous[i] || !equal(previous[i], data[i])) series.update(data[i]);
      }
    } else if (!initialized || data.length || previous.length) {
      series.setData(data);
    }
    previous = data;
    initialized = true;
  };
}

interface BarLike { time: number; open: number; high: number; low: number; close: number; volume: number }

/**
 * What changed between two writes of a chart's bars, worked out once for
 * all the series drawn from them:
 *
 * - `same`: nothing -- skip the indicator pass and every write.
 * - `tail`: only the last previous bar changed and/or bars were appended,
 *   so every point (candle, volume or indicator) earlier than
 *   `unchangedBefore` is unchanged too. All the indicators here are
 *   causal -- a point depends only on its own bar and earlier ones -- which
 *   is what makes that true for them.
 * - `full`: history changed (a refetch, a trim, a timeframe switch); let
 *   each writer decide, as before.
 *
 * Bars are compared by value, not identity: the live merge rebuilds bar
 * objects on every update.
 */
export function barsChange(prev: readonly BarLike[], next: readonly BarLike[]): { kind: "same" } | { kind: "tail"; unchangedBefore: number } | { kind: "full" } {
  if (prev.length === 0 || next.length < prev.length) return { kind: "full" };
  const n = prev.length;
  let i = 0;
  for (; i < n; i++) {
    const a = prev[i];
    const b = next[i];
    if (a !== b && (a.time !== b.time || a.open !== b.open || a.high !== b.high || a.low !== b.low || a.close !== b.close || a.volume !== b.volume)) break;
  }
  if (i === n && next.length === n) return { kind: "same" };
  if (i < n - 1) return { kind: "full" };
  return { kind: "tail", unchangedBefore: next[i].time };
}
