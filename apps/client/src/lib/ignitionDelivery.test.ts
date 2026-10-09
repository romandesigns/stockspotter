import { expect, test } from "bun:test";
import type { IgnitionEvent } from "@stockspotter/shared-types";
import { collectFreshIgnitions, createIgnitionDeliveryState } from "./ignitionDelivery";
const now = Date.parse("2026-10-09T14:20:00Z");
const event = (symbol: string, ago: number): IgnitionEvent => ({ type: "ignition_event", kind: "follow_through_confirmed", symbol, price: 2, timestamp: new Date(now - ago).toISOString() });
test("snapshot replay does not alert stale confirmations or reset its cooldown", () => {
  const state = createIgnitionDeliveryState();
  expect(collectFreshIgnitions(state, [event("OLD", 8 * 60_000)], now)).toEqual([]);
  expect(collectFreshIgnitions(state, [event("OLD", 0), event("NEW", 30_000)], now).map((e) => e.symbol)).toEqual(["NEW"]);
  expect(collectFreshIgnitions(state, [event("NEW", 30_000)], now)).toEqual([]);
});
test("cooldown expires and malformed/future timestamps do not poison it", () => {
  const state = createIgnitionDeliveryState();
  expect(collectFreshIgnitions(state, [event("X", -60_000), { ...event("X", 0), timestamp: "invalid" }], now)).toEqual([]);
  expect(collectFreshIgnitions(state, [event("X", 0)], now)).toHaveLength(1);
  expect(collectFreshIgnitions(state, [event("X", -16 * 60_000)], now + 16 * 60_000)).toHaveLength(1);
});
test("retained-event dedup storage is bounded during a large burst", () => {
  const state = createIgnitionDeliveryState();
  collectFreshIgnitions(state, Array.from({ length: 4000 }, (_, i) => event(`S${i}`, 0)), now);
  expect(state.seen.size).toBeLessThanOrEqual(2048);
});
