// Halt Early-Warning as a *speed* meter (2026-10-16, Roman: the panel is
// where most of his real profit came from, in premarket and in regular
// hours alike, i.e. mostly when no halt happens). What he reads is
// halt_detector's proximity_ratio: |price - 5-minute rolling reference| /
// LULD band width. That is computed in every session; only `level` is
// pinned to calm outside 09:30-16:00. So this module treats the gauge as
// "how fast is it moving, scaled to its price tier" and adds the three
// things the panel could not say before: where the gauge was over the last
// 10 minutes, how fast it is filling, and when to alert.
//
// One module for web, desktop and phone so the three show identical
// numbers. ws-server's speed_alert.rs mirrors SPEED_ALERT_DEFAULTS and
// shouldSpeedAlert for the phone push; change them together or not at all.
//
// None of the thresholds below is measured. They are starting values,
// labelled as such in the UI; the "recent alerts" outcomes the panel now
// shows are the data that can tune them later.
import type { HaltWarning } from "./index";

export const SPEED_HISTORY_WINDOW_MS = 10 * 60_000;
/** One stored point per symbol per bucket: halt_warning fires on every
 * trade (~2,000/min across the universe), far more than a 10-minute line
 * needs. The newest reading in a bucket replaces the older one. */
export const SPEED_SAMPLE_MS = 5_000;
/** Gauge points gained over the last minute that count as "filling fast". */
export const FILLING_FAST_POINTS = 15;
/** A run starts when the gauge rises through this level; "moving N min"
 * is measured from there. */
export const MOVING_FLOOR_PCT = 25;

export interface SpeedAlertSettings {
  /** Gauge percent (0-100) the reading must rise through. */
  threshold: number;
  upOnly: boolean;
  premarket: boolean;
  sound: boolean;
  /** Phone only: whether this device is registered for speed pushes. */
  push: boolean;
  cooldownMs: number;
}

export const SPEED_ALERT_THRESHOLDS = [60, 70, 80, 90] as const;

export const SPEED_ALERT_DEFAULTS: SpeedAlertSettings = {
  threshold: 70,
  upOnly: true,
  premarket: true,
  sound: true,
  push: true,
  cooldownMs: 10 * 60_000,
};

export interface SpeedPoint {
  at: number; // epoch ms of the reading
  pct: number; // gauge percent, clamped 0-100
}

export function gaugePct(reading: Pick<HaltWarning, "proximityRatio">): number {
  const ratio = Number.isFinite(reading.proximityRatio) ? reading.proximityRatio : 0;
  return Math.max(0, Math.min(100, Math.round(ratio * 100)));
}

export function isBullishReading(reading: Pick<HaltWarning, "currentPrice" | "referencePrice">): boolean {
  return reading.currentPrice >= reading.referencePrice;
}

/** Move vs the 5-minute rolling reference, in percent. Null when the
 * reference is unusable. */
export function speedMovePct(reading: Pick<HaltWarning, "currentPrice" | "referencePrice">): number | null {
  if (!(reading.referencePrice > 0) || !Number.isFinite(reading.currentPrice)) return null;
  return ((reading.currentPrice - reading.referencePrice) / reading.referencePrice) * 100;
}

export function readingTime(reading: Pick<HaltWarning, "timestamp">): number {
  const at = Date.parse(reading.timestamp);
  return Number.isFinite(at) ? at : NaN;
}

/** Appends `reading` to `points` in place, one point per sample bucket,
 * pruned to the history window. Out-of-order readings are ignored. */
export function addSpeedPoint(points: SpeedPoint[], reading: HaltWarning): void {
  const at = readingTime(reading);
  if (!Number.isFinite(at)) return;
  const last = points[points.length - 1];
  if (last && at < last.at) return;
  const point = { at, pct: gaugePct(reading) };
  if (last && Math.floor(last.at / SPEED_SAMPLE_MS) === Math.floor(at / SPEED_SAMPLE_MS)) points[points.length - 1] = point;
  else points.push(point);
  const cutoff = at - SPEED_HISTORY_WINDOW_MS;
  let drop = 0;
  while (drop < points.length - 1 && points[drop].at < cutoff) drop += 1;
  if (drop) points.splice(0, drop);
}

/** Gauge points gained over the last minute: newest minus the newest point
 * at least 60 s older. Null until the history reaches back a minute. */
export function speedDelta1m(points: SpeedPoint[]): number | null {
  const last = points[points.length - 1];
  if (!last) return null;
  for (let i = points.length - 2; i >= 0; i -= 1) {
    if (last.at - points[i].at >= 60_000) return last.pct - points[i].pct;
  }
  return null;
}

export function isFillingFast(points: SpeedPoint[]): boolean {
  const delta = speedDelta1m(points);
  return delta !== null && delta >= FILLING_FAST_POINTS;
}

/** When the current run began: the oldest point of the unbroken stretch
 * at or above MOVING_FLOOR_PCT that ends at the newest point. Null when
 * the gauge is below the floor now. `atWindowStart` is true when the run
 * reaches back past the stored history, so the UI says "10+ min". */
export function movingSince(points: SpeedPoint[]): { since: number; atWindowStart: boolean } | null {
  const last = points[points.length - 1];
  if (!last || last.pct < MOVING_FLOOR_PCT) return null;
  let i = points.length - 1;
  while (i > 0 && points[i - 1].pct >= MOVING_FLOOR_PCT) i -= 1;
  return { since: points[i].at, atWindowStart: i === 0 && last.at - points[0].at >= SPEED_HISTORY_WINDOW_MS - SPEED_SAMPLE_MS };
}

