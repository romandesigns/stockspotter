// Web/mobile parity for the Ignition user-attention rule.
//
// Ported by hand from ee47759 (claude/ignition-green-only-ux) and extended
// for the $25 ceiling. It deliberately reaches across into apps/mobile's real
// derive.ts and runs it, rather than asserting parity by inspection: the
// failure it exists to catch -- mobile listing a row web hides, or the other
// way round -- is invisible to any test that only ever imports one side.
//
// It can do this because mobile's derive.ts and its imports are pure
// TypeScript with no React Native runtime in the graph. apps/mobile has no
// test runner of its own, so the assertion lives in the suite that runs.

import { describe, expect, test } from "bun:test";
import type { CatalystUpdate, ConsolidationEvent, IgnitionEvent, MomentumUpdate } from "@stockspotter/shared-types";
import { qualifiesForUserAttention } from "@stockspotter/shared-types";
import { deriveIgnitionFeed } from "./derive";
import { ignitionPanelSubtitle, ignitionPanelView } from "./ignitionRow";
import { buildAlerts } from "../../../mobile/src/derive";

const ignition = (kind: IgnitionEvent["kind"], symbol: string, iso: string, price = 2.5): IgnitionEvent => ({
  type: "ignition_event", symbol, timestamp: iso, price, kind,
});
const consolidation = (kind: ConsolidationEvent["kind"], symbol: string, iso: string, price = 2.5): ConsolidationEvent => ({
  type: "consolidation_event", symbol, timestamp: iso, price, kind, strategy: "micropullback",
});

// One mixed stream, the shape a regular session produces: mostly
// bookkeeping, some confirmations, and confirmations on both sides of $25.
const stream = [
  ignition("candidate_opened", "AAA", "2026-09-21T14:10:00Z"),
  ignition("follow_through_confirmed", "BBB", "2026-09-21T14:09:00Z", 24.99),
  ignition("follow_through_confirmed", "CEIL", "2026-09-21T14:08:00Z", 25.0),
  ignition("follow_through_confirmed", "OVER", "2026-09-21T14:07:00Z", 25.01),
  ignition("follow_through_rejected", "CCC", "2026-09-21T14:06:00Z"),
  consolidation("surge_detected", "DDD", "2026-09-21T14:05:00Z"),
  consolidation("consolidation_confirmed", "EEE", "2026-09-21T14:04:00Z"),
  consolidation("entry_triggered", "FFF", "2026-09-21T14:03:00Z", 3),
  consolidation("entry_triggered", "PRCY", "2026-09-21T14:02:00Z", 60),
  ignition("follow_through_confirmed", "NANP", "2026-09-21T14:01:00Z", Number.NaN),
];

const QUALIFYING = ["BBB", "CEIL", "FFF"];

describe("web and mobile agree on what reaches the user", () => {
  test("web's panel list shows exactly the qualifying symbols", () => {
    const view = ignitionPanelView(deriveIgnitionFeed(stream), false);
    expect(view.visible.map((i) => i.event.symbol)).toEqual(QUALIFYING);
  });

  test("mobile's alert list shows exactly the same symbols", () => {
    const rows = buildAlerts(stream, new Map<string, CatalystUpdate>(), new Map<string, MomentumUpdate>());
    expect(rows.map((r) => r.symbol)).toEqual(QUALIFYING);
  });

  test("notification eligibility (both clients' call-site filter) picks the same symbols", () => {
    expect(stream.filter((e) => qualifiesForUserAttention(e)).map((e) => e.symbol)).toEqual(QUALIFYING);
  });

  test("mobile still surfaces catalysts, which the ignition rule must not filter", () => {
    const catalysts = new Map<string, CatalystUpdate>([
      ["ZZZ", { type: "catalyst_update", symbol: "ZZZ", timestamp: "2026-09-21T14:11:00Z", catalystTags: ["earnings"], headlineCount: 3, mostRecentHeadline: "Q3 beat" } as CatalystUpdate],
    ]);
    const rows = buildAlerts(stream, catalysts, new Map<string, MomentumUpdate>());
    expect(rows.map((r) => r.symbol)).toContain("ZZZ");
    expect(rows.filter((r) => r.label === "Catalyst").length).toBe(1);
  });

  test("no non-qualifying detector event survives on either platform", () => {
    const mobile = new Set(buildAlerts(stream, new Map(), new Map()).map((r) => r.symbol));
    const web = new Set(ignitionPanelView(deriveIgnitionFeed(stream), false).visible.map((i) => i.event.symbol));
    for (const e of stream) {
      if (!qualifiesForUserAttention(e)) {
        expect(mobile.has(e.symbol)).toBe(false);
        expect(web.has(e.symbol)).toBe(false);
      }
    }
  });
});

describe("panel view accounting", () => {
  test("All mode shows the raw feed and hides nothing", () => {
    const view = ignitionPanelView(deriveIgnitionFeed(stream), true);
    expect(view.visible).toHaveLength(stream.length);
    expect(view.hiddenUnconfirmed + view.hiddenAboveCeiling + view.hiddenUnpriced).toBe(0);
    expect(ignitionPanelSubtitle(view, true)).toBe("all detector events (diagnostic)");
  });

  test("hidden rows are split by cause, and add up", () => {
    const view = ignitionPanelView(deriveIgnitionFeed(stream), false);
    // AAA, CCC, DDD, EEE are not confirmed; OVER and PRCY are green but
    // above $25; NANP is green with an unusable price.
    expect(view.hiddenUnconfirmed).toBe(4);
    expect(view.hiddenAboveCeiling).toBe(2);
    expect(view.hiddenUnpriced).toBe(1);
    expect(view.visible.length + view.hiddenUnconfirmed + view.hiddenAboveCeiling + view.hiddenUnpriced).toBe(stream.length);
    expect(ignitionPanelSubtitle(view, false)).toBe("confirmed + breakout entries ≤ $25 · hidden: 2 above $25, 1 unpriced, 4 unconfirmed");
  });

  test("subtitle names nothing hidden when nothing is", () => {
    const view = ignitionPanelView(deriveIgnitionFeed([ignition("follow_through_confirmed", "OK", "2026-09-21T14:00:00Z", 5)]), false);
    expect(ignitionPanelSubtitle(view, false)).toBe("confirmed + breakout entries ≤ $25");
  });
});
