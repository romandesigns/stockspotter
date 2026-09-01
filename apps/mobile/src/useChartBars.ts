// Real historical + live bars for one symbol -- ported from
// apps/client/src/lib/derive.ts's toChartBars/mergeBars (same functions,
// not re-derived) plus useHistoricalBackfill.ts's REST-fetch pattern,
// reusing the exact same /bars/:symbol endpoint the web app's chart
// backfill already uses.

import { useEffect, useMemo, useState } from "react";
import type { BarUpdate } from "@stockspotter/shared-types";
import { HTTP_URL } from "./config";
import type { CandleBar } from "./types";

const BACKFILL_MINUTES = 240;

/** Ported verbatim from derive.ts. */
function toChartBars(bars: BarUpdate[]): CandleBar[] {
  const result: CandleBar[] = [];
  for (const b of bars) {
    const time = Math.floor(new Date(b.timestamp).getTime() / 1000);
    const prev = result[result.length - 1];
    if (prev && time <= prev.time) continue;
    result.push({ time, open: b.open, high: b.high, low: b.low, close: b.close, volume: b.volume });
  }
  return result;
}

/** Ported verbatim from derive.ts. */
function mergeBars(historical: CandleBar[], live: CandleBar[]): CandleBar[] {
  const byTime = new Map<number, CandleBar>();
  for (const b of historical) byTime.set(b.time, b);
  for (const b of live) byTime.set(b.time, b);
  return [...byTime.values()].sort((a, b) => a.time - b.time);
}

export function useChartBars(symbol: string | null, liveBarsForSymbol: BarUpdate[]): CandleBar[] {
  const [historical, setHistorical] = useState<CandleBar[]>([]);

  useEffect(() => {
    if (!symbol) { setHistorical([]); return; }
    setHistorical([]);
    let cancelled = false;
    fetch(`${HTTP_URL}/bars/${encodeURIComponent(symbol)}?minutes=${BACKFILL_MINUTES}`)
      .then((r) => { if (!r.ok) throw new Error(`backfill failed: ${r.status}`); return r.json() as Promise<CandleBar[]>; })
      .then((fetched) => { if (!cancelled) setHistorical(fetched); })
      .catch(() => { /* best-effort -- live bars alone still work, just sparser */ });
    return () => { cancelled = true; };
  }, [symbol]);

  const live = useMemo(() => toChartBars(liveBarsForSymbol), [liveBarsForSymbol]);
  return useMemo(() => mergeBars(historical, live), [historical, live]);
}
