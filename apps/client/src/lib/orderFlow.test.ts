import { expect, test } from "bun:test";
import {
  MAX_CARRY_FORWARD_SECS,
  chartArrayLagSecs,
  cumulativeDeltaSeries,
  decodeOrderFlowBody,
  decodeOrderFlowFrame,
  formatShares,
  formatSignedShares,
  formatWindowLabel,
  isFullSummary,
  levelSegments,
  orderFlowWindowFor,
  sideShares,
  withChartArrays,
  type OrderFlowSummary,
  type OrderFlowWindow,
} from "./orderFlow";

function window(windowSecs: number, overrides: Partial<OrderFlowWindow> = {}): OrderFlowWindow {
  return {
    windowSecs,
    buyVolume: 0,
    sellVolume: 0,
    unknownVolume: 0,
    delta: 0,
    deltaRatio: null,
    trades: 0,
    shares: 0,
    dollars: 0,
    tradesPerSec: 0,
    sharesPerSec: 0,
    dollarsPerSec: 0,
    shareVelocityAcceleration: null,
    ...overrides,
  };
}

function summary(overrides: Partial<OrderFlowSummary> = {}): OrderFlowSummary {
  return {
    asOfSecs: 1000,
    hasQuotes: true,
    windows: [window(1), window(5), window(15), window(30), window(60), window(300)],
    cumulativeDeltaPremarket: 0,
    cumulativeDeltaRegular: 0,
    cumulativeDeltaAfterHours: 0,
    large: { threshold: null, count: 0, volume: 0, buyVolume: 0, sellVolume: 0, delta: 0, intensity: null },
    absorption: { level: "none", side: null, intensity: null },
    absorptionLevels: [],
    netImpactEfficiency: null,
    exhaustion: { level: "none", velocityFading: false, deltaFading: false, largeTradesFading: false },
    sweeps: { state: "none", count: 0, buyCount: 0, sellCount: 0, flaggedBuyVolume: 0, flaggedSellVolume: 0, last: null },
    book: {
      ask: { removedSize: 0, addedSize: 0, levelsCleared: 0, refillsAtTouch: 0, possiblePersistentLiquidity: false },
      bid: { removedSize: 0, addedSize: 0, levelsCleared: 0, refillsAtTouch: 0, possiblePersistentLiquidity: false },
    },
    profile: null,
    interpretation: { pressureSide: "balanced", pressureLevel: "none", largeTrades: "none", flow: "neutral" },
    deltaBuckets: [],
    largePrints: [],
    ...overrides,
  };
}

function bucket(startSecs: number, buyVolume: number, sellVolume: number) {
  return { startSecs, buyVolume, sellVolume, unknownVolume: 0, open: 1, close: 1, bidSize: 5, askSize: 6 };
}

test("a frame that is not an order-flow frame is never decoded as one", () => {
  expect(decodeOrderFlowFrame(null)).toBeNull();
  expect(decodeOrderFlowFrame("order_flow")).toBeNull();
  expect(decodeOrderFlowFrame({ type: "bar_update", symbol: "AAA" })).toBeNull();
  // Right tag, unusable body: a missing symbol, a non-boolean `enabled`,
  // and a summary that is neither null nor a readable summary.
  expect(decodeOrderFlowFrame({ type: "order_flow", enabled: true, summary: null })).toBeNull();
  expect(decodeOrderFlowFrame({ type: "order_flow", symbol: "AAA", summary: null })).toBeNull();
  expect(decodeOrderFlowFrame({ type: "order_flow", symbol: "AAA", enabled: true, summary: 7 })).toBeNull();
  expect(decodeOrderFlowFrame({ type: "order_flow", symbol: "AAA", enabled: true, summary: { asOfSecs: 1 } })).toBeNull();
});

