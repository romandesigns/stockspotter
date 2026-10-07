import { describe, expect, test } from "bun:test";
import type { CatalystUpdate, FunnelSignal } from "./index";
import { assessRossFivePillars } from "./rossFivePillars";

const funnel: FunnelSignal = {
  type: "funnel_signal", symbol: "TEST", timestamp: "2026-10-05T14:00:00Z",
  price: 5, gapPct: 12, sessionVolume: 100_000, floatShares: 5_000_000,
  relativeVolume: 6, priceOk: true, floatOk: true, relVolOk: true, gapOk: true, passed: true,
};
const catalyst: CatalystUpdate = {
  type: "catalyst_update", symbol: "TEST", timestamp: "2026-10-05T14:00:00Z",
  catalystTags: ["earnings"], headlineCount: 1, mostRecentHeadline: "Positive earnings",
  mostRecentPublishedAt: "2026-10-05T13:59:00Z",
};
const now = Date.parse("2026-10-05T14:00:00Z");

describe("assessRossFivePillars", () => {
  test("confirms only when all five measured criteria pass", () => {
    expect(assessRossFivePillars(funnel, catalyst, now)).toMatchObject({ state: "confirmed", passed: 5, catalystStatus: "verified", remaining: [] });
  });

  test("shows four measured pillars as a candidate when a fresh lookup reports no catalyst", () => {
    const noNews: CatalystUpdate = { ...catalyst, catalystTags: [], headlineCount: 0, mostRecentHeadline: null, mostRecentPublishedAt: undefined };
    expect(assessRossFivePillars(funnel, noNews, now)).toMatchObject({
      state: "forming", passed: 4, catalystStatus: "none-reported", remaining: ["News catalyst"],
    });
  });

  test("distinguishes unavailable or stale catalyst data from an explicit no-news result", () => {
    expect(assessRossFivePillars(funnel, undefined, now)?.catalystStatus).toBe("unverified");
    expect(assessRossFivePillars(funnel, { ...catalyst, timestamp: "2026-10-04T13:59:00Z" }, now)?.catalystStatus).toBe("unverified");
  });

  test("marks a candidate forming at two verified pillars while listing what's missing", () => {
    const result = assessRossFivePillars({ ...funnel, gapPct: 5, relativeVolume: 2 }, undefined, now);
    expect(result).toMatchObject({ state: "forming", passed: 2, remaining: ["Relative volume", "Daily gain", "News catalyst"] });
  });

  test("does not label a symbol with fewer than two verified pillars", () => {
    expect(assessRossFivePillars({ ...funnel, price: 25, gapPct: 3, floatShares: null }, undefined, now)).toBeNull();
  });

  test("unknown measurements never pass", () => {
    expect(assessRossFivePillars({ ...funnel, relativeVolume: null, floatShares: undefined }, catalyst, now))
      .toMatchObject({ state: "forming", passed: 3, remaining: ["Relative volume", "Float"] });
  });

  test("enforces source thresholds exactly at boundaries", () => {
    expect(assessRossFivePillars({ ...funnel, price: 1, floatShares: 9_999_999, relativeVolume: 5, gapPct: 10 }, catalyst, now)?.state).toBe("confirmed");
    expect(assessRossFivePillars({ ...funnel, price: 0.99 }, catalyst, now)?.remaining).toContain("Price");
    expect(assessRossFivePillars({ ...funnel, floatShares: 10_000_000 }, catalyst, now)?.remaining).toContain("Float");
    expect(assessRossFivePillars({ ...funnel, floatShares: 0 }, catalyst, now)?.remaining).toContain("Float");
  });

  test("requires identified, timestamped, non-future news no older than 24 hours", () => {
    expect(assessRossFivePillars(funnel, { ...catalyst, mostRecentPublishedAt: undefined }, now)?.remaining).toContain("News catalyst");
    expect(assessRossFivePillars(funnel, { ...catalyst, mostRecentPublishedAt: "2026-10-04T13:59:00Z" }, now)?.remaining).toContain("News catalyst");
    expect(assessRossFivePillars(funnel, { ...catalyst, mostRecentPublishedAt: "2026-10-05T14:01:00Z" }, now)?.remaining).toContain("News catalyst");
  });

  test("does not badge an expired or future scanner reading", () => {
    expect(assessRossFivePillars({ ...funnel, timestamp: "2026-10-05T13:57:59Z" }, catalyst, now)).toBeNull();
    expect(assessRossFivePillars({ ...funnel, timestamp: "2026-10-05T14:00:31Z" }, catalyst, now)).toBeNull();
  });
});
