// Tracks update cadence for the ONE series a chart is displaying, and
// reports its freshness.
//
// Scoped to the displayed symbol rather than held globally in
// useRealtimeFeed on purpose: the cadence of a symbol nobody is looking at
// is not needed, and a per-symbol map in the feed hook would churn global
// state on every bar for every symbol -- exactly the identity thrash the
// 2026-09-20 memo work removed.
//
// Updates are detected from the identity of the per-symbol bar array. That
// only changes when THIS symbol's series changes, because reconcileBars
// returns a new array for the updated symbol alone -- so this is a faithful
// "an update for my symbol arrived" signal and not a proxy for general
// socket traffic. It is also why the cadence cannot be polluted by other
// symbols: their arrays keep their identity.

import { useEffect, useRef, useState } from "react";
import {
  emptyCadence,
  observeUpdate,
  resolveSymbolFreshness,
  type SymbolCadenceState,
  type SymbolFreshnessResult,
} from "./feedHealth";

/** How often to re-evaluate age against the thresholds.
 *
 * The chart itself is memoised, so this must not cause a render per second.
 * The result is only pushed into state when the VERDICT changes, which for a
 * healthy active symbol is never -- matching the app's existing 1s
 * updated-age cadence without reintroducing per-second chart re-renders. */
const EVALUATE_EVERY_MS = 1000;

export function useSymbolCadence(
  symbol: string | null,
  intervalSecs: number,
  /** The per-symbol series whose identity changes on each update. */
  series: unknown,
): SymbolFreshnessResult {
  const stateRef = useRef<SymbolCadenceState>(emptyCadence(symbol ?? "", intervalSecs));
  const [result, setResult] = useState<SymbolFreshnessResult>(() =>
    resolveSymbolFreshness(stateRef.current, Date.now()),
  );
  // Keeps the evaluator honest without making it a dependency: comparing the
  // previous verdict inside the interval would otherwise need `result` in the
  // dep array and restart the timer on every change.
  const lastRef = useRef(result);

  // Record an update whenever this symbol's own series changes identity.
  useEffect(() => {
    if (!symbol) return;
    stateRef.current = observeUpdate(stateRef.current, symbol, intervalSecs, Date.now());
    const next = resolveSymbolFreshness(stateRef.current, Date.now());
    lastRef.current = next;
    setResult(next);
  }, [symbol, intervalSecs, series]);

  // Re-evaluate as time passes, but only surface a change of verdict.
  useEffect(() => {
    const id = setInterval(() => {
      const next = resolveSymbolFreshness(stateRef.current, Date.now());
      const prev = lastRef.current;
      if (next.freshness !== prev.freshness) {
        lastRef.current = next;
        setResult(next);
        return;
      }
      // Age is rendered for the non-live states, so it has to advance -- but
      // only at whole-second granularity, which is all formatAge shows.
      if (next.freshness !== "live" && Math.round(next.ageSecs ?? 0) !== Math.round(prev.ageSecs ?? 0)) {
        lastRef.current = next;
        setResult(next);
      }
    }, EVALUATE_EVERY_MS);
    return () => clearInterval(id);
  }, []);

  return result;
}
