import { test, expect } from "bun:test";
import type { BarUpdate } from "@stockspotter/shared-types";
import {
  barAuthority,
  coverageOf,
  isCoverageComplete,
  isCoveragePartial,
  isTimeFinal,
  mayReplace,
  reconcileBars,
} from "@stockspotter/shared-types";
import { mergeBars, toChartBars, type CandleBar } from "./derive";

const bucket = "2026-09-21T15:37:00.000Z";
const T = Date.parse(bucket);

function bar(over: Partial<BarUpdate> = {}): BarUpdate {
  return {
    type: "bar_update",
    symbol: "DDC",
    timestamp: bucket,
    open: 10, high: 11, low: 9, close: 10.5,
    volume: 1000,
    intervalSecs: 60,
    ...over,
  };
}

// ---------------------------------------------------------------- section 18
test("absent coverage reads as unknown, never as complete", () => {
  // Every frame from a server predating the field arrives like this. Reading
  // it as complete would reintroduce the exact lie the field prevents.
  const old = bar();
  expect(old.coverage).toBeUndefined();
  expect(coverageOf(old)).toEqual({ state: "unknown" });
  expect(isCoverageComplete(old)).toBe(false);
  expect(isCoveragePartial(old)).toBe(false);
});

test("unknown is neither complete nor partial — the third state is real", () => {
  const u = bar({ coverage: { state: "unknown" } });
  expect(isCoverageComplete(u)).toBe(false);
  expect(isCoveragePartial(u)).toBe(false);
  // So the predicates are not each other's negation, and callers must
  // handle all three.
  expect(isCoverageComplete(u) || isCoveragePartial(u)).toBe(false);
});

test("a provider-final bar is complete regardless of any coverage field", () => {
  expect(isCoverageComplete(bar({ isFinal: true }))).toBe(true);
  expect(isCoverageComplete(bar({ isFinal: true, coverage: { state: "partial", observedFrom: bucket } }))).toBe(true);
  expect(isCoveragePartial(bar({ isFinal: true, coverage: { state: "partial", observedFrom: bucket } }))).toBe(false);
});

// ----------------------------------------------------------------- section 7
test("time-final does not imply coverage-complete", () => {
  // 30s bucket 15:37:00-15:37:30, observation began 15:37:12.
  const b = bar({
    intervalSecs: 30,
    coverage: { state: "partial", observedFrom: "2026-09-21T15:37:12.000Z" },
  });
  // At 15:37:30 the interval has elapsed...
  expect(isTimeFinal(T, 30, Date.parse("2026-09-21T15:37:30.000Z"))).toBe(true);
  // ...and it is STILL partial, and still not provider-final.
  expect(isCoveragePartial(b)).toBe(true);
  expect(isCoverageComplete(b)).toBe(false);
  expect(b.isFinal).toBeUndefined();
});

test("time-finality is a pure function of clock and interval", () => {
  expect(isTimeFinal(T, 60, T + 59_999)).toBe(false);
  expect(isTimeFinal(T, 60, T + 60_000)).toBe(true);
  expect(isTimeFinal(T, 30, T + 29_999)).toBe(false);
  expect(isTimeFinal(T, 30, T + 30_000)).toBe(true);
});

// ---------------------------------------------------------------- section 10
test("authority ordering: final > complete > unknown > partial", () => {
  expect(barAuthority(bar({ isFinal: true }))).toBe(3);
  expect(barAuthority(bar({ coverage: { state: "complete" } }))).toBe(2);
  expect(barAuthority(bar())).toBe(1);
  expect(barAuthority(bar({ coverage: { state: "partial", observedFrom: bucket } }))).toBe(0);
});

test("a partial bar may never degrade a complete or final bar", () => {
  const partial = bar({ volume: 3, coverage: { state: "partial", observedFrom: bucket } });
  expect(mayReplace(bar({ isFinal: true }), partial)).toBe(false);
  expect(mayReplace(bar({ coverage: { state: "complete" } }), partial)).toBe(false);
  // Even over an unknown-coverage bar, which may well have been complete.
  expect(mayReplace(bar(), partial)).toBe(false);
});

test("equal authority still replaces, so live updating keeps working", () => {
  const a = bar({ volume: 100, coverage: { state: "complete" } });
  const b = bar({ volume: 200, coverage: { state: "complete" } });
  expect(mayReplace(a, b)).toBe(true);
  const p1 = bar({ volume: 10, coverage: { state: "partial", observedFrom: bucket } });
  const p2 = bar({ volume: 20, coverage: { state: "partial", observedFrom: bucket } });
  expect(mayReplace(p1, p2)).toBe(true);
  expect(mayReplace(undefined, p1)).toBe(true);
});

test("reconcileBars refuses the downgrade, in the retained series", () => {
  const complete = bar({ volume: 1000, coverage: { state: "complete" } });
  const partial = bar({ volume: 3, coverage: { state: "partial", observedFrom: bucket } });
  const kept = reconcileBars([complete], partial, 500);
  expect(kept).toHaveLength(1);
  expect(kept[0].volume).toBe(1000);
  // And the provider-final guard it always had still holds.
  expect(reconcileBars([bar({ isFinal: true, volume: 999 })], bar({ volume: 1 }), 500)[0].volume).toBe(999);
});

