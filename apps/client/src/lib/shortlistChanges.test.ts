import { describe, expect, test } from "bun:test";
import type { ShortlistCandidate, ShortlistHalt, ShortlistResponse } from "@stockspotter/shared-types";
import { appendChanges, describeChange, diffShortlist, EMPTY_HISTORY, type ShortlistChange } from "./shortlistChanges";

function cand(rank: number, symbol: string, extra: Partial<ShortlistCandidate> = {}): ShortlistCandidate {
  return {
    rank, symbol, score: 100 - rank, price: 5, gapPct: 30, relativeVolume: 5, sessionVolume: 1_000_000,
    funnelPassed: true, funnelAt: "2026-10-15T13:44:00Z", levels: { premarketHigh: 6, barsComplete: true }, ...extra,
  };
}

function halt(level: ShortlistHalt["level"], luldInEffect = true): ShortlistHalt {
  return { level, proximityRatio: 0.5, upperBand: 6, lowerBand: 4, distanceToUpperPct: 5, estimated: false, luldInEffect, ageSecs: 1 };
}

function body(candidates: ShortlistCandidate[], generatedAt = "2026-10-15T13:45:00Z", marketDay: string | null = "2026-10-15"): ShortlistResponse {
  return {
    generatedAt, marketDay, session: "regular", rankedBy: "gapPct*relativeVolume", poolSize: 20,
    excluded: { outsidePriceRange: 0, notGappingUp: 0, noRelativeVolume: 0 }, candidates,
  };
}

const LATER = "2026-10-15T13:45:05Z";

describe("diffShortlist", () => {
  test("a null prev (first poll) yields nothing", () => {
    expect(diffShortlist(null, body([cand(1, "AAA"), cand(2, "BBB")]))).toEqual([]);
  });

  test("an identical poll yields nothing", () => {
    const b = body([cand(1, "AAA"), cand(2, "BBB")]);
    expect(diffShortlist(b, body(b.candidates, LATER))).toEqual([]);
  });

  test("entered: new to the top 3, whether new to the list or climbing into it", () => {
    const prev = body([cand(1, "AAA"), cand(2, "BBB"), cand(3, "CCC"), cand(4, "DDD")]);
    const next = body([cand(1, "NEW"), cand(2, "AAA"), cand(3, "DDD"), cand(4, "BBB"), cand(5, "CCC")], LATER);
    expect(diffShortlist(prev, next)).toEqual([
      { kind: "entered", symbol: "NEW", at: LATER, rank: 1 },
      { kind: "entered", symbol: "DDD", at: LATER, rank: 3 },
    ]);
  });

  test("entered: moving within the top 3 is not a change", () => {
    const prev = body([cand(1, "AAA"), cand(2, "BBB"), cand(3, "CCC")]);
    const next = body([cand(1, "CCC"), cand(2, "AAA"), cand(3, "BBB")], LATER);
    expect(diffShortlist(prev, next)).toEqual([]);
  });

  test("crossedPremarketHigh: prev at or below the level, next strictly above", () => {
    const prev = body([cand(1, "AAA", { price: 6 }), cand(2, "BBB", { price: 5.9 }), cand(3, "CCC", { price: 6.1 })]);
    const next = body([cand(1, "AAA", { price: 6.01 }), cand(2, "BBB", { price: 6 }), cand(3, "CCC", { price: 6.3 })], LATER);
    expect(diffShortlist(prev, next)).toEqual([
      { kind: "crossedPremarketHigh", symbol: "AAA", at: LATER, premarketHigh: 6, price: 6.01 },
    ]);
  });

  test("crossedPremarketHigh needs the level on both polls", () => {
    const prev = body([cand(1, "AAA", { price: 5, levels: { barsComplete: false } })]);
    const next = body([cand(1, "AAA", { price: 7 })], LATER);
    expect(diffShortlist(prev, next)).toEqual([]);
  });

  test("haltEscalated: calm->amber, calm->red, amber->red while LULD is in effect", () => {
    const prev = body([cand(1, "AAA", { halt: halt("calm") }), cand(2, "BBB", { halt: halt("calm") }), cand(3, "CCC", { halt: halt("amber") })]);
    const next = body([cand(1, "AAA", { halt: halt("amber") }), cand(2, "BBB", { halt: halt("red") }), cand(3, "CCC", { halt: halt("red") })], LATER);
    expect(diffShortlist(prev, next)).toEqual([
      { kind: "haltEscalated", symbol: "AAA", at: LATER, from: "calm", to: "amber" },
      { kind: "haltEscalated", symbol: "BBB", at: LATER, from: "calm", to: "red" },
      { kind: "haltEscalated", symbol: "CCC", at: LATER, from: "amber", to: "red" },
    ]);
  });

  test("haltEscalated: easing, holding, or outside LULD hours is not a change", () => {
    const prev = body([cand(1, "AAA", { halt: halt("red") }), cand(2, "BBB", { halt: halt("amber") }), cand(3, "CCC", { halt: halt("calm", false) })]);
    const next = body([cand(1, "AAA", { halt: halt("amber") }), cand(2, "BBB", { halt: halt("amber") }), cand(3, "CCC", { halt: halt("red", false) })], LATER);
    expect(diffShortlist(prev, next)).toEqual([]);
  });

  test("left: a symbol on prev's list and absent from next", () => {
    const prev = body([cand(1, "AAA"), cand(2, "BBB"), cand(3, "CCC"), cand(4, "DDD")]);
    const next = body([cand(1, "AAA"), cand(2, "BBB"), cand(3, "CCC")], LATER);
    expect(diffShortlist(prev, next)).toEqual([{ kind: "left", symbol: "DDD", at: LATER }]);
  });

  test("changes come out in a fixed order: entered, crossed, halt, left", () => {
    const prev = body([cand(1, "AAA", { price: 5.5, halt: halt("calm") }), cand(2, "BBB"), cand(3, "CCC")]);
    const next = body([cand(1, "AAA", { price: 6.5, halt: halt("red") }), cand(2, "BBB"), cand(3, "NEW")], LATER);
    expect(diffShortlist(prev, next).map((c) => c.kind)).toEqual(["entered", "crossedPremarketHigh", "haltEscalated", "left"]);
  });
});

