import { expect, test } from "bun:test";
import { moverProvenanceLabel, moverRefreshLabel, moverVolumeKnown } from "./moverProvenance";
import type { Mover, TodayMovers } from "./useMovers";

const row: Mover = { symbol: "AAA", price: 3, changePct: 10, volume: 1, session: "regular", provenance: {
  priceSource: "latest_trade", latestTradeAt: "2026-10-09T19:00:00Z", tradeTimeReason: null,
  snapshotReceivedAt: "2026-10-09T19:01:00Z", volumeRetrievedAt: null,
  dailyBarDate: "2026-10-09", dailyBarRelation: "current_day",
} };
const now = Date.parse("2026-10-11T19:00:00Z");

test("unknown or unrecognized volume provenance is never rendered as known zero", () => {
  expect(moverVolumeKnown({ ...row, volumeSource: "unknown", volume: 0 })).toBe(false);
  expect(moverVolumeKnown({ ...row, volumeSource: undefined })).toBe(false);
  expect(moverVolumeKnown({ ...row, volumeSource: "missing" as Mover["volumeSource"] })).toBe(false);
  expect(moverVolumeKnown({ ...row, volumeSource: "snapshot_daily_bar_current", volume: 0 })).toBe(true);
  expect(moverVolumeKnown({ ...row, volumeSource: null }, true)).toBe(true);
});
test("first-load failure does not invent a previous snapshot", () => {
  const today: TodayMovers = { gainers: [], mostActive: [], peakGainers: [], peakMostActive: [], currentAvailable: true, lastUpdated: null, refreshStatus: "failed" };
  expect(moverRefreshLabel(today)?.label).toBe("Refresh failed · no snapshot yet");
});

test("Sunday retains the actual Friday trade time without session judgement", () => {
  const result = moverProvenanceLabel(row, now);
  expect(result.label).toContain("Oct 9, 2026");
  expect(result.label).toContain("15:00");
  expect(result.label).not.toMatch(/invalid|stale|live/i);
  expect(result.title).toContain("does not imply a new trade");
});
test("legacy, absent and malformed timestamps are explicit unknowns", () => {
  expect(moverProvenanceLabel({ ...row, provenance: undefined }, now).label).toBe("Trade time unavailable");
  for (const latestTradeAt of [null, "garbage"]) {
    expect(moverProvenanceLabel({ ...row, provenance: { ...row.provenance!, latestTradeAt } }, now).label).toBe("Trade time unavailable");
  }
});
test("future timestamp cannot become a fresh-trade claim", () => {
  expect(moverProvenanceLabel(row, Date.parse("2026-10-09T18:59:59Z")).label).toBe("Trade time ahead");
});
test("daily-bar price remains labelled even when a trade timestamp is missing", () => {
  const result = moverProvenanceLabel({ ...row, provenance: { ...row.provenance!, priceSource: "daily_bar", latestTradeAt: null } }, now);
  expect(result.label).toBe("Daily bar · 2026-10-09");
});
test("failed attempts do not disguise the previous successful snapshot", () => {
  const today: TodayMovers = { gainers: [row], mostActive: [row], peakGainers: [], peakMostActive: [], currentAvailable: true, lastUpdated: new Date("2026-10-09T19:01:00Z"), refreshStatus: "failed" };
  expect(moverRefreshLabel(today)?.label).toBe("Refresh failed · previous snapshot");
  expect(today.lastUpdated?.toISOString()).toBe("2026-10-09T19:01:00.000Z");
});
