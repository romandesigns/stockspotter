// Web/native parity for the experimental lane's wire decoder, plus the
// native-only pure helpers around it.
//
// apps/mobile deliberately carries its own copy of the decoder instead of
// promoting the contract back into @stockspotter/shared-types (which every
// normal client imports, and which this lane is supposed to stay out of).
// The copy's only real risk is drift, so this test reaches across into
// apps/mobile's actual module and runs BOTH decoders over the same wire
// vectors -- the failure it exists to catch, one side accepting a frame the
// other rejects, is invisible to any test that imports only one of them.
//
// Same pattern and same justification as attentionParity.test.ts: both
// mobile modules under test are pure TypeScript with no React Native in the
// import graph, and apps/mobile has no test runner of its own, so the
// assertion lives in the suite that actually runs (`bun run test:client`).

import { describe, expect, test } from "bun:test";
import { decodeExperimentalMessage as decodeWeb, reduceExperimentalRows as reduceWeb } from "./experimentalStrategies";
import {
  decodeExperimentalMessage as decodeNative,
  reduceExperimentalRows as reduceNative,
  type ExperimentalStrategyStatus,
  type ExperimentalStrategyUpdate,
} from "../../../mobile/src/lib/experimentalStrategies";
import {
  EXPERIMENTAL_RECONNECT_BASE_MS,
  EXPERIMENTAL_RECONNECT_CEILING_MS,
  EXPERIMENTAL_STALE_AFTER_MS,
  experimentalBackoff,
  experimentalCoverageNote,
  experimentalEvidenceLine,
  experimentalFeedIncomplete,
  experimentalFeedNote,
  experimentalObservationAgeSeconds,
  experimentalPanels,
} from "../../../mobile/src/lib/experimentalView";

function update(): ExperimentalStrategyUpdate {
  return { type:"experimental_strategy_update",contractVersion:"iac-extreme-minute-v1",eventId:"run:TEST:1",lifecycleId:"run:TEST:0:0",supersedes:null,
    symbol:"TEST",strategy:"iac",kind:"candidate",marketDay:"2026-08-26",eventTimeMs:1000,receivedAtMs:1100,decisionAtMs:1200,
    anchorAvailableAtMs:1200,anchorEventTimeMs:1000,anchorPrice:30,observedPrice:30,reason:"research_thresholds_met",
    evidence:{baselineBars:20,baselineRange:1,baselineVolume:100,compressionRatio:0.5,expansionRatio:2,participationRatio:3,
      moveFraction:0.03,pathEfficiency:null,floatEvidence:"unavailable"} };
}
function status(overrides: Partial<ExperimentalStrategyStatus> = {}): ExperimentalStrategyStatus {
  return { type:"experimental_strategy_status",iacEnabled:true,extremeMomentumEnabled:true,healthy:true,sourceAtMs:10_000,
    laggedEvents:0,evictions:0,processingWarnings:0,activeSymbols:7,reason:"observing",coverage:"observed_subscribed_symbols",...overrides };
}

