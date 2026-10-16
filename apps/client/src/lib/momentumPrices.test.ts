import { expect, test } from "bun:test";
import { withinScanPrice } from "@stockspotter/shared-types";
import { momentumPrices } from "./momentumPrices";

test("the scanner range ($0.10-$20.50, inclusive) excludes unknown and invalid prices", () => {
  for (const price of [undefined, NaN, Infinity, 0, -1, 0.09, 20.51]) expect(withinScanPrice(price)).toBe(false);
  for (const price of [0.1, 19.999, 20.5]) expect(withinScanPrice(price)).toBe(true);
});

test("newest unsorted observation wins, including invalid prices; references win ties", () => {
  const quotes = new Map([["ABC", [
    { price: 25, timestamp: "2026-10-12T15:02:00Z", changePct: 0 },
    { price: 19, timestamp: "2026-10-12T15:01:00Z", changePct: 0 },
  ]]]);
  expect(withinScanPrice(momentumPrices(new Map(), quotes).get("ABC"))).toBe(false);
  quotes.get("ABC")!.push({ price: NaN, timestamp: "2026-10-12T15:03:00Z", changePct: 0 });
  expect(withinScanPrice(momentumPrices(new Map(), quotes).get("ABC"))).toBe(false);
  quotes.get("ABC")!.push({ price: 18, timestamp: "2026-10-12T15:03:00Z", changePct: 0 });
  expect(momentumPrices(new Map(), quotes).get("ABC")).toBe(18);
});
