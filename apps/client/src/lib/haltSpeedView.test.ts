import { expect, test } from "bun:test";
import { SpeedTracker, speedRows, type HaltWarning, type SpeedAlert } from "@stockspotter/shared-types";
import {
  CLIENT_SPEED_DEFAULTS,
  NEW_TAG_MS,
  NEW_TAG_SNAPSHOT_GRACE_MS,
  SPEED_MUTE_MS,
  SPEED_SETTINGS_KEY,
  alertChangePct,
  alertDetail,
  isMuted,
  isNewSymbol,
  muteSymbol,
  readSpeedDirection,
  readSpeedSettings,
  saveSpeedDirection,
  saveSpeedSettings,
  sparklinePoints,
  speedSummary,
  trackerSettings,
  type SpeedMutes,
} from "./haltSpeedView";

const T0 = Date.parse("2026-10-16T12:00:00Z");

function reading(symbol: string, at: number, ratio: number, opts: Partial<HaltWarning> = {}): HaltWarning {
  return {
    type: "halt_warning",
    symbol,
    timestamp: new Date(at).toISOString(),
    referencePrice: 1,
    currentPrice: 1 + ratio * 0.1,
    bandWidthDollars: 0.1,
    bandDoubled: false,
    proximityRatio: ratio,
    relativeVolume: 4.2,
    level: "calm",
    luldInEffect: false,
    ...opts,
  };
}

function memoryStorage(initial: Record<string, string> = {}) {
  const data = new Map(Object.entries(initial));
  return { getItem: (k: string) => data.get(k) ?? null, setItem: (k: string, v: string) => { data.set(k, v); }, data };
}

test("settings round-trip and fall back field by field on junk storage", () => {
  const storage = memoryStorage();
  expect(readSpeedSettings(storage)).toEqual(CLIENT_SPEED_DEFAULTS);
  const chosen = { enabled: false, threshold: 90, upOnly: false, premarket: false, sound: false };
  expect(saveSpeedSettings(storage, chosen)).toBe(true);
  expect(readSpeedSettings(storage)).toEqual(chosen);

  // One bad field keeps the others.
  const partial = memoryStorage({ [SPEED_SETTINGS_KEY]: JSON.stringify({ threshold: 65, sound: false, upOnly: "yes" }) });
  expect(readSpeedSettings(partial)).toEqual({ ...CLIENT_SPEED_DEFAULTS, sound: false });

  for (const junk of ["not json", "null", "42", "[]", '"x"']) {
    expect(readSpeedSettings(memoryStorage({ [SPEED_SETTINGS_KEY]: junk }))).toEqual(CLIENT_SPEED_DEFAULTS);
  }
  expect(readSpeedSettings(null)).toEqual(CLIENT_SPEED_DEFAULTS);
  expect(readSpeedSettings({ getItem: () => { throw Error("denied"); } })).toEqual(CLIENT_SPEED_DEFAULTS);
  expect(saveSpeedSettings(null, chosen)).toBe(false);
  expect(saveSpeedSettings({ setItem: () => { throw Error("quota"); } }, chosen)).toBe(false);
});

test("direction defaults to up and survives junk storage", () => {
  const storage = memoryStorage();
  expect(readSpeedDirection(storage)).toBe("up");
  expect(saveSpeedDirection(storage, "all")).toBe(true);
  expect(readSpeedDirection(storage)).toBe("all");
  expect(readSpeedDirection(memoryStorage({ "stockspotter.haltSpeedDirection.v1": "sideways" }))).toBe("up");
  expect(readSpeedDirection({ getItem: () => { throw Error("denied"); } })).toBe("up");
  expect(saveSpeedDirection({ setItem: () => { throw Error("denied"); } }, "down")).toBe(false);
});

test("mute lasts 30 minutes per symbol, then expires and is forgotten", () => {
  const mutes: SpeedMutes = new Map();
  muteSymbol(mutes, "ESLA", T0);
  expect(isMuted(mutes, "ESLA", T0)).toBe(true);
  expect(isMuted(mutes, "ESLA", T0 + SPEED_MUTE_MS - 1)).toBe(true);
  expect(isMuted(mutes, "XXII", T0)).toBe(false);
  expect(isMuted(mutes, "ESLA", T0 + SPEED_MUTE_MS)).toBe(false);
  expect(mutes.has("ESLA")).toBe(false);
  // Re-muting restarts the window.
  muteSymbol(mutes, "ESLA", T0 + 10 * 60_000);
  expect(isMuted(mutes, "ESLA", T0 + 10 * 60_000 + SPEED_MUTE_MS - 1)).toBe(true);
});