// Every frame shape that matters: the happy path, each independently
// rejectable field, the two causality rules, and both non-update message
// types. Anything the two decoders disagree on anywhere in here is drift.
const VECTORS: unknown[] = [
  update(),
  { ...update(), strategy: "extreme_momentum" },
  { ...update(), kind: "held", supersedes: "run:TEST:0" },
  { ...update(), observedPrice: 50 },
  { ...update(), evidence: { ...update().evidence, compressionRatio: null, pathEfficiency: 0.9 } },
  { ...update(), contractVersion: "iac-extreme-minute-v2" },
  { ...update(), symbol: "BAD SYMBOL" },
  { ...update(), symbol: "" },
  { ...update(), marketDay: "26-08-26" },
  { ...update(), kind: "profit" },
  { ...update(), strategy: "scalp" },
  { ...update(), anchorPrice: 0 },
  { ...update(), observedPrice: Number.NaN },
  { ...update(), eventTimeMs: -1 },
  { ...update(), decisionAtMs: 1000 },
  { ...update(), anchorAvailableAtMs: 1300 },
  { ...update(), supersedes: "" },
  { ...update(), evidence: {} },
  { ...update(), evidence: { ...update().evidence, baselineBars: 1 } },
  { ...update(), evidence: { ...update().evidence, floatEvidence: "known" } },
  status(),
  status({ budgetBreaches: 3, ignoredProvisional: 2, recentEvictedSymbols: ["AAA", "BBB"] }),
  status({ sourceAtMs: null, healthy: false, reason: "upstream_feed_gap" }),
  { ...status(), coverage: "all_symbols" },
  { ...status(), reason: "" },
  { ...status(), activeSymbols: 1.5 },
  { ...status(), recentEvictedSymbols: [1] },
  { type: "experimental_strategy_gap" },
  { type: "bar_update", symbol: "TEST" },
  { type: "experimental_strategy_update" },
  null,
  [],
  "experimental_strategy_update",
];

describe("experimental lane web/native parity", () => {
  test("both decoders agree frame by frame", () => {
    for (const vector of VECTORS) {
      expect(decodeNative(vector)).toEqual(decodeWeb(vector));
    }
  });

  test("the vector set actually exercises both outcomes", () => {
    const accepted = VECTORS.filter((v) => decodeWeb(v) !== null).length;
    expect(accepted).toBeGreaterThan(5);
    expect(VECTORS.length - accepted).toBeGreaterThan(5);
  });

  test("both reducers keep the same immutable lifecycle history", () => {
    const candidate = update();
    const held = { ...candidate, eventId: "run:TEST:2", supersedes: candidate.eventId, kind: "held" as const, decisionAtMs: 5000 };
    expect(reduceNative([candidate], held)).toEqual(reduceWeb([candidate], held));
    // A replayed earlier snapshot must not overwrite the later one, and a
    // re-delivered identical event must not duplicate the row.
    const rows = reduceNative([candidate], held);
    expect(reduceNative(rows, candidate)).toBe(rows);
    expect(reduceNative(rows, held)).toBe(rows);
    expect(reduceWeb(rows, candidate)).toBe(rows);
  });

  test("native lifecycle list is bounded independently of the normal feed", () => {
    let rows: ExperimentalStrategyUpdate[] = [];
    for (let i = 0; i < 150; i++) rows = reduceNative(rows, { ...update(), eventId: `e${i}`, lifecycleId: `l${i}` });
    expect(rows).toHaveLength(100);
    expect(rows[0].lifecycleId).toBe("l149");
  });
});

