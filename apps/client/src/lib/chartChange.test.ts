import { expect, test } from "bun:test";
import { chartChangeReference } from "./chartChange";
const now = Date.parse("2026-10-09T14:20:00Z");
const bars = [{ time: now / 1000 - 60, open: 5.11, high: 6.5, low: 5.1, close: 6.47, volume: 100 }];
test("daily reference comes from the coherent current pair, not the loaded-window open", () => {
  const ref = chartChangeReference(bars, [{ price: 6.46, changePct: 66.9, timestamp: new Date(now - 30_000).toISOString() }], now);
  expect(ref.label).toBe("Day");
  expect((6.47 / ref.base - 1) * 100).toBeCloseTo(67.158, 2);
});
test("old, previous-day, malformed and unknown-zero references fall back honestly", () => {
  for (const quote of [
    { price: 6.46, changePct: 66.9, timestamp: new Date(now - 240_000).toISOString() },
    { price: 6.46, changePct: 66.9, timestamp: "invalid" },
    { price: 6.46, changePct: 0, timestamp: new Date(now).toISOString() },
    { price: 0, changePct: 66.9, timestamp: new Date(now).toISOString() },
  ]) expect(chartChangeReference(bars, [quote], now).label).toStartWith("Since ");
  const priorDay = [{ ...bars[0], time: (now - 24 * 3600_000) / 1000 }];
  expect(chartChangeReference(priorDay, [{ price: 6.46, changePct: 66.9, timestamp: new Date(now).toISOString() }], now).label).toStartWith("Since ");
});

test("scanner wins initially; reference stays fixed through source changes until New York day rolls", () => {
  const cache = new Map();
  const scanner = { price: 6, changePct: 100, timestamp: new Date(now - 30_000).toISOString(), source: "scanner" as const };
  const snapshot = { price: 6, changePct: 50, timestamp: new Date(now).toISOString(), source: "snapshot" as const };
  expect(chartChangeReference(bars, [snapshot, scanner], now, undefined, cache, "AAA").base).toBe(3);
  expect(chartChangeReference(bars, [snapshot], now + 1000, undefined, cache, "AAA").base).toBe(3);
  expect(chartChangeReference(bars, [], now + 240_000, undefined, cache, "AAA").label).toBe("Day");
  const next = now + 24 * 3600_000;
  const nextBars = [{ ...bars[0], time: next / 1000 }];
  expect(chartChangeReference(nextBars, [{ ...snapshot, timestamp: new Date(next).toISOString() }], next, undefined, cache, "AAA").base).toBe(4);
});
