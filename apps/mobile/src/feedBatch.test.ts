// Run from the repo root: bun test apps/mobile/src/feedBatch.test.ts
// Excluded from the mobile tsconfig, which has no bun types.
import { describe, expect, test } from "bun:test";
import type { BarUpdate, HaltWarning, RealtimeMessage } from "@stockspotter/shared-types";
import { SPEED_ALERT_DEFAULTS, SpeedTracker, speedRows } from "@stockspotter/shared-types";
import { applyFeedBatch, coalesceFeedBatch, emptyFeedData, marketTimestampMs, MAX_EVENTS, type FeedData } from "./feedBatch";
import { topHaltsByProximity } from "./derive";

const T0 = Date.parse("2026-10-16T14:30:00Z");
const iso = (ms: number) => new Date(ms).toISOString();

function bar(symbol: string, minute: number, close: number, opts: { final?: boolean; interval?: number; at?: number } = {}): BarUpdate {
  const interval = opts.interval ?? 60;
  return {
    type: "bar_update", symbol, timestamp: iso(T0 + minute * interval * 1000), open: 1, high: Math.max(1, close), low: Math.min(1, close), close,
    volume: Math.round(close * 100), intervalSecs: interval, ...(opts.final === undefined ? {} : { isFinal: opts.final }),
  };
}
function halt(symbol: string, ratio: number, atMs = T0): HaltWarning {
  return {
    type: "halt_warning", symbol, timestamp: iso(atMs), referencePrice: 1, currentPrice: 1.2, bandWidthDollars: 0.1, bandDoubled: false,
    proximityRatio: ratio, relativeVolume: 3, level: "calm", luldInEffect: false,
  } as HaltWarning;
}
const momentum = (symbol: string, overall: number) => ({ type: "momentum_update", symbol, timestamp: iso(T0), overall }) as unknown as RealtimeMessage;
const funnel = (symbol: string, price: number) => ({ type: "funnel_signal", symbol, timestamp: iso(T0), price }) as unknown as RealtimeMessage;
const catalyst = (symbol: string, n: number) => ({ type: "catalyst_update", symbol, timestamp: iso(T0), catalystTags: [], headlineCount: n, mostRecentHeadline: null }) as RealtimeMessage;
const ignition = (symbol: string, kind: string, price: number) => ({ type: "ignition_event", symbol, timestamp: iso(T0), price, kind }) as unknown as RealtimeMessage;
const micro = (symbol: string, kind: string, price: number) =>
  ({ type: "consolidation_event", symbol, timestamp: iso(T0), price, kind, strategy: "micropullback" }) as unknown as RealtimeMessage;

/** The old behaviour: one state update per message. */
function sequential(state: FeedData, messages: RealtimeMessage[]): FeedData {
  return messages.reduce((s, m) => applyFeedBatch(s, [m]), state);
}

/** Deterministic pseudo-random feed resembling a regular-hours burst. */
function randomFeed(seed: number, length: number): RealtimeMessage[] {
  let x = seed;
  const rnd = () => { x = (x * 1103515245 + 12345) % 2147483648; return x / 2147483648; };
  const symbols = ["AAA", "BBB", "CCC", "DDD"];
  const out: RealtimeMessage[] = [];
  const minuteBySymbol = new Map<string, number>();
  for (let i = 0; i < length; i++) {
    const symbol = symbols[Math.floor(rnd() * symbols.length)];
    const r = rnd();
    if (r < 0.4) {
      let minute = minuteBySymbol.get(symbol) ?? 0;
      if (rnd() < 0.15) minute++;
      minuteBySymbol.set(symbol, minute);
      const final = rnd() < 0.2;
      // Occasionally an older bucket (a correction, or a late preview).
      const m = rnd() < 0.05 ? Math.max(0, minute - 1) : minute;
      out.push(bar(symbol, m, 1 + rnd(), { final, interval: rnd() < 0.3 ? 30 : 60 }));
    } else if (r < 0.65) out.push(halt(symbol, rnd(), T0 + i * 100));
    else if (r < 0.75) out.push(momentum(symbol, rnd()));
    else if (r < 0.8) out.push(funnel(symbol, rnd()));
    else if (r < 0.83) out.push(catalyst(symbol, i));
    else if (r < 0.93) out.push(ignition(symbol, rnd() < 0.5 ? "follow_through_confirmed" : "candidate_opened", i));
    else out.push(micro(symbol, rnd() < 0.5 ? "entry_triggered" : "surge_detected", i));
  }
  return out;
}

function snapshot(state: FeedData) {
  return {
    events: state.events,
    bars: [...state.barsBySymbol.entries()].sort(),
    sub: [...state.subMinuteBarsBySymbol.entries()].sort(),
    momentum: [...state.momentumBySymbol.entries()].sort(),
    funnel: [...state.funnelBySymbol.entries()].sort(),
    catalysts: [...state.catalystsBySymbol.entries()].sort(),
    micro: state.micropullbackEvents,
    ignitions: state.ignitionConfirmedEvents,
  };
}

