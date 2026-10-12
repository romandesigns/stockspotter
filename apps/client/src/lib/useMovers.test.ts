import { expect, test } from "bun:test";
import { decodeTodayMovers, type Mover } from "./useMovers";
const peak: Mover = { symbol: "VEEA", price: 8.8, changePct: 127.4, volume: 1000, session: "regular" };
const current: Mover = { ...peak, price: 6.46, changePct: 66.9 };
const at = "2026-10-09T14:20:00Z";

test("a failed initial or partial modern response never advances successful age", () => {
  const failed = { gainers: [], mostActive: [], refreshStatus: "failed" as const, lastAttemptAt: "2026-10-09T15:20:00Z", lastErrorAt: "2026-10-09T15:20:01Z" };
  expect(decodeTodayMovers(failed, Date.parse(failed.lastAttemptAt)).lastUpdated).toBeNull();
  const previous = decodeTodayMovers({ gainers: [peak], mostActive: [peak], currentGainers: [current], currentMostActive: [current], observedAt: at });
  for (const observedAt of [undefined, "bad-time"]) {
    const decoded = decodeTodayMovers({ ...failed, currentGainers: [], currentMostActive: [], observedAt }, Date.parse(failed.lastAttemptAt), previous);
    expect(decoded.gainers).toEqual([current]);
    expect(decoded.peakGainers).toEqual([peak]);
    expect(decoded.lastUpdated?.toISOString()).toBe(previous.lastUpdated?.toISOString());
    expect(decoded.lastAttemptAt).toBe(failed.lastAttemptAt);
    expect(decoded.refreshStatus).toBe("failed");
  }
});
test("modern missing observation uses actual success time or unknown, never poll time", () => {
  const response = { gainers: [], mostActive: [], refreshStatus: "success" as const };
  expect(decodeTodayMovers(response).lastUpdated).toBeNull();
  expect(decodeTodayMovers({ ...response, lastSuccessAt: at }).lastUpdated?.toISOString()).toBe(new Date(at).toISOString());
});

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

