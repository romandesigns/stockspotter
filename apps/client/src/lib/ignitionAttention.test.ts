import { test, expect } from "bun:test";
import { isIgnitionFamilyEvent, qualifiesForIgnitionAttention } from "@stockspotter/shared-types";
import { rowClass } from "./ignitionRow";
import { deriveIgnitionFeed } from "./derive";
import type { DetectionEvent } from "./useRealtimeFeed";

const ign = (kind: string, symbol = "ONCO", ts = "2026-09-21T15:00:00.000Z") =>
  ({ type: "ignition_event", symbol, timestamp: ts, price: 2.5, kind }) as unknown as DetectionEvent;
const cons = (kind: string, strategy = "consolidation_breakout", symbol = "SWVL") =>
  ({ type: "consolidation_event", symbol, timestamp: "2026-09-21T15:00:00.000Z", price: 3, kind, strategy }) as unknown as DetectionEvent;
const halt = () =>
  ({ type: "halt_warning", symbol: "VEEE", timestamp: "2026-09-21T15:00:00.000Z", level: "red", proximityRatio: 0.9 }) as unknown as DetectionEvent;

/** What the panel renders by default: the feed, filtered by the predicate. */
const visible = (evs: DetectionEvent[]) =>
  deriveIgnitionFeed(evs).filter((it) => qualifiesForIgnitionAttention(it.event));

// --- 1 / 16 -----------------------------------------------------------------
test("green ignition is visible, and green-only is the default", () => {
  const evs = [ign("follow_through_confirmed"), ign("candidate_opened")];
  expect(deriveIgnitionFeed(evs)).toHaveLength(2); // nothing destroyed
  expect(visible(evs)).toHaveLength(1);
  expect(visible(evs)[0].event.kind).toBe("follow_through_confirmed");
});

// --- 2 / 10 -----------------------------------------------------------------
test("notification eligibility uses the same predicate as visibility", () => {
  // Both clients feed useIgnitionAlerts from ignitionConfirmedEvents, which
  // useRealtimeFeed populates only for follow_through_confirmed -- which is
  // exactly this predicate. Asserted on the predicate itself.
  expect(qualifiesForIgnitionAttention({ type: "ignition_event", kind: "follow_through_confirmed" })).toBe(true);
  expect(qualifiesForIgnitionAttention({ type: "ignition_event", kind: "candidate_opened" })).toBe(false);
  expect(qualifiesForIgnitionAttention({ type: "ignition_event", kind: "follow_through_rejected" })).toBe(false);
});

// --- 3 ---------------------------------------------------------------------
test("non-green ignition is hidden from the normal panel", () => {
  expect(visible([ign("candidate_opened")])).toHaveLength(0);
  expect(visible([ign("follow_through_rejected")])).toHaveLength(0);
  expect(visible([cons("surge_detected")])).toHaveLength(0);
  expect(visible([cons("consolidation_confirmed")])).toHaveLength(0);
});

// --- 4 ---------------------------------------------------------------------
test("non-green ignition yields no notification", () => {
  expect(qualifiesForIgnitionAttention({ type: "ignition_event", kind: "candidate_opened" })).toBe(false);
  expect(qualifiesForIgnitionAttention({ type: "ignition_event", kind: "follow_through_rejected" })).toBe(false);
  expect(qualifiesForIgnitionAttention({ type: "consolidation_event", kind: "surge_detected" })).toBe(false);
  expect(qualifiesForIgnitionAttention({ type: "consolidation_event", kind: "consolidation_confirmed" })).toBe(false);
});

// --- 5 / 11 ----------------------------------------------------------------
test("non-green events still reach internal consumers unfiltered", () => {
  // deriveIgnitionFeed, the shared derivation, is untouched. The filter is
  // applied at render only, so nothing is removed from client state.
  const evs = [ign("candidate_opened"), ign("follow_through_rejected"), ign("follow_through_confirmed"), cons("surge_detected")];
  expect(deriveIgnitionFeed(evs)).toHaveLength(4);
  expect(visible(evs)).toHaveLength(1);
});

// --- 6 / 7 ----------------------------------------------------------------
test("a confirmation arriving later becomes visible as its own distinct item", () => {
  // The predicate reads `kind`, immutable per event, so a row never mutates.
  // The transition happens because a SEPARATE event arrives.
  const before = [ign("candidate_opened", "ONCO", "2026-09-21T15:00:00.000Z")];
  expect(visible(before)).toHaveLength(0);
  const after = [ign("follow_through_confirmed", "ONCO", "2026-09-21T15:00:09.000Z"), ...before];
  expect(visible(after)).toHaveLength(1);
  expect(visible(after)[0].event.timestamp).toBe("2026-09-21T15:00:09.000Z");
});

