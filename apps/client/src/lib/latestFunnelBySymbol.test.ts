import { describe, expect, test } from "bun:test";
import type { FunnelSignal } from "@stockspotter/shared-types";
import { latestFunnelBySymbol } from "./latestFunnelBySymbol";

function signal(symbol: string, timestamp: string, price: number): FunnelSignal {
  return {
    type: "funnel_signal",
    symbol,
    timestamp,
    price,
    gapPct: 10,
    sessionVolume: 100_000,
    floatShares: 5_000_000,
    relativeVolume: 5,
    priceOk: true,
    floatOk: true,
    relVolOk: true,
    gapOk: true,
    passed: true,
  };
}

describe("latestFunnelBySymbol", () => {
  test("keeps the first (newest) signal when the feed is newest-first", () => {
    const newest = signal("ABC", "2026-10-06T14:02:00Z", 12);
    const older = signal("ABC", "2026-10-06T14:01:00Z", 11);
    const other = signal("XYZ", "2026-10-06T14:00:00Z", 3);

    const bySymbol = latestFunnelBySymbol([newest, older, other]);

    expect(bySymbol.get("ABC")).toBe(newest);
    expect(bySymbol.get("XYZ")).toBe(other);
    expect(bySymbol.size).toBe(2);
  });
});
