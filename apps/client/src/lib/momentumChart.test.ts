import { describe, expect, test } from "bun:test";
import type { MomentumUpdate } from "@stockspotter/shared-types";
import { bandOf, bandColor, momentumCards } from "./momentumChart";

const m = (symbol: string, overall: number, at: string, f: [number, number, number, number] = [0.5, 0.5, 0.5, 0.5]): MomentumUpdate => ({
  type: "momentum_update", symbol, timestamp: at, volumeConfirmation: f[0], structure: f[1], maSlope: f[2], wickRejection: f[3], overall, qualifies: true,
});

describe("momentumCards", () => {
  const prices = new Map([["AAA", 3], ["BBB", 5], ["PRICEY", 25]]);

  test("one card per symbol, its latest confirmation, strongest first, under $20 only", () => {
    const cards = momentumCards([
      m("AAA", 0.62, "2026-10-15T13:30:00Z"),
      m("BBB", 0.7, "2026-10-15T13:31:00Z"),
      m("AAA", 0.8, "2026-10-15T13:40:00Z"),
      m("PRICEY", 0.9, "2026-10-15T13:40:00Z"),
      m("NOPRICE", 0.9, "2026-10-15T13:40:00Z"),
    ], prices);
    expect(cards.map((c) => [c.symbol, c.score])).toEqual([["AAA", 0.8], ["BBB", 0.7]]);
  });

  test("drivers name the top factor, and the runner-up only when it is close", () => {
    const [close] = momentumCards([m("AAA", 0.64, "2026-10-15T13:30:00Z", [0.72, 0.24, 1.0, 0.93])], prices);
    expect(close.drivers).toEqual(["Rising trend", "Clean candles"]);
    const [single] = momentumCards([m("AAA", 0.63, "2026-10-15T13:30:00Z", [0.69, 0.4, 0.67, 1.0])], prices);
    expect(single.drivers).toEqual(["Clean candles"]);
  });

  test("out-of-range factor values are clamped", () => {
    const [c] = momentumCards([m("AAA", 1.4, "2026-10-15T13:30:00Z", [2, -1, Number.NaN, 0.5])], prices);
    expect(c.score).toBe(1);
    expect(c.factors.map((f) => f.value)).toEqual([1, 0, 0, 0.5]);
  });
});

describe("strength bands", () => {
  test("boundaries", () => {
    expect([0.6, 0.6499, 0.65, 0.7499, 0.75, 1].map(bandOf)).toEqual(["Mild", "Mild", "Solid", "Solid", "Strong", "Strong"]);
  });
  test("each band has its own colour, dim to bright", () => {
    expect(new Set(["Mild", "Solid", "Strong"].map((b) => bandColor(b as "Mild")))).toHaveProperty("size", 3);
  });
});
