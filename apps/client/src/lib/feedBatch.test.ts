import { describe, expect, test } from "bun:test";
import type { BarUpdate, ConsolidationEvent, FunnelSignal, HaltWarning, IgnitionEvent, MomentumUpdate } from "@stockspotter/shared-types";
import {
  applyFeedBatch,
  emptyFeedState,
  foldBarsByBucket,
  MAX_BARS_PER_SYMBOL,
  MAX_EVENTS,
  MAX_FUNNEL_SIGNALS,
  MAX_HALT_SYMBOLS,
  MAX_IGNITION_CONFIRMED_EVENTS,
  MAX_MICROPULLBACK_EVENTS,
  MAX_MOMENTUM_CONFIRMATIONS,
  type DetectionEvent,
  type FeedState,
  type PanelEvent,
} from "./feedBatch";
import { reconcileBars } from "./reconcileBars";
import { listChartableSymbols } from "./derive";

// The reference: the per-message code useRealtimeFeed ran before batching,
// one message at a time, transcribed from its setState updaters. Batching
// is only correct if every batch split gives exactly this result.
function applyOneAtATime(state: FeedState, msg: DetectionEvent): FeedState {
  const s = { ...state };
  const pushAlert = (m: PanelEvent) => {
    const alerts = [m, ...s.events.filter((e) => e.type !== "halt_warning")].slice(0, MAX_EVENTS);
    s.events = [...alerts, ...s.events.filter((e) => e.type === "halt_warning")];
  };
  switch (msg.type) {
    case "bar_update": {
      const key = msg.intervalSecs === 30 ? "subMinuteBarsBySymbol" : "barsBySymbol";
      const copy = new Map(s[key]);
      copy.set(msg.symbol, reconcileBars(s[key].get(msg.symbol) ?? [], msg, MAX_BARS_PER_SYMBOL));
      s[key] = copy;
      if (key === "barsBySymbol") s.chartSymbols = listChartableSymbols(copy);
      return s;
    }
    case "momentum_update": {
      const was = s.momentumQualified.get(msg.symbol) ?? false;
      s.momentumQualified = new Map(s.momentumQualified).set(msg.symbol, msg.qualifies);
      if (msg.qualifies && !was) s.momentumConfirmations = [msg, ...s.momentumConfirmations].slice(0, MAX_MOMENTUM_CONFIRMATIONS);
      if (!msg.qualifies && was) s.momentumFadesBySymbol = new Map(s.momentumFadesBySymbol).set(msg.symbol, msg);
      s.momentumBySymbol = new Map(s.momentumBySymbol).set(msg.symbol, msg);
      return s;
    }
    case "funnel_signal":
      s.funnelSignals = [msg, ...s.funnelSignals].slice(0, MAX_FUNNEL_SIGNALS);
      return s;
    case "catalyst_update":
      s.catalystsBySymbol = new Map(s.catalystsBySymbol).set(msg.symbol, msg);
      return s;
    case "funnel_health":
      s.funnelHealth = msg;
      return s;
    case "ignition_event":
      if (msg.kind === "follow_through_confirmed") s.ignitionConfirmedEvents = [msg, ...s.ignitionConfirmedEvents].slice(0, MAX_IGNITION_CONFIRMED_EVENTS);
      pushAlert(msg);
      return s;
    case "consolidation_event":
      if (msg.kind === "entry_triggered" && msg.strategy === "micropullback") s.micropullbackEvents = [msg, ...s.micropullbackEvents].slice(0, MAX_MICROPULLBACK_EVENTS);
      pushAlert(msg);
      return s;
    case "halt_warning": {
      const alerts = s.events.filter((e) => e.type !== "halt_warning");
      const halts = s.events.filter((e) => e.type === "halt_warning" && e.symbol !== msg.symbol);
      s.events = [...alerts, msg, ...halts.slice(0, MAX_HALT_SYMBOLS - 1)];
      return s;
    }
    default:
      pushAlert(msg);
      return s;
  }
}

// Small deterministic PRNG so a failure reproduces.
function rng(seed: number) {
  let x = seed >>> 0 || 1;
  return () => {
    x ^= x << 13; x >>>= 0;
    x ^= x >>> 17;
    x ^= x << 5; x >>>= 0;
    return x / 0x100000000;
  };
}

