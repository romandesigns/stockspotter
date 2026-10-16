import { authenticatedFetch } from "@stockspotter/shared-types";
// Real historical + live bars for one symbol, across a selectable
// Robinhood-style time range -- ported from apps/client/src/lib/
// derive.ts's toChartBars/mergeBars (now chartLive.ts) and chartIndicators.ts's resample
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

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { BarUpdate } from "@stockspotter/shared-types";
import { HTTP_URL } from "./config";
import { resample } from "./chartIndicators";
import { mergeHistoryAndLive, toChartBars } from "./chartLive";
import type { CandleBar } from "./types";

export type ChartRange = "1D" | "1W" | "1M";

// Exported so the candle countdown (chartHtml.ts, driven from
// ChartScreen.tsx) can know each range's real bucket size for 1W/1M --
// those aren't user-selectable via the timeframe pills the way 1D's
// 1/5/15m is, but they're still real candle boundaries the countdown
// should track, not just skip.
export const RANGE_CONFIG: Record<ChartRange, { days: number; bucketMinutes: number }> = {
  "1D": { days: 1, bucketMinutes: 1 },
  "1W": { days: 7, bucketMinutes: 5 },
  "1M": { days: 30, bucketMinutes: 30 },
};

const BACKFILL_MINUTES = 240;
const EMPTY_HISTORY: CandleBar[] = [];
/** History refresh cadence while a 1D chart is open and the app is in
 * the foreground (2026-10-16 chart diagnosis, root cause #5: history
 * used to be fetched once per open and never again, so a minute the live
 * stream missed stayed missing, and so did the hole between the end of
 * the first fetch and the first live bar). */
export const HISTORY_REFRESH_MS = 60_000;

function toDateStr(d: Date): string {
  return d.toISOString().slice(0, 10);
}

interface History { symbol: string; range: ChartRange; bars: CandleBar[]; fetchedAtSec: number; repairedNonce: number }

export interface ChartBars {
  bars: CandleBar[];
  /** The highest resyncNonce whose gap this history is known to cover: a
   * fetch that STARTED at or after that nonce succeeded. Equal to the
   * current nonce means the 1-minute series was re-fetched over every
   * recorded gap and is no longer suspect -- same contract as the web
   * app's useHistoricalBackfill. -1 until the first successful fetch. */
  repairedNonce: number;
}

export interface ChartBarsOptions {
  /** useRealtimeFeed's resyncNonce: bumped on every recorded gap
   * (reconnect, stream_lagged, upstream feed down -> up). Each bump
   * refetches history with `fresh=1`, so ws-server answers from the
   * provider rather than any in-memory copy -- gap repair stays
   * authoritative. */
  resyncNonce?: number;
  /** False while the app is backgrounded: no periodic refresh then. */
  active?: boolean;
}