test("alerts switched off still sample history but raise nothing", () => {
  const off = trackerSettings({ ...CLIENT_SPEED_DEFAULTS, enabled: false });
  const on = trackerSettings(CLIENT_SPEED_DEFAULTS);
  expect(on.threshold).toBe(70);
  expect(on.cooldownMs).toBe(10 * 60_000);
  const tracker = new SpeedTracker();
  expect(tracker.observe(reading("ESLA", T0, 0.2), off)).toBeNull();
  expect(tracker.observe(reading("ESLA", T0 + 10_000, 1.4), off)).toBeNull();
  expect(tracker.points("ESLA").length).toBe(2);
  expect(tracker.recentAlerts()).toEqual([]);
  // The same crossing with alerts on does alert.
  const live = new SpeedTracker();
  live.observe(reading("ESLA", T0, 0.2), on);
  expect(live.observe(reading("ESLA", T0 + 10_000, 0.75), on)?.symbol).toBe("ESLA");
});

test("NEW only for symbols first seen after the opening snapshot, for two minutes", () => {
  const start = T0;
  expect(isNewSymbol(null, start, T0)).toBe(false);
  expect(isNewSymbol(T0, null, T0)).toBe(false);
  // Part of the opening snapshot.
  expect(isNewSymbol(T0 - 60_000, start, T0)).toBe(false);
  expect(isNewSymbol(T0 + NEW_TAG_SNAPSHOT_GRACE_MS - 1, start, T0 + 20_000)).toBe(false);
  // Appeared later.
  const later = T0 + 5 * 60_000;
  expect(isNewSymbol(later, start, later + 1_000)).toBe(true);
  expect(isNewSymbol(later, start, later + NEW_TAG_MS - 1)).toBe(true);
  expect(isNewSymbol(later, start, later + NEW_TAG_MS)).toBe(false);
});

test("change since the alert is signed and null without a usable price", () => {
  expect(alertChangePct(0.531, 0.5599)).toBeCloseTo(5.443, 2);
  expect(alertChangePct(1.29, 1.22)).toBeCloseTo(-5.426, 2);
  expect(alertChangePct(1, 1)).toBe(0);
  expect(alertChangePct(1, undefined)).toBeNull();
  expect(alertChangePct(1, null)).toBeNull();
  expect(alertChangePct(1, Number.NaN)).toBeNull();
  expect(alertChangePct(0, 1)).toBeNull();
});

test("summary names filling-fast count, the leader and the last alert, omitting what is missing", () => {
  const settings = trackerSettings(CLIENT_SPEED_DEFAULTS);
  const tracker = new SpeedTracker();
  // ESLA fills from 10% to 92% over a minute; XXII barely moves.
  tracker.observe(reading("ESLA", T0, 0.1), settings);
  tracker.observe(reading("XXII", T0, 0.4), settings);
  const latest = [reading("ESLA", T0 + 65_000, 0.92), reading("XXII", T0 + 65_000, 0.45)];
  for (const r of latest) tracker.observe(r, settings);
  const rows = speedRows(latest, tracker, "up");
  const clock = (at: number) => new Date(at).toISOString().slice(11, 16);
  expect(speedSummary(rows, tracker.recentAlerts(), clock)).toEqual([
    { text: "1 filling fast", strong: true },
    { text: "ESLA leads at 92%" },
    { text: "last alert 12:01" },
  ]);
  expect(speedSummary([], [], clock)).toEqual([]);
  const quiet = speedRows([reading("ZERO", T0, 0)], new SpeedTracker(), "all");
  expect(speedSummary(quiet, [], clock)).toEqual([]);
});

test("toast detail line matches the board's wording", () => {
  const alert: SpeedAlert = { id: "a", symbol: "ESLA", at: T0, price: 0.531, pct: 72, movePct: 19.63, relativeVolume: 5.2, fillingFast: true };
  expect(alertDetail(alert)).toBe("+19.6% in 5 min · rel vol 5.2x · filling fast");
  expect(alertDetail({ ...alert, movePct: -12.44, fillingFast: false })).toBe("−12.4% in 5 min · rel vol 5.2x");
  expect(alertDetail({ ...alert, movePct: null, relativeVolume: null, fillingFast: false })).toBe("");
});

test("sparkline spans the 10 minutes ending at the newest point", () => {
  const points = [{ at: T0, pct: 0 }, { at: T0 + 300_000, pct: 50 }, { at: T0 + 600_000, pct: 100 }];
  expect(sparklinePoints(points, 100, 20, 600_000)).toBe("0.0,20.0 50.0,10.0 100.0,0.0");
  expect(sparklinePoints([], 100, 20, 600_000)).toBe("");
});
