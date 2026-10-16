// Pure pieces of the web/desktop Halt Early-Warning speed view (2026-10-16
// redesign): settings persistence, per-symbol mute, the NEW tag, the
// one-line summary and the "since the alert" outcome. Every number the
// panel shows comes from @stockspotter/shared-types' haltSpeed module so
// web, desktop and phone agree; this file only holds what is specific to
// the browser (localStorage, wall-clock mute) and is kept free of React so
// it can be unit tested.
import {
  SPEED_ALERT_DEFAULTS,
  SPEED_ALERT_THRESHOLDS,
  type SpeedAlert,
  type SpeedAlertSettings,
  type SpeedDirection,
  type SpeedRow,
} from "@stockspotter/shared-types";

export const SPEED_SETTINGS_KEY = "stockspotter.haltSpeedAlerts.v1";
export const SPEED_DIRECTION_KEY = "stockspotter.haltSpeedDirection.v1";
export const SPEED_MUTE_MS = 30 * 60_000;
/** How long a symbol first seen after the session's opening snapshot
 * keeps its NEW tag. */
export const NEW_TAG_MS = 2 * 60_000;
/** Readings first seen within this long of the first observation belong
 * to the opening snapshot, so they are not NEW: otherwise every card wore
 * the tag for two minutes after each page load. */
export const NEW_TAG_SNAPSHOT_GRACE_MS = 15_000;

/** What the web/desktop popover edits. `push` is phone-only and the
 * cooldown is fixed ("once per stock every 10 min"), so neither is stored
 * here; `enabled` is the bell's own on/off, which the shared settings do
 * not carry because the phone expresses it through `push`. */
export interface ClientSpeedSettings {
  enabled: boolean;
  threshold: number;
  upOnly: boolean;
  premarket: boolean;
  sound: boolean;
}

export const CLIENT_SPEED_DEFAULTS: ClientSpeedSettings = {
  enabled: true,
  threshold: SPEED_ALERT_DEFAULTS.threshold,
  upOnly: SPEED_ALERT_DEFAULTS.upOnly,
  premarket: SPEED_ALERT_DEFAULTS.premarket,
  sound: SPEED_ALERT_DEFAULTS.sound,
};

type Reader = Pick<Storage, "getItem">;
type Writer = Pick<Storage, "setItem">;

/** Field by field, so one junk or missing value falls back to its own
 * default instead of discarding the rest of the user's choices. */
export function readSpeedSettings(storage: Reader | null): ClientSpeedSettings {
  let raw: unknown = null;
  try {
    const text = storage?.getItem(SPEED_SETTINGS_KEY);
    raw = text ? JSON.parse(text) : null;
  } catch { /* unreadable or malformed storage: defaults */ }
  const value = raw && typeof raw === "object" ? (raw as Record<string, unknown>) : {};
  const bool = (key: keyof ClientSpeedSettings) => (typeof value[key] === "boolean" ? (value[key] as boolean) : (CLIENT_SPEED_DEFAULTS[key] as boolean));
  const threshold = (SPEED_ALERT_THRESHOLDS as readonly number[]).includes(value.threshold as number) ? (value.threshold as number) : CLIENT_SPEED_DEFAULTS.threshold;
  return { enabled: bool("enabled"), threshold, upOnly: bool("upOnly"), premarket: bool("premarket"), sound: bool("sound") };
}

export function saveSpeedSettings(storage: Writer | null, settings: ClientSpeedSettings): boolean {
  if (!storage) return false;
  try { storage.setItem(SPEED_SETTINGS_KEY, JSON.stringify(settings)); return true; }
  catch { return false; }
}

export function readSpeedDirection(storage: Reader | null): SpeedDirection {
  try {
    const value = storage?.getItem(SPEED_DIRECTION_KEY);
    if (value === "up" || value === "down" || value === "all") return value;
  } catch { /* default below */ }
  return "up";
}

export function saveSpeedDirection(storage: Writer | null, direction: SpeedDirection): boolean {
  if (!storage) return false;
  try { storage.setItem(SPEED_DIRECTION_KEY, direction); return true; }
  catch { return false; }
}

