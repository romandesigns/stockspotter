// Run from the repo root: bun test apps/mobile/src/shortlist.test.ts
// (mobile has no test runner of its own; this module is pure, so bun:test
// runs it directly). Excluded from the mobile tsconfig, which has no bun types.
import { describe, expect, test } from "bun:test";
import type { JournalEntry, ShortlistCandidate } from "@stockspotter/shared-types";
import {
  JOURNAL_ID_PATTERN,
  buildJournalEntry,
  closeJournalEntry,
  decodeCandidate,
  decodeJournal,
  decodeShortlist,
  decodeTrackRecord,
  formatEtDateTime,
  formatEtTime,
  formatShortlistPrice,
  formatSignedPct,
  newJournalId,
  parsePositive,
  planDefaults,
  planFromForm,
  priceInput,
  shortlistObservedAt,
  updateJournalNotes,
  upsertJournalEntry,
} from "./shortlist";

function rawCandidate(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    rank: 1,
    symbol: "ABCD",
    score: 500,
    price: 4,
    gapPct: 50,
    relativeVolume: 10,
    sessionVolume: 5_000_000,
    funnelPassed: true,
    funnelAt: "2026-10-15T13:30:00Z",
    levels: { barsComplete: true, vwap: 3.8, premarketHigh: 4.2, premarketLow: 3.1, dollarVolume: 20_000_000 },
    quote: { bid: 3.99, ask: 4.01, spreadPct: 0.5, ageSecs: 2 },
    ...overrides,
  };
}

function candidate(overrides: Record<string, unknown> = {}): ShortlistCandidate {
  const c = decodeCandidate(rawCandidate(overrides));
  if (!c) throw new Error("fixture did not decode");
  return c;
}

describe("decodeShortlist", () => {
  test("rejects bodies that are not a shortlist", () => {
    expect(decodeShortlist(null)).toBeNull();
    expect(decodeShortlist("<html>")).toBeNull();
    expect(decodeShortlist({ candidates: [] })).toBeNull();
    expect(decodeShortlist({ generatedAt: "x", candidates: {} })).toBeNull();
  });

  test("drops malformed candidates, sorts by rank, de-duplicates", () => {
    const body = decodeShortlist({
      generatedAt: "2026-10-15T14:00:00Z",
      marketDay: "2026-10-15",
      session: "regular",
      rankedBy: "gapPct x relativeVolume",
      poolSize: 40,
      excluded: { outsidePriceRange: 3, notGappingUp: "x" },
      candidates: [
        rawCandidate({ rank: 3, symbol: "CCC" }),
        rawCandidate({ rank: 1, symbol: "AAA" }),
        rawCandidate({ rank: 2, symbol: "BBB", price: "4" }), // malformed price
        rawCandidate({ rank: 4, symbol: "" }), // no symbol
        rawCandidate({ rank: 5, symbol: "DDD", gapPct: Number.NaN }),
        rawCandidate({ rank: 6, symbol: "AAA" }), // duplicate
        "garbage",
      ],
    });
    expect(body).not.toBeNull();
    expect(body!.candidates.map((c) => c.symbol)).toEqual(["AAA", "CCC"]);
    expect(body!.session).toBe("regular");
    expect(body!.poolSize).toBe(40);
    expect(body!.excluded).toEqual({ outsidePriceRange: 3, notGappingUp: 0, noRelativeVolume: 0 });
  });

  test("unknown session falls back, missing optional blocks become undefined", () => {
    const body = decodeShortlist({ generatedAt: "2026-10-15T14:00:00Z", session: "lunch", candidates: [rawCandidate({ quote: { bid: 1 }, levels: undefined, halt: 5 })] });
    const c = body!.candidates[0];
    expect(body!.session).toBe("overnight");
    expect(c.quote).toBeUndefined();
    expect(c.halt).toBeUndefined();
    expect(c.levels.barsComplete).toBe(false);
    expect(c.catalyst).toBeUndefined();
    expect(c.trackRecord).toBeUndefined();
  });

  test("observed time is the server's generatedAt", () => {
    const body = decodeShortlist({ generatedAt: "2026-10-15T14:00:00Z", candidates: [] });
    expect(shortlistObservedAt(body)?.toISOString()).toBe("2026-10-15T14:00:00.000Z");
    expect(shortlistObservedAt(null)).toBeNull();
  });
});