test("THE MEASURED FAILURE CLASS: REST 1,000 is not overwritten by live 3", () => {
  // 1. REST installs the complete bucket (marked authoritative by the
  //    backfill hook). 2. live aggregation begins mid-bucket. 3. the partial
  //    live bar arrives. 4. the client merges.
  const rest: CandleBar[] = [{ time: T / 1000, open: 10, high: 12, low: 9, close: 11, volume: 1000, isFinal: true }];
  const live = toChartBars([bar({ volume: 3, coverage: { state: "partial", observedFrom: "2026-09-21T15:37:50.000Z" } })]);
  const merged = mergeBars(rest, live);
  expect(merged).toHaveLength(1);
  expect(merged[0].volume).toBe(1000);
});

test("a complete live bar DOES still update over REST", () => {
  // The precedence must not freeze the chart: equal-or-greater authority wins.
  const rest: CandleBar[] = [{ time: T / 1000, open: 10, high: 12, low: 9, close: 11, volume: 1000, isFinal: true }];
  const liveFinal = toChartBars([bar({ volume: 1200, isFinal: true })]);
  expect(mergeBars(rest, liveFinal)[0].volume).toBe(1200);
});

// ---------------------------------------------------------------- section 11
test("1-minute lifecycle: provisional partial -> authoritative, in place", () => {
  let series: BarUpdate[] = [];
  // Observation begins mid-minute: provisional and partial.
  series = reconcileBars(series, bar({
    volume: 96_108, close: 10.25,
    coverage: { state: "partial", observedFrom: "2026-09-21T15:37:17.000Z" },
  }), 500);
  expect(series).toHaveLength(1);
  expect(isCoveragePartial(series[0])).toBe(true);
  expect(series[0].volume).toBe(96_108);

  // The provider's official bar for the same minute arrives ~100ms after the
  // boundary and reconciles the SAME bucket.
  series = reconcileBars(series, bar({
    isFinal: true, open: 10.0, high: 11.5, low: 9.8, close: 10.4, volume: 119_482,
    coverage: { state: "complete" },
  }), 500);

  expect(series).toHaveLength(1);                       // no duplicate candle
  expect(series[0].timestamp).toBe(bucket);             // same bucket identity
  expect(series[0].isFinal).toBe(true);                 // authority transition
  expect(isCoverageComplete(series[0])).toBe(true);     // coverage transition
  expect(series[0].open).toBe(10.0);
  expect(series[0].high).toBe(11.5);
  expect(series[0].low).toBe(9.8);
  expect(series[0].close).toBe(10.4);
  expect(series[0].volume).toBe(119_482);               // equals authoritative

  // A late provisional straggler cannot undo it.
  series = reconcileBars(series, bar({ volume: 5, coverage: { state: "partial", observedFrom: bucket } }), 500);
  expect(series[0].volume).toBe(119_482);
});

// ---------------------------------------------------------------- section 12
test("30-second lifecycle: a partial bucket stays partial permanently", () => {
  let series: BarUpdate[] = [];
  const partial30 = bar({
    intervalSecs: 30, volume: 500,
    coverage: { state: "partial", observedFrom: "2026-09-21T15:37:12.000Z" },
  });
  series = reconcileBars(series, partial30, 500);
  expect(isCoveragePartial(series[0])).toBe(true);

  // There is no provider-authoritative 30s bar, so nothing can arrive to
  // complete it. Later provisional updates for the same bucket are also
  // partial, and it never becomes complete.
  series = reconcileBars(series, bar({
    intervalSecs: 30, volume: 650,
    coverage: { state: "partial", observedFrom: "2026-09-21T15:37:12.000Z" },
  }), 500);
  expect(series[0].volume).toBe(650);
  expect(isCoveragePartial(series[0])).toBe(true);
  expect(isCoverageComplete(series[0])).toBe(false);

  // Time-final and still partial. This is the permanent state for this bucket.
  expect(isTimeFinal(T, 30, T + 30_000)).toBe(true);
  expect(isCoverageComplete(series[0])).toBe(false);
});

test("a 30-second bucket opened after observation began IS complete", () => {
  // The other half: partial-forever applies to the bucket that straddled the
  // observation start, not to the symbol.
  const later = bar({ intervalSecs: 30, timestamp: "2026-09-21T15:38:00.000Z", coverage: { state: "complete" } });
  expect(isCoverageComplete(later)).toBe(true);
});

test("toChartBars carries coverage and finality through to the chart layer", () => {
  const out = toChartBars([
    bar({ coverage: { state: "partial", observedFrom: bucket } }),
    bar({ timestamp: "2026-09-21T15:38:00.000Z", isFinal: true, coverage: { state: "complete" } }),
  ]);
  expect(out).toHaveLength(2);
  expect(out[0].coverage).toEqual({ state: "partial", observedFrom: bucket });
  expect(out[1].isFinal).toBe(true);
});
