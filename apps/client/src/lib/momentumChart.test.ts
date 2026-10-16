import { describe, expect, test } from "bun:test";
import type { FunnelSignal, MomentumUpdate, ShortlistCandidate, ShortlistResponse } from "@stockspotter/shared-types";
import { bandColor, bandOf, BELOW_BAND_COLOR, confirmedAgo, formatMinutes, hiddenSummary, momentumCards, readingSession, type MomentumCardsInput } from "./momentumChart";

const m = (symbol: string, overall: number, at: string, f: [number, number, number, number] = [0.5, 0.5, 0.5, 0.5], qualifies = overall >= 0.6): MomentumUpdate => ({
  type: "momentum_update", symbol, timestamp: at, volumeConfirmation: f[0], structure: f[1], maSlope: f[2], wickRejection: f[3], overall, qualifies,
});
// 2026-10-15 is EDT (UTC-4): 14:00Z = 10:00 ET.
const NOW = Date.parse("2026-10-15T14:00:00Z");
const funnel = (symbol: string, price: number, gapPct: number): FunnelSignal => ({
  type: "funnel_signal", symbol, timestamp: "2026-10-15T13:50:00Z", price, gapPct, sessionVolume: 1, priceOk: true, floatOk: true, relVolOk: true, gapOk: true, passed: true,
});

function input(over: Partial<MomentumCardsInput>): MomentumCardsInput {
  return {
    confirmations: [], current: new Map(), fades: new Map(), prices: new Map([["AAA", 3], ["BBB", 5], ["CCC", 7], ["PRICEY", 25]]),
    funnelBySymbol: new Map(), funds: new Set(), shortlist: null, nowMs: NOW, ...over,
  };
}

