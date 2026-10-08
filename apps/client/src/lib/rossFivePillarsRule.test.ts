// Runs the Ross five-pillar rule tests inside the suite that actually runs.
//
// The nine rule tests live beside the rule, in
// packages/shared-types/src/rossFivePillars.test.ts, but packages/shared-types
// has no test script and nothing invokes one: CI, the root `test` script and
// the web image builder (apps/client/Dockerfile) all run `bun test` from
// apps/client only. So the rule the badge renders from shipped with passing
// tests that no gate ever executed -- a threshold could change and every
// check would stay green.
//
// Importing the file registers its `describe` block in this run. The tests
// stay where they are, next to the code they cover, rather than being copied
// here: a copy would drift, and mobile uses the same rule. Same reasoning as
// attentionParity.test.ts reaching into apps/mobile -- the assertion lives in
// the suite that runs. `tsc -b` typechecks the imported file too, because
// tests are deliberately inside this project (see tsconfig.app.json).

import "../../../../packages/shared-types/src/rossFivePillars.test";
import { describe, expect, test } from "bun:test";
import type { CatalystUpdate, FunnelSignal, RossPillarName } from "@stockspotter/shared-types";
import { assessRossFivePillars } from "@stockspotter/shared-types";

// ---------------------------------------------------------------------------
// Exact-boundary tests for every numeric threshold in the rule.
//
// The nine imported tests exercise each pillar but do not pin each threshold
// from both sides. A mutation check of the rule showed it: changing relative
// volume `>= 5` to `>= 4`, or daily gain `>= 10` to `>= 9`, left all nine
// green, because the only assertions near those numbers used values that pass
// either way (5 and 10 themselves, or 2 and 5 far below). A badge that reads
// "confirmed" on 4x volume is exactly the silent drift these tests exist to
// stop, and the rule is shared with mobile, so it would drift there too.
//
// Each case below sits ON a threshold or immediately beside it, and each
// threshold has a case on both sides, so moving a number or flipping an
// inclusive comparison to an exclusive one (or back) fails at least one of
// them. The expected values restate the rule as written; nothing here changes
// what the rule does.
//
// They live in this file rather than beside the rule on purpose. A change
// under packages/shared-types/** makes the mobile advisory check applicable to
// the pull request, and that check is unrelated to a test-only change. They
// import the rule through the package entry point, i.e. the same export the
// badge component uses.
// ---------------------------------------------------------------------------

const NOW = Date.parse("2026-10-05T14:00:00.000Z");
const at = (offsetMs: number) => new Date(NOW + offsetMs).toISOString();
const SECOND = 1000;
const MINUTE = 60 * SECOND;
const DAY = 24 * 60 * MINUTE;

// All five pillars pass comfortably, so a case that moves one field to a
// boundary changes exactly one pillar and nothing else.
const passing: FunnelSignal = {
  type: "funnel_signal", symbol: "EDGE", timestamp: at(0),
  price: 5, gapPct: 12, sessionVolume: 100_000, floatShares: 5_000_000,
  relativeVolume: 6, priceOk: true, floatOk: true, relVolOk: true, gapOk: true, passed: true,
};
const news: CatalystUpdate = {
  type: "catalyst_update", symbol: "EDGE", timestamp: at(0),
  catalystTags: ["earnings"], headlineCount: 1, mostRecentHeadline: "Positive earnings",
  mostRecentPublishedAt: at(-MINUTE),
};
// A recent lookup that came back empty: the only shape reported as "none-reported".
const noNews: CatalystUpdate = { ...news, catalystTags: [], headlineCount: 0, mostRecentHeadline: null, mostRecentPublishedAt: undefined };

/** Whether `pillar` passed, read off the assessment's own `remaining` list. */
function pillarPasses(pillar: RossPillarName, funnel: Partial<FunnelSignal>, catalyst: CatalystUpdate | undefined = news): boolean {
  const result = assessRossFivePillars({ ...passing, ...funnel }, catalyst, NOW);
  // Four other pillars pass in every case here, so the badge is always shown.
  if (!result) throw new Error("expected an assessment");
  expect(result.passed).toBe(5 - result.remaining.length);
  return !result.remaining.includes(pillar);
}

const catalystStatus = (catalyst: CatalystUpdate) => assessRossFivePillars(passing, catalyst, NOW)?.catalystStatus;

