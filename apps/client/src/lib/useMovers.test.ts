import { expect, test } from "bun:test";
import { decodeTodayMovers, type Mover } from "./useMovers";
const peak: Mover = { symbol: "VEEA", price: 8.8, changePct: 127.4, volume: 1000, session: "regular" };
const current: Mover = { ...peak, price: 6.46, changePct: 66.9 };
const at = "2026-10-09T14:20:00Z";

test("legacy or partial responses preserve peak rows without calling them current", () => {
  for (const response of [
    { gainers: [peak], mostActive: [peak] },
    { gainers: [peak], mostActive: [peak], currentGainers: [current] },
  ]) {
    const decoded = decodeTodayMovers(response, Date.parse(at));
    expect(decoded.currentAvailable).toBe(false);
    expect(decoded.gainers).toEqual([]);
    expect(decoded.mostActive).toEqual([]);
    expect(decoded.peakGainers).toEqual([peak]);
    expect(decoded.peakMostActive).toEqual([peak]);
    expect(decoded.lastUpdated?.getTime()).toBe(Date.parse(at));
  }
});

test("complete current arrays retain server observation age even when empty", () => {
  const decoded = decodeTodayMovers({ gainers: [peak], mostActive: [peak], currentGainers: [current], currentMostActive: [current], observedAt: at }, Date.parse(at) + 60_000);
  expect(decoded.currentAvailable).toBe(true);
  expect(decoded.gainers).toEqual([current]);
  expect(decoded.mostActive).toEqual([current]);
  expect(decoded.lastUpdated?.getTime()).toBe(Date.parse(at));
  expect(decodeTodayMovers({ gainers: [peak], mostActive: [peak], currentGainers: [], currentMostActive: [] }).currentAvailable).toBe(true);
});

