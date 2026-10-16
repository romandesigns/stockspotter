import { describe, expect, test } from "bun:test";
import type { HaltWarning } from "./index";
import {
  addSpeedPoint,
  gaugePct,
  movingLabel,
  movingSince,
  shouldSpeedAlert,
  SPEED_ALERT_DEFAULTS,
  SPEED_HISTORY_WINDOW_MS,
  speedDelta1m,
  speedMovePct,
  speedRows,
  SpeedTracker,
  type SpeedPoint,
} from "./haltSpeed";

const T0 = Date.parse("2026-10-16T12:40:00Z");

function reading(symbol: string, atMs: number, ratio: number, opts: Partial<HaltWarning> = {}): HaltWarning {
  return {
    type: "halt_warning",
    symbol,
    timestamp: new Date(atMs).toISOString(),
    referencePrice: 1,
    currentPrice: 1 + ratio * 0.2,
    bandWidthDollars: 0.2,
    bandDoubled: false,
    proximityRatio: ratio,
    relativeVolume: 4,
    level: "calm",
    luldInEffect: false,
    ...opts,
  };
}

describe("gauge and move", () => {
  test("gauge is clamped to 0-100 and survives junk", () => {
    expect(gaugePct({ proximityRatio: 1.4 })).toBe(100);
    expect(gaugePct({ proximityRatio: 0.714 })).toBe(71);
    expect(gaugePct({ proximityRatio: Number.NaN })).toBe(0);
  });

  test("move is relative to the rolling reference", () => {
    expect(speedMovePct({ currentPrice: 1.142, referencePrice: 1 })).toBeCloseTo(14.2, 6);
    expect(speedMovePct({ currentPrice: 1, referencePrice: 0 })).toBeNull();
  });
});

describe("history", () => {
  test("one point per 5 s bucket, newest wins, pruned to 10 min", () => {
    const points: SpeedPoint[] = [];
    addSpeedPoint(points, reading("A", T0, 0.1));
    addSpeedPoint(points, reading("A", T0 + 1_000, 0.2));
    expect(points).toEqual([{ at: T0 + 1_000, pct: 20 }]);
    addSpeedPoint(points, reading("A", T0 + 6_000, 0.3));
    expect(points.length).toBe(2);
    addSpeedPoint(points, reading("A", T0 + SPEED_HISTORY_WINDOW_MS + 30_000, 0.4));
    expect(points[0].at).toBeGreaterThanOrEqual(T0 + 30_000);
  });

  test("out-of-order readings are ignored", () => {
    const points: SpeedPoint[] = [];
    addSpeedPoint(points, reading("A", T0 + 10_000, 0.5));
    addSpeedPoint(points, reading("A", T0, 0.1));
    expect(points).toEqual([{ at: T0 + 10_000, pct: 50 }]);
  });

  test("delta over the last minute needs a minute of history", () => {
    const points: SpeedPoint[] = [{ at: T0, pct: 30 }];
    expect(speedDelta1m(points)).toBeNull();
    points.push({ at: T0 + 30_000, pct: 40 }, { at: T0 + 65_000, pct: 52 });
    expect(speedDelta1m(points)).toBe(22);
  });

  test("moving since the gauge rose through 25%", () => {
    const points: SpeedPoint[] = [
      { at: T0, pct: 10 },
      { at: T0 + 60_000, pct: 30 },
      { at: T0 + 120_000, pct: 50 },
      { at: T0 + 240_000, pct: 70 },
    ];
    expect(movingSince(points)).toEqual({ since: T0 + 60_000, atWindowStart: false });
    expect(movingLabel(movingSince(points), T0 + 240_000)).toBe("3 min");
    expect(movingSince([{ at: T0, pct: 20 }])).toBeNull();
  });
});

