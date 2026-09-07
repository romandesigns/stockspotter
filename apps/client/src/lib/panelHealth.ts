// Pure decision logic for the two "this panel can't answer right now"
// states, extracted from the panels that render them so they can be
// tested without a DOM.
//
// Both exist for the same reason: a detection panel showing nothing is
// ambiguous. "No setups qualify" is a trading observation; "the scanner
// couldn't check" is an outage; and outside LULD hours "calm" means "no
// halt is possible", not "this stock is quiet". Rendering those
// identically is how a monitoring surface quietly stops being trusted,
// so the distinction is computed here and stated in the UI.

import type { FunnelHealth, HaltWarning } from "@stockspotter/shared-types";

/**
 * Why the Gap & Go panel can't currently answer, or null when it can.
 *
 * Stage 1 fails closed on unknown float, so an exhausted FMP budget or a
 * missing API key empties the panel exactly the way a quiet market does.
 */
export function funnelBlindReason(health: FunnelHealth | null | undefined): string | null {
  if (!health) return null;
  if (health.apiKeyMissing) {
    return "FMP_API_KEY not set — Stage 1 can't check float, so nothing can qualify.";
  }
  if (health.starvedCandidates > 0) {
    const n = health.starvedCandidates;
    return `${n} candidate${n === 1 ? "" : "s"} cleared Stage 2 but couldn't be float-checked — daily FMP budget spent (${health.floatBudgetRemaining}/${health.floatBudget} left).`;
  }
  return null;
}

/**
 * Whether LULD bands are actually in force for this reading.
 *
 * A ws-server predating the `luldInEffect` field sends nothing here;
 * that's treated as "in effect", the behavior the panel had before the
 * field existed, rather than blanking every card against an older
 * server.
 */
export function isLuldInEffect(reading: Pick<HaltWarning, "luldInEffect">): boolean {
  return reading.luldInEffect !== false;
}

/**
 * True when every visible halt reading is outside LULD hours — the
 * whole-panel case worth saying once in the header rather than repeating
 * on every card.
 *
 * Requires a non-empty list: an empty panel is "no data yet", which is a
 * different message entirely.
 */
export function isOutsideLuldHours(readings: Pick<HaltWarning, "luldInEffect">[]): boolean {
  return readings.length > 0 && readings.every((r) => !isLuldInEffect(r));
}
