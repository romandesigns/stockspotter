import { describe, expect, test } from "bun:test";
import { computePlan, suggestStop } from "./plan";
import { journalResult, summarizeJournal, type JournalEntry } from "./journal";
import type { ShortlistCandidate } from "./shortlist";

describe("computePlan", () => {
  test("sizes shares from the dollar risk and the stop distance", () => {
    const r = computePlan({ entry: 4.0, stop: 3.8, riskDollars: 100 });
    expect(r.ok).toBe(true);
    if (!r.ok) return;
    expect(r.plan.shares).toBe(500);
    expect(r.plan.positionDollars).toBeCloseTo(2000);
    expect(r.plan.warnings).toContain("No fresh spread: the cost of crossing it is not included.");
  });

  test("adds the spread to the risk and warns when it is a large share of it", () => {
    const r = computePlan({ entry: 4.0, stop: 3.8, riskDollars: 100, spreadPct: 1.5 });
    if (!r.ok) throw new Error(r.error);
    expect(r.plan.spreadCostDollars).toBeCloseTo(30);
    expect(r.plan.totalRiskDollars).toBeCloseTo(130);
    expect(r.plan.warnings.some((w) => w.includes("30% of the planned risk"))).toBe(true);
  });

  test("flags a stop inside the spread and a target past the halt band", () => {
    const r = computePlan({ entry: 4.0, stop: 3.99, riskDollars: 100, spreadPct: 0.5, target: 4.5, haltUpperBand: 4.4 });
    if (!r.ok) throw new Error(r.error);
    expect(r.plan.rewardToRisk).toBeCloseTo(50);
    expect(r.plan.warnings.some((w) => w.includes("inside the spread"))).toBe(true);
    expect(r.plan.warnings.some((w) => w.includes("beyond the halt band"))).toBe(true);
  });

  test("refuses impossible plans with a reason", () => {
    expect(computePlan({ entry: 4, stop: 4, riskDollars: 100 })).toEqual({ ok: false, error: "The stop must be below the entry." });
    expect(computePlan({ entry: 4, stop: 3, riskDollars: 0.5 }).ok).toBe(false);
    expect(computePlan({ entry: Number.NaN, stop: 3, riskDollars: 100 }).ok).toBe(false);
  });
});

describe("suggestStop", () => {
  const c = (levels: Partial<ShortlistCandidate["levels"]>, spreadPct = 0.5) =>
    ({ levels: { barsComplete: true, ...levels }, quote: { bid: 0, ask: 0, spreadPct, ageSecs: 1 } }) as unknown as ShortlistCandidate;

  test("picks the nearest key level more than a spread below entry", () => {
    expect(suggestStop(c({ vwap: 3.9, premarketHigh: 3.98, premarketLow: 3.0 }), 4.0)).toEqual({ price: 3.9, basis: "VWAP" });
  });

  test("falls back to 5% below entry", () => {
    const s = suggestStop(c({ vwap: 4.2 }), 4.0);
    expect(s.basis).toBe("5% below entry");
    expect(s.price).toBeCloseTo(3.8);
  });
});

describe("journal", () => {
  const e = (exit?: number): JournalEntry => ({
    id: "1", symbol: "AAA", createdAt: "2026-10-15T13:40:00Z", updatedAt: "2026-10-15T13:40:00Z", entry: 4, stop: 3.8, shares: 100,
    exit: exit === undefined ? undefined : { price: exit, at: "2026-10-15T14:00:00Z" },
  });

  test("results in dollars and R, only once closed", () => {
    expect(journalResult(e())).toBeNull();
    expect(journalResult(e(4.4))).toEqual({ pnlDollars: expect.closeTo(40) as unknown as number, rMultiple: expect.closeTo(2) as unknown as number });
  });

  test("summary counts wins over closed trades", () => {
    const s = summarizeJournal([e(4.4), e(3.8), e()]);
    expect(s.trades).toBe(3);
    expect(s.closed).toBe(2);
    expect(s.wins).toBe(1);
    expect(s.winRate).toBe(0.5);
    expect(s.totalPnlDollars).toBeCloseTo(20);
    expect(s.averageR).toBeCloseTo(0.5);
  });
});