describe("native experimental presentation", () => {
  test("backoff grows to a ceiling and stays jittered inside ±20%", () => {
    let delay = EXPERIMENTAL_RECONNECT_BASE_MS;
    const waits: number[] = [];
    for (let i = 0; i < 12; i++) {
      const step = experimentalBackoff(delay, i / 12);
      waits.push(step.waitMs);
      expect(step.waitMs).toBeGreaterThanOrEqual(Math.round(delay * 0.8));
      expect(step.waitMs).toBeLessThanOrEqual(Math.round(delay * 1.2));
      delay = step.nextDelayMs;
    }
    expect(delay).toBe(EXPERIMENTAL_RECONNECT_CEILING_MS);
    expect(Math.max(...waits)).toBeLessThanOrEqual(Math.round(EXPERIMENTAL_RECONNECT_CEILING_MS * 1.2));
    // A hostile or absent random source must not produce a zero/negative
    // wait, which would turn the backoff into a hot reconnect loop.
    for (const random of [Number.NaN, -5, 5]) expect(experimentalBackoff(EXPERIMENTAL_RECONNECT_BASE_MS, random).waitMs).toBeGreaterThan(0);
    expect(experimentalBackoff(0, 0.5).waitMs).toBeGreaterThanOrEqual(Math.round(EXPERIMENTAL_RECONNECT_BASE_MS * 0.8));
    expect(experimentalBackoff(Number.POSITIVE_INFINITY, 0.5).nextDelayMs).toBe(EXPERIMENTAL_RECONNECT_CEILING_MS);
  });

  test("only the strategies the server reports enabled get a panel", () => {
    expect(experimentalPanels(null)).toEqual([]);
    expect(experimentalPanels(status())).toEqual(["iac", "extreme_momentum"]);
    expect(experimentalPanels(status({ extremeMomentumEnabled: false }))).toEqual(["iac"]);
    expect(experimentalPanels(status({ iacEnabled: false, extremeMomentumEnabled: false }))).toEqual([]);
  });

  test("incompleteness defaults to true whenever anything is unknown", () => {
    const base = { connection: "open" as const, status: status(), gap: false, nowMs: 20_000 };
    expect(experimentalFeedIncomplete(base)).toBe(false);
    expect(experimentalFeedIncomplete({ ...base, connection: "connecting" })).toBe(true);
    expect(experimentalFeedIncomplete({ ...base, connection: "closed" })).toBe(true);
    expect(experimentalFeedIncomplete({ ...base, gap: true })).toBe(true);
    expect(experimentalFeedIncomplete({ ...base, status: null })).toBe(true);
    expect(experimentalFeedIncomplete({ ...base, status: status({ healthy: false }) })).toBe(true);
    expect(experimentalFeedIncomplete({ ...base, status: status({ sourceAtMs: null }) })).toBe(true);
    // Exactly at the window it is still complete; one millisecond past it
    // is not -- the panel must not claim freshness it cannot support.
    expect(experimentalFeedIncomplete({ ...base, nowMs: 10_000 + EXPERIMENTAL_STALE_AFTER_MS })).toBe(false);
    expect(experimentalFeedIncomplete({ ...base, nowMs: 10_001 + EXPERIMENTAL_STALE_AFTER_MS })).toBe(true);
  });

  test("status line carries coverage, warmup and server reasons", () => {
    expect(experimentalFeedNote(status(), false)).toBe("Experimental · completed minute bars · observed symbols only");
    expect(experimentalFeedNote(status({ reason: "warming_up" }), false)).toContain("warming up");
    expect(experimentalFeedNote(status({ reason: "processing_budget_exceeded" }), false)).toContain("research feed paused");
    expect(experimentalFeedNote(status(), true)).toContain("feed incomplete or stale");
    // An unrecognised server reason surfaces verbatim rather than vanishing.
    expect(experimentalFeedNote(status({ reason: "future_reason" }), false)).toContain("future_reason");
  });

  test("coverage counters are shown, including the optional ones", () => {
    expect(experimentalCoverageNote(status())).toBe("7 observed · 0 lagged · 0 evicted · 0 warnings");
    const detailed = experimentalCoverageNote(status({ laggedEvents: 2, evictions: 1, processingWarnings: 4, budgetBreaches: 3, ignoredProvisional: 5 }));
    expect(detailed).toContain("2 lagged");
    expect(detailed).toContain("3 budget breaches");
    expect(detailed).toContain("5 provisional ignored");
  });

  test("row evidence always discloses float and non-prediction, age never negative", () => {
    const line = experimentalEvidenceLine(update());
    expect(line).toContain("expansion 2.00×");
    expect(line).toContain("participation 3.00×");
    expect(line).toContain("compression 0.50×");
    expect(line).toContain("float unavailable");
    expect(line).toContain("not a prediction");
    // A null compression ratio is omitted, not rendered as "null×".
    const noCompression = experimentalEvidenceLine({ ...update(), evidence: { ...update().evidence, compressionRatio: null } });
    expect(noCompression).not.toContain("compression");
    expect(noCompression).toContain("float unavailable");
    expect(experimentalObservationAgeSeconds(5_000, 1_000)).toBe(4);
    expect(experimentalObservationAgeSeconds(1_000, 5_000)).toBe(0);
  });
});
