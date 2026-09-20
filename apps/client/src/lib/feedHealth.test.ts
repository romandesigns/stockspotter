import { test, expect } from "bun:test";
import { recordGap, resolveChartFreshness, seriesSpansGap, type FeedGap } from "./feedHealth";

const LAG: FeedGap = { at: "2026-09-21T14:00:00.000Z", reason: "stream_lagged", missedEvents: 6 };
const LATER: FeedGap = { at: "2026-09-21T14:05:00.000Z", reason: "reconnect", missedEvents: null };
const secondsAt = (iso: string) => Date.parse(iso) / 1000;

const base = {
  transport: "open" as "connecting" | "open" | "closed" | "stale",
  gap: null as FeedGap | null,
  earliestBarTimeSeconds: null as number | null,
};

test("recordGap keeps the earliest unresolved gap, not the most recent", () => {
  expect(recordGap(null, LAG)).toEqual(LAG);
  // A later gap must not shrink the suspect window.
  expect(recordGap(LAG, LATER)).toEqual(LAG);
  // An earlier one arriving second does widen it.
  expect(recordGap(LATER, LAG)).toEqual(LAG);
  // Unparseable input never displaces a good record.
  expect(recordGap(LAG, { ...LATER, at: "not-a-date" })).toEqual(LAG);
  expect(recordGap({ ...LAG, at: "not-a-date" }, LATER)).toEqual(LATER);
});

test("a series spans the gap only while it still straddles the lost instant", () => {
  expect(seriesSpansGap(secondsAt("2026-09-21T13:50:00.000Z"), LAG)).toBe(true);
  // Exactly at the gap instant still counts: that bar may be incomplete.
  expect(seriesSpansGap(secondsAt("2026-09-21T14:00:00.000Z"), LAG)).toBe(true);
  // Window has rolled past the discontinuity -- honest again, no re-fetch.
  expect(seriesSpansGap(secondsAt("2026-09-21T14:01:00.000Z"), LAG)).toBe(false);
  // Nothing drawn, nothing to mislead anyone with.
  expect(seriesSpansGap(null, LAG)).toBe(false);
  expect(seriesSpansGap(secondsAt("2026-09-21T13:00:00.000Z"), null)).toBe(false);
});

test("fresh data does NOT clear an outstanding gap", () => {
  // The exact regression this module exists for: before it, one message
  // with a current timestamp flipped the UI back to live.
  const gapped = {
    ...base,
    gap: LAG,
    earliestBarTimeSeconds: secondsAt("2026-09-21T13:30:00.000Z"),
  };
  expect(resolveChartFreshness(gapped)).toBe("gap");
  // Even while the transport reports a perfectly healthy "open".
  expect(resolveChartFreshness({ ...gapped, transport: "open" })).toBe("gap");
});

test("freshness precedence: transport > gap > stale > live", () => {
  const spanning = secondsAt("2026-09-21T13:30:00.000Z");
  expect(resolveChartFreshness({ ...base, transport: "connecting" })).toBe("connecting");
  // Disconnected outranks a gap: we cannot yet know what else was lost.
  expect(resolveChartFreshness({ ...base, transport: "closed", gap: LAG, earliestBarTimeSeconds: spanning })).toBe("reconnecting");
  // A gap outranks stale -- stale self-heals, a gap needs action.
  expect(resolveChartFreshness({ ...base, transport: "stale", gap: LAG, earliestBarTimeSeconds: spanning })).toBe("gap");
  expect(resolveChartFreshness({ ...base, transport: "stale" })).toBe("stale");
  expect(resolveChartFreshness(base)).toBe("live");
});

test("staleness is taken from the transport, never recomputed here", () => {
  // The 90s market-data threshold lives in useRealtimeFeed. This module
  // consumes its verdict so the two cannot drift apart.
  expect(resolveChartFreshness({ ...base, transport: "stale" })).toBe("stale");
  expect(resolveChartFreshness({ ...base, transport: "open" })).toBe("live");
});

test("a resolved gap lets the chart return to live without a reconnect", () => {
  // Window rolled forward past the gap: every bar arrived after it.
  expect(
    resolveChartFreshness({ ...base, gap: LAG, earliestBarTimeSeconds: secondsAt("2026-09-21T14:02:00.000Z") }),
  ).toBe("live");
});
