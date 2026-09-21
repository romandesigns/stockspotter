import { test, expect } from "bun:test";
import {
  CADENCE_WINDOW,
  MIN_GAPS_FOR_CADENCE,
  QUIET_FLOOR_SECS,
  cadenceThresholds,
  emptyCadence,
  observeUpdate,
  resolveSymbolFreshness,
  type SymbolCadenceState,
} from "@stockspotter/shared-types";
import { resolveChartStatus, formatAge, type FeedGap } from "./feedHealth";

const T0 = 1_789_000_000_000;

/** Feed a series of gaps (seconds) and return the state plus the final time. */
function feed(symbol: string, intervalSecs: number, gaps: number[], start = T0) {
  let st = emptyCadence(symbol, intervalSecs);
  let at = start;
  st = observeUpdate(st, symbol, intervalSecs, at);
  for (const g of gaps) {
    at += g * 1000;
    st = observeUpdate(st, symbol, intervalSecs, at);
  }
  return { state: st, at };
}

// 1
test("an active symbol updating at its own cadence stays live", () => {
  // GRML's measured shape: ~0.53s median gap.
  const { state, at } = feed("GRML", 60, Array(10).fill(0.53));
  const r = resolveSymbolFreshness(state, at + 500);
  expect(r.freshness).toBe("live");
  expect(r.thresholds!.quietAfterSecs).toBe(QUIET_FLOOR_SECS);
});

// 2
test("a sparse but healthy symbol goes quiet, never stale", () => {
  // SONM's measured shape: 17.5s median, 157s legitimate maximum.
  const { state, at } = feed("SONM", 60, [12, 157, 9, 20, 14, 31, 8]);
  const t = cadenceThresholds(state)!;
  expect(t.staleAfterSecs).toBeGreaterThan(157);
  // 60s of silence is well past normal spacing but nowhere near its worst.
  const r = resolveSymbolFreshness(state, at + 60_000);
  expect(r.freshness).toBe("quiet");
  // Even at 150s -- still inside its own observed maximum.
  expect(resolveSymbolFreshness(state, at + 150_000).freshness).toBe("quiet");
});

// 3
test("an active symbol that stops updating becomes stale", () => {
  const { state, at } = feed("BTTC", 60, Array(10).fill(0.59));
  expect(resolveSymbolFreshness(state, at + 10_000).freshness).toBe("live");
  expect(resolveSymbolFreshness(state, at + 30_000).freshness).toBe("quiet");
  expect(resolveSymbolFreshness(state, at + 60_000).freshness).toBe("stale");
});

// 4
test("updates to an unrelated symbol cannot make the displayed symbol live", () => {
  const displayed = feed("RGNT", 60, Array(6).fill(30));
  // Another symbol updates furiously. It shares no state with RGNT, and
  // resolveSymbolFreshness takes no transport/global argument at all.
  let other = emptyCadence("GRML", 60);
  for (let i = 0; i < 50; i++) other = observeUpdate(other, "GRML", 60, displayed.at + i * 100);
  const late = displayed.at + 200_000;
  expect(resolveSymbolFreshness(displayed.state, late).freshness).toBe("stale");
  // And through the combined resolver, with a perfectly healthy transport.
  expect(
    resolveChartStatus({
      transport: "open",
      gap: null,
      earliestBarTimeSeconds: null,
      symbol: resolveSymbolFreshness(displayed.state, late),
    }),
  ).toBe("stale");
});

// 5
test("reconnecting outranks the symbol layer", () => {
  const { state, at } = feed("GRML", 60, Array(8).fill(0.5));
  const fresh = resolveSymbolFreshness(state, at);
  expect(fresh.freshness).toBe("live");
  expect(resolveChartStatus({ transport: "closed", gap: null, earliestBarTimeSeconds: null, symbol: fresh })).toBe("reconnecting");
  expect(resolveChartStatus({ transport: "connecting", gap: null, earliestBarTimeSeconds: null, symbol: fresh })).toBe("connecting");
});

// 6
test("a known gap outranks the symbol layer", () => {
  const gap: FeedGap = { at: "2026-09-21T15:00:00.000Z", reason: "stream_lagged", missedEvents: 6 };
  const { state, at } = feed("GRML", 60, Array(8).fill(0.5));
  const fresh = resolveSymbolFreshness(state, at);
  const spanning = Date.parse("2026-09-21T14:30:00.000Z") / 1000;
  expect(resolveChartStatus({ transport: "open", gap, earliestBarTimeSeconds: spanning, symbol: fresh })).toBe("gap");
  // Documented precedence choice: transport is evaluated before gap.
  expect(resolveChartStatus({ transport: "closed", gap, earliestBarTimeSeconds: spanning, symbol: fresh })).toBe("reconnecting");
});

// 7
test("a fresh tick cannot clear an unresolved gap", () => {
  const gap: FeedGap = { at: "2026-09-21T15:00:00.000Z", reason: "stream_lagged", missedEvents: 3 };
  const spanning = Date.parse("2026-09-21T14:30:00.000Z") / 1000;
  // Symbol is as live as it gets; the series still straddles the lost instant.
  const { state, at } = feed("GRML", 60, Array(8).fill(0.5));
  const fresh = resolveSymbolFreshness(state, at);
  expect(fresh.freshness).toBe("live");
  expect(resolveChartStatus({ transport: "open", gap, earliestBarTimeSeconds: spanning, symbol: fresh })).toBe("gap");
});

