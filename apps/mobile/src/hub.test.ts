// Run from the repo root: bun test apps/mobile/src/hub.test.ts
// (mobile has no test runner of its own; hub.ts is pure, so bun:test runs
// it directly). Excluded from the mobile tsconfig, which has no bun types.
import { describe, expect, test } from "bun:test";
import type { CatalystUpdate, FunnelSignal, HaltWarning, IgnitionEvent, MomentumUpdate } from "@stockspotter/shared-types";
import { SPEED_ALERT_DEFAULTS, SpeedTracker, speedRows } from "@stockspotter/shared-types";
import { decodeSpeedSettings } from "./speedSettings";
import {
  IGNITION_FRESH_MS,
  buildHub,
  changeSince,
  formatQuote,
  latestPrice,
  marketSessionAt,
  mostActiveSignal,
  panelsContaining,
  sessionLine,
  type HubInput,
} from "./hub";
import type { Mover } from "./types";
import { bandColor, bandOf } from "./momentumLabel";

const NOW = Date.parse("2026-10-16T12:42:00Z"); // 08:42 ET, a Friday

function halt(symbol: string, ratio: number, up = true, overrides: Partial<HaltWarning> = {}): HaltWarning {
  return {
    type: "halt_warning", symbol, timestamp: new Date(NOW).toISOString(), referencePrice: 1, currentPrice: up ? 1.2 : 0.8,
    bandWidthDollars: 0.1, bandDoubled: false, proximityRatio: ratio, relativeVolume: 3, level: "calm", luldInEffect: false, ...overrides,
  };
}
function momentum(symbol: string, overall: number): MomentumUpdate {
  return { type: "momentum_update", symbol, timestamp: new Date(NOW).toISOString(), volumeConfirmation: 0.7, structure: 0.7, maSlope: 0.7, wickRejection: 0.7, overall, qualifies: overall >= 0.6 };
}
function funnel(symbol: string, gapPct: number, passed = true): FunnelSignal {
  return { type: "funnel_signal", symbol, timestamp: new Date(NOW).toISOString(), price: 2, gapPct, sessionVolume: 1e6, floatShares: 4e6, relativeVolume: 12, priceOk: true, floatOk: true, relVolOk: true, gapOk: true, passed };
}
function ignition(symbol: string, agoMs: number, price = 1): IgnitionEvent {
  return { type: "ignition_event", symbol, timestamp: new Date(NOW - agoMs).toISOString(), price, kind: "follow_through_confirmed" };
}
function mover(symbol: string, changePct = 10, price = 1): Mover {
  return { symbol, price, changePct, volume: 1e6, session: "premarket" };
}
function catalyst(symbol: string, at: string, publishedAt?: string): CatalystUpdate {
  return { type: "catalyst_update", symbol, timestamp: at, catalystTags: ["earnings"], headlineCount: 1, mostRecentHeadline: `${symbol} news`, mostRecentPublishedAt: publishedAt };
}

function input(overrides: Partial<HubInput> & { halts?: HaltWarning[] } = {}): HubInput {
  const tracker = new SpeedTracker();
  const halts = overrides.halts ?? [];
  for (const h of halts) tracker.observe(h, { threshold: 70, upOnly: true, premarket: true, cooldownMs: 600_000 });
  return {
    upRows: speedRows(halts, tracker, "up"),
    momentumBySymbol: new Map(),
    funnelBySymbol: new Map(),
    ignitions: [],
    gainers: [],
    mostActive: [],
    catalystsBySymbol: new Map(),
    now: NOW,
    ...overrides,
  };
}

describe("session line", () => {
  test("labels each session by New York time", () => {
    expect(marketSessionAt(new Date("2026-10-16T12:42:00Z"))).toEqual({ session: "premarket", time: "08:42" });
    expect(marketSessionAt(new Date("2026-10-16T13:30:00Z")).session).toBe("regular");
    expect(marketSessionAt(new Date("2026-10-16T20:00:00Z")).session).toBe("after_hours");
    expect(marketSessionAt(new Date("2026-10-17T00:00:00Z")).session).toBe("closed"); // 20:00 ET
    expect(marketSessionAt(new Date("2026-10-17T14:00:00Z")).session).toBe("closed"); // Saturday
    expect(marketSessionAt(new Date("2026-10-16T08:00:00Z")).session).toBe("premarket"); // 04:00 ET
  });
  test("joins session, clock and feed status", () => {
    expect(sessionLine(new Date(NOW), "live")).toBe("Premarket · 08:42 ET · live");
    expect(sessionLine(new Date("2026-10-16T14:05:00Z"), null)).toBe("Regular hours · 10:05 ET");
  });
});

describe("most active signal", () => {
  test("picks the fullest bullish gauge that is actually moving", () => {
    const hub = buildHub(input({ halts: [halt("AAA", 0.5), halt("BBB", 0.92), halt("CCC", 0.99, false)] }));
    const signal = mostActiveSignal(hub);
    expect(signal?.kind).toBe("speed");
    expect(signal?.symbol).toBe("BBB");
  });
  test("ignores a near-empty gauge and falls back to momentum", () => {
    const hub = buildHub(input({ halts: [halt("AAA", 0.1)], momentumBySymbol: new Map([["MMM", momentum("MMM", 0.66)], ["NNN", momentum("NNN", 0.71)]]) }));
    expect(hub.halt.rows).toHaveLength(1); // still listed in the section
    expect(hub.halt.moving).toHaveLength(0);
    expect(mostActiveSignal(hub)).toMatchObject({ kind: "momentum", symbol: "NNN" });
  });
  test("hides when nothing qualifies", () => {
    const hub = buildHub(input({ momentumBySymbol: new Map([["WEAK", momentum("WEAK", 0.5)]]) }));
    expect(mostActiveSignal(hub)).toBeNull();
  });
});