describe("assessRossFivePillars numeric boundaries", () => {
  test("price is inclusive at both ends: $1.00 and $20.00 pass, $0.99 and $20.01 do not", () => {
    expect(pillarPasses("Price", { price: 1 })).toBe(true);
    expect(pillarPasses("Price", { price: 0.99 })).toBe(false);
    expect(pillarPasses("Price", { price: 20 })).toBe(true);
    expect(pillarPasses("Price", { price: 20.01 })).toBe(false);
  });

  test("relative volume passes at exactly 5x and fails just below it", () => {
    expect(pillarPasses("Relative volume", { relativeVolume: 5 })).toBe(true);
    expect(pillarPasses("Relative volume", { relativeVolume: 4.99 })).toBe(false);
    // 4.99 already catches a threshold lowered to 4; this states it directly
    // so the failure names the number.
    expect(pillarPasses("Relative volume", { relativeVolume: 4 })).toBe(false);
  });

  test("daily gain passes at exactly 10% and fails just below it", () => {
    expect(pillarPasses("Daily gain", { gapPct: 10 })).toBe(true);
    expect(pillarPasses("Daily gain", { gapPct: 9.99 })).toBe(false);
    expect(pillarPasses("Daily gain", { gapPct: 9 })).toBe(false);
  });

  test("float is exclusive at both ends: 1 and 9,999,999 pass, 0 and 10,000,000 do not", () => {
    expect(pillarPasses("Float", { floatShares: 1 })).toBe(true);
    expect(pillarPasses("Float", { floatShares: 0 })).toBe(false);
    expect(pillarPasses("Float", { floatShares: 9_999_999 })).toBe(true);
    expect(pillarPasses("Float", { floatShares: 10_000_000 })).toBe(false);
  });

  test("a pillar sitting on its boundary still counts toward confirmed, and one step off does not", () => {
    const onEveryBoundary = { price: 20, relativeVolume: 5, gapPct: 10, floatShares: 9_999_999 };
    expect(assessRossFivePillars({ ...passing, ...onEveryBoundary }, news, NOW))
      .toMatchObject({ state: "confirmed", passed: 5, remaining: [] });
    expect(assessRossFivePillars({ ...passing, ...onEveryBoundary, relativeVolume: 4.99 }, news, NOW))
      .toMatchObject({ state: "forming", passed: 4, remaining: ["Relative volume"] });
    expect(assessRossFivePillars({ ...passing, ...onEveryBoundary, gapPct: 9.99 }, news, NOW))
      .toMatchObject({ state: "forming", passed: 4, remaining: ["Daily gain"] });
  });

  test("the badge needs two passing pillars: two show it, one does not, and only five confirm", () => {
    // Price and float pass at first; the others are already off, then float goes too.
    const two = { relativeVolume: 4.99, gapPct: 9.99 };
    expect(assessRossFivePillars({ ...passing, ...two }, undefined, NOW)).toMatchObject({ state: "forming", passed: 2 });
    expect(assessRossFivePillars({ ...passing, ...two, floatShares: 10_000_000 }, undefined, NOW)).toBeNull();
    expect(assessRossFivePillars(passing, undefined, NOW)).toMatchObject({ state: "forming", passed: 4 });
    expect(assessRossFivePillars(passing, news, NOW)).toMatchObject({ state: "confirmed", passed: 5 });
  });

  test("a scanner reading is used up to exactly two minutes old and not one millisecond older", () => {
    expect(assessRossFivePillars({ ...passing, timestamp: at(-2 * MINUTE) }, news, NOW)).not.toBeNull();
    expect(assessRossFivePillars({ ...passing, timestamp: at(-2 * MINUTE - 1) }, news, NOW)).toBeNull();
  });

  test("a scanner reading may be up to exactly 30 seconds ahead of the client clock and no further", () => {
    expect(assessRossFivePillars({ ...passing, timestamp: at(30 * SECOND) }, news, NOW)).not.toBeNull();
    expect(assessRossFivePillars({ ...passing, timestamp: at(30 * SECOND + 1) }, news, NOW)).toBeNull();
  });

  test("a catalyst lookup is usable up to exactly 30 seconds ahead of the client clock", () => {
    expect(pillarPasses("News catalyst", {}, { ...news, timestamp: at(30 * SECOND) })).toBe(true);
    expect(pillarPasses("News catalyst", {}, { ...news, timestamp: at(30 * SECOND + 1) })).toBe(false);
    // The lookup's time also decides whether "no headlines" may be reported.
    expect(catalystStatus({ ...noNews, timestamp: at(30 * SECOND) })).toBe("none-reported");
    expect(catalystStatus({ ...noNews, timestamp: at(30 * SECOND + 1) })).toBe("unverified");
  });

  test("a catalyst lookup is usable up to exactly 24 hours old", () => {
    expect(pillarPasses("News catalyst", {}, { ...news, timestamp: at(-DAY) })).toBe(true);
    expect(pillarPasses("News catalyst", {}, { ...news, timestamp: at(-DAY - 1) })).toBe(false);
    expect(catalystStatus({ ...noNews, timestamp: at(-DAY) })).toBe("none-reported");
    expect(catalystStatus({ ...noNews, timestamp: at(-DAY - 1) })).toBe("unverified");
  });

  test("news counts when published exactly now or exactly 24 hours ago, and not one millisecond outside", () => {
    expect(pillarPasses("News catalyst", {}, { ...news, mostRecentPublishedAt: at(0) })).toBe(true);
    expect(pillarPasses("News catalyst", {}, { ...news, mostRecentPublishedAt: at(1) })).toBe(false);
    expect(pillarPasses("News catalyst", {}, { ...news, mostRecentPublishedAt: at(-DAY) })).toBe(true);
    expect(pillarPasses("News catalyst", {}, { ...news, mostRecentPublishedAt: at(-DAY - 1) })).toBe(false);
  });

  test("news needs at least one headline and at least one catalyst tag", () => {
    expect(pillarPasses("News catalyst", {}, { ...news, headlineCount: 1, catalystTags: ["earnings"] })).toBe(true);
    expect(pillarPasses("News catalyst", {}, { ...news, headlineCount: 0 })).toBe(false);
    expect(pillarPasses("News catalyst", {}, { ...news, catalystTags: [] })).toBe(false);
  });

  test("none-reported means exactly zero headlines, zero tags and no headline text", () => {
    expect(catalystStatus(noNews)).toBe("none-reported");
    // One step off zero on either count is contradictory data, not "no news".
    expect(catalystStatus({ ...noNews, headlineCount: 1 })).toBe("unverified");
    expect(catalystStatus({ ...noNews, catalystTags: ["earnings"] })).toBe("unverified");
    expect(catalystStatus({ ...noNews, mostRecentHeadline: "Positive earnings" })).toBe("unverified");
  });
});
