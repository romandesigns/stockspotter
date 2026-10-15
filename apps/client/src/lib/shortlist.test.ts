import { describe, expect, test } from "bun:test";
import type { CatalystUpdate, FunnelSignal } from "@stockspotter/shared-types";
import {
  formatEtTime,
  candidateCatalyst, candidateFunnel, decodeShortlist, formatAge, formatDollars, readWorkspaceView, saveWorkspaceView,
  shortlistObservedAt, WORKSPACE_VIEW_KEY,
} from "./shortlist";

const at = "2026-10-15T13:45:00Z";
const candidate = {
  rank: 1, symbol: "ABCD", score: 150, price: 4.2, gapPct: 30, relativeVolume: 5, floatShares: 3_000_000,
  sessionVolume: 2_000_000, funnelPassed: true, funnelAt: "2026-10-15T13:44:30Z",
  levels: { premarketHigh: 4.5, dollarVolume: 8_000_000, barsComplete: true },
  catalyst: { tags: ["earnings"], headlineCount: 2, headline: "Beats", publishedAt: "2026-10-15T12:00:00Z" },
};
const body = {
  generatedAt: at, marketDay: "2026-10-15", session: "premarket", rankedBy: "gapPct*relativeVolume", poolSize: 40,
  excluded: { outsidePriceRange: 3, notGappingUp: 10, noRelativeVolume: 2 },
  candidates: [{ ...candidate, rank: 2, symbol: "WXYZ" }, candidate],
};

describe("decodeShortlist", () => {
  test("keeps a well-formed body and orders candidates by rank", () => {
    const decoded = decodeShortlist(body);
    expect(decoded?.candidates.map((c) => c.symbol)).toEqual(["ABCD", "WXYZ"]);
    expect(decoded?.excluded).toEqual(body.excluded);
    expect(decoded?.session).toBe("premarket");
  });

  test("rejects anything that isn't recognisably a shortlist", () => {
    for (const raw of [null, "<html>", [], { candidates: [] }, { generatedAt: at }]) expect(decodeShortlist(raw)).toBeNull();
  });

  test("drops malformed candidates instead of rendering NaN or $0.00", () => {
    const decoded = decodeShortlist({ ...body, candidates: [candidate, { ...candidate, price: null }, { ...candidate, symbol: "" }, "junk"] });
    expect(decoded?.candidates.map((c) => c.symbol)).toEqual(["ABCD"]);
  });

  test("missing optional blocks normalise to safe defaults", () => {
    const decoded = decodeShortlist({ generatedAt: at, candidates: [{ ...candidate, levels: undefined, catalyst: { headline: "x" } }] });
    expect(decoded).toMatchObject({ marketDay: null, session: "overnight", poolSize: 0, excluded: { outsidePriceRange: 0, notGappingUp: 0, noRelativeVolume: 0 } });
    expect(decoded?.candidates[0].levels).toEqual({ barsComplete: false });
    expect(decoded?.candidates[0].catalyst).toBeUndefined();
  });

  test("age counts from the server's generatedAt, not the poll", () => {
    expect(shortlistObservedAt(decodeShortlist(body))?.toISOString()).toBe(new Date(at).toISOString());
    expect(shortlistObservedAt(decodeShortlist({ ...body, generatedAt: "nope" }))).toBeNull();
  });
});

describe("candidate adapters", () => {
  const c = decodeShortlist(body)!.candidates[0];

  test("funnel: the fresher of live feed and candidate wins, otherwise one is rebuilt at funnelAt", () => {
    const live = { type: "funnel_signal", symbol: "ABCD", timestamp: "2026-10-15T13:44:45Z" } as FunnelSignal;
    expect(candidateFunnel(c, live)).toBe(live);
    // An older live signal loses to the candidate's own, fresher evaluation.
    expect(candidateFunnel(c, { ...live, timestamp: "2026-10-15T13:30:00Z" }).timestamp).toBe(c.funnelAt);
    expect(candidateFunnel(c)).toMatchObject({ timestamp: c.funnelAt, price: 4.2, gapPct: 30, relativeVolume: 5, floatShares: 3_000_000, passed: true });
  });

  test("catalyst: live record wins, candidate's own is observed at generatedAt, none stays undefined", () => {
    const live = { type: "catalyst_update", symbol: "ABCD" } as CatalystUpdate;
    expect(candidateCatalyst(c, at, live)).toBe(live);
    expect(candidateCatalyst(c, at)).toMatchObject({ timestamp: at, catalystTags: ["earnings"], mostRecentHeadline: "Beats", mostRecentPublishedAt: "2026-10-15T12:00:00Z" });
    expect(candidateCatalyst({ ...c, catalyst: undefined }, at)).toBeUndefined();
  });
});

test("formatDollars", () => {
  expect(formatDollars(12_340_000)).toBe("$12.3M");
  expect(formatDollars(480_400)).toBe("$480K");
  expect(formatDollars(900)).toBe("$900");
  expect(formatDollars(2_500_000_000)).toBe("$2.50B");
});

test("formatAge", () => {
  const now = Date.parse(at);
  expect(formatAge("2026-10-15T13:40:00Z", now)).toBe("5m ago");
  expect(formatAge("2026-10-15T10:45:00Z", now)).toBe("3h ago");
  expect(formatAge("2026-10-12T13:45:00Z", now)).toBe("3d ago");
  expect(formatAge(undefined, now)).toBeNull();
  expect(formatAge("bad", now)).toBeNull();
});

test("workspace view preference defaults to shortlist and survives throwing storage", () => {
  const store = new Map<string, string>();
  const storage = { getItem: (k: string) => store.get(k) ?? null, setItem: (k: string, v: string) => void store.set(k, v) };
  expect(readWorkspaceView(storage)).toBe("shortlist");
  expect(saveWorkspaceView(storage, "classic")).toBe(true);
  expect(store.get(WORKSPACE_VIEW_KEY)).toBe("classic");
  expect(readWorkspaceView(storage)).toBe("classic");
  const broken = { getItem: () => { throw new Error("blocked"); }, setItem: () => { throw new Error("blocked"); } };
  expect(readWorkspaceView(broken)).toBe("shortlist");
  expect(saveWorkspaceView(broken, "classic")).toBe(false);
});

describe("formatEtTime", () => {
  test("reads market times in New York time whatever the viewer's zone", () => {
    expect(formatEtTime("2026-10-15T13:41:00Z")).toBe("9:41 AM ET");
    expect(formatEtTime("2026-12-15T14:41:00Z")).toBe("9:41 AM ET");
    expect(formatEtTime("not a time")).toBe("not a time");
  });
});
