import { describe, expect, test } from "bun:test";
import type { JournalEntry, ShortlistCandidate } from "@stockspotter/shared-types";
import {
  activeUpperBand, buildJournalEntry, closeJournalEntry, decodeJournal, DEFAULT_PLAN_RISK, formatEtDay, formatMoney, formatPriceInput, formatR, freshSpreadPct,
  isValidJournalId, journalPayload, newJournalId, parseNumberInput, PLAN_RISK_KEY, planDefaults, readPlanRisk, savePlanRisk,
  withJournalNotes,
} from "./journal";

const candidate: ShortlistCandidate = {
  rank: 2, symbol: "ABCD", score: 150, price: 4.2, gapPct: 30, relativeVolume: 5, sessionVolume: 2_000_000,
  funnelPassed: true, funnelAt: "2026-10-15T13:44:30Z",
  levels: { premarketHigh: 4.5, premarketLow: 3.1, vwap: 3.9, dollarVolume: 8_000_000, barsComplete: true },
  quote: { bid: 4.19, ask: 4.213, spreadPct: 0.4, ageSecs: 5 },
  halt: { level: "calm", proximityRatio: 0.2, upperBand: 4.6, lowerBand: 3.8, distanceToUpperPct: 9, estimated: false, luldInEffect: true, ageSecs: 1 },
  trackRecord: {
    profile: "25-50% day change, 3-10x relative volume", n: 40, sufficient: true, targetRate: 0.42, stopFirstRate: 0.5,
    targetPct: 10, stopPct: 5, decisionTimeEt: "09:35", sourceNote: "test", generatedAt: "2026-10-15T00:00:00Z",
  },
};
const NOW = "2026-10-15T14:00:00.000Z";

describe("plan defaults", () => {
  test("entry at the fresh ask, stop at the nearest level below it", () => {
    expect(planDefaults(candidate)).toEqual({ entry: "4.21", entryBasis: "ask", stop: "3.90", stopBasis: "VWAP" });
  });

  test("entry falls back to the last price when the quote is stale or missing", () => {
    const stale = { ...candidate, quote: { ...candidate.quote!, ageSecs: 61 } };
    expect(planDefaults(stale).entry).toBe("4.20");
    expect(planDefaults(stale).entryBasis).toBe("last price");
    const none = { ...candidate, quote: undefined, levels: { barsComplete: false }, halt: undefined };
    expect(planDefaults(none)).toEqual({ entry: "4.20", entryBasis: "last price", stop: "3.99", stopBasis: "5% below entry" });
  });

  test("sub-dollar prices keep four decimals", () => {
    expect(formatPriceInput(0.51234)).toBe("0.5123");
    expect(formatPriceInput(12)).toBe("12.00");
    expect(formatPriceInput(Number.NaN)).toBe("");
  });

  test("spread and upper band only when they apply", () => {
    expect(freshSpreadPct(candidate)).toBe(0.4);
    expect(freshSpreadPct({ ...candidate, quote: { ...candidate.quote!, ageSecs: 90 } })).toBeUndefined();
    expect(activeUpperBand(candidate)).toBe(4.6);
    expect(activeUpperBand({ ...candidate, halt: { ...candidate.halt!, luldInEffect: false } })).toBeUndefined();
  });

  test("parseNumberInput reads blanks and junk as undefined", () => {
    expect(parseNumberInput(" 4.5 ")).toBe(4.5);
    expect(parseNumberInput("")).toBeUndefined();
    expect(parseNumberInput("abc")).toBeUndefined();
  });

  test("risk preference defaults to 100, persists, and survives throwing storage", () => {
    const store = new Map<string, string>();
    const storage = { getItem: (k: string) => store.get(k) ?? null, setItem: (k: string, v: string) => void store.set(k, v) };
    expect(readPlanRisk(storage)).toBe(DEFAULT_PLAN_RISK);
    expect(savePlanRisk(storage, 250)).toBe(true);
    expect(store.get(PLAN_RISK_KEY)).toBe("250");
    expect(readPlanRisk(storage)).toBe(250);
    expect(savePlanRisk(storage, -1)).toBe(false);
    store.set(PLAN_RISK_KEY, "nonsense");
    expect(readPlanRisk(storage)).toBe(DEFAULT_PLAN_RISK);
    const broken = { getItem: () => { throw new Error("blocked"); }, setItem: () => { throw new Error("blocked"); } };
    expect(readPlanRisk(broken)).toBe(DEFAULT_PLAN_RISK);
    expect(savePlanRisk(broken, 50)).toBe(false);
  });
});