export function useChartBars(symbol: string | null, liveBarsForSymbol: BarUpdate[], range: ChartRange, options: ChartBarsOptions = {}): ChartBars {
  const { resyncNonce = 0, active = true } = options;
  const [history, setHistory] = useState<History | null>(null);
  // Effects run after commit: never expose another symbol or range's history
  // while the new request is being started.
  const historical = history?.symbol === symbol && history.range === range ? history.bars : EMPTY_HISTORY;
  const fetchedAtSec = history?.symbol === symbol && history.range === range ? history.fetchedAtSec : null;
  const { days, bucketMinutes } = RANGE_CONFIG[range];

  // Requests can overlap (a periodic refresh and a resync, say) and finish
  // out of order: only a response newer than the last one applied lands.
  const requestSeq = useRef(0);
  const appliedSeq = useRef(0);
  const lastFetchAt = useRef(0);
  const nonceRef = useRef(resyncNonce);
  nonceRef.current = resyncNonce;

  const fetchHistory = useCallback((fresh: boolean) => {
    if (!symbol) return;
    const seq = ++requestSeq.current;
    const nonce = nonceRef.current;
    lastFetchAt.current = Date.now();
    const fetchedAtSec = Math.floor(Date.now() / 1000);
    const url =
      range === "1D"
        ? `${HTTP_URL}/bars/${encodeURIComponent(symbol)}?minutes=${BACKFILL_MINUTES}${fresh ? "&fresh=1" : ""}`
        : `${HTTP_URL}/replay/bars/${encodeURIComponent(symbol)}?start=${toDateStr(new Date(Date.now() - days * 86400_000))}&end=${toDateStr(new Date())}`;
    authenticatedFetch(url)
      .then((r) => { if (!r.ok) throw new Error(`backfill failed: ${r.status}`); return r.json() as Promise<CandleBar[]>; })
      .then((fetched) => {
        if (seq <= appliedSeq.current) return;
        appliedSeq.current = seq;
        setHistory((prev) => ({
          symbol, range, bars: resample(fetched, bucketMinutes), fetchedAtSec,
          repairedNonce: Math.max(nonce, prev?.symbol === symbol && prev.range === range ? prev.repairedNonce : -1),
        }));
      })
      // Best-effort -- live bars alone still work, just sparser, on 1D. A
      // failed refresh keeps the history already on screen; a failed
      // resync leaves the gap flagged, which is the honest outcome.
      .catch(() => {});
  }, [symbol, range, days, bucketMinutes]);

  // Armed on every open; disarmed by the first live bar that arrives after
  // it, which triggers one extra refetch (see liveBars effect below).
  // The baseline is the live list as it was at open, so bars that were
  // already in the feed's map before the chart opened do not count.
  const firstLive = useRef<{ armed: boolean; baseline: BarUpdate[] | null }>({ armed: false, baseline: null });
  const liveRef = useRef(liveBarsForSymbol);
  liveRef.current = liveBarsForSymbol;

  useEffect(() => {
    setHistory(null);
    // Anything still in flight belongs to the previous symbol/range.
    appliedSeq.current = requestSeq.current;
    firstLive.current = { armed: range === "1D", baseline: liveRef.current };
    fetchHistory(false);
  }, [fetchHistory, range]);

  useEffect(() => {
    const state = firstLive.current;
    if (!state.armed || liveBarsForSymbol === state.baseline || liveBarsForSymbol.length === 0) return;
    state.armed = false;
    fetchHistory(false);
  }, [liveBarsForSymbol, fetchHistory]);

  // Gap repair. The first run is the mount (or a symbol switch, already
  // fetched above), not a resync.
  const lastNonce = useRef(resyncNonce);
  useEffect(() => {
    if (lastNonce.current === resyncNonce) return;
    lastNonce.current = resyncNonce;
    fetchHistory(true);
  }, [resyncNonce, fetchHistory]);

  // Periodic refresh: 1D only (1W/1M are resampled replay data with no
  // live merge), and only while the app is in the foreground -- caught up
  // at once on return if a refresh came due while it was away.
  useEffect(() => {
    if (!symbol || !active || range !== "1D") return;
    if (lastFetchAt.current > 0 && Date.now() - lastFetchAt.current >= HISTORY_REFRESH_MS) fetchHistory(false);
    const timer = setInterval(() => fetchHistory(false), HISTORY_REFRESH_MS);
    return () => clearInterval(timer);
  }, [symbol, active, range, fetchHistory]);

  // Live ticks only matter for keeping "right now" current -- meaningful
  // for 1D; on 1W/1M a single unresampled 1-minute tick would sit at a
  // finer resolution than the rest of the (bucketed) series, so it's
  // left out there rather than faked into a bucket it doesn't really
  // represent yet. Final bars win over forming previews; the forming bar
  // is kept (chartLive.ts's mergeHistoryAndLive).
  const live = range === "1D" ? liveBarsForSymbol : NO_LIVE;
  const bars = useMemo(() => mergeHistoryAndLive(historical, fetchedAtSec, live), [historical, fetchedAtSec, live]);
  const repairedNonce = history?.symbol === symbol && history.range === range ? history.repairedNonce : -1;
  return useMemo(() => ({ bars, repairedNonce }), [bars, repairedNonce]);
}

const NO_LIVE: BarUpdate[] = [];

/**
 * Real sub-minute (30s) live candles (2026-09-03) -- Roman's own ask to
 * "observe the price action at a granular label from 30 seconds and
 * up". No historical fetch here at all, unlike useChartBars above:
 * confirmed live against Alpaca's own REST API before building this
 * (`timeframe=30Sec` is rejected outright, `1Min` is the real floor for
 * historical bars) -- this is genuinely live-only, growing forward from
 * whenever the symbol started being tracked, nothing to backfill. Just
 * a thin memoized wrapper over the same toChartBars() every other range
 * already uses, fed from ws-server's new intervalSecs:30 BarUpdate
 * stream (useRealtimeFeed.ts's own subMinuteBarsBySymbol).
 */
export function useSubMinuteChartBars(subMinuteLiveBarsForSymbol: BarUpdate[]): CandleBar[] {
  return useMemo(() => toChartBars(subMinuteLiveBarsForSymbol), [subMinuteLiveBarsForSymbol]);
}