// --- 8 ---------------------------------------------------------------------
test("repeated confirmations are distinct items; dedup is the hook's job, not the predicate's", () => {
  // useIgnitionAlerts dedupes on symbol+timestamp plus a 15-minute per-symbol
  // cooldown. The predicate is stateless and must stay so.
  const a = ign("follow_through_confirmed", "ONCO", "2026-09-21T15:00:00.000Z");
  const b = ign("follow_through_confirmed", "ONCO", "2026-09-21T15:00:31.000Z");
  expect(visible([a, b])).toHaveLength(2);
  expect(qualifiesForIgnitionAttention(a as never)).toBe(true);
  expect(qualifiesForIgnitionAttention(b as never)).toBe(true);
});

// --- 9 ---------------------------------------------------------------------
test("green styling is driven by the same predicate as visibility", () => {
  const cases: DetectionEvent[] = [
    ign("follow_through_confirmed"), ign("candidate_opened"), ign("follow_through_rejected"),
    cons("entry_triggered"), cons("surge_detected"), cons("consolidation_confirmed"),
  ];
  for (const e of cases) {
    const item = deriveIgnitionFeed([e])[0];
    expect(rowClass(item).includes("feed-row-hit")).toBe(qualifiesForIgnitionAttention(item.event));
  }
});

test("rejections keep their muted treatment, which still matters in All mode", () => {
  expect(rowClass(deriveIgnitionFeed([ign("follow_through_rejected")])[0])).toContain("feed-row-muted");
});

// --- 15 --------------------------------------------------------------------
test("All mode shows every Ignition-family event", () => {
  const evs = [ign("candidate_opened"), ign("follow_through_rejected"), ign("follow_through_confirmed"),
               cons("surge_detected"), cons("entry_triggered")];
  expect(deriveIgnitionFeed(evs)).toHaveLength(5);
  expect(visible(evs)).toHaveLength(2);
});

// --- 17 --------------------------------------------------------------------
test("the user-facing count equals the number of visible rows", () => {
  const evs: DetectionEvent[] = [];
  for (let i = 0; i < 40; i++) evs.push(ign("candidate_opened"));
  evs.push(ign("follow_through_confirmed"), cons("entry_triggered"));
  expect(deriveIgnitionFeed(evs)).toHaveLength(42);
  expect(visible(evs)).toHaveLength(2); // the panel count uses this list
});

// --- 18 / 19 / 20 ----------------------------------------------------------
test("the predicate is session-independent across premarket, regular and after-hours", () => {
  for (const ts of ["2026-09-21T09:15:00.000Z", "2026-09-21T15:00:00.000Z", "2026-09-21T22:30:00.000Z"]) {
    expect(visible([ign("follow_through_confirmed", "ONCO", ts)])).toHaveLength(1);
    expect(visible([ign("candidate_opened", "ONCO", ts)])).toHaveLength(0);
  }
  // It reads no clock and no session field, so it cannot drift by session.
});

// --- 21 --------------------------------------------------------------------
test("web and mobile share one predicate implementation", () => {
  // Both import from @stockspotter/shared-types; there is no second copy to
  // diverge. Asserted on the raw event shape both clients hold.
  const raw = { type: "ignition_event", kind: "follow_through_confirmed" };
  expect(qualifiesForIgnitionAttention(raw)).toBe(true);
  expect(isIgnitionFamilyEvent(raw)).toBe(true);
});

// --- 22 --------------------------------------------------------------------
test("unrelated detector events are untouched by the filter", () => {
  const h = halt();
  expect(isIgnitionFamilyEvent(h as never)).toBe(false);
  expect(qualifiesForIgnitionAttention(h as never)).toBe(false);
  // Mobile keeps non-family events: (!family || qualifies) must hold.
  expect(!isIgnitionFamilyEvent(h as never) || qualifiesForIgnitionAttention(h as never)).toBe(true);
  // And they never enter the Ignition feed at all.
  expect(deriveIgnitionFeed([h])).toHaveLength(0);
});

test("micropullback entry qualifies; its earlier stages do not", () => {
  expect(visible([cons("entry_triggered", "micropullback")])).toHaveLength(1);
  expect(visible([cons("consolidation_confirmed", "micropullback")])).toHaveLength(0);
});