describe("journal entries", () => {
  test("ids match the server's rule, with or without randomUUID", () => {
    expect(isValidJournalId(newJournalId())).toBe(true);
    const zeros = <T extends ArrayBufferView | null>(a: T) => a;
    const fallback = newJournalId({ getRandomValues: zeros });
    expect(fallback).toBe("0".repeat(32));
    expect(isValidJournalId(newJournalId({ getRandomValues: zeros, randomUUID: () => "{abc-123}" }))).toBe(true);
    expect(isValidJournalId("has space")).toBe(false);
    expect(isValidJournalId("x".repeat(65))).toBe(false);
  });

  test("buildJournalEntry carries exactly the JournalEntry fields", () => {
    const e = buildJournalEntry({ id: "t1", candidate, entry: 4.21, stop: 3.9, shares: 322, nowIso: NOW });
    expect(e).toEqual({
      id: "t1", symbol: "ABCD", createdAt: NOW, updatedAt: NOW, entry: 4.21, stop: 3.9, shares: 322,
      snapshot: { rank: 2, gapPct: 30, relativeVolume: 5, spreadPct: 0.4, tradability: "good", profile: candidate.trackRecord!.profile, targetRate: 0.42 },
    });
    expect("target" in e).toBe(false);
    expect(buildJournalEntry({ id: "t1", candidate, entry: 4.21, stop: 3.9, target: 5, shares: 1, nowIso: NOW }).target).toBe(5);
  });

  test("the snapshot leaves out what is unknown", () => {
    const thin = { ...candidate, quote: undefined, trackRecord: undefined };
    const e = buildJournalEntry({ id: "t2", candidate: thin, entry: 4.2, stop: 4, shares: 500, nowIso: NOW });
    expect(e.snapshot).toEqual({ rank: 2, gapPct: 30, relativeVolume: 5, tradability: "unknown" });
  });

  const open: JournalEntry = { id: "t1", symbol: "ABCD", createdAt: NOW, updatedAt: NOW, entry: 4, stop: 3.8, shares: 500 };

  test("closing keeps the id and stamps the exit and update time", () => {
    const later = "2026-10-15T15:00:00.000Z";
    expect(closeJournalEntry(open, 4.4, later)).toEqual({ ...open, exit: { price: 4.4, at: later }, updatedAt: later });
  });

  test("notes are set, capped, and removed when blank", () => {
    const later = "2026-10-15T15:00:00.000Z";
    const noted = withJournalNotes(open, "chased it", later);
    expect(noted).toEqual({ ...open, notes: "chased it", updatedAt: later });
    expect(withJournalNotes(open, "x".repeat(2100), later).notes?.length).toBe(2000);
    expect("notes" in withJournalNotes(noted, "   ", later)).toBe(false);
  });

  test("journalPayload strips anything that isn't a JournalEntry field", () => {
    const extra = { ...open, serverOnly: 1, snapshot: { gapPct: 1, relativeVolume: 2, junk: true }, exit: { price: 4, at: NOW, why: "x" } } as unknown as JournalEntry;
    expect(journalPayload(extra)).toEqual({ ...open, snapshot: { gapPct: 1, relativeVolume: 2 }, exit: { price: 4, at: NOW } });
  });
});

describe("decodeJournal", () => {
  test("keeps well-formed entries and drops malformed ones", () => {
    const good = { ...open(), exit: { price: 4.4, at: NOW }, notes: "ok", target: 5 };
    const decoded = decodeJournal([good, { id: "bad" }, null, { ...open(), entry: "4" }, { ...open(), exit: { price: "x" } }]);
    expect(decoded).toEqual([good, open()]);
  });

  test("rejects a body that isn't a list", () => {
    expect(decodeJournal({ entries: [] })).toBeNull();
    expect(decodeJournal("<html>")).toBeNull();
    expect(decodeJournal([])).toEqual([]);
  });

  function open(): JournalEntry {
    return { id: "t1", symbol: "ABCD", createdAt: NOW, updatedAt: NOW, entry: 4, stop: 3.8, shares: 500 };
  }
});

test("money and R read as signed amounts", () => {
  expect(formatMoney(1234.5)).toBe("$1,234.50");
  expect(formatMoney(-80)).toBe("-$80.00");
  expect(formatMoney(12, true)).toBe("+$12.00");
  expect(formatMoney(0, true)).toBe("$0.00");
  expect(formatMoney(Number.NaN)).toBe("—");
  expect(formatR(1.254)).toBe("+1.25R");
  expect(formatR(-1)).toBe("-1.00R");
  expect(formatR(0)).toBe("0.00R");
});

test("formatEtDay reads the day in New York time", () => {
  expect(formatEtDay("2026-10-16T02:30:00Z")).toBe("Oct 15");
  expect(formatEtDay("bad")).toBe("");
});
