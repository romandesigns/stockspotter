import { expect, test } from "bun:test";
import { isUnderTwenty, momentumPrices } from "./momentumPrices";

test("strict under-$20 boundary excludes unknown and invalid prices", () => {
  for (const price of [undefined, NaN, Infinity, 0, -1, 20, 20.01]) expect(isUnderTwenty(price)).toBe(false);
  expect(isUnderTwenty(19.999)).toBe(true);
});

test("newest unsorted observation wins, including invalid prices; references win ties", () => {
  const quotes = new Map([["ABC", [
    { price: 25, timestamp: "2026-10-12T15:02:00Z", changePct: 0 },
    { price: 19, timestamp: "2026-10-12T15:01:00Z", changePct: 0 },
  ]]]);
  expect(isUnderTwenty(momentumPrices(new Map(), quotes).get("ABC"))).toBe(false);
  quotes.get("ABC")!.push({ price: NaN, timestamp: "2026-10-12T15:03:00Z", changePct: 0 });
  expect(isUnderTwenty(momentumPrices(new Map(), quotes).get("ABC"))).toBe(false);
  quotes.get("ABC")!.push({ price: 18, timestamp: "2026-10-12T15:03:00Z", changePct: 0 });
  expect(momentumPrices(new Map(), quotes).get("ABC")).toBe(18);
});