test("a body missing any field the strip dereferences is rejected whole", () => {
  // Every one of these used to decode: the frame decoder checked
  // `asOfSecs`, `windows` and `interpretation`, and the HTTP path checked
  // nothing at all, while OrderFlowStrip reads `exhaustion.level`,
  // `book.ask.possiblePersistentLiquidity`, `sweeps.state`, `large`,
  // `absorption`, `deltaBuckets` and `interpretation.largeTrades` without
  // asking. A body like these threw inside a React render.
  const missing: Partial<Record<keyof OrderFlowSummary, unknown>>[] = [
    { exhaustion: undefined },
    { book: undefined },
    { book: { ask: summary().book.ask } }, // one side only
    { sweeps: undefined },
    { large: undefined },
    { absorption: undefined },
    { deltaBuckets: undefined },
    { largePrints: undefined },
    { absorptionLevels: undefined },
    { hasQuotes: undefined },
    { cumulativeDeltaRegular: undefined },
  ];
  for (const overrides of missing) {
    const body = { ...summary(), ...overrides } as Record<string, unknown>;
    for (const [key, value] of Object.entries(overrides)) if (value === undefined) delete body[key];
    expect(decodeOrderFlowBody({ enabled: true, symbol: "AAA", summary: body }), JSON.stringify(Object.keys(overrides))).toBeNull();
  }
});

test("a field of the wrong shape is rejected rather than coerced", () => {
  const cases: Record<string, unknown>[] = [
    // Arrays that are not arrays. `deltaBuckets.slice(-40)` on a string
    // returns a string, and the strip then maps over characters.
    { deltaBuckets: "many" },
    { windows: { windowSecs: 60 } },
    { absorptionLevels: 3 },
    // Numbers that are not finite numbers. These arrive as a rendered
    // "NaN%" or an infinite bar width, not as an error.
    { asOfSecs: "1000" },
    { asOfSecs: Number.NaN },
    { cumulativeDeltaRegular: Number.POSITIVE_INFINITY },
    // Enumerations the UI indexes a label map with.
    { interpretation: { pressureSide: "up", pressureLevel: "none", largeTrades: "none", flow: "neutral" } },
    { interpretation: { pressureSide: "buy", pressureLevel: "massive", largeTrades: "none", flow: "neutral" } },
    { exhaustion: { level: "certain", velocityFading: false, deltaFading: false, largeTradesFading: false } },
    { sweeps: { ...summary().sweeps, state: "" } },
    { absorption: { level: "high", side: "middle", intensity: null } },
    // A nullable number that is neither.
    { netImpactEfficiency: "0.5" },
    // Nested array elements.
    { deltaBuckets: [{ startSecs: 0 }] },
    { largePrints: [{ timestampSecs: 1, price: 2, size: 3, side: "upward" }] },
    { profile: { poc: 2, valueAreaLow: 1, valueAreaHigh: 3, hvn: [], lvn: [], bins: [{ price: 2 }], totalVolume: 1, sinceSecs: 1 } },
    { profile: { poc: 2, valueAreaLow: 1, valueAreaHigh: 3, hvn: ["high"], lvn: [], bins: [], totalVolume: 1, sinceSecs: 1 } },
  ];
  for (const overrides of cases) {
    expect(decodeOrderFlowBody({ enabled: true, symbol: "AAA", summary: { ...summary(), ...overrides } }), JSON.stringify(overrides)).toBeNull();
  }
  // And the envelope around it.
  expect(decodeOrderFlowBody({ enabled: true, symbol: "AAA", coverage: "whole_day", summary: null })).toBeNull();
  expect(decodeOrderFlowBody({ enabled: true, symbol: "AAA", backfill: "maybe", summary: null })).toBeNull();
  expect(decodeOrderFlowBody({ enabled: true, symbol: "AAA", tape: { provenance: "reconciled", correctionsSeen: 0, cancelsSeen: 0 }, summary: null })).toBeNull();
  expect(decodeOrderFlowBody({ enabled: true, symbol: "AAA", tape: { provenance: "uncorrected", correctionsSeen: null, cancelsSeen: 0 }, summary: null })).toBeNull();
  // A valid body still decodes, so none of the above is passing by
  // rejecting everything.
  expect(decodeOrderFlowBody({ enabled: true, symbol: "AAA", coverage: "session", summary: summary() })).not.toBeNull();
});

