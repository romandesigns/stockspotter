// Real historical + live bars for one symbol, across a selectable
// Robinhood-style time range -- ported from apps/client/src/lib/
// derive.ts's toChartBars/mergeBars and chartIndicators.ts's resample
// (same functions, not re-derived), plus useHistoricalBackfill.ts's
// REST-fetch pattern.
//
// 1D reuses the same /bars/:symbol?minutes= endpoint the web app's live
// chart backfill already uses, at native 1-minute resolution, and keeps
// merging in live ticks as they arrive. 1W/1M reuse the real
// /replay/bars/:symbol?start&end endpoint -- built for the Backtest
// Replay dialog, but it's real multi-day 1-minute data for any symbol,
// exactly what a longer-range view needs too -- then resample() (the
// real chartIndicators.ts bucketing function, not array-index chunking)
// coarsens it so a month of 1-minute bars doesn't mean ~10k points
// pushed into the WebView. Longer ranges intentionally stop at 1M: the
// backend's own MAX_REPLAY_SPAN_DAYS cap (see http.rs) is 45 days, so
// 3M/1Y/ALL aren't honestly buildable without backend work this pass
// didn't include.

import { useEffect, useMemo, useState } from "react";
import type { BarUpdate } from "@stockspotter/shared-types";
import { HTTP_URL } from "./config";
import type { CandleBar } from "./types";

export type ChartRange = "1D" | "1W" | "1M";

const RANGE_CONFIG: Record<ChartRange, { days: number; bucketMinutes: number }> = {
  "1D": { days: 1, bucketMinutes: 1 },
  "1W": { days: 7, bucketMinutes: 5 },
  "1M": { days: 30, bucketMinutes: 30 },
};

const BACKFILL_MINUTES = 240;

function toDateStr(d: Date): string {
  return d.toISOString().slice(0, 10);
}

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

/** Ported verbatim from chartIndicators.ts. */
function resample(bars: CandleBar[], minutesPerBucket: number): CandleBar[] {
  if (minutesPerBucket === 1) return bars;
  const bucketSec = minutesPerBucket * 60;
  const out: CandleBar[] = [];
  let current: CandleBar | null = null;
  let currentBucketStart: number | null = null;
  for (const b of bars) {
    const bucketStart = Math.floor(b.time / bucketSec) * bucketSec;
    if (bucketStart !== currentBucketStart) {
      if (current) out.push(current);
      currentBucketStart = bucketStart;
      current = { time: bucketStart, open: b.open, high: b.high, low: b.low, close: b.close, volume: b.volume };
    } else if (current) {
      current.high = Math.max(current.high, b.high);
      current.low = Math.min(current.low, b.low);
      current.close = b.close;
      current.volume += b.volume;
    }
  }
  if (current) out.push(current);
  return out;
}

export function useChartBars(symbol: string | null, liveBarsForSymbol: BarUpdate[], range: ChartRange): CandleBar[] {
  const [historical, setHistorical] = useState<CandleBar[]>([]);
  const { days, bucketMinutes } = RANGE_CONFIG[range];

  useEffect(() => {
    if (!symbol) { setHistorical([]); return; }
    setHistorical([]);
    let cancelled = false;

    const url =
      range === "1D"
        ? `${HTTP_URL}/bars/${encodeURIComponent(symbol)}?minutes=${BACKFILL_MINUTES}`
        : `${HTTP_URL}/replay/bars/${encodeURIComponent(symbol)}?start=${toDateStr(new Date(Date.now() - days * 86400_000))}&end=${toDateStr(new Date())}`;

    fetch(url)
      .then((r) => { if (!r.ok) throw new Error(`backfill failed: ${r.status}`); return r.json() as Promise<CandleBar[]>; })
      .then((fetched) => { if (!cancelled) setHistorical(resample(fetched, bucketMinutes)); })
      .catch(() => { /* best-effort -- live bars alone still work, just sparser, on 1D */ });
    return () => { cancelled = true; };
  }, [symbol, range, days, bucketMinutes]);

  // Live ticks only matter for keeping "right now" current -- meaningful
  // for 1D; on 1W/1M a single unresampled 1-minute tick would sit at a
  // finer resolution than the rest of the (bucketed) series, so it's
  // left out there rather than faked into a bucket it doesn't really
  // represent yet.
  const live = useMemo(() => (range === "1D" ? toChartBars(liveBarsForSymbol) : []), [liveBarsForSymbol, range]);
  return useMemo(() => mergeBars(historical, live), [historical, live]);
}
