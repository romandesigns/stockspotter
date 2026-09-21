// Mobile mirror of apps/client/src/lib/useSymbolCadence.ts.
//
// Only the React glue is duplicated; every semantic -- the cadence window,
// the thresholds, the classification -- comes from
// @stockspotter/shared-types/symbolFreshness, so the two clients cannot
// disagree about what "quiet" or "stale" means. The alternative was putting
// a hook in shared-types, which would give a plain type/logic package a
// React dependency it otherwise does not have.
//
// The identity signal here is cleaner than on web: ChartScreen already
// receives `liveBars` as a per-symbol array from App.tsx, so its identity
// changes only when this symbol's own series changes.

import { useEffect, useRef, useState } from "react";
import {
  emptyCadence,
  observeUpdate,
  resolveSymbolFreshness,
  type SymbolCadenceState,
  type SymbolFreshnessResult,
} from "@stockspotter/shared-types";

const EVALUATE_EVERY_MS = 1000;

export function useSymbolCadence(
  symbol: string | null,
  intervalSecs: number,
  series: unknown,
): SymbolFreshnessResult {
  const stateRef = useRef<SymbolCadenceState>(emptyCadence(symbol ?? "", intervalSecs));
  const [result, setResult] = useState<SymbolFreshnessResult>(() =>
    resolveSymbolFreshness(stateRef.current, Date.now()),
  );
  const lastRef = useRef(result);

  useEffect(() => {
    if (!symbol) return;
    stateRef.current = observeUpdate(stateRef.current, symbol, intervalSecs, Date.now());
    const next = resolveSymbolFreshness(stateRef.current, Date.now());
    lastRef.current = next;
    setResult(next);
  }, [symbol, intervalSecs, series]);

  useEffect(() => {
    const id = setInterval(() => {
      const next = resolveSymbolFreshness(stateRef.current, Date.now());
      const prev = lastRef.current;
      if (next.freshness !== prev.freshness) {
        lastRef.current = next;
        setResult(next);
        return;
      }
      if (next.freshness !== "live" && Math.round(next.ageSecs ?? 0) !== Math.round(prev.ageSecs ?? 0)) {
        lastRef.current = next;
        setResult(next);
      }
    }, EVALUATE_EVERY_MS);
    return () => clearInterval(id);
  }, []);

  return result;
}
