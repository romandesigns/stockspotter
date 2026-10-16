// Speed alert settings as saved on the phone (useSpeedAlerts.ts). Pure, so
// bun:test covers the decoding (hub.test.ts).
import type { SpeedAlertSettings } from "@stockspotter/shared-types";
import { SPEED_ALERT_DEFAULTS, SPEED_ALERT_THRESHOLDS } from "@stockspotter/shared-types";

/** Saved settings merged over the defaults, field by field, so a value
 * from an older build (or a corrupt one) never yields an unusable mix. */
export function decodeSpeedSettings(raw: string | null): SpeedAlertSettings {
  if (!raw) return SPEED_ALERT_DEFAULTS;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return SPEED_ALERT_DEFAULTS;
  }
  if (!parsed || typeof parsed !== "object") return SPEED_ALERT_DEFAULTS;
  const p = parsed as Record<string, unknown>;
  const bool = (key: keyof SpeedAlertSettings) => (typeof p[key] === "boolean" ? (p[key] as boolean) : (SPEED_ALERT_DEFAULTS[key] as boolean));
  const threshold = (SPEED_ALERT_THRESHOLDS as readonly number[]).includes(p.threshold as number) ? (p.threshold as number) : SPEED_ALERT_DEFAULTS.threshold;
  return {
    threshold,
    upOnly: bool("upOnly"),
    premarket: bool("premarket"),
    sound: bool("sound"),
    push: bool("push"),
    // Not user-editable on the phone; always the shared default.
    cooldownMs: SPEED_ALERT_DEFAULTS.cooldownMs,
  };
}

