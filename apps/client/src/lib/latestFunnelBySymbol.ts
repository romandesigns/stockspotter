import type { FunnelSignal } from "@stockspotter/shared-types";

/** funnelSignals is newest-first; keep the first observation for each symbol. */
export function latestFunnelBySymbol(signals: readonly FunnelSignal[]): Map<string, FunnelSignal> {
  const latest = new Map<string, FunnelSignal>();
  for (const signal of signals) {
    if (!latest.has(signal.symbol)) latest.set(signal.symbol, signal);
  }
  return latest;
}
