import { expect, test } from "bun:test";
import type { BarUpdate } from "@stockspotter/shared-types";
import { reconcileBars } from "./reconcileBars";
const bar = (minute: number, close: number, isFinal = false): BarUpdate => ({
  type: "bar_update", symbol: "TEST", timestamp: new Date(1800000000000 + minute * 60000).toISOString(),
  open: close, high: close, low: close, close, volume: 100, intervalSecs: 60, isFinal,
});
test("late corrections preserve subsequent updates", () => {
  let bars = [bar(0, 10), bar(1, 11)];
  bars = reconcileBars(bars, bar(0, 10.5, true), 500);
  bars = reconcileBars(bars, bar(1, 12, true), 500);
  expect(bars.map((b) => b.close)).toEqual([10.5, 12]);
});
test("a preview cannot replace an official bar, but an official correction can", () => {
  let bars = [bar(0, 10, true)];
  bars = reconcileBars(bars, bar(0, 9), 500);
  expect(bars[0].close).toBe(10);
  bars = reconcileBars(bars, bar(0, 10.5, true), 500);
  expect(bars[0].close).toBe(10.5);
});
