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
  EXPERIMENTAL_CONCERN_LABEL,
  EXPERIMENTAL_RECONNECT_BASE_MS,
  EXPERIMENTAL_RECONNECT_CEILING_MS,
  EXPERIMENTAL_RETENTION_NOTE,
  EXPERIMENTAL_STALE_AFTER_MS,
  experimentalBackoff,
  experimentalCapacityLimited,
  experimentalCapacityNote,
  experimentalCapacitySeen,
  experimentalCoverageNote,
  experimentalEmptyState,
  experimentalEvidenceLine,
  experimentalFeedIncomplete,
  experimentalFeedNote,
  experimentalLimitNote,
  experimentalMissedUpdatesNote,
  experimentalObservationAgeSeconds,
  experimentalPanelConcern,
  experimentalPanels,
  experimentalReadinessLine,
  experimentalReadySymbols,
  experimentalResetNote,
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
  // The capacity/readiness telemetry added by the 2026-10-13 collector
  // capacity amendment. Every field is optional, so "absent" and "zero"
  // are different frames and both decoders have to treat them alike.
  status({ symbolLimit: 16_384, iacReadySymbols: 3, extremeReadySymbols: 7, capacityDroppedUpdates: 4,
    recentCapacityDroppedSymbols: ["ZZZ", "YYY"], idleReclaims: 1, idleResets: 2, acceptedBarUpdates: 900,
    warmupBarUpdates: 40, duplicateBarUpdates: 6, revisionResets: 3, missingMinuteResets: 5,
    invalidBarUpdates: 8, outputLaggedEvents: 11 }),
  // Readiness may equal the observed count, but a history cannot be
  // ready for a symbol that is not being observed at all.
  status({ iacReadySymbols: 7 }),
  { ...status(), iacReadySymbols: 8 },
  { ...status(), extremeReadySymbols: 99 },
  // A ceiling below the count already observed, or below one symbol, is
  // self-contradictory and must not be rendered as a coverage claim.
  { ...status(), symbolLimit: 3 },
  { ...status(), symbolLimit: 0 },
  // The sticky totals are counts: negative, fractional, unsafe, null and
  // non-numeric are each independently rejectable.
  { ...status(), invalidBarUpdates: -1 },
  { ...status(), invalidBarUpdates: 1.5 },
  { ...status(), revisionResets: "3" },
  { ...status(), missingMinuteResets: null },
  { ...status(), outputLaggedEvents: Number.NaN },
  { ...status(), idleResets: Number.MAX_SAFE_INTEGER + 2 },
  { ...status(), duplicateBarUpdates: -0.5 },
  { ...status(), recentCapacityDroppedSymbols: [1] },
  { ...status(), recentCapacityDroppedSymbols: ["WAY_TOO_LONG_SYMBOL_NAME"] },
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

  test("per-strategy readiness is reported separately and never invented", () => {
    const partial = status({ iacReadySymbols: 3, extremeReadySymbols: 0 });
    expect(experimentalReadySymbols(partial, "iac")).toBe(3);
    expect(experimentalReadySymbols(partial, "extreme_momentum")).toBe(0);
    expect(experimentalReadySymbols(status(), "iac")).toBeUndefined();
    expect(experimentalReadinessLine(partial, "iac")).toBe("3 / 7 histories ready · observed feed only");
    expect(experimentalReadinessLine(partial, "extreme_momentum")).toBe("0 / 7 histories ready · observed feed only");
    // A server that reports no readiness at all must not be rendered as
    // "0 ready" -- absent and zero are opposite claims about warmup.
    expect(experimentalReadinessLine(status(), "iac")).toBe("7 observed · observed feed only");
    expect(experimentalReadinessLine(status(), "iac")).not.toContain("ready");
  });

  test("capacity rejection is sticky across a frame that omits the field", () => {
    expect(experimentalCapacityLimited(null)).toBe(false);
    expect(experimentalCapacityLimited(status())).toBe(false);
    expect(experimentalCapacityLimited(status({ capacityDroppedUpdates: 2 }))).toBe(true);
    // `evictions` stays zero under the new policy, so a nonzero one is
    // still worth the warning.
    expect(experimentalCapacityLimited(status({ evictions: 1 }))).toBe(true);
    expect(experimentalCapacityLimited(status({ capacityDroppedUpdates: 0 }))).toBe(false);
    let seen = experimentalCapacitySeen(false, status());
    expect(seen).toBe(false);
    seen = experimentalCapacitySeen(seen, status({ capacityDroppedUpdates: 2 }));
    expect(seen).toBe(true);
    // The latch is the point: the optional field going missing cannot
    // retract a rejection the rows on screen were gathered under.
    expect(experimentalCapacitySeen(seen, status())).toBe(true);
    expect(experimentalCapacitySeen(seen, null)).toBe(true);
  });

  test("empty panels distinguish not-ready from ready-with-no-candidates", () => {
    const base = { status: status(), incomplete: false, capacityLimited: false, ready: undefined as number | undefined };
    expect(experimentalEmptyState({ ...base, status: null }).reason).toBe("connecting");
    expect(experimentalEmptyState({ ...base, incomplete: true }).reason).toBe("incomplete");
    expect(experimentalEmptyState({ ...base, capacityLimited: true }).reason).toBe("incomplete");
    expect(experimentalEmptyState({ ...base, ready: 0 }).reason).toBe("not_ready");
    expect(experimentalEmptyState({ ...base, ready: 4 }).reason).toBe("ready_no_candidates");
    expect(experimentalEmptyState(base).reason).toBe("readiness_unknown");
    // Nothing could be evaluated yet, so the panel must not say it
    // looked and found none -- that would be evidence of absence.
    expect(experimentalEmptyState({ ...base, ready: 0 }).label).not.toContain("No candidates");
    expect(experimentalEmptyState({ ...base, ready: 0 }).label).toContain("continuous minute bars");
    expect(experimentalEmptyState({ ...base, ready: 4 }).label).toContain("ready histories");
    // Incompleteness outranks warmup: a stale or capacity-limited feed
    // cannot claim warmup is the reason it is empty.
    expect(experimentalEmptyState({ ...base, incomplete: true, ready: 0 }).reason).toBe("incomplete");
    expect(experimentalEmptyState({ ...base, capacityLimited: true, ready: 4 }).reason).toBe("incomplete");
  });

  test("one worst-first concern badge per panel, not three", () => {
    const base = { status: status(), incomplete: false, capacityLimited: false, ready: undefined as number | undefined };
    expect(experimentalPanelConcern(base)).toBeNull();
    expect(experimentalPanelConcern({ ...base, ready: 4 })).toBeNull();
    expect(experimentalPanelConcern({ ...base, status: null })).toBe("incomplete");
    expect(experimentalPanelConcern({ ...base, incomplete: true })).toBe("incomplete");
    expect(experimentalPanelConcern({ ...base, capacityLimited: true })).toBe("capacity");
    expect(experimentalPanelConcern({ ...base, ready: 0 })).toBe("warming");
    expect(experimentalPanelConcern({ ...base, ready: 0, capacityLimited: true })).toBe("capacity");
    expect(experimentalPanelConcern({ ...base, ready: 0, capacityLimited: true, incomplete: true })).toBe("incomplete");
    expect(EXPERIMENTAL_CONCERN_LABEL.warming).toBe("warming up");
  });

  test("coverage disclosure carries limit, resets, capacity and retention", () => {
    // No invented ceiling when the server reports none.
    expect(experimentalLimitNote(status())).toBeNull();
    const limit = experimentalLimitNote(status({ symbolLimit: 16_384 }));
    expect(limit).toContain("16,384 symbols");
    expect(limit).toContain("not whole-market coverage");
    // A legacy server reporting none of the reset counters gets no line
    // at all -- an absent counter is not a reported zero, and a row of
    // manufactured zeroes would be a clean-coverage claim nothing made.
    expect(experimentalResetNote(status())).toBeNull();
    // Partially reporting: the absent fields say so, the present ones
    // keep their literal value.
    const partialResets = experimentalResetNote(status({ missingMinuteResets: 5, idleResets: 2 }));
    expect(partialResets).toBe("5 gap resets · revised unavailable · invalid unavailable · 2 idle reset");
    // A genuine zero is a real measurement and stays a literal 0.
    expect(experimentalResetNote(status({ missingMinuteResets: 0, revisionResets: 0, invalidBarUpdates: 0, idleResets: 0 })))
      .toBe("0 gap resets · 0 revised · 0 invalid · 0 idle reset");
    expect(experimentalResetNote(status({ invalidBarUpdates: 0 })))
      .toBe("gap resets unavailable · revised unavailable · 0 invalid · idle reset unavailable");
    expect(experimentalResetNote(status({ missingMinuteResets: 5, revisionResets: 3, invalidBarUpdates: 8, idleResets: 2 })))
      .toBe("5 gap resets · 3 revised · 8 invalid · 2 idle reset");
    expect(experimentalCapacityNote(status())).toBeNull();
    const capacity = experimentalCapacityNote(status({ capacityDroppedUpdates: 4, evictions: 2, recentCapacityDroppedSymbols: ["ZZZ", "YYY"] }));
    expect(capacity).toContain("4 updates rejected at capacity");
    expect(capacity).toContain("2 histories evicted");
    expect(capacity).toContain("ZZZ, YYY");
    // Under the new policy evictions are zero, so the note must not
    // claim any.
    expect(experimentalCapacityNote(status({ capacityDroppedUpdates: 4 }))).not.toContain("evicted");
    // An old evictions-only server reports real evictions and no
    // rejection count. Saying "0 updates rejected" beside them would
    // read as "capacity turned nothing away" -- the opposite of the frame.
    const evictionsOnly = experimentalCapacityNote(status({ evictions: 2 }));
    expect(evictionsOnly).toBe("2 histories evicted");
    expect(evictionsOnly).not.toContain("rejected");
    // A reported zero alongside real evictions is literal and stays.
    expect(experimentalCapacityNote(status({ capacityDroppedUpdates: 0, evictions: 2 })))
      .toBe("0 updates rejected at capacity · 2 histories evicted");
    // Latch set but the current frame omits every capacity field: the
    // warning must stand generically, never silently vanish.
    const GENERIC = "Capacity interrupted coverage earlier this session; current rejection count unavailable.";
    expect(experimentalCapacityNote(status(), true)).toBe(GENERIC);
    // ...and a frame that now reports a genuine 0 must not read as a
    // retraction of a rejection already observed.
    expect(experimentalCapacityNote(status({ capacityDroppedUpdates: 0 }), true))
      .toBe("Capacity interrupted coverage earlier this session; the current server reports 0 rejected updates.");
    expect(experimentalCapacityNote(status({ capacityDroppedUpdates: 0 }), false)).toBeNull();
    // With the latch set, live counts still win over the generic text.
    expect(experimentalCapacityNote(status({ capacityDroppedUpdates: 4 }), true)).toBe("4 updates rejected at capacity");
    expect(experimentalMissedUpdatesNote(status())).toBeNull();
    expect(experimentalMissedUpdatesNote(status({ outputLaggedEvents: 0 }))).toBeNull();
    // Aggregate across clients, not this phone's own loss.
    expect(experimentalMissedUpdatesNote(status({ outputLaggedEvents: 11 }))).toBe("11 research updates missed across connected clients");
    expect(EXPERIMENTAL_RETENTION_NOTE).toContain("100 lifecycle observations across both strategies");
    expect(EXPERIMENTAL_RETENTION_NOTE).toContain("latest 100 updates");
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