describe("panels containing a symbol", () => {
  test("lists panels in Home order", () => {
    const hub = buildHub(input({
      halts: [halt("ESLA", 0.92)],
      funnelBySymbol: new Map([["ESLA", funnel("ESLA", 38)], ["OTHR", funnel("OTHR", 50, false)]]),
      gainers: [mover("BTOC"), mover("ESLA")],
      mostActive: [mover("SDST")],
      catalystsBySymbol: new Map([["XXII", catalyst("XXII", "2026-10-16T12:35:00Z")]]),
    }));
    expect(panelsContaining(hub, "ESLA")).toEqual(["halt", "gapgo", "gainers"]);
    expect(panelsContaining(hub, "OTHR")).toEqual([]); // failed funnel is not Gap & Go
    expect(panelsContaining(hub, "XXII")).toEqual(["catalysts"]);
  });
  test("ignition counts only while fresh, one row per symbol", () => {
    const hub = buildHub(input({ ignitions: [ignition("AAA", 60_000), ignition("AAA", 120_000), ignition("OLD", IGNITION_FRESH_MS + 1)] }));
    expect(hub.ignition.fresh.map((e) => e.symbol)).toEqual(["AAA"]);
    expect(hub.ignition.last?.symbol).toBe("AAA");
    expect(panelsContaining(hub, "OLD")).toEqual([]);
    const quiet = buildHub(input({ ignitions: [ignition("OLD", IGNITION_FRESH_MS + 1)] }));
    expect(quiet.ignition.fresh).toEqual([]);
    expect(quiet.ignition.last?.symbol).toBe("OLD");
  });
});

describe("section ordering", () => {
  test("momentum strongest first, qualifiers only; gap & go by gap", () => {
    const hub = buildHub(input({
      momentumBySymbol: new Map([["A", momentum("A", 0.62)], ["B", momentum("B", 0.73)], ["C", momentum("C", 0.4)]]),
      funnelBySymbol: new Map([["X", funnel("X", 12)], ["Y", funnel("Y", 38)]]),
    }));
    expect(hub.momentum.map((m) => m.symbol)).toEqual(["B", "A"]);
    expect(hub.gapgo.map((f) => f.symbol)).toEqual(["Y", "X"]);
  });
  test("catalysts newest by publication time when known", () => {
    const hub = buildHub(input({ catalystsBySymbol: new Map([
      ["OLDNEWS", catalyst("OLDNEWS", "2026-10-16T12:40:00Z", "2026-10-01T12:00:00Z")],
      ["FRESH", catalyst("FRESH", "2026-10-16T12:30:00Z", "2026-10-16T12:20:00Z")],
      ["NOPUB", catalyst("NOPUB", "2026-10-16T12:10:00Z")],
    ]) }));
    expect(hub.catalysts.map((c) => c.symbol)).toEqual(["FRESH", "NOPUB", "OLDNEWS"]);
  });
});

describe("momentum bands", () => {
  test("match the web cut-offs", () => {
    expect([0.59, 0.6, 0.6499, 0.65, 0.6999, 0.7, 1].map(bandOf)).toEqual([null, "Mild", "Mild", "Solid", "Solid", "Strong", "Strong"]);
    expect(bandColor("Mild")).toBe("#3d8a57");
    expect(bandColor("Solid")).toBe("#3fb86c");
    expect(bandColor("Strong")).toBe("#86efac");
  });
});

describe("prices", () => {
  test("prefers the halt reading, then the bar, then the movers poll", () => {
    const h = new Map([["A", halt("A", 0.3, true, { currentPrice: 1.5 })]]);
    const bars = new Map([["B", [{ type: "bar_update" as const, symbol: "B", timestamp: "", open: 1, high: 1, low: 1, close: 2.25, volume: 1, intervalSecs: 60 }]]]);
    expect(latestPrice("A", h, bars, [])).toBe(1.5);
    expect(latestPrice("B", h, bars, [])).toBe(2.25);
    expect(latestPrice("C", h, bars, [mover("C", 5, 3)])).toBe(3);
    expect(latestPrice("D", h, bars, [])).toBeNull();
  });
  test("change since and quote format", () => {
    expect(changeSince(1, 1.1)).toBeCloseTo(10);
    expect(changeSince(0, 1)).toBeNull();
    expect(changeSince(1, null)).toBeNull();
    expect(formatQuote(0.5599)).toBe("$0.5599");
    expect(formatQuote(1.03)).toBe("$1.03");
  });
});

describe("speed settings", () => {
  test("missing, corrupt or partial saves fall back field by field", () => {
    expect(decodeSpeedSettings(null)).toEqual(SPEED_ALERT_DEFAULTS);
    expect(decodeSpeedSettings("{not json")).toEqual(SPEED_ALERT_DEFAULTS);
    expect(decodeSpeedSettings(JSON.stringify({ threshold: 80, sound: false }))).toEqual({ ...SPEED_ALERT_DEFAULTS, threshold: 80, sound: false });
    expect(decodeSpeedSettings(JSON.stringify({ threshold: 75, push: "yes", cooldownMs: 1 }))).toEqual(SPEED_ALERT_DEFAULTS);
  });
});