describe("momentumCards", () => {
  test("one card per symbol from its latest crossing, scored by the CURRENT reading", () => {
    const { cards } = momentumCards(input({
      confirmations: [m("AAA", 0.61, "2026-10-15T13:40:00Z"), m("AAA", 0.62, "2026-10-15T13:30:00Z"), m("BBB", 0.66, "2026-10-15T13:45:00Z")],
      current: new Map([["AAA", m("AAA", 0.71, "2026-10-15T13:59:00Z", [0.9, 0.8, 0.7, 0.6])], ["BBB", m("BBB", 0.64, "2026-10-15T13:59:00Z")]]),
    }));
    expect(cards.map((c) => [c.symbol, c.score, c.band, c.status])).toEqual([["AAA", 0.71, "Strong", "active"], ["BBB", 0.64, "Mild", "active"]]);
    expect(cards[0].confirmedAt).toBe("2026-10-15T13:40:00Z");
    expect(cards[0].confirmedBand).toBe("Mild");
    expect(cards[0].factors.map((f) => f.value)).toEqual([0.9, 0.8, 0.7, 0.6]);
    expect(cards[1].confirmedBand).toBe("Solid");
  });

  test("falls back to the crossing reading when there is no newer current one", () => {
    const { cards } = momentumCards(input({ confirmations: [m("AAA", 0.63, "2026-10-15T13:58:00Z")] }));
    expect(cards[0].score).toBe(0.63);
    expect(cards[0].readingAt).toBe("2026-10-15T13:58:00Z");
  });

  test("faded cards stay 15 minutes after the fading reading, then leave", () => {
    const confirmations = [m("AAA", 0.62, "2026-10-15T13:30:00Z"), m("BBB", 0.62, "2026-10-15T13:30:00Z")];
    const current = new Map([["AAA", m("AAA", 0.55, "2026-10-15T13:59:00Z")], ["BBB", m("BBB", 0.5, "2026-10-15T13:59:00Z")]]);
    const fades = new Map([["AAA", m("AAA", 0.58, "2026-10-15T13:50:00Z")], ["BBB", m("BBB", 0.58, "2026-10-15T13:44:00Z")]]);
    const { cards } = momentumCards(input({ confirmations, current, fades }));
    expect(cards.map((c) => [c.symbol, c.status, c.fadedAt, c.band])).toEqual([["AAA", "faded", "2026-10-15T13:50:00Z", null]]);
  });

  test("a fade edge from before the latest crossing is ignored", () => {
    const { cards } = momentumCards(input({
      confirmations: [m("AAA", 0.62, "2026-10-15T13:52:00Z")],
      current: new Map([["AAA", m("AAA", 0.55, "2026-10-15T13:56:00Z")]]),
      fades: new Map([["AAA", m("AAA", 0.58, "2026-10-15T13:20:00Z")]]),
    }));
    expect(cards[0].fadedAt).toBe("2026-10-15T13:56:00Z");
  });

  test("an active card with no reading for over an hour is dropped", () => {
    const { cards } = momentumCards(input({
      confirmations: [m("AAA", 0.62, "2026-10-15T12:40:00Z"), m("BBB", 0.62, "2026-10-15T12:40:00Z")],
      current: new Map([["AAA", m("AAA", 0.63, "2026-10-15T12:58:00Z")], ["BBB", m("BBB", 0.62, "2026-10-15T13:01:00Z")]]),
    }));
    expect(cards.map((c) => c.symbol)).toEqual(["BBB"]);
  });

  test("a current reading older than 10 minutes is flagged stale", () => {
    const { cards } = momentumCards(input({
      confirmations: [m("AAA", 0.62, "2026-10-15T13:40:00Z"), m("BBB", 0.62, "2026-10-15T13:40:00Z")],
      current: new Map([["AAA", m("AAA", 0.63, "2026-10-15T13:48:00Z")], ["BBB", m("BBB", 0.62, "2026-10-15T13:51:00Z")]]),
    }));
    expect(cards.map((c) => [c.symbol, c.staleMinutes])).toEqual([["AAA", 12], ["BBB", null]]);
  });

  test("order: active regular session by score, then active outside regular hours, then faded", () => {
    const { cards } = momentumCards(input({
      confirmations: [
        m("AAA", 0.62, "2026-10-15T13:50:00Z"),
        m("BBB", 0.69, "2026-10-15T13:20:00Z"),
        m("CCC", 0.66, "2026-10-15T13:50:00Z"),
        m("DDD", 0.75, "2026-10-15T13:50:00Z"),
      ],
      current: new Map([
        ["BBB", m("BBB", 0.69, "2026-10-15T13:25:00Z")], // 09:25 ET: pre-market
        ["DDD", m("DDD", 0.5, "2026-10-15T13:55:00Z")],
      ]),
      prices: new Map([["AAA", 3], ["BBB", 5], ["CCC", 7], ["DDD", 4]]),
    }));
    expect(cards.map((c) => [c.symbol, c.session, c.status])).toEqual([["CCC", "regular", "active"], ["AAA", "regular", "active"], ["BBB", "pre", "active"], ["DDD", "regular", "faded"]]);
  });

  test("funds, missing prices and prices outside the scanner range are counted, not dropped silently", () => {
    const { cards, hidden } = momentumCards(input({
      confirmations: ["AAA", "NVDY", "NOPRICE", "PRICEY", "FUNNEL"].map((s) => m(s, 0.62, "2026-10-15T13:50:00Z")),
      prices: new Map([["AAA", 3], ["NVDY", 8], ["PRICEY", 20.51]]),
      funds: new Set(["NVDY"]),
      funnelBySymbol: new Map([["FUNNEL", funnel("FUNNEL", 4.2, 31.5)]]),
    }));
    expect(cards.map((c) => [c.symbol, c.price, c.dayChangePct])).toEqual([["AAA", 3, undefined], ["FUNNEL", 4.2, 31.5]]);
    expect(hidden).toEqual({ funds: 1, noPrice: 1, outsideRange: 1 });
    expect(hiddenSummary(hidden)).toBe("Hidden: 1 fund · 1 with no price yet · 1 outside $0.10–$20.50");
    expect(hiddenSummary({ funds: 2, noPrice: 0, outsideRange: 0 })).toBe("Hidden: 2 funds");
    expect(hiddenSummary({ funds: 0, noPrice: 0, outsideRange: 0 })).toBeNull();
  });

  test("bar price wins over the funnel price", () => {
    const { cards } = momentumCards(input({ confirmations: [m("AAA", 0.62, "2026-10-15T13:50:00Z")], funnelBySymbol: new Map([["AAA", funnel("AAA", 30, 12)]]) }));
    expect(cards[0].price).toBe(3);
    expect(cards[0].dayChangePct).toBe(12);
  });

  test("shortlist rank and tradability are attached", () => {
    const candidate = { rank: 2, symbol: "AAA", price: 3, sessionVolume: 1_000_000 } as unknown as ShortlistCandidate;
    const shortlist = { candidates: [candidate] } as unknown as ShortlistResponse;
    const { cards } = momentumCards(input({ confirmations: [m("AAA", 0.62, "2026-10-15T13:50:00Z"), m("BBB", 0.62, "2026-10-15T13:50:00Z")], shortlist }));
    const byS = new Map(cards.map((c) => [c.symbol, c]));
    expect(byS.get("AAA")!.shortlist!.rank).toBe(2);
    expect(["good", "caution", "poor", "unknown"]).toContain(byS.get("AAA")!.shortlist!.tradability.verdict);
    expect(byS.get("BBB")!.shortlist).toBeUndefined();
  });

  test("factor names say what the scorer measures; drivers use them", () => {
    const { cards: [close] } = momentumCards(input({ confirmations: [m("AAA", 0.64, "2026-10-15T13:50:00Z", [0.72, 0.24, 1.0, 0.93])] }));
    expect(close.factors.map((f) => f.short)).toEqual(["Buying", "Highs", "Trend", "Wicks"]);
    expect(close.drivers).toEqual(["Above moving averages", "No selling wicks"]);
    const { cards: [single] } = momentumCards(input({ confirmations: [m("AAA", 0.63, "2026-10-15T13:50:00Z", [0.95, 0.4, 0.67, 0.5])] }));
    expect(single.drivers).toEqual(["Buying pressure"]);
  });

  test("out-of-range values are clamped", () => {
    const { cards: [c] } = momentumCards(input({ confirmations: [m("AAA", 1.4, "2026-10-15T13:50:00Z", [2, -1, Number.NaN, 0.5], true)] }));
    expect(c.score).toBe(1);
    expect(c.factors.map((f) => f.value)).toEqual([1, 0, 0, 0.5]);
  });
});

