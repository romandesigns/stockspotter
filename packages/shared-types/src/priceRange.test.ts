import { expect, test } from "bun:test";
import { withinScanPrice } from "./priceRange";

test("the scanner range is $0.10-$20.50 inclusive", () => {
  for (const p of [0.1, 0.25, 1, 19.99, 20, 20.5]) expect(withinScanPrice(p)).toBe(true);
  for (const p of [undefined, null, NaN, Infinity, 0, 0.09, 20.51, 25]) expect(withinScanPrice(p as number)).toBe(false);
});