test("a body about another symbol is not shown against the one asked for", () => {
  const body = { enabled: true, symbol: "BBB", summary: summary() };
  expect(decodeOrderFlowBody(body, "AAA")).toBeNull();
  // The server upper-cases the symbol it echoes; the client sends what the
  // user picked. That is not a mismatch.
  expect(decodeOrderFlowBody({ ...body, symbol: "AAA" }, "aaa")).not.toBeNull();
  expect(decodeOrderFlowBody(body, "BBB")).not.toBeNull();
  // With no expectation, any symbol decodes: the push path addresses
  // frames by its own registry.
  expect(decodeOrderFlowBody(body)).not.toBeNull();
});

test("the optional fields of an older server are absent, not invalid", () => {
  const body = decodeOrderFlowBody({ enabled: true, symbol: "AAA", summary: summary() });
  expect(body!.coverage).toBeUndefined();
  expect(body!.backfill).toBeUndefined();
  expect(body!.tape).toBeUndefined();
  expect(body!.summary!.chartArraysAsOfSecs).toBeUndefined();
  expect(body!.summary!.sweeps.multiVenueCount).toBeUndefined();
});

test("a disabled frame decodes with its provenance and without a summary", () => {
  const frame = decodeOrderFlowFrame({ type: "order_flow", symbol: "AAA", enabled: false, summary: null });
  expect(frame).not.toBeNull();
  expect(frame!.enabled).toBe(false);
  expect(frame!.summary).toBeNull();
  expect(frame!.coverage).toBeUndefined();
});

test("a ready frame carries coverage and the uncorrected-tape counts", () => {
  const frame = decodeOrderFlowFrame({
    type: "order_flow",
    symbol: "aaa",
    enabled: true,
    backfill: "none",
    coverage: "partial",
    tape: { provenance: "corrections_pending", correctionsSeen: 3, cancelsSeen: 1 },
    summary: summary(),
  });
  expect(frame!.symbol).toBe("aaa");
  expect(frame!.coverage).toBe("partial");
  expect(frame!.tape).toEqual({ provenance: "corrections_pending", correctionsSeen: 3, cancelsSeen: 1 });
});

test("carried-forward chart arrays keep the age they actually have", () => {
  const full = summary({ asOfSecs: 1000, deltaBuckets: [bucket(0, 10, 4)], largePrints: [{ timestampSecs: 1, price: 2, size: 9, side: "buy" }], profile: { poc: 2, valueAreaLow: 1, valueAreaHigh: 3, hvn: [], lvn: [], bins: [{ price: 2, volume: 9, buyVolume: 9, sellVolume: 0 }], totalVolume: 9, sinceSecs: 500 } });
  const lite = summary({ asOfSecs: 1180, profile: { ...full.profile!, bins: [] } });

  const merged = withChartArrays(lite, full);
  expect(merged.asOfSecs).toBe(1180);
  expect(merged.deltaBuckets).toHaveLength(1);
  expect(merged.profile!.bins).toHaveLength(1);
  // The whole point: the arrays are three minutes older than the header.
  expect(merged.chartArraysAsOfSecs).toBe(1000);
  expect(chartArrayLagSecs(merged)).toBe(180);

  // Merging a second time must not re-stamp the arrays as current.
  const again = withChartArrays(summary({ asOfSecs: 1300, profile: lite.profile }), merged);
  expect(again.chartArraysAsOfSecs).toBe(1000);
  expect(chartArrayLagSecs(again)).toBe(300);
});

test("with no full summary yet the arrays are empty and claim no age", () => {
  const merged = withChartArrays(summary({ asOfSecs: 50 }), null);
  expect(merged.deltaBuckets).toEqual([]);
  expect(merged.largePrints).toEqual([]);
  expect(merged.chartArraysAsOfSecs).toBeUndefined();
  expect(chartArrayLagSecs(merged)).toBeNull();
  // A fresh full summary is current, not stale.
  expect(chartArrayLagSecs(summary({ asOfSecs: 50, chartArraysAsOfSecs: 50 }))).toBeNull();
});

test("a lite summary for a symbol with no profile stays profile-less", () => {
  const merged = withChartArrays(summary({ profile: null }), summary({ profile: { poc: 2, valueAreaLow: 1, valueAreaHigh: 3, hvn: [], lvn: [], bins: [], totalVolume: 1, sinceSecs: 1 } }));
  expect(merged.profile).toBeNull();
});