describe("decodeTrackRecord", () => {
  const base = { profile: "50%+, 10x+ RVOL", n: 47, targetRate: 0.32, stopFirstRate: 0.68, medianCloseReturnNetPct: -16.5, medianMaxAdversePct: -9, targetPct: 10, stopPct: 5, decisionTimeEt: "09:35", sourceNote: "2026 cache", generatedAt: "2026-10-15T00:00:00Z" };

  test("keeps rates only when sufficient === true", () => {
    expect(decodeTrackRecord({ ...base, sufficient: true })?.targetRate).toBe(0.32);
    const thin = decodeTrackRecord({ ...base, n: 12, sufficient: false });
    expect(thin?.sufficient).toBe(false);
    expect(thin?.n).toBe(12);
    expect(thin?.targetRate).toBeUndefined();
    expect(thin?.stopFirstRate).toBeUndefined();
    expect(thin?.medianCloseReturnNetPct).toBeUndefined();
    // A truthy non-boolean is not "sufficient".
    expect(decodeTrackRecord({ ...base, sufficient: "yes" })?.targetRate).toBeUndefined();
  });

  test("rejects records without a profile or count, and out-of-range rates", () => {
    expect(decodeTrackRecord({ ...base, profile: undefined })).toBeUndefined();
    expect(decodeTrackRecord({ ...base, n: "47" })).toBeUndefined();
    expect(decodeTrackRecord({ ...base, sufficient: true, targetRate: 32 })?.targetRate).toBeUndefined();
  });
});

describe("formatting", () => {
  test("prices: 4 decimals under $1, else 2", () => {
    expect(formatShortlistPrice(0.4567)).toBe("$0.4567");
    expect(formatShortlistPrice(12.345)).toBe("$12.35");
    expect(formatShortlistPrice(undefined)).toBe("—");
    expect(priceInput(0.12)).toBe("0.1200");
    expect(priceInput(3.8)).toBe("3.80");
  });

  test("signed percent", () => {
    expect(formatSignedPct(45.23)).toBe("+45.2%");
    expect(formatSignedPct(-5)).toBe("−5.0%");
  });

  test("ET times regardless of the device zone", () => {
    expect(formatEtTime("2026-10-15T13:41:00Z")).toBe("9:41 AM ET");
    expect(formatEtDateTime("2026-10-15T20:05:00Z")).toBe("Oct 15, 4:05 PM ET");
    expect(formatEtTime("nope")).toBe("—");
  });

  test("parsePositive", () => {
    expect(parsePositive("4.01")).toBe(4.01);
    expect(parsePositive("$1,250")).toBe(1250);
    expect(parsePositive(".5")).toBe(0.5);
    expect(Number.isNaN(parsePositive(""))).toBe(true);
    expect(Number.isNaN(parsePositive("0"))).toBe(true);
    expect(Number.isNaN(parsePositive("-3"))).toBe(true);
    expect(Number.isNaN(parsePositive("4.0.1"))).toBe(true);
    expect(Number.isNaN(parsePositive("1e3"))).toBe(true);
  });
});

describe("plan defaults", () => {
  test("entry is the fresh ask, stop the nearest level below", () => {
    const d = planDefaults(candidate());
    expect(d.entry).toBe(4.01);
    expect(d.entryBasis).toBe("ask");
    expect(d.stop).toBe(3.8);
    expect(d.stopBasis).toBe("VWAP");
  });

  test("a stale quote falls back to the last price", () => {
    const d = planDefaults(candidate({ quote: { bid: 3.9, ask: 4.5, spreadPct: 2, ageSecs: 61 } }));
    expect(d.entry).toBe(4);
    expect(d.entryBasis).toBe("last price");
  });

  test("planFromForm sizes the position and validates the target", () => {
    const c = candidate();
    const r = planFromForm(c, { entry: "4.00", stop: "3.80", target: "", risk: "100" });
    expect(r.ok).toBe(true);
    if (r.ok) expect(r.plan.shares).toBe(500);
    const bad = planFromForm(c, { entry: "4", stop: "3.8", target: "abc", risk: "100" });
    expect(bad.ok).toBe(false);
    expect(planFromForm(c, { entry: "4", stop: "4.2", target: "", risk: "100" }).ok).toBe(false);
  });
});

