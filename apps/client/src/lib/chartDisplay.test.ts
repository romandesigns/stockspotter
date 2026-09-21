import { test, expect } from "bun:test";
import { resolveChartDisplay, type FeedGap } from "./feedHealth";
import { emptyCadence, observeUpdate, resolveSymbolFreshness } from "./feedHealth";

const T0 = 1_789_000_000_000;

function cadence(symbol: string, gaps: number[], nowOffsetMs = 0) {
  let st = emptyCadence(symbol, 60);
  let at = T0;
  st = observeUpdate(st, symbol, 60, at);
  for (const g of gaps) {
    at += g * 1000;
    st = observeUpdate(st, symbol, 60, at);
  }
  return resolveSymbolFreshness(st, at + nowOffsetMs);
}

const base = { transport: "open" as const, gap: null as FeedGap | null, earliestBarTimeSeconds: null as number | null };
const complete = { coverage: { state: "complete" as const } };
const partial = { coverage: { state: "partial" as const, observedFrom: "2026-09-21T15:37:17.000Z" } };
const unknown = {};

// The six combinations section 15 enumerates, asserted as SEPARATE axes.
test("live + complete", () => {
  const d = resolveChartDisplay({ ...base, symbol: cadence("GRML", Array(8).fill(0.5)), activeBar: complete });
  expect(d.status).toBe("live");
  expect(d.partialInterval).toBe(false);
  expect(d.coverageUnknown).toBe(false);
});

test("live + partial — fresh symbol, incomplete candle (the DDC state)", () => {
  // This combination is the reason the axes cannot be collapsed: DDC was
  // updating normally while 19.6% of its minute was unobserved.
  const d = resolveChartDisplay({ ...base, symbol: cadence("DDC", Array(8).fill(0.6)), activeBar: partial });
  expect(d.status).toBe("live");
  expect(d.partialInterval).toBe(true);
});

test("quiet + complete — sparse symbol whose last candle is trustworthy", () => {
  const d = resolveChartDisplay({ ...base, symbol: cadence("SCNI", Array(8).fill(7), 30_000), activeBar: complete });
  expect(d.status).toBe("quiet");
  expect(d.partialInterval).toBe(false);
});

test("quiet + partial", () => {
  const d = resolveChartDisplay({ ...base, symbol: cadence("SCNI", Array(8).fill(7), 30_000), activeBar: partial });
  expect(d.status).toBe("quiet");
  expect(d.partialInterval).toBe(true);
});

test("stale — symbol missed its own cadence, regardless of coverage", () => {
  const d = resolveChartDisplay({ ...base, symbol: cadence("BTTC", Array(8).fill(0.6), 120_000), activeBar: complete });
  expect(d.status).toBe("stale");
});

test("gap/resync outranks everything, and coverage is still reported", () => {
  const gap: FeedGap = { at: "2026-09-21T15:00:00.000Z", reason: "stream_lagged", missedEvents: 6 };
  const d = resolveChartDisplay({
    ...base, gap,
    earliestBarTimeSeconds: Date.parse("2026-09-21T14:30:00.000Z") / 1000,
    symbol: cadence("GRML", Array(8).fill(0.5)),
    activeBar: partial,
  });
  expect(d.status).toBe("gap");
  // Coverage is orthogonal, so it does not disappear behind the gap.
  expect(d.partialInterval).toBe(true);
});

test("a healthy socket cannot make a stale symbol look live", () => {
  // transport "open", nothing wrong with the feed at all.
  const d = resolveChartDisplay({ ...base, symbol: cadence("BTTC", Array(8).fill(0.6), 300_000), activeBar: complete });
  expect(d.status).not.toBe("live");
  expect(d.status).toBe("stale");
});

test("partial coverage does NOT downgrade the status", () => {
  // Deliberate: a symbol updating at its own cadence IS live. The candle
  // being incomplete is a separate statement, made separately. Collapsing
  // them would lose the ability to say "fresh but incomplete".
  const d = resolveChartDisplay({ ...base, symbol: cadence("DDC", Array(8).fill(0.6)), activeBar: partial });
  expect(d.status).toBe("live");
  expect(d.partialInterval).toBe(true);
});

test("no active bar, or an old-server bar, reports coverage unknown", () => {
  const none = resolveChartDisplay({ ...base, symbol: cadence("X", Array(8).fill(1)), activeBar: null });
  expect(none.coverageUnknown).toBe(true);
  expect(none.partialInterval).toBe(false);
  const old = resolveChartDisplay({ ...base, symbol: cadence("X", Array(8).fill(1)), activeBar: unknown });
  expect(old.coverageUnknown).toBe(true);
  // Unknown must never be rendered as partial, which would cry wolf on every
  // frame from a server predating the field.
  expect(old.partialInterval).toBe(false);
});

test("a provider-final bar is never marked partial", () => {
  const d = resolveChartDisplay({
    ...base, symbol: cadence("GRML", Array(8).fill(0.5)),
    activeBar: { isFinal: true, ...partial },
  });
  expect(d.partialInterval).toBe(false);
  expect(d.coverageUnknown).toBe(false);
});