describe("strength bands (must match base_rates.rs MOMENTUM_BANDS)", () => {
  test("boundaries", () => {
    expect([0.59, 0.6, 0.6499, 0.65, 0.6999, 0.7, 1].map(bandOf)).toEqual([null, "Mild", "Mild", "Solid", "Solid", "Strong", "Strong"]);
  });
  test("each band has its own colour; below the line is grey", () => {
    expect(new Set((["Mild", "Solid", "Strong"] as const).map(bandColor)).size).toBe(3);
    expect(bandColor(null)).toBe(BELOW_BAND_COLOR);
  });
});

describe("time helpers", () => {
  test("session by New York time, across DST", () => {
    expect(readingSession("2026-10-15T13:29:00Z")).toBe("pre");
    expect(readingSession("2026-10-15T13:30:00Z")).toBe("regular");
    expect(readingSession("2026-10-15T19:59:00Z")).toBe("regular");
    expect(readingSession("2026-10-15T20:00:00Z")).toBe("after");
    // January is EST (UTC-5): 14:29Z = 09:29 ET.
    expect(readingSession("2026-01-15T14:29:00Z")).toBe("pre");
    expect(readingSession("2026-01-15T14:30:00Z")).toBe("regular");
  });
  test("ages", () => {
    expect(confirmedAgo("2026-10-15T13:59:30Z", NOW)).toBe("confirmed just now");
    expect(confirmedAgo("2026-10-15T13:48:00Z", NOW)).toBe("confirmed 12 min ago");
    expect(confirmedAgo("2026-10-15T11:55:00Z", NOW)).toBe("confirmed 2 h 5 min ago");
    expect(formatMinutes(120)).toBe("2 h");
  });
});