test("chart arrays from another session are dropped rather than labelled", () => {
  const full = summary({ asOfSecs: 1000, chartArraysAsOfSecs: 1000, deltaBuckets: [bucket(0, 10, 4)] });
  // Just inside the window: carried, with their age.
  const near = withChartArrays(summary({ asOfSecs: 1000 + MAX_CARRY_FORWARD_SECS }), full);
  expect(near.deltaBuckets).toHaveLength(1);
  expect(near.chartArraysAsOfSecs).toBe(1000);
  // A chart left open across a market day: the buckets and the profile
  // bins are yesterday's, and "20 hours old" is not a pane to draw.
  const nextDay = withChartArrays(summary({ asOfSecs: 1000 + 24 * 3600, profile: { poc: 2, valueAreaLow: 1, valueAreaHigh: 3, hvn: [], lvn: [], bins: [], totalVolume: 1, sinceSecs: 1 } }), full);
  expect(nextDay.deltaBuckets).toEqual([]);
  expect(nextDay.profile!.bins).toEqual([]);
  expect(nextDay.chartArraysAsOfSecs).toBeUndefined();
  expect(chartArrayLagSecs(nextDay)).toBeNull();
  // And a summary whose clock is that far the other way is not the same
  // session either.
  expect(withChartArrays(summary({ asOfSecs: 1000 - 24 * 3600 }), full).deltaBuckets).toEqual([]);
});

test("the full shape is recognised by its own marker, not by whether it is empty", () => {
  expect(isFullSummary(null)).toBe(false);
  expect(isFullSummary(summary())).toBe(false);
  // The marker the server stamps on a full body and only a full body.
  // Without this, a quiet symbol's full answer -- no large prints, no
  // buckets yet -- read as "lite", and the client carried older arrays
  // forward over it indefinitely.
  expect(isFullSummary(summary({ chartArraysAsOfSecs: 1000, deltaBuckets: [] }))).toBe(true);
  // A server predating the marker answers the lite route with everything.
  expect(isFullSummary(summary({ deltaBuckets: [bucket(0, 1, 1)] }))).toBe(true);
});

test("the summary window matches the chart's own bucket size", () => {
  const s = summary();
  expect(orderFlowWindowFor(s, 1)?.windowSecs).toBe(60);
  expect(orderFlowWindowFor(s, 5)?.windowSecs).toBe(300);
  // Coarser than the longest window falls back to the longest there is,
  // and sub-minute charts to the shortest window the strip shows.
  expect(orderFlowWindowFor(s, 15)?.windowSecs).toBe(300);
  expect(orderFlowWindowFor(s, 0.5)?.windowSecs).toBe(30);
  expect(orderFlowWindowFor(summary({ windows: [] }), 1)).toBeNull();
});

test("side shares always sum to 100 and unknown takes the remainder", () => {
  expect(sideShares(window(60))).toBeNull();
  const shares = sideShares(window(60, { shares: 300, buyVolume: 100, sellVolume: 100 }))!;
  expect(shares.buy + shares.sell + shares.unknown).toBe(100);
  expect(shares.unknown).toBe(34);
  // Rounding that would overshoot never produces a negative unknown.
  const tight = sideShares(window(60, { shares: 3, buyVolume: 1, sellVolume: 2 }))!;
  expect(tight.unknown).toBe(0);
});

test("cumulative delta runs across the buckets in order", () => {
  expect(cumulativeDeltaSeries([bucket(0, 10, 4), bucket(30, 1, 7), bucket(60, 5, 5)])).toEqual([6, 0, 0]);
  expect(cumulativeDeltaSeries([])).toEqual([]);
});

test("levels, labels and share formatting", () => {
  expect(levelSegments("none")).toBe(0);
  expect(levelSegments("extreme")).toBe(4);
  // An unknown level from a newer server fills nothing rather than -1.
  expect(levelSegments("brand_new" as never)).toBe(0);
  expect(formatWindowLabel(30)).toBe("30s");
  expect(formatWindowLabel(300)).toBe("5m");
  expect(formatShares(850)).toBe("850");
  expect(formatShares(342_000)).toBe("342K");
  expect(formatShares(1_200_000)).toBe("1.2M");
  expect(formatSignedShares(-1_200_000)).toBe("-1.2M");
  expect(formatSignedShares(0)).toBe("0");
});
