// Run from the repo root: bun test apps/mobile/src/chartLive.test.ts
// Excluded from the mobile tsconfig, which has no bun types.
import { describe, expect, test } from "bun:test";
import type { BarUpdate } from "@stockspotter/shared-types";
import { applyChartUpdate, MAX_UPDATE_APPEND, mergeHistoryAndLive, planChartPost, toChartBars, type PostedChart } from "./chartLive";
import { resample } from "./chartIndicators";
import type { CandleBar } from "./types";

const T0 = Date.parse("2026-10-16T14:30:00Z") / 1000;
const candle = (minute: number, close: number, volume = 100): CandleBar => ({ time: T0 + minute * 60, open: 1, high: Math.max(1, close), low: Math.min(1, close), close, volume });
const live = (minute: number, close: number, isFinal?: boolean): BarUpdate => ({
  type: "bar_update", symbol: "AAA", timestamp: new Date((T0 + minute * 60) * 1000).toISOString(),
  open: 1, high: Math.max(1, close), low: Math.min(1, close), close, volume: 100, intervalSecs: 60, ...(isFinal === undefined ? {} : { isFinal }),
});

describe("mergeHistoryAndLive", () => {
  const history = [candle(0, 1.1), candle(1, 1.2), candle(2, 1.3)];
  // Fetched 30 s into minute 2: minutes 0-1 are closed, minute 2 partial.
  const fetchedAt = T0 + 2 * 60 + 30;

  test("a forming live bar never replaces a closed history minute", () => {
    expect(mergeHistoryAndLive(history, fetchedAt, [live(1, 9)]).find((b) => b.time === T0 + 60)?.close).toBe(1.2);
  });
  test("a forming live bar replaces history's partial copy of the current minute, and is kept when history lacks it", () => {
    const merged = mergeHistoryAndLive(history, fetchedAt, [live(2, 1.35), live(3, 1.4)]);
    expect(merged.map((b) => b.close)).toEqual([1.1, 1.2, 1.35, 1.4]);
  });
  test("a final live bar wins over history", () => {
    expect(mergeHistoryAndLive(history, fetchedAt, [live(1, 1.25, true)])[1].close).toBe(1.25);
  });
  test("with no fetch time every history bar counts as final; output is sorted", () => {
    expect(mergeHistoryAndLive([candle(2, 1.3), candle(0, 1.1)], null, [live(0, 5), live(1, 1.2)]).map((b) => b.close)).toEqual([1.1, 1.2, 1.3]);
  });
  test("live-only and history-only both work", () => {
    expect(mergeHistoryAndLive([], null, [live(0, 1.1)])).toEqual(toChartBars([live(0, 1.1)]));
    expect(mergeHistoryAndLive(history, fetchedAt, [])).toEqual(history);
  });
});

function roundTrip(posted: PostedChart | null, pageBars: CandleBar[], next: CandleBar[], viewKey: string): { kind: string; page: CandleBar[] | null } {
  const post = planChartPost(posted, next, viewKey);
  if (post.kind === "none") return { kind: "none", page: pageBars };
  if (post.kind === "full") return { kind: "full", page: post.bars };
  return { kind: "update", page: applyChartUpdate(pageBars, posted?.viewKey ?? null, post.payload) };
}

describe("planChartPost / applyChartUpdate", () => {
  const base = Array.from({ length: 200 }, (_, i) => candle(i, 1 + i / 1000));
  const key = '["AAA","1D",1]';

  test("first post, view change and page reload are full", () => {
    expect(planChartPost(null, base, key).kind).toBe("full");
    expect(planChartPost({ viewKey: key, bars: base }, base, '["BBB","1D",1]').kind).toBe("full");
  });

  test("a growing forming candle is a one-bar update that the page applies to the exact array", () => {
    const next = [...base.slice(0, -1), candle(199, 1.5, 300)];
    const post = planChartPost({ viewKey: key, bars: base }, next, key);
    expect(post.kind).toBe("update");
    if (post.kind !== "update") return;
    expect(post.payload.bars).toHaveLength(1);
    expect(applyChartUpdate(base, key, post.payload)).toEqual(next);
  });

  test("a new minute plus the previous minute's final bar is a two-bar update", () => {
    const next = [...base.slice(0, -1), candle(199, 1.6), candle(200, 1.61)];
    const r = roundTrip({ viewKey: key, bars: base }, base, next, key);
    expect(r).toEqual({ kind: "update", page: next });
  });

  test("unchanged bars post nothing", () => {
    expect(planChartPost({ viewKey: key, bars: base }, base.slice(), key).kind).toBe("none");
  });

  test("a history refetch that rewrote older bars (gap repair) is full", () => {
    const repaired = base.map((b, i) => (i === 50 ? { ...b, close: 9 } : b));
    expect(planChartPost({ viewKey: key, bars: base }, repaired, key).kind).toBe("full");
    const filled = [...base, ...Array.from({ length: MAX_UPDATE_APPEND + 1 }, (_, i) => candle(200 + i, 2))];
    expect(planChartPost({ viewKey: key, bars: base }, filled, key).kind).toBe("full");
    expect(planChartPost({ viewKey: key, bars: base }, base.slice(1), key).kind).toBe("full");
  });

  test("resampled 5 m view: a 1 m tick updates only the last bucket", () => {
    const five = resample(base, 5);
    const nextFive = resample([...base.slice(0, -1), candle(199, 3)], 5);
    const r = roundTrip({ viewKey: key, bars: five }, five, nextFive, key);
    expect(r).toEqual({ kind: "update", page: nextFive });
  });

  test("an out-of-step page refuses an update instead of drawing a wrong series", () => {
    const next = [...base.slice(0, -1), candle(199, 1.5)];
    const post = planChartPost({ viewKey: key, bars: base }, next, key);
    if (post.kind !== "update") throw new Error("expected update");
    expect(applyChartUpdate(base.slice(0, 100), key, post.payload)).toBeNull(); // missed bars
    expect(applyChartUpdate(base, "other", post.payload)).toBeNull(); // different view
    expect(applyChartUpdate([], key, post.payload)).toBeNull(); // reloaded page
  });

  test("a long session of ticks stays identical to full posts", () => {
    let posted: PostedChart | null = null;
    let page: CandleBar[] = [];
    let bars: CandleBar[] = base.slice(0, 10);
    for (let tick = 0; tick < 300; tick++) {
      const minute = 10 + Math.floor(tick / 7);
      const last = bars[bars.length - 1];
      bars = last.time === T0 + minute * 60 ? [...bars.slice(0, -1), candle(minute, 1 + tick / 100)] : [...bars, candle(minute, 1 + tick / 100)];
      const r = roundTrip(posted, page, bars, key);
      expect(r.page).toEqual(bars);
      page = r.page!;
      posted = { viewKey: key, bars };
    }
  });
});