const T0 = Date.parse("2026-10-15T15:00:00Z");
let seq = 0;
function bar(symbol: string, minute: number, close: number, isFinal: boolean, intervalSecs = 60): BarUpdate {
  return { type: "bar_update", symbol, timestamp: new Date(T0 + minute * intervalSecs * 1000).toISOString(), open: close, high: close, low: close, close, volume: ++seq, intervalSecs, isFinal };
}
function halt(symbol: string): HaltWarning {
  return { type: "halt_warning", symbol, timestamp: new Date(T0 + ++seq).toISOString(), referencePrice: 1, currentPrice: 1, bandWidthDollars: 0.1, bandDoubled: false, proximityRatio: (seq % 100) / 100, relativeVolume: null, level: "calm" };
}
function ignition(symbol: string, kind: IgnitionEvent["kind"]): IgnitionEvent {
  return { type: "ignition_event", symbol, timestamp: new Date(T0 + ++seq).toISOString(), price: seq, kind };
}
function momentum(symbol: string, qualifies: boolean): MomentumUpdate {
  return { type: "momentum_update", symbol, timestamp: new Date(T0 + ++seq).toISOString(), volumeConfirmation: 0, structure: 0, maSlope: 0, wickRejection: 0, overall: qualifies ? 0.7 : 0.3, qualifies };
}
function consolidation(symbol: string, kind: ConsolidationEvent["kind"], strategy: ConsolidationEvent["strategy"]): ConsolidationEvent {
  return { type: "consolidation_event", symbol, timestamp: new Date(T0 + ++seq).toISOString(), price: seq, kind, strategy };
}

function randomMessages(seed: number, count: number, symbolPool: number, minutes: number): DetectionEvent[] {
  const r = rng(seed);
  const pick = <T,>(xs: readonly T[]) => xs[Math.floor(r() * xs.length)];
  const sym = () => `S${Math.floor(r() * symbolPool)}`;
  const out: DetectionEvent[] = [];
  let minute = 0;
  for (let i = 0; i < count; i++) {
    const roll = r();
    if (r() < 0.02) minute = Math.min(minute + 1, minutes);
    if (roll < 0.35) out.push(halt(sym()));
    else if (roll < 0.55) out.push(ignition(sym(), pick(["candidate_opened", "follow_through_confirmed", "follow_through_rejected"] as const)));
    else if (roll < 0.8) {
      // Forming bars mostly for the current minute, sometimes a late print
      // for an older one, finals for current or older minutes.
      const m = r() < 0.85 ? minute : Math.max(0, minute - 1 - Math.floor(r() * 3));
      out.push(bar(`S${Math.floor(r() * 3)}`, m, 1 + r(), r() < 0.2, r() < 0.3 ? 30 : 60));
    } else if (roll < 0.9) out.push(momentum(`S${Math.floor(r() * 4)}`, r() < 0.5));
    else if (roll < 0.94) out.push(consolidation(sym(), pick(["surge_detected", "entry_triggered"] as const), pick(["micropullback", "consolidation_breakout"] as const)));
    else if (roll < 0.97) out.push({ type: "funnel_signal", symbol: sym(), timestamp: new Date(T0 + ++seq).toISOString(), price: 1, gapPct: 10, sessionVolume: 1, priceOk: true, floatOk: true, relVolOk: true } as FunnelSignal);
    else if (roll < 0.985) out.push({ type: "catalyst_update", symbol: sym(), timestamp: new Date(T0 + ++seq).toISOString(), catalystTags: [], headlineCount: 0, mostRecentHeadline: null });
    else out.push({ type: "funnel_health", timestamp: new Date(T0 + ++seq).toISOString(), floatBudgetRemaining: seq, floatBudget: 1, starvedCandidates: 0, apiKeyMissing: false });
  }
  return out;
}

function batchedRandomly(messages: DetectionEvent[], seed: number, maxBatch: number): FeedState {
  const r = rng(seed);
  let state = emptyFeedState();
  for (let i = 0; i < messages.length; ) {
    const size = 1 + Math.floor(r() * maxBatch);
    state = applyFeedBatch(state, messages.slice(i, i + size));
    i += size;
  }
  return state;
}

// The reference only maintains `events`, as the old code did; its two
// halves are read back off it so the comparison covers them too.
function sequential(messages: DetectionEvent[]): FeedState {
  const s = messages.reduce(applyOneAtATime, emptyFeedState());
  return { ...s, alerts: s.events.filter((e) => e.type !== "halt_warning"), halts: s.events.filter((e) => e.type === "halt_warning") };
}

describe("applyFeedBatch matches one-message-at-a-time application", () => {
  for (const [seed, maxBatch] of [[1, 1], [2, 4], [3, 40], [4, 400], [5, 5000]] as const) {
    test(`seed ${seed}, batches up to ${maxBatch}`, () => {
      const messages = randomMessages(seed, 4000, 40, 900);
      expect(batchedRandomly(messages, seed * 7, maxBatch)).toEqual(sequential(messages));
    });
  }

  test("halt cap: more distinct symbols than MAX_HALT_SYMBOLS, in one batch and across batches", () => {
    const messages = randomMessages(11, 6000, 320, 10).map((m) => (m.type === "bar_update" ? halt(m.symbol) : m));
    const expected = sequential(messages);
    expect(expected.events.filter((e) => e.type === "halt_warning").length).toBe(MAX_HALT_SYMBOLS);
    expect(applyFeedBatch(emptyFeedState(), messages)).toEqual(expected);
    expect(batchedRandomly(messages, 12, 30)).toEqual(expected);
  });

  test("bar trim: more buckets than MAX_BARS_PER_SYMBOL", () => {
    const messages: DetectionEvent[] = [];
    for (let m = 0; m < MAX_BARS_PER_SYMBOL + 80; m++) {
      messages.push(bar("T", m, 1, false), bar("T", m, 2, false));
      if (m % 3 === 0) messages.push(bar("T", m, 3, true));
      if (m % 50 === 0) messages.push(bar("T", 0, 9, true)); // a correction for a bucket long since trimmed
    }
    const expected = sequential(messages);
    expect(expected.barsBySymbol.get("T")!.length).toBe(MAX_BARS_PER_SYMBOL);
    expect(applyFeedBatch(emptyFeedState(), messages)).toEqual(expected);
    expect(batchedRandomly(messages, 3, 25)).toEqual(expected);
  });
});