describe("coalesceFeedBatch + applyFeedBatch", () => {
  test("a coalesced batch produces exactly the per-message state (randomised)", () => {
    for (let seed = 1; seed <= 40; seed++) {
      const feed = randomFeed(seed, 400);
      // Split into batches of varying size, as the flush timer would.
      let batched = emptyFeedData();
      let i = 0;
      while (i < feed.length) {
        const size = 1 + ((seed * 7 + i) % 37);
        batched = applyFeedBatch(batched, coalesceFeedBatch(feed.slice(i, i + size)));
        i += size;
      }
      expect(snapshot(batched)).toEqual(snapshot(sequential(emptyFeedData(), feed)));
    }
  });

  test("coalescing actually shrinks a halt/preview flood", () => {
    const flood: RealtimeMessage[] = [];
    for (let i = 0; i < 100; i++) {
      flood.push(halt("AAA", i / 100, T0 + i));
      flood.push(bar("AAA", 0, 1 + i / 100));
    }
    const out = coalesceFeedBatch(flood);
    expect(out).toHaveLength(2);
    expect((out.find((m) => m.type === "halt_warning") as HaltWarning).proximityRatio).toBe(0.99);
    expect((out.find((m) => m.type === "bar_update") as BarUpdate).close).toBeCloseTo(1.99);
  });

  test("a final bar is never replaced by a later forming preview of the same candle", () => {
    const state = applyFeedBatch(emptyFeedData(), coalesceFeedBatch([bar("AAA", 0, 2, { final: true }), bar("AAA", 0, 3, { final: false })]));
    expect(state.barsBySymbol.get("AAA")).toEqual([bar("AAA", 0, 2, { final: true })]);
    // ...and a later final replaces an earlier preview.
    const next = applyFeedBatch(emptyFeedData(), coalesceFeedBatch([bar("AAA", 0, 3), bar("AAA", 0, 2, { final: true })]));
    expect(next.barsBySymbol.get("AAA")).toEqual([bar("AAA", 0, 2, { final: true })]);
  });

  test("an older bucket's last preview survives when the next bucket starts in the same batch", () => {
    const out = coalesceFeedBatch([bar("AAA", 0, 1.1), bar("AAA", 0, 1.2), bar("AAA", 1, 1.3)]);
    expect(out.map((m) => (m as BarUpdate).close)).toEqual([1.2, 1.3]);
  });

  test("30 s and 60 s streams stay separate", () => {
    const state = applyFeedBatch(emptyFeedData(), coalesceFeedBatch([bar("AAA", 0, 1.1, { interval: 30 }), bar("AAA", 0, 1.2)]));
    expect(state.subMinuteBarsBySymbol.get("AAA")?.map((b) => b.close)).toEqual([1.1]);
    expect(state.barsBySymbol.get("AAA")?.map((b) => b.close)).toEqual([1.2]);
  });

  test("untouched fields keep their identity; an empty or control-only batch returns the same state", () => {
    const start = applyFeedBatch(emptyFeedData(), [momentum("AAA", 0.5), halt("AAA", 0.4)]);
    const next = applyFeedBatch(start, [bar("AAA", 0, 1.5)]);
    expect(next.momentumBySymbol).toBe(start.momentumBySymbol);
    expect(next.events).toBe(start.events);
    expect(next.barsBySymbol).not.toBe(start.barsBySymbol);
    expect(applyFeedBatch(start, [])).toBe(start);
    const control = [{ type: "feed_status", state: "down", since: iso(T0) }, { type: "subscribe_chart", symbols: ["AAA"] }] as RealtimeMessage[];
    expect(applyFeedBatch(start, control)).toBe(start);
  });

  test("alerts are capped and every ignition/micropullback still reaches its dedicated list", () => {
    const many: RealtimeMessage[] = [];
    for (let i = 0; i < MAX_EVENTS + 20; i++) many.push(ignition(`S${i}`, "follow_through_confirmed", i));
    many.push(micro("AAA", "entry_triggered", 1));
    const state = applyFeedBatch(emptyFeedData(), coalesceFeedBatch(many));
    expect(state.events.length).toBe(MAX_EVENTS);
    expect(state.ignitionConfirmedEvents.length).toBe(100);
    expect(state.micropullbackEvents.length).toBe(1);
    expect(state.events[0].type).toBe("consolidation_event");
  });

  test("speed alerts: the tracker sees each symbol's latest reading per batch, in order", () => {
    const tracker = new SpeedTracker();
    let state = emptyFeedData();
    // Three flushes, each carrying several readings for AAA; the gauge
    // rows the Home hub draws must follow the newest one each time.
    for (const [k, ratio] of [[0, 0.3], [1, 0.5], [2, 0.8]] as const) {
      const batch: RealtimeMessage[] = [halt("AAA", ratio - 0.1, T0 + k * 10_000), halt("BBB", 0.2, T0 + k * 10_000), halt("AAA", ratio, T0 + k * 10_000 + 500)];
      state = applyFeedBatch(state, coalesceFeedBatch(batch));
      const readings = topHaltsByProximity(state.events);
      expect(readings.find((r) => r.symbol === "AAA")?.proximityRatio).toBe(ratio);
      for (const reading of readings) tracker.observe(reading, SPEED_ALERT_DEFAULTS);
      expect(speedRows(readings, tracker, "up").length).toBeGreaterThan(0);
    }
  });

  test("marketTimestampMs ignores observation-time messages", () => {
    expect(marketTimestampMs(bar("AAA", 0, 1))).toBe(T0);
    expect(marketTimestampMs(catalyst("AAA", 1))).toBeNull();
    expect(marketTimestampMs({ type: "ping", at: iso(T0) } as RealtimeMessage)).toBeNull();
  });
});
