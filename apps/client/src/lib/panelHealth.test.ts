// Tests for the two "this panel can't answer" decisions.
//
// These matter more than their size suggests: getting them wrong doesn't
// crash anything, it just makes an outage look like a quiet market and a
// premarket gapper look calm. Both failure modes are silent, which is
// exactly the kind of thing worth pinning in a test.

import { describe, expect, test } from "bun:test";
import type { FunnelHealth } from "@stockspotter/shared-types";
import { funnelBlindReason, isLuldInEffect, isOutsideLuldHours } from "./panelHealth";

function health(extra: Partial<FunnelHealth> = {}): FunnelHealth {
  return {
    type: "funnel_health",
    timestamp: "2026-09-06T14:00:00Z",
    floatBudgetRemaining: 200,
    floatBudget: 240,
    starvedCandidates: 0,
    apiKeyMissing: false,
    ...extra,
  };
}

describe("funnelBlindReason", () => {
  test("a healthy scan has no reason to report", () => {
    expect(funnelBlindReason(health())).toBeNull();
  });

  test("no health data yet is not a warning", () => {
    // Before the first universe rescan lands. An empty panel then is
    // genuinely "nothing yet", not a fault.
    expect(funnelBlindReason(null)).toBeNull();
    expect(funnelBlindReason(undefined)).toBeNull();
  });

  test("a missing API key is reported as its own distinct cause", () => {
    const reason = funnelBlindReason(health({ apiKeyMissing: true }));
    expect(reason).toContain("FMP_API_KEY");
  });

  test("a missing API key takes priority over a starved count", () => {
    // With no key configured, nothing was ever going to be checked --
    // reporting a budget figure would point at the wrong fix.
    const reason = funnelBlindReason(health({ apiKeyMissing: true, starvedCandidates: 5 }));
    expect(reason).toContain("FMP_API_KEY");
    expect(reason).not.toContain("budget spent");
  });

  test("starved candidates report the count and the remaining budget", () => {
    const reason = funnelBlindReason(health({ starvedCandidates: 7, floatBudgetRemaining: 0, floatBudget: 240 }));
    expect(reason).toContain("7 candidates");
    expect(reason).toContain("0/240");
  });

  test("a single starved candidate is not pluralized", () => {
    expect(funnelBlindReason(health({ starvedCandidates: 1 }))).toContain("1 candidate cleared");
  });

  test("an exhausted budget with nothing starved is not a warning", () => {
    // Spending the whole budget is only a problem if something actually
    // needed checking and couldn't be. A quiet afternoon that used it up
    // earlier is fine.
    expect(funnelBlindReason(health({ floatBudgetRemaining: 0, starvedCandidates: 0 }))).toBeNull();
  });
});

describe("isLuldInEffect", () => {
  test("explicit false means bands are not in force", () => {
    expect(isLuldInEffect({ luldInEffect: false })).toBe(false);
  });

  test("explicit true means they are", () => {
    expect(isLuldInEffect({ luldInEffect: true })).toBe(true);
  });

  test("a missing field is treated as in effect", () => {
    // Backward compatibility with a ws-server that predates the field.
    // Defaulting the other way would dim every card against an older
    // server, which is worse than the pre-existing behavior.
    expect(isLuldInEffect({})).toBe(true);
    expect(isLuldInEffect({ luldInEffect: undefined })).toBe(true);
  });
});

describe("isOutsideLuldHours", () => {
  test("true only when every reading is outside", () => {
    expect(isOutsideLuldHours([{ luldInEffect: false }, { luldInEffect: false }])).toBe(true);
  });

  test("false if any reading is in effect", () => {
    // Mixed state is possible right at 9:30 as readings roll over.
    expect(isOutsideLuldHours([{ luldInEffect: false }, { luldInEffect: true }])).toBe(false);
  });

  test("an empty panel is 'no data', not 'outside hours'", () => {
    expect(isOutsideLuldHours([])).toBe(false);
  });
});
