import { expect, test } from "bun:test";
import type { ConsolidationEvent, MomentumUpdate } from "@stockspotter/shared-types";
import { isFreshMicropullback, reduceMicropullbackDelivery } from "./micropullbackDelivery";

const now = Date.parse("2026-10-12T14:00:00Z");
const event = (symbol: string, ago = 0): ConsolidationEvent => ({ type: "consolidation_event", strategy: "micropullback", kind: "entry_triggered", symbol, price: 2, timestamp: new Date(now - ago).toISOString() });
const momentum = (symbol: string, score = 0.8): MomentumUpdate => ({ type: "momentum_update", symbol, timestamp: new Date(now).toISOString(), overall: score, volumeConfirmation: score, structure: score, maSlope: score, wickRejection: score, qualifies: score >= 0.6 });
const confirmed = (...symbols: string[]) => new Map(symbols.map((s) => [s, momentum(s)]));

test("freshness boundaries, future tolerance and malformed payloads are explicit", () => {
  expect(isFreshMicropullback(event("X", 120_000), now)).toBe(true);
  expect(isFreshMicropullback(event("X", 120_001), now)).toBe(false);
  expect(isFreshMicropullback(event("X", -30_000), now)).toBe(true);
  expect(isFreshMicropullback(event("X", -30_001), now)).toBe(false);
  for (const bad of [{ ...event("X"), timestamp: "invalid" }, { ...event("X"), price: NaN }, { ...event("X"), price: 0 }, event(" ")]) expect(isFreshMicropullback(bad, now)).toBe(false);
});

test("late momentum can confirm a fresh event without repeating its alert", () => {
  const first = reduceMicropullbackDelivery(new Map(), [event("X")], new Map(), now);
  expect(first.fresh).toEqual([]);
  const late = reduceMicropullbackDelivery(first.seen, [event("X")], confirmed("X"), now + 1000);
  expect(late.fresh).toHaveLength(1);
  expect(reduceMicropullbackDelivery(late.seen, [event("X")], confirmed("X"), now + 2000).fresh).toEqual([]);
  expect(reduceMicropullbackDelivery(new Map(), [event("X")], new Map([["X", momentum("X", 0.59)]]), now).fresh).toEqual([]);
  expect(reduceMicropullbackDelivery(new Map(), [event("X")], confirmed("X"), now).fresh).toHaveLength(1);
});

test("fresh evicted keys survive retained-buffer rollover and replay", () => {
  const first = reduceMicropullbackDelivery(new Map(), [event("EARLY")], confirmed("EARLY"), now);
  const later = Array.from({ length: 50 }, (_, i) => event(`S${i}`));
  const rolled = reduceMicropullbackDelivery(first.seen, later, confirmed(...later.map((e) => e.symbol)), now + 1000);
  expect(rolled.seen.size).toBe(51);
  expect(reduceMicropullbackDelivery(rolled.seen, [event("EARLY"), ...later.slice(0, 49)], confirmed("EARLY"), now + 2000).fresh).toEqual([]);
});

test("TTL uses arrival time, retains buffered keys, and prunes inactive history", () => {
  const previous = new Map<string, number>();
  const first = reduceMicropullbackDelivery(previous, [event("LAG", 119_000)], confirmed("LAG"), now);
  expect(previous.size).toBe(0);
  expect(first.fresh).toHaveLength(1);
  const retained = reduceMicropullbackDelivery(first.seen, [event("LAG", 119_000)], confirmed("LAG"), now + 161_000);
  expect(retained.seen.size).toBe(1);
  expect(reduceMicropullbackDelivery(retained.seen, [], new Map(), now + 161_000).seen.size).toBe(0);
});

test("future-tolerated events remain deduped throughout their eligibility window", () => {
  const future = event("FUTURE", -30_000);
  const first = reduceMicropullbackDelivery(new Map(), [future], confirmed("FUTURE"), now);
  const evicted = reduceMicropullbackDelivery(first.seen, [], new Map(), now + 130_001);
  expect(reduceMicropullbackDelivery(evicted.seen, [future], confirmed("FUTURE"), now + 130_001).fresh).toEqual([]);
});

test("key retention has explicit TTL boundaries separate from event freshness", () => {
  const first = reduceMicropullbackDelivery(new Map(), [event("X")], confirmed("X"), now);
  expect(reduceMicropullbackDelivery(first.seen, [], new Map(), now + 159_999).seen.size).toBe(1);
  expect(reduceMicropullbackDelivery(first.seen, [], new Map(), now + 160_000).seen.size).toBe(1);
  expect(reduceMicropullbackDelivery(first.seen, [], new Map(), now + 160_001).seen.size).toBe(0);
  expect(isFreshMicropullback(event("FUTURE", -30_000), now + 150_000)).toBe(true);
  expect(isFreshMicropullback(event("FUTURE", -30_000), now + 150_001)).toBe(false);
});

test("an undecided event that expires cannot announce on late confirmation", () => {
  const first = reduceMicropullbackDelivery(new Map(), [event("X")], new Map(), now);
  expect(first.seen.size).toBe(0);
  expect(reduceMicropullbackDelivery(first.seen, [event("X")], confirmed("X"), now + 120_001).fresh).toEqual([]);
});

test("same-key payload revisions collapse intentionally and new timestamps remain eligible", () => {
  const first = reduceMicropullbackDelivery(new Map(), [event("X"), { ...event("X"), price: 3 }], confirmed("X"), now);
  expect(first.fresh).toHaveLength(1);
  expect(reduceMicropullbackDelivery(first.seen, [event("X", -1000)], confirmed("X"), now + 1000).fresh).toHaveLength(1);
});

test("timestamp formatting aliases dedup while sub-millisecond events stay distinct", () => {
  const base = event("X");
  const first = reduceMicropullbackDelivery(new Map(), [base], confirmed("X"), now);
  for (const timestamp of ["2026-10-12T14:00:00Z", "2026-10-12T16:00:00.000+02:00", "2026-10-12T14:00:00.000000000Z"]) {
    expect(reduceMicropullbackDelivery(first.seen, [{ ...base, timestamp }], confirmed("X"), now).fresh).toEqual([]);
  }
  expect(reduceMicropullbackDelivery(new Map(), [{ ...base, timestamp: "2026-10-12T14:00:00.000000123Z" }, { ...base, timestamp: "2026-10-12T14:00:00.000000124Z" }], confirmed("X"), now).fresh).toHaveLength(2);
});
