// Pure pieces of the live phone chart (2026-10-16 chart diagnosis): how
// history and live bars merge, and what crosses the WebView bridge on
// each change. No React or React Native imports, so bun:test runs it.
import type { BarUpdate } from "@stockspotter/shared-types";
import type { CandleBar } from "./types";

/** Seconds in the 1-minute candles both the REST history and the live
 * 60 s stream use. */
const MINUTE_SEC = 60;

/**
 * Merge authoritative REST history with the live 1-minute stream.
 *
 * Same rule as shared-types' reconcileBars, applied across the two
 * sources in one linear pass instead of one sort per bar:
 * - A FINAL bar wins. History bars whose minute had already closed when
 *   the request was made are final; so are live bars flagged isFinal.
 *   A final live bar replaces history (both are the provider's closed
 *   minute; the live one is newer), a forming live bar never replaces a
 *   final history bar.
 * - The FORMING bar is kept. A live preview for a minute history does not
 *   have (the usual case: the current minute), or for one history only
 *   had a partial copy of (fetched mid-minute), is what the chart shows.
 *
 * `historyFetchedAtSec` is when the history request was issued, in unix
 * seconds; null treats every history bar as final.
 */
export function mergeHistoryAndLive(history: readonly CandleBar[], historyFetchedAtSec: number | null, live: readonly BarUpdate[]): CandleBar[] {
  const byTime = new Map<number, { bar: CandleBar; final: boolean }>();
  for (const bar of history) {
    const final = historyFetchedAtSec === null || bar.time + MINUTE_SEC <= historyFetchedAtSec;
    byTime.set(bar.time, { bar, final });
  }
  let previousLiveTime = -Infinity;
  for (const update of live) {
    const time = Math.floor(Date.parse(update.timestamp) / 1000);
    // Same guard as the old toChartBars: unparsable or out-of-order live
    // entries are skipped rather than trusted.
    if (!Number.isFinite(time) || time <= previousLiveTime) continue;
    previousLiveTime = time;
    const final = update.isFinal === true;
    const existing = byTime.get(time);
    if (existing?.final && !final) continue;
    byTime.set(time, { bar: { time, open: update.open, high: update.high, low: update.low, close: update.close, volume: update.volume }, final });
  }
  return [...byTime.values()].map((entry) => entry.bar).sort((a, b) => a.time - b.time);
}

/** Live bars as plain candles, in order, duplicates and unparsable
 * timestamps dropped -- the old derive.ts toChartBars, unchanged. */
export function toChartBars(bars: readonly BarUpdate[]): CandleBar[] {
  const result: CandleBar[] = [];
  for (const b of bars) {
    const time = Math.floor(new Date(b.timestamp).getTime() / 1000);
    const prev = result[result.length - 1];
    if (!Number.isFinite(time) || (prev && time <= prev.time)) continue;
    result.push({ time, open: b.open, high: b.high, low: b.low, close: b.close, volume: b.volume });
  }
  return result;
}

// ---------------------------------------------------------------------
// WebView bridge protocol.
//
// The chart page used to receive the WHOLE bar array (up to ~500 bars,
// JSON-encoded and evaluated through injectJavaScript) on every live
// tick. Now RN remembers what it last posted and sends one of:
//
//   full   window.__setBars(bars, viewKey)
//          On a new symbol/range/timeframe (viewKey), when history was
//          refetched and changed older bars (gap repair), when the page
//          (re)loaded, or when the page asks for it.
//   update window.__updateBars({ viewKey, from, anchorTime, length, bars })
//          Only the tail that changed: bars[from..] replace the page's
//          own bars from index `from` on. `anchorTime` is the time of the
//          bar just before `from` (null when from is 0) and `length` the
//          resulting count, so the page can tell it is out of step and
//          answer {type:"needFull"} instead of drawing a wrong series.
//   none   nothing changed.
// ---------------------------------------------------------------------

/** How many of the previously posted bars an update may rewrite. The
 * forming bar is the last one; a minute's final bar can land just after
 * the next minute's first preview, which makes it the second to last. */
export const MAX_UPDATE_REWRITE = 3;
/** More new bars than this at once (e.g. a refetch filled a hole) is a
 * full post: cheaper to redraw than to stream a long tail. */
export const MAX_UPDATE_APPEND = 10;

export interface ChartUpdatePayload {
  viewKey: string;
  from: number;
  anchorTime: number | null;
  length: number;
  bars: CandleBar[];
}

export type ChartPost =
  | { kind: "none" }
  | { kind: "full"; bars: CandleBar[]; viewKey: string }
  | { kind: "update"; payload: ChartUpdatePayload };

export interface PostedChart {
  viewKey: string;
  bars: readonly CandleBar[];
}

function sameBar(a: CandleBar, b: CandleBar): boolean {
  return a.time === b.time && a.open === b.open && a.high === b.high && a.low === b.low && a.close === b.close && a.volume === b.volume;
}

/**
 * Decide what to send the page, given what it was last sent (null after
 * a page load or a needFull request) and what it should now show.
 */
export function planChartPost(posted: PostedChart | null, next: readonly CandleBar[], viewKey: string): ChartPost {
  if (!posted || posted.viewKey !== viewKey) return { kind: "full", bars: next.slice(), viewKey };
  const prev = posted.bars;
  if (prev === next) return { kind: "none" };
  if (prev.length === 0 || next.length === 0 || next.length < prev.length) {
    return prev.length === 0 && next.length === 0 ? { kind: "none" } : { kind: "full", bars: next.slice(), viewKey };
  }
  let from = 0;
  while (from < prev.length && sameBar(prev[from], next[from])) from++;
  if (from === prev.length && next.length === prev.length) return { kind: "none" };
  if (from < prev.length - MAX_UPDATE_REWRITE || next.length - prev.length > MAX_UPDATE_APPEND) {
    return { kind: "full", bars: next.slice(), viewKey };
  }
  return {
    kind: "update",
    payload: { viewKey, from, anchorTime: from > 0 ? next[from - 1].time : null, length: next.length, bars: next.slice(from) },
  };
}

/**
 * The page-side half of an update, as a pure function: returns the new
 * bar list, or null when the page is out of step and must ask for a full
 * post. chartHtml.ts embeds the same logic (applyBarUpdate); the test
 * keeps the two definitions honest by exercising this one against
 * planChartPost.
 */
export function applyChartUpdate(current: readonly CandleBar[], currentViewKey: string | null, update: ChartUpdatePayload): CandleBar[] | null {
  if (update.viewKey !== currentViewKey) return null;
  if (update.from > current.length) return null;
  if (update.from > 0 && current[update.from - 1]?.time !== update.anchorTime) return null;
  const next = current.slice(0, update.from).concat(update.bars);
  return next.length === update.length ? next : null;
}
