import { test, expect } from "bun:test";
import {
  USER_ATTENTION_PRICE_CEILING,
  isIgnitionFamilyEvent,
  qualifiesForIgnitionAttention,
  qualifiesForUserAttention,
  withinUserAttentionPrice,
} from "@stockspotter/shared-types";
import { rowClass } from "./ignitionRow";
import { deriveIgnitionFeed } from "./derive";
import type { DetectionEvent } from "./useRealtimeFeed";

const ign = (kind: string, price: number, symbol = "ONCO", ts = "2026-09-21T15:00:00.000Z") =>
  ({ type: "ignition_event", symbol, timestamp: ts, price, kind }) as unknown as DetectionEvent;
const cons = (kind: string, price: number, strategy = "consolidation_breakout") =>
  ({ type: "consolidation_event", symbol: "SWVL", timestamp: "2026-09-21T15:00:00.000Z", price, kind, strategy }) as unknown as DetectionEvent;
const confirmed = (price: number, symbol = "ONCO", ts?: string) => ign("follow_through_confirmed", price, symbol, ts);

/** Normal panel: the feed filtered by the user-attention rule. */
const visible = (evs: DetectionEvent[]) =>
  deriveIgnitionFeed(evs).filter((it) => qualifiesForUserAttention(it.event));
/** Diagnostic All: the raw feed. */
const all = (evs: DetectionEvent[]) => deriveIgnitionFeed(evs);
/** Notification eligibility uses the identical predicate (see App.tsx). */
const notifiable = (evs: DetectionEvent[]) => evs.filter((e) => qualifiesForUserAttention(e));

test("the ceiling is 25.00 and inclusive", () => {
  expect(USER_ATTENTION_PRICE_CEILING).toBe(25.0);
});

// --- 1 / 2 / 3 / 4 --------------------------------------------------------
test("green at $24.99 and at exactly $25.00 are visible and notification-eligible", () => {
  for (const p of [24.99, 25.0]) {
    expect(visible([confirmed(p)])).toHaveLength(1);
    expect(notifiable([confirmed(p)])).toHaveLength(1);
  }
});

// --- 5 / 6 / 7 / 8 --------------------------------------------------------
test("green at $25.01 and at $218 are hidden and silent", () => {
  for (const p of [25.01, 218.0]) {
    expect(visible([confirmed(p)])).toHaveLength(0);
    expect(notifiable([confirmed(p)])).toHaveLength(0);
  }
});

test("boundary is price > 25 excluded, never price < 25", () => {
  expect(withinUserAttentionPrice(24.999999)).toBe(true);
  expect(withinUserAttentionPrice(25.0)).toBe(true);        // NOT reinterpreted as < 25
  expect(withinUserAttentionPrice(25.000001)).toBe(false);
  expect(withinUserAttentionPrice(25.01)).toBe(false);
});

// --- 9 / 10 ---------------------------------------------------------------
test("non-green at $5 is hidden and silent despite being cheap", () => {
  expect(visible([ign("candidate_opened", 5)])).toHaveLength(0);
  expect(notifiable([ign("candidate_opened", 5)])).toHaveLength(0);
  expect(visible([ign("follow_through_rejected", 5)])).toHaveLength(0);
  expect(visible([cons("surge_detected", 5)])).toHaveLength(0);
});

// --- 11 / 12 --------------------------------------------------------------
test("a $218 green event survives in the raw stream and in All mode", () => {
  const nvda = confirmed(218.0, "NVDA");
  // Raw client stream: untouched, nothing removed.
  expect(all([nvda])).toHaveLength(1);
  expect(all([nvda])[0].event.price).toBe(218.0);
  // Still a meaningful DETECTOR event, so All mode keeps its green treatment.
  expect(qualifiesForIgnitionAttention(all([nvda])[0].event)).toBe(true);
  expect(rowClass(all([nvda])[0])).toContain("feed-row-hit");
  // But not user attention.
  expect(qualifiesForUserAttention(all([nvda])[0].event)).toBe(false);
  expect(visible([nvda])).toHaveLength(0);
});

// --- 13 / 14 --------------------------------------------------------------
test("missing or invalid price is conservative: hidden and silent, never assumed cheap", () => {
  const bad: unknown[] = [undefined, null, NaN, Infinity, -Infinity, 0, -5, "12", {}];
  for (const p of bad) {
    expect(withinUserAttentionPrice(p)).toBe(false);
    const e = { type: "ignition_event", kind: "follow_through_confirmed", price: p } as never;
    expect(qualifiesForUserAttention(e)).toBe(false);
  }
  // Zero must not slip through as "<= 25".
  expect(withinUserAttentionPrice(0)).toBe(false);
});

// --- 15 / 16 --------------------------------------------------------------
test("eligibility is decided at the event and no later price can change it", () => {
  // Confirmed at $24.80, then the stock runs to $25.40. Still eligible: the
  // decision is a property of the event, not of the current quote.
  const atEvent = confirmed(24.8, "ONCO", "2026-09-21T15:00:00.000Z");
  const later = confirmed(25.4, "ONCO", "2026-09-21T15:30:00.000Z");
  expect(qualifiesForUserAttention(atEvent)).toBe(true);
  expect(visible([later, atEvent])).toHaveLength(1);
  expect(visible([later, atEvent])[0].event.price).toBe(24.8);

  // Confirmed at $25.30, later falls to $24.50. NOT retroactively promoted:
  // the $25.30 event stays ineligible and the $24.50 event is its own,
  // separately eligible event.
  const high = confirmed(25.3, "AAPL", "2026-09-21T15:00:00.000Z");
  expect(qualifiesForUserAttention(high)).toBe(false);
  const fell = confirmed(24.5, "AAPL", "2026-09-21T15:45:00.000Z");
  const both = visible([fell, high]);
  expect(both).toHaveLength(1);
  expect(both[0].event.timestamp).toBe("2026-09-21T15:45:00.000Z");
});