describe("alert rule", () => {
  const settings = SPEED_ALERT_DEFAULTS;

  test("fires only when rising through the threshold", () => {
    expect(shouldSpeedAlert({ reading: reading("A", T0, 0.72), previousPct: 65, lastAlertAt: null, settings })).toBe(true);
    expect(shouldSpeedAlert({ reading: reading("A", T0, 0.75), previousPct: 72, lastAlertAt: null, settings })).toBe(false);
    expect(shouldSpeedAlert({ reading: reading("A", T0, 0.69), previousPct: 60, lastAlertAt: null, settings })).toBe(false);
  });

  test("the first reading is a baseline, never an alert", () => {
    expect(shouldSpeedAlert({ reading: reading("A", T0, 0.95), previousPct: null, lastAlertAt: null, settings })).toBe(false);
  });

  test("down moves are skipped when up-only", () => {
    const down = reading("A", T0, 0.72, { currentPrice: 0.86 });
    expect(shouldSpeedAlert({ reading: down, previousPct: 60, lastAlertAt: null, settings })).toBe(false);
    expect(shouldSpeedAlert({ reading: down, previousPct: 60, lastAlertAt: null, settings: { ...settings, upOnly: false } })).toBe(true);
  });

  test("premarket can be switched off; a missing flag counts as regular hours", () => {
    const off = { ...settings, premarket: false };
    expect(shouldSpeedAlert({ reading: reading("A", T0, 0.72), previousPct: 60, lastAlertAt: null, settings: off })).toBe(false);
    const oldServer = reading("A", T0, 0.72, { luldInEffect: undefined });
    expect(shouldSpeedAlert({ reading: oldServer, previousPct: 60, lastAlertAt: null, settings: off })).toBe(true);
  });

  test("cooldown is per symbol and measured in reading time", () => {
    const r = reading("A", T0 + 9 * 60_000, 0.72);
    expect(shouldSpeedAlert({ reading: r, previousPct: 60, lastAlertAt: T0, settings })).toBe(false);
    const later = reading("A", T0 + 10 * 60_000, 0.72);
    expect(shouldSpeedAlert({ reading: later, previousPct: 60, lastAlertAt: T0, settings })).toBe(true);
  });
});

describe("tracker", () => {
  test("raises one alert per crossing and remembers it", () => {
    const tracker = new SpeedTracker();
    expect(tracker.observe(reading("A", T0, 0.5), SPEED_ALERT_DEFAULTS)).toBeNull();
    const alert = tracker.observe(reading("A", T0 + 5_000, 0.72), SPEED_ALERT_DEFAULTS);
    expect(alert?.symbol).toBe("A");
    expect(alert?.pct).toBe(72);
    // Same reading again (a re-render) is a no-op.
    expect(tracker.observe(reading("A", T0 + 5_000, 0.72), SPEED_ALERT_DEFAULTS)).toBeNull();
    // Dipping and recrossing inside the cooldown stays quiet.
    tracker.observe(reading("A", T0 + 60_000, 0.5), SPEED_ALERT_DEFAULTS);
    expect(tracker.observe(reading("A", T0 + 120_000, 0.75), SPEED_ALERT_DEFAULTS)).toBeNull();
    expect(tracker.recentAlerts().map((a) => a.id)).toEqual([`A-${T0 + 5_000}`]);
    expect(tracker.firstSeenAt("A")).toBe(T0);
  });

  test("rows sort fullest first and filter by direction", () => {
    const tracker = new SpeedTracker();
    const up = reading("UP", T0, 0.4);
    const big = reading("BIG", T0, 0.9);
    const down = reading("DN", T0, 0.95, { currentPrice: 0.8 });
    for (const r of [up, big, down]) tracker.observe(r, SPEED_ALERT_DEFAULTS);
    expect(speedRows([up, big, down], tracker, "up").map((r) => r.reading.symbol)).toEqual(["BIG", "UP"]);
    expect(speedRows([up, big, down], tracker, "down").map((r) => r.reading.symbol)).toEqual(["DN"]);
    expect(speedRows([up, big, down], tracker, "all").map((r) => r.reading.symbol)).toEqual(["DN", "BIG", "UP"]);
  });
});
