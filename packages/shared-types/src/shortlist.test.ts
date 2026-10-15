import { describe, expect, test } from "bun:test";
import type { ShortlistCandidate } from "./shortlist";
import { assessTradability } from "./shortlist";

// Every input good: tight fresh quote, deep liquidity, far from the band.
const good: ShortlistCandidate = {
  rank: 1, symbol: "TEST", score: 120, price: 5, gapPct: 20, relativeVolume: 6, floatShares: 5_000_000,
  sessionVolume: 1_000_000, funnelPassed: true, funnelAt: "2026-10-15T14:00:00Z",
  levels: { dollarVolume: 5_000_000, barsComplete: true },
  quote: { bid: 4.99, ask: 5.01, spreadPct: 0.4, ageSecs: 2 },
  halt: { level: "calm", proximityRatio: 0.2, upperBand: 5.5, lowerBand: 4.5, distanceToUpperPct: 10, estimated: true, luldInEffect: true, ageSecs: 1 },
};
const withQuote = (q: Partial<NonNullable<ShortlistCandidate["quote"]>>) => ({ ...good, quote: { ...good.quote!, ...q } });
const withDollars = (dollarVolume: number | undefined) => ({ ...good, levels: { ...good.levels, dollarVolume } });
const withHalt = (h: Partial<NonNullable<ShortlistCandidate["halt"]>>) => ({ ...good, halt: { ...good.halt!, ...h } });

describe("assessTradability", () => {
  test("all-good inputs read as good with no reasons", () => {
    expect(assessTradability(good)).toEqual({ verdict: "good", reasons: [] });
  });

  test("spread thresholds are inclusive at 0.5% and 1.5%", () => {
    expect(assessTradability(withQuote({ spreadPct: 0.5 })).verdict).toBe("good");
    expect(assessTradability(withQuote({ spreadPct: 0.51 }))).toEqual({ verdict: "caution", reasons: ["spread 0.5%"] });
    expect(assessTradability(withQuote({ spreadPct: 1.5 })).verdict).toBe("caution");
    expect(assessTradability(withQuote({ spreadPct: 1.6 }))).toEqual({ verdict: "poor", reasons: ["wide spread 1.6%"] });
  });

  test("a missing or stale quote is unknown, never good", () => {
    expect(assessTradability({ ...good, quote: undefined })).toEqual({ verdict: "unknown", reasons: ["no fresh quote"] });
    expect(assessTradability(withQuote({ ageSecs: 60 })).verdict).toBe("good");
    expect(assessTradability(withQuote({ ageSecs: 61 }))).toEqual({ verdict: "unknown", reasons: ["no fresh quote"] });
  });

  test("dollar volume thresholds: under $500K poor, under $2M caution, missing unknown", () => {
    expect(assessTradability(withDollars(499_000))).toEqual({ verdict: "poor", reasons: ["thin: $499K traded"] });
    expect(assessTradability(withDollars(500_000)).verdict).toBe("caution");
    expect(assessTradability(withDollars(1_999_999)).verdict).toBe("caution");
    expect(assessTradability(withDollars(2_000_000)).verdict).toBe("good");
    expect(assessTradability(withDollars(undefined)).verdict).toBe("unknown");
  });

  test("halt proximity counts only while LULD is in effect", () => {
    expect(assessTradability(withHalt({ proximityRatio: 0.8 }))).toEqual({ verdict: "poor", reasons: ["near halt band"] });
    expect(assessTradability(withHalt({ proximityRatio: 0.79 })).verdict).toBe("caution");
    expect(assessTradability(withHalt({ proximityRatio: 0.5 })).verdict).toBe("caution");
    expect(assessTradability(withHalt({ proximityRatio: 0.49 })).verdict).toBe("good");
    expect(assessTradability(withHalt({ proximityRatio: 0.95, luldInEffect: false })).verdict).toBe("good");
    expect(assessTradability({ ...good, halt: undefined })).toEqual({ verdict: "good", reasons: [] });
  });

  test("verdict is the worst contribution: poor > caution > unknown > good", () => {
    // unknown + caution -> caution
    expect(assessTradability({ ...withDollars(1_000_000), quote: undefined }).verdict).toBe("caution");
    // unknown + poor -> poor
    expect(assessTradability({ ...withDollars(100_000), quote: undefined }).verdict).toBe("poor");
    // caution + poor -> poor, and every reason is kept
    const mixed = assessTradability({ ...withHalt({ proximityRatio: 0.9 }), quote: { ...good.quote!, spreadPct: 1 } });
    expect(mixed).toEqual({ verdict: "poor", reasons: ["spread 1.0%", "near halt band"] });
    // unknown alone beats good
    expect(assessTradability(withDollars(undefined)).reasons).toEqual(["dollar volume unknown"]);
  });
});