// 8
test("authoritative recovery clears the gap", () => {
  const gap: FeedGap = { at: "2026-09-21T15:00:00.000Z", reason: "reconnect", missedEvents: null };
  // Backfill replaced the window: every retained bar now starts after the gap.
  const after = Date.parse("2026-09-21T15:02:00.000Z") / 1000;
  const { state, at } = feed("GRML", 60, Array(8).fill(0.5));
  expect(
    resolveChartStatus({
      transport: "open", gap, earliestBarTimeSeconds: after,
      symbol: resolveSymbolFreshness(state, at),
    }),
  ).toBe("live");
});

// 9
test("the cadence estimate is causal", () => {
  // The verdict at time t must depend only on updates at or before t.
  const gaps = [1, 1, 1, 1, 1, 1];
  const { state, at } = feed("X", 60, gaps);
  const before = cadenceThresholds(state)!;
  // A later update cannot retroactively change the earlier estimate.
  const later = observeUpdate(state, "X", 60, at + 900_000);
  expect(cadenceThresholds(state)).toEqual(before);
  expect(cadenceThresholds(later)!.maxGapSecs).toBe(900);
});

// 10
test("the estimator is bounded in memory", () => {
  const { state } = feed("X", 60, Array(500).fill(1));
  expect(state.gapsSecs.length).toBe(CADENCE_WINDOW);
  expect(cadenceThresholds(state)!.samples).toBe(CADENCE_WINDOW);
});

// 11
test("switching symbol or interval cannot inherit the previous cadence", () => {
  const { state, at } = feed("GRML", 60, Array(10).fill(0.5));
  const switched = observeUpdate(state, "SONM", 60, at + 1000);
  expect(switched.symbol).toBe("SONM");
  expect(switched.gapsSecs).toEqual([]);
  expect(cadenceThresholds(switched)).toBeNull();
  // Same symbol, different interval, is also a different series.
  const other = observeUpdate(state, "GRML", 30, at + 1000);
  expect(other.intervalSecs).toBe(30);
  expect(other.gapsSecs).toEqual([]);
});

// 12
test("insufficient history behaves conservatively", () => {
  let st: SymbolCadenceState = emptyCadence("NEW", 60);
  // Never updated: no claim either way.
  expect(resolveSymbolFreshness(st, T0).freshness).toBe("insufficient_history");
  st = observeUpdate(st, "NEW", 60, T0);
  // A recent update IS evidence of liveness even without a cadence...
  expect(resolveSymbolFreshness(st, T0 + 5_000).freshness).toBe("live");
  // ...but past the floor we decline to claim live, and decline to claim
  // stale on a bound we cannot justify.
  expect(resolveSymbolFreshness(st, T0 + 60_000).freshness).toBe("insufficient_history");
  // Still below the minimum sample count.
  const few = feed("NEW", 60, Array(MIN_GAPS_FOR_CADENCE - 1).fill(1));
  expect(cadenceThresholds(few.state)).toBeNull();
});

// 13
test("30-second and 1-minute streams are tracked independently", () => {
  const m1 = feed("VEEE", 60, Array(8).fill(0.97));
  const s30 = feed("VEEE", 30, Array(8).fill(9.0));
  const t1 = cadenceThresholds(m1.state)!;
  const t30 = cadenceThresholds(s30.state)!;
  expect(t1.medianGapSecs).toBeCloseTo(0.97, 2);
  expect(t30.medianGapSecs).toBeCloseTo(9.0, 2);
  // Same wall-clock silence, different verdicts, because different cadence.
  expect(resolveSymbolFreshness(m1.state, m1.at + 40_000).freshness).toBe("quiet");
  expect(resolveSymbolFreshness(s30.state, s30.at + 40_000).freshness).toBe("quiet");
  expect(resolveSymbolFreshness(m1.state, m1.at + 120_000).freshness).toBe("stale");
});

// 14
test("one long outlier does not permanently distort the cadence", () => {
  // A single 300s stall inside an otherwise 1s series.
  let { state, at } = feed("X", 60, [1, 1, 300, 1, 1, 1]);
  expect(cadenceThresholds(state)!.medianGapSecs).toBe(1);   // median is robust
  expect(cadenceThresholds(state)!.maxGapSecs).toBe(300);    // stale bound is not, yet
  // It leaves the window after CADENCE_WINDOW further updates.
  for (let i = 0; i < CADENCE_WINDOW; i++) { at += 1000; state = observeUpdate(state, "X", 60, at); }
  expect(cadenceThresholds(state)!.maxGapSecs).toBe(1);
  expect(resolveSymbolFreshness(state, at + 60_000).freshness).toBe("stale");
});

// 15
test("retained bar history does not drive freshness", () => {
  // Freshness is a function of UPDATE RECEIPT times, not of how many bars
  // the 500-bar buffer happens to hold, nor of bar bucket timestamps. A
  // symbol with a long retained history that stopped updating is stale.
  const { state, at } = feed("LOBO", 60, Array(CADENCE_WINDOW + 20).fill(0.8));
  expect(state.gapsSecs.length).toBe(CADENCE_WINDOW);
  expect(resolveSymbolFreshness(state, at + 200_000).freshness).toBe("stale");
  // And a nearly-empty history that is updating right now is live.
  const fresh = feed("NEWSYM", 60, Array(MIN_GAPS_FOR_CADENCE).fill(1));
  expect(resolveSymbolFreshness(fresh.state, fresh.at + 1000).freshness).toBe("live");
});

test("age formatting carries no false precision", () => {
  expect(formatAge(null)).toBeNull();
  expect(formatAge(0)).toBe("0s");
  expect(formatAge(46.7)).toBe("47s");
  expect(formatAge(89)).toBe("89s");
  expect(formatAge(132.7)).toBe("2m");
  expect(formatAge(252.8)).toBe("4m");
});
