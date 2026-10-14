import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { OrderFlowStrip } from "./OrderFlowStrip";
import type { OrderFlowSummary, OrderFlowTape } from "../lib/orderFlow";

const summary: OrderFlowSummary = {
  asOfSecs: 1000, hasQuotes: true,
  windows: [1, 5, 15, 30, 60, 300].map(windowSecs => ({
    windowSecs, buyVolume: 0, sellVolume: 0, unknownVolume: 0, delta: 0,
    deltaRatio: null, trades: 0, shares: 0, dollars: 0, tradesPerSec: 0,
    sharesPerSec: 0, dollarsPerSec: 0, shareVelocityAcceleration: null,
  })),
  cumulativeDeltaPremarket: 0, cumulativeDeltaRegular: 0, cumulativeDeltaAfterHours: 0,
  large: { threshold: null, count: 0, volume: 0, buyVolume: 0, sellVolume: 0, delta: 0, intensity: null },
  absorption: { level: "none", side: null, intensity: null }, absorptionLevels: [], netImpactEfficiency: null,
  exhaustion: { level: "none", velocityFading: false, deltaFading: false, largeTradesFading: false },
  sweeps: { state: "none", count: 0, buyCount: 0, sellCount: 0, flaggedBuyVolume: 0, flaggedSellVolume: 0, last: null },
  book: {
    ask: { removedSize: 0, addedSize: 0, levelsCleared: 0, refillsAtTouch: 0, possiblePersistentLiquidity: false },
    bid: { removedSize: 0, addedSize: 0, levelsCleared: 0, refillsAtTouch: 0, possiblePersistentLiquidity: false },
  },
  profile: null, interpretation: { pressureSide: "balanced", pressureLevel: "none", largeTrades: "none", flow: "neutral" },
  deltaBuckets: [], largePrints: [],
};

function render(tape: OrderFlowTape | null, collapsed = true, overrides: Partial<OrderFlowSummary> = {}) {
  return renderToStaticMarkup(<OrderFlowStrip state={{ status: "ready", summary: { ...summary, ...overrides }, coverage: "partial", tape }}
    chartBucketMinutes={1} collapsed={collapsed} onToggleCollapsed={() => {}} />);
}

test("missing tape telemetry never claims zero observed corrections", () => {
  const html = render(null);
  expect(html).toContain("tape telemetry unavailable");
  expect(html).not.toContain("none has been received yet");
  expect(html).toContain("RESEARCH");
});

test("known zero and process-wide correction counts stay distinguishable", () => {
  expect(render({ provenance: "uncorrected", correctionsSeen: 0, cancelsSeen: 0 })).toContain("none has been received yet");
  const html = render({ provenance: "corrections_pending", correctionsSeen: 2, cancelsSeen: 1 });
  expect(html).toContain("uncorrected tape · 3");
  expect(html).toContain("process-wide");
  expect(html).not.toContain("tape telemetry unavailable");
});

test("a zero-volume window has unavailable side percentages", () => {
  expect(render(null, false)).toContain("SELL —");
});

test("a large-trade detector that has not warmed up says so instead of reading zero", () => {
  // `threshold: null` is "detection has not started on this symbol".
  const html = render(null, false);
  expect(html).toContain("COLLECTING");
  expect(html).toContain("Large buy and sell volume unavailable");
  expect(html).toContain("needs more prints on this symbol");
  // Specifically not the measurement it has not made -- the old markup
  // announced "Large buy volume 0, large sell volume 0" to screen readers.
  expect(html).not.toContain("Large buy volume");
});

test("a warmed-up detector that found no large trades still reports a real zero", () => {
  // A threshold exists, so 500 prints have been seen and none was large.
  // That IS a measurement and must keep reading as one.
  const html = render(null, false, {
    large: { threshold: 500, count: 0, volume: 0, buyVolume: 0, sellVolume: 0, delta: 0, intensity: null },
  });
  expect(html).toContain("Large buy volume 0, large sell volume 0");
  expect(html).toContain("at or above 500 shares");
  expect(html).not.toContain("COLLECTING");
  expect(html).not.toContain("Large buy and sell volume unavailable");
});