/** The settings the shared tracker decides with. Alerts switched off pass
 * a threshold no clamped gauge (0-100) can rise through, so the tracker
 * keeps sampling history (sparklines, "moving N min") but raises nothing,
 * and "Recent alerts" / "last alert" never claim an alert that was not
 * delivered. */
export function trackerSettings(settings: ClientSpeedSettings): Pick<SpeedAlertSettings, "threshold" | "upOnly" | "premarket" | "cooldownMs"> {
  return {
    threshold: settings.enabled ? settings.threshold : Number.POSITIVE_INFINITY,
    upOnly: settings.upOnly,
    premarket: settings.premarket,
    cooldownMs: SPEED_ALERT_DEFAULTS.cooldownMs,
  };
}

/** symbol -> epoch ms the mute ends (wall clock). */
export type SpeedMutes = Map<string, number>;

export function muteSymbol(mutes: SpeedMutes, symbol: string, now: number): void {
  mutes.set(symbol, now + SPEED_MUTE_MS);
}

export function isMuted(mutes: SpeedMutes, symbol: string, now: number): boolean {
  const until = mutes.get(symbol);
  if (until === undefined) return false;
  if (now < until) return true;
  mutes.delete(symbol);
  return false;
}

/** NEW: first seen after the opening snapshot, and within the last two
 * minutes. `sessionStart` is the wall-clock time of the tracker's first
 * observation, or null before it. */
export function isNewSymbol(firstSeenAt: number | null, sessionStart: number | null, now: number): boolean {
  if (firstSeenAt === null || sessionStart === null) return false;
  return firstSeenAt >= sessionStart + NEW_TAG_SNAPSHOT_GRACE_MS && now - firstSeenAt < NEW_TAG_MS;
}

/** Change since the alert, in percent; null without a usable price. */
export function alertChangePct(alertPrice: number, currentPrice: number | null | undefined): number | null {
  if (!(alertPrice > 0) || currentPrice == null || !Number.isFinite(currentPrice)) return null;
  return ((currentPrice - alertPrice) / alertPrice) * 100;
}

export function formatClock(at: number): string {
  return new Date(at).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", hour12: false });
}

export interface SummaryPart {
  text: string;
  strong?: boolean;
}

/** "N filling fast · SYMBOL leads at NN% · last alert HH:MM", each part
 * omitted when there is nothing true to say. `rows` are the rows of the
 * selected direction, fullest first (speedRows' order). */
export function speedSummary(rows: SpeedRow[], alerts: SpeedAlert[], clock: (at: number) => string = formatClock): SummaryPart[] {
  const parts: SummaryPart[] = [];
  const filling = rows.filter((row) => row.fillingFast).length;
  if (filling > 0) parts.push({ text: `${filling} filling fast`, strong: true });
  const leader = rows[0];
  if (leader && leader.pct > 0) parts.push({ text: `${leader.reading.symbol} leads at ${leader.pct}%` });
  if (alerts[0]) parts.push({ text: `last alert ${clock(alerts[0].at)}` });
  return parts;
}

/** "+19.6% in 5 min · rel vol 5.2x · filling fast" for a toast. */
export function alertDetail(alert: SpeedAlert): string {
  const parts: string[] = [];
  if (alert.movePct !== null) parts.push(`${alert.movePct >= 0 ? "+" : "−"}${Math.abs(alert.movePct).toFixed(1)}% in 5 min`);
  if (alert.relativeVolume !== null) parts.push(`rel vol ${alert.relativeVolume.toFixed(1)}x`);
  if (alert.fillingFast) parts.push("filling fast");
  return parts.join(" · ");
}

export function signedPct(value: number): string {
  return `${value >= 0 ? "+" : "−"}${Math.abs(value).toFixed(1)}%`;
}

/** Polyline points for the 10-minute gauge sparkline in a `width` x
 * `height` box: x spans the 10 minutes ending at the newest point, y is
 * the gauge (0 at the bottom). */
export function sparklinePoints(points: { at: number; pct: number }[], width: number, height: number, windowMs: number): string {
  const last = points[points.length - 1];
  if (!last) return "";
  const start = last.at - windowMs;
  return points
    .map((p) => `${(((p.at - start) / windowMs) * width).toFixed(1)},${(height - (p.pct / 100) * height).toFixed(1)}`)
    .join(" ");
}