describe("appendChanges", () => {
  const day1a = body([cand(1, "AAA"), cand(2, "BBB")], "2026-10-15T13:45:00Z");
  const day1b = body([cand(1, "AAA")], "2026-10-15T13:45:05Z");

  test("first poll starts an empty history for its market day", () => {
    expect(appendChanges(EMPTY_HISTORY, null, day1a)).toEqual({ marketDay: "2026-10-15", changes: [] });
  });

  test("adds newest first and keeps the same object when nothing changed", () => {
    const h1 = appendChanges(EMPTY_HISTORY, null, day1a);
    const h2 = appendChanges(h1, day1a, day1b);
    expect(h2.changes).toEqual([{ kind: "left", symbol: "BBB", at: "2026-10-15T13:45:05Z" }]);
    const h3 = appendChanges(h2, day1b, body([cand(1, "AAA"), cand(2, "CCC")], "2026-10-15T13:45:10Z"));
    expect(h3.changes.map((c) => c.symbol)).toEqual(["CCC", "BBB"]);
    expect(appendChanges(h3, day1b, body(day1b.candidates, "2026-10-15T13:45:15Z"))).toBe(h3);
  });

  test("a new market day resets the history and is not diffed against yesterday", () => {
    const h = appendChanges(appendChanges(EMPTY_HISTORY, null, day1a), day1a, day1b);
    const nextDay = body([cand(1, "ZZZ")], "2026-10-16T08:00:00Z", "2026-10-16");
    expect(appendChanges(h, day1b, nextDay)).toEqual({ marketDay: "2026-10-16", changes: [] });
  });

  test("is capped", () => {
    let history = appendChanges(EMPTY_HISTORY, null, body([], "2026-10-15T13:00:00Z"));
    let prev = body([], "2026-10-15T13:00:00Z");
    for (let i = 0; i < 10; i++) {
      const next = body([cand(1, `S${i}`)], `2026-10-15T13:0${i}:30Z`);
      history = appendChanges(history, prev, next, 4);
      prev = next;
    }
    expect(history.changes.length).toBe(4);
    expect(history.changes[0]).toEqual({ kind: "entered", symbol: "S9", at: "2026-10-15T13:09:30Z", rank: 1 });
  });
});

test("describeChange reads as one short line", () => {
  const at = "2026-10-15T13:45:00Z";
  const lines = ([
    { kind: "entered", symbol: "A", at, rank: 2 },
    { kind: "crossedPremarketHigh", symbol: "A", at, premarketHigh: 4.5, price: 4.6 },
    { kind: "haltEscalated", symbol: "A", at, from: "calm", to: "red" },
    { kind: "left", symbol: "A", at },
  ] as ShortlistChange[]).map(describeChange);
  expect(lines).toEqual(["entered the top 3 at #2", "crossed its premarket high 4.50", "halt band calm → red", "left the list"]);
});