export interface SpeedAlertDecisionInput {
  reading: HaltWarning;
  /** Gauge percent of this symbol's previous reading, or null for the first. */
  previousPct: number | null;
  /** Epoch ms of this symbol's last alert, or null. */
  lastAlertAt: number | null;
  settings: Pick<SpeedAlertSettings, "threshold" | "upOnly" | "premarket" | "cooldownMs">;
}

/** True when this reading should raise a speed alert: the gauge rose
 * through the threshold (the first reading is a baseline, never an
 * alert, so a snapshot replay on reconnect cannot fire a burst), the
 * direction is allowed, the session is allowed, and the symbol is out of
 * its cooldown. Mirrored by ws-server's speed_alert.rs. */
export function shouldSpeedAlert(input: SpeedAlertDecisionInput): boolean {
  const { reading, previousPct, lastAlertAt, settings } = input;
  if (previousPct === null) return false;
  const pct = gaugePct(reading);
  if (!(previousPct < settings.threshold && pct >= settings.threshold)) return false;
  if (settings.upOnly && !isBullishReading(reading)) return false;
  // luldInEffect === false means outside 09:30-16:00 ET; a missing field
  // is an old server, treated as in effect like everywhere else.
  if (!settings.premarket && reading.luldInEffect === false) return false;
  const at = readingTime(reading);
  if (!Number.isFinite(at)) return false;
  return lastAlertAt === null || at - lastAlertAt >= settings.cooldownMs;
}

export interface SpeedAlert {
  id: string;
  symbol: string;
  at: number;
  price: number;
  pct: number;
  movePct: number | null;
  relativeVolume: number | null;
  fillingFast: boolean;
}

/** Per-session tracker shared by every client: feed it the latest halt
 * reading per symbol as often as you like; it keeps the sampled history,
 * decides alerts, and remembers the alerts it raised (newest first). */
export class SpeedTracker {
  private history = new Map<string, SpeedPoint[]>();
  private lastPct = new Map<string, number>();
  private lastSeenAt = new Map<string, number>();
  private lastAlertAt = new Map<string, number>();
  private firstSeen = new Map<string, number>();
  private raised: SpeedAlert[] = [];

  constructor(private maxAlerts = 20) {}

  /** Returns the alert this reading raised, if any. Re-observing the same
   * reading (same symbol and timestamp) is a no-op. */
  observe(reading: HaltWarning, settings: SpeedAlertDecisionInput["settings"]): SpeedAlert | null {
    const at = readingTime(reading);
    if (!Number.isFinite(at)) return null;
    const seen = this.lastSeenAt.get(reading.symbol);
    if (seen !== undefined && at <= seen) return null;
    this.lastSeenAt.set(reading.symbol, at);
    if (!this.firstSeen.has(reading.symbol)) this.firstSeen.set(reading.symbol, at);
    const points = this.history.get(reading.symbol) ?? [];
    addSpeedPoint(points, reading);
    this.history.set(reading.symbol, points);
    const previousPct = this.lastPct.get(reading.symbol) ?? null;
    this.lastPct.set(reading.symbol, gaugePct(reading));
    const lastAlertAt = this.lastAlertAt.get(reading.symbol) ?? null;
    if (!shouldSpeedAlert({ reading, previousPct, lastAlertAt, settings })) return null;
    this.lastAlertAt.set(reading.symbol, at);
    const alert: SpeedAlert = {
      id: `${reading.symbol}-${at}`,
      symbol: reading.symbol,
      at,
      price: reading.currentPrice,
      pct: gaugePct(reading),
      movePct: speedMovePct(reading),
      relativeVolume: reading.relativeVolume,
      fillingFast: isFillingFast(points),
    };
    this.raised = [alert, ...this.raised].slice(0, this.maxAlerts);
    return alert;
  }

  points(symbol: string): SpeedPoint[] {
    return this.history.get(symbol) ?? [];
  }

  /** Epoch ms this session first saw the symbol (drives the NEW tag). */
  firstSeenAt(symbol: string): number | null {
    return this.firstSeen.get(symbol) ?? null;
  }

  recentAlerts(): SpeedAlert[] {
    return this.raised;
  }
}

export interface SpeedRow {
  reading: HaltWarning;
  pct: number;
  bullish: boolean;
  movePct: number | null;
  points: SpeedPoint[];
  delta1m: number | null;
  fillingFast: boolean;
  moving: ReturnType<typeof movingSince>;
}

export type SpeedDirection = "up" | "down" | "all";

/** Panel rows: fullest gauge first, filtered by direction. */
export function speedRows(readings: HaltWarning[], tracker: SpeedTracker, direction: SpeedDirection): SpeedRow[] {
  return readings
    .map((reading) => {
      const points = tracker.points(reading.symbol);
      return {
        reading,
        pct: gaugePct(reading),
        bullish: isBullishReading(reading),
        movePct: speedMovePct(reading),
        points,
        delta1m: speedDelta1m(points),
        fillingFast: isFillingFast(points),
        moving: movingSince(points),
      };
    })
    .filter((row) => direction === "all" || (direction === "up") === row.bullish)
    .sort((a, b) => b.pct - a.pct || a.reading.symbol.localeCompare(b.reading.symbol));
}

/** "Moving 4 min" text for a row; null when not moving. */
export function movingLabel(moving: ReturnType<typeof movingSince>, now: number): string | null {
  if (!moving) return null;
  if (moving.atWindowStart) return "10+ min";
  const minutes = Math.max(0, Math.floor((now - moving.since) / 60_000));
  return minutes < 1 ? "<1 min" : `${minutes} min`;
}
