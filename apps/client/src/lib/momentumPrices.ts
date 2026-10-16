import type { BarUpdate } from "@stockspotter/shared-types";
import type { ChartReferenceQuote } from "./chartChange";

/** Latest available observations, not a guarantee of a fresh trade. */
export function momentumPrices(
  bars: Map<string, BarUpdate[]>,
  references: Map<string, ChartReferenceQuote[]>,
) {
  const latest = new Map<string, { price: number; at: number }>();
  function add(symbol: string, price: number, timestamp: string) {
    const at = Date.parse(timestamp);
    if (!Number.isFinite(at)) return;
    const previous = latest.get(symbol);
    // References win equal timestamps; an invalid newest price remains
    // invalid rather than falling back to an older qualifying price.
    if (!previous || at >= previous.at) latest.set(symbol, { price, at });
  }
  for (const [symbol, rows] of bars) for (const row of rows) add(symbol, row.close, row.timestamp);
  for (const [symbol, rows] of references) for (const row of rows) add(symbol, row.price, row.timestamp);
  return new Map([...latest].map(([symbol, quote]) => [symbol, quote.price]));
}