describe("coalescing", () => {
  test("only the newest forming bar per bucket survives a batch", () => {
    const folded = foldBarsByBucket([bar("A", 0, 1, false), bar("A", 0, 2, false), bar("A", 1, 3, false), bar("A", 0, 4, false)]);
    expect(folded.map((b) => b.close)).toEqual([3, 4]);
  });

  test("a forming bar never replaces a final one, in the same batch or a later one", () => {
    expect(foldBarsByBucket([bar("A", 0, 1, true), bar("A", 0, 2, false)]).map((b) => b.close)).toEqual([1]);
    expect(foldBarsByBucket([bar("A", 0, 1, false), bar("A", 0, 2, true)]).map((b) => b.close)).toEqual([2]);
    let state = applyFeedBatch(emptyFeedState(), [bar("A", 0, 1, true)]);
    const before = state.barsBySymbol.get("A");
    state = applyFeedBatch(state, [bar("A", 0, 5, false)]);
    expect(state.barsBySymbol.get("A")).toBe(before); // identity kept: nothing changed
  });

  test("an official correction still replaces a final bar", () => {
    const state = applyFeedBatch(emptyFeedState(), [bar("A", 0, 1, true), bar("A", 0, 2, false), bar("A", 0, 7, true)]);
    expect(state.barsBySymbol.get("A")!.map((b) => b.close)).toEqual([7]);
  });

  test("30-second and 1-minute bars never mix", () => {
    const state = applyFeedBatch(emptyFeedState(), [bar("A", 0, 1, false, 60), bar("A", 0, 2, false, 30)]);
    expect(state.barsBySymbol.get("A")!.map((b) => b.close)).toEqual([1]);
    expect(state.subMinuteBarsBySymbol.get("A")!.map((b) => b.close)).toEqual([2]);
  });

  test("latest halt_warning per symbol, after every alert", () => {
    const a1 = halt("A"), b = halt("B"), ign = ignition("C", "candidate_opened"), a2 = halt("A");
    const state = applyFeedBatch(emptyFeedState(), [a1, b, ign, a2]);
    expect(state.events).toEqual([ign, a2, b]);
  });

  test("momentum crossing up and back down inside one batch records both edges", () => {
    const up = momentum("M", true), down = momentum("M", false), up2 = momentum("M", true);
    const state = applyFeedBatch(emptyFeedState(), [up, momentum("M", true), down, up2]);
    expect(state.momentumConfirmations).toEqual([up2, up]);
    expect(state.momentumFadesBySymbol.get("M")).toBe(down);
    expect(state.momentumBySymbol.get("M")).toBe(up2);
  });
});

describe("identity", () => {
  test("an empty batch returns the same state", () => {
    const state = emptyFeedState();
    expect(applyFeedBatch(state, [])).toBe(state);
  });

  test("collections a batch does not touch keep their identity", () => {
    const state = applyFeedBatch(emptyFeedState(), [bar("A", 0, 1, false), bar("B", 0, 1, false), momentum("A", true)]);
    const next = applyFeedBatch(state, [halt("X"), halt("Y")]);
    expect(next.barsBySymbol).toBe(state.barsBySymbol);
    expect(next.momentumBySymbol).toBe(state.momentumBySymbol);
    expect(next.funnelSignals).toBe(state.funnelSignals);
    expect(next.events).not.toBe(state.events);
    expect(next.alerts).toBe(state.alerts); // halt-only batch: the Ignition feed's half is untouched
    const afterBar = applyFeedBatch(next, [bar("A", 0, 2, false)]);
    expect(afterBar.barsBySymbol).not.toBe(next.barsBySymbol);
    expect(afterBar.barsBySymbol.get("B")).toBe(next.barsBySymbol.get("B"));
    expect(afterBar.events).toBe(next.events);
    expect(afterBar.chartSymbols).toBe(next.chartSymbols);
    expect(applyFeedBatch(afterBar, [bar("C", 0, 1, false)]).chartSymbols).toEqual(["A", "B", "C"]);
  });
});