describe("journal", () => {
  test("ids always satisfy the server rule", () => {
    for (let i = 0; i < 200; i++) expect(JOURNAL_ID_PATTERN.test(newJournalId())).toBe(true);
    const fixed = newJournalId(0, (n) => new Uint8Array(n).fill(255));
    expect(JOURNAL_ID_PATTERN.test(fixed)).toBe(true);
    expect(fixed.length).toBeLessThanOrEqual(64);
    expect(newJournalId()).not.toBe(newJournalId());
  });

  test("buildJournalEntry snapshots the card and omits an absent target", () => {
    const c = candidate({
      trackRecord: { profile: "50%+, 10x+ RVOL", n: 47, sufficient: true, targetRate: 0.32, stopFirstRate: 0.68, targetPct: 10, stopPct: 5, decisionTimeEt: "09:35", sourceNote: "", generatedAt: "" },
    });
    const e = buildJournalEntry({ id: "m-1-abc", candidate: c, entry: 4.01, stop: 3.8, shares: 476, now: new Date("2026-10-15T13:41:00Z") });
    expect(e.createdAt).toBe("2026-10-15T13:41:00.000Z");
    expect(e.updatedAt).toBe(e.createdAt);
    expect("target" in e).toBe(false);
    expect(e.snapshot).toEqual({ rank: 1, gapPct: 50, relativeVolume: 10, tradability: "good", spreadPct: 0.5, profile: "50%+, 10x+ RVOL", targetRate: 0.32 });
    // Round-trips through JSON as a valid body.
    expect(decodeJournal(JSON.parse(JSON.stringify([e])))).toEqual([e]);
  });

  test("snapshot leaves out a stale spread and an insufficient rate", () => {
    const c = candidate({
      quote: { bid: 3.9, ask: 4.1, spreadPct: 5, ageSecs: 300 },
      trackRecord: { profile: "0–10%, <1x RVOL", n: 4, sufficient: false, targetRate: 0.9, targetPct: 10, stopPct: 5, decisionTimeEt: "09:35", sourceNote: "", generatedAt: "" },
    });
    const e = buildJournalEntry({ id: "x", candidate: c, entry: 4, stop: 3.8, target: 4.4, shares: 500, now: new Date() });
    expect(e.target).toBe(4.4);
    expect(e.snapshot?.spreadPct).toBeUndefined();
    expect(e.snapshot?.targetRate).toBeUndefined();
    expect(e.snapshot?.profile).toBe("0–10%, <1x RVOL");
  });

  test("close and notes keep the id and bump updatedAt", () => {
    const e: JournalEntry = { id: "t1", symbol: "ABCD", createdAt: "2026-10-15T13:41:00Z", updatedAt: "2026-10-15T13:41:00Z", entry: 4, stop: 3.8, shares: 500 };
    const now = new Date("2026-10-15T15:00:00Z");
    const closed = closeJournalEntry(e, 4.4, now);
    expect(closed.id).toBe("t1");
    expect(closed.exit).toEqual({ price: 4.4, at: "2026-10-15T15:00:00.000Z" });
    expect(closed.updatedAt).toBe("2026-10-15T15:00:00.000Z");
    const noted = updateJournalNotes(closed, "  chased it  ", now);
    expect(noted.notes).toBe("chased it");
    expect(noted.exit).toEqual(closed.exit);
    expect("notes" in updateJournalNotes(noted, "   ", now)).toBe(false);
  });

  test("decodeJournal drops bad rows, sorts newest first; upsert replaces by id", () => {
    const rows = decodeJournal([
      { id: "old", symbol: "AAA", createdAt: "2026-10-14T13:00:00Z", updatedAt: "2026-10-14T13:00:00Z", entry: 2, stop: 1.9, shares: 100 },
      { id: "new", symbol: "BBB", createdAt: "2026-10-15T13:00:00Z", updatedAt: "2026-10-15T13:00:00Z", entry: 3, stop: 2.9, shares: 100, exit: { price: "x" } },
      { id: "bad id!", symbol: "CCC", createdAt: "2026-10-15T13:00:00Z", entry: 3, stop: 2.9, shares: 100 },
      { id: "nostop", symbol: "DDD", createdAt: "2026-10-15T13:00:00Z", entry: 3, shares: 100 },
    ]);
    expect(rows?.map((r) => r.id)).toEqual(["new", "old"]);
    expect(rows?.[0].exit).toBeUndefined();
    expect(decodeJournal({})).toBeNull();
    const updated = upsertJournalEntry(rows!, { ...rows![1], notes: "n" });
    expect(updated.map((r) => r.id)).toEqual(["new", "old"]);
    expect(updated[1].notes).toBe("n");
  });
});
