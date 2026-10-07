// Tests for the bar-shaping logic that sits between the wire and the
// chart. Both functions here exist to defend against real conditions the
// live feed produces — duplicate and out-of-order timestamps, and the
// same minute arriving from two independent sources — so the tests are
// written around those conditions rather than around happy-path input.

import { describe, expect, test } from "bun:test";
import type { BarUpdate } from "@stockspotter/shared-types";
import { listChartableSymbols, mergeBars, toChartBars, type CandleBar } from "./derive";

function update(iso: string, close: number, extra: Partial<BarUpdate> = {}): BarUpdate {
  return {
    type: "bar_update",
    symbol: "TEST",
    timestamp: iso,
    open: close,
    high: close,
    low: close,
    close,
    volume: 100,
    intervalSecs: 60,
    ...extra,
  } as BarUpdate;
}

function candle(time: number, close: number): CandleBar {
  return { time, open: close, high: close, low: close, close, volume: 1 };
}

describe("toChartBars", () => {
  test("converts ISO timestamps to unix seconds", () => {
    const [b] = toChartBars([update("2026-08-28T13:30:00Z", 10)]);
    expect(b.time).toBe(Date.parse("2026-08-28T13:30:00Z") / 1000);
    expect(b.close).toBe(10);
  });

  test("replaces a repeated timestamp", () => {
    // lightweight-charts rejects a series with duplicate times outright,
    // so this is a hard requirement, not a nicety.
    const out = toChartBars([
      update("2026-08-28T13:30:00Z", 10),
      update("2026-08-28T13:30:00Z", 11),
    ]);
    expect(out.length).toBe(1);
    // The latest correction wins.
    expect(out[0].close).toBe(11);
  });

  test("sorts an out-of-order timestamp", () => {
    const out = toChartBars([
      update("2026-08-28T13:31:00Z", 11),
      update("2026-08-28T13:30:00Z", 10), // arrives late, older
      update("2026-08-28T13:32:00Z", 12),
    ]);
    expect(out.map((b) => b.close)).toEqual([10, 11, 12]);
  });

  test("output is strictly increasing in time", () => {
    const out = toChartBars([
      update("2026-08-28T13:30:00Z", 10),
      update("2026-08-28T13:30:00Z", 10),
      update("2026-08-28T13:29:00Z", 9),
      update("2026-08-28T13:31:00Z", 11),
    ]);
    for (let i = 1; i < out.length; i++) {
      expect(out[i].time).toBeGreaterThan(out[i - 1].time);
    }
  });

  test("handles an empty list", () => {
    expect(toChartBars([])).toEqual([]);
  });
});

describe("mergeBars", () => {
  test("live wins over historical for the same minute", () => {
    // The whole point: a still-forming minute fetched from the REST
    // backfill is stale the moment the live socket sends its own version.
    const merged = mergeBars([candle(100, 10)], [candle(100, 99)]);
    expect(merged.length).toBe(1);
    expect(merged[0].close).toBe(99);
  });

  test("unions disjoint ranges and sorts them", () => {
    const merged = mergeBars([candle(300, 3), candle(100, 1)], [candle(200, 2)]);
    expect(merged.map((b) => b.time)).toEqual([100, 200, 300]);
  });

  test("never produces duplicate timestamps", () => {
    const merged = mergeBars(
      [candle(100, 1), candle(200, 2)],
      [candle(200, 22), candle(300, 3)],
    );
    const times = merged.map((b) => b.time);
    expect(new Set(times).size).toBe(times.length);
  });

  test("either side being empty is fine", () => {
    expect(mergeBars([], [candle(100, 1)]).length).toBe(1);
    expect(mergeBars([candle(100, 1)], []).length).toBe(1);
    expect(mergeBars([], [])).toEqual([]);
  });
});

describe("listChartableSymbols", () => {
  test("sorts alphabetically so the picker order is stable", () => {
    const map = new Map<string, BarUpdate[]>([
      ["ZTG", []],
      ["AEHL", []],
      ["MERC", []],
    ]);
    expect(listChartableSymbols(map)).toEqual(["AEHL", "MERC", "ZTG"]);
  });

  test("is empty when nothing is tracked", () => {
    expect(listChartableSymbols(new Map())).toEqual([]);
  });
});