// --- 17 / 18 --------------------------------------------------------------
test("the cooldown and dedup live in the hook, untouched by the price gate", () => {
  // The gate is a stateless pre-filter applied before useIgnitionAlerts, so
  // its 15-minute per-symbol cooldown and symbol+timestamp dedup are intact.
  const a = confirmed(10, "ONCO", "2026-09-21T15:00:00.000Z");
  const b = confirmed(10, "ONCO", "2026-09-21T15:00:31.000Z");
  expect(notifiable([a, b])).toHaveLength(2);        // both pass the gate
  // Dedup/cooldown then reduce them inside the hook; the predicate is
  // stateless and must not attempt it.
  expect(qualifiesForUserAttention(a)).toBe(true);
  expect(qualifiesForUserAttention(b)).toBe(true);
});

// --- 19 / 20 --------------------------------------------------------------
test("catalyst and halt notifications are unaffected", () => {
  const halt = { type: "halt_warning", symbol: "VEEE", timestamp: "2026-09-21T15:00:00.000Z", level: "red", proximityRatio: 0.9, price: 900 } as unknown as DetectionEvent;
  const cat = { type: "catalyst_update", symbol: "NVDA", timestamp: "2026-09-21T15:00:00.000Z" } as unknown as DetectionEvent;
  for (const e of [halt, cat]) {
    expect(isIgnitionFamilyEvent(e as never)).toBe(false);
    expect(qualifiesForUserAttention(e as never)).toBe(false);
    // Mobile keeps non-family events: (!family || qualifies) must hold.
    expect(!isIgnitionFamilyEvent(e as never) || qualifiesForUserAttention(e as never)).toBe(true);
    expect(deriveIgnitionFeed([e])).toHaveLength(0);
  }
  // A $900 halt is still a halt: the ceiling must not reach other families.
  expect(isIgnitionFamilyEvent(halt as never)).toBe(false);
});

// --- 21 / 22 --------------------------------------------------------------
test("one predicate serves web, mobile and desktop", () => {
  // All three import qualifiesForUserAttention from shared-types; desktop
  // packages the web bundle, so it inherits by construction. Asserted on the
  // raw event shape every client holds.
  const raw = { type: "ignition_event", kind: "follow_through_confirmed", price: 12.5 };
  expect(qualifiesForUserAttention(raw)).toBe(true);
  expect(qualifiesForUserAttention({ ...raw, price: 125 })).toBe(false);
});

// --- 23 / 24 --------------------------------------------------------------
test("panel count uses attention-qualified rows; All count stays truthful", () => {
  const evs: DetectionEvent[] = [];
  for (let i = 0; i < 30; i++) evs.push(ign("candidate_opened", 4));      // non-green
  for (let i = 0; i < 10; i++) evs.push(confirmed(120, `H${i}`));         // green, too expensive
  evs.push(confirmed(8, "CHEAP1"), cons("entry_triggered", 3));           // attention-qualified
  expect(all(evs)).toHaveLength(42);        // All count: raw and truthful
  expect(visible(evs)).toHaveLength(2);     // normal count: what is on screen
});

test("green-but-too-expensive is a distinct hidden class from non-green", () => {
  // The hidden set is the union of both, which matters for how a hidden
  // count would be explained to the user.
  const evs = [ign("candidate_opened", 4), confirmed(120, "H"), confirmed(8, "C")];
  const hidden = all(evs).filter((it) => !qualifiesForUserAttention(it.event));
  expect(hidden).toHaveLength(2);
  const nonGreen = hidden.filter((it) => !qualifiesForIgnitionAttention(it.event));
  const tooPricey = hidden.filter((it) => qualifiesForIgnitionAttention(it.event));
  expect(nonGreen).toHaveLength(1);
  expect(tooPricey).toHaveLength(1);
});

test("green remains structurally separate from user attention", () => {
  // Section 5: do NOT redefine green to mean <= $25.
  const pricey = confirmed(218);
  expect(qualifiesForIgnitionAttention(pricey)).toBe(true);   // still green
  expect(qualifiesForUserAttention(pricey)).toBe(false);      // not attention
  const cheapNonGreen = ign("candidate_opened", 3);
  expect(qualifiesForIgnitionAttention(cheapNonGreen)).toBe(false);
  expect(qualifiesForUserAttention(cheapNonGreen)).toBe(false);
});

test("the ceiling applies to consolidation entries too, not just ignition", () => {
  expect(visible([cons("entry_triggered", 12)])).toHaveLength(1);
  expect(visible([cons("entry_triggered", 60)])).toHaveLength(0);
  expect(visible([cons("entry_triggered", 12, "micropullback")])).toHaveLength(1);
  expect(visible([cons("entry_triggered", 60, "micropullback")])).toHaveLength(0);
});
