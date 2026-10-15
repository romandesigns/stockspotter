import type { ConsolidationEvent, MomentumUpdate } from "./index";


function confirmedMomentum(value: MomentumUpdate | undefined) { return Boolean(value && value.overall >= 0.6 && value.volumeConfirmation >= 0.6); }

export const MICROPULLBACK_MAX_AGE_MS = 120_000;
export const MICROPULLBACK_FUTURE_TOLERANCE_MS = 30_000;
export const MICROPULLBACK_SEEN_TTL_MS = MICROPULLBACK_MAX_AGE_MS + MICROPULLBACK_FUTURE_TOLERANCE_MS + 10_000;
export const MAX_VISIBLE_MICROPULLBACK_TOASTS = 3;
export function micropullbackKey(event: Pick<ConsolidationEvent, "symbol" | "timestamp">) {
  const at = typeof event.timestamp === "string" ? Date.parse(event.timestamp) : NaN;
  if (!Number.isFinite(at)) return JSON.stringify([event.symbol, event.timestamp]);
  // Date.parse truncates to milliseconds. Preserve the remaining fraction
  // so formatting aliases dedup without collapsing precise server events.
  const fraction = /\.(\d+)(?:Z|[+-]\d{2}:\d{2})$/.exec(event.timestamp)?.[1] ?? "";
  return JSON.stringify([event.symbol, at, fraction.slice(3).replace(/0+$/, "")]);
}

export function isFreshMicropullback(event: Pick<ConsolidationEvent, "symbol" | "timestamp" | "price">, now: number): boolean {
  if (typeof event.symbol !== "string" || typeof event.timestamp !== "string") return false;
  const at = Date.parse(event.timestamp);
  return Number.isFinite(at) && at <= now + MICROPULLBACK_FUTURE_TOLERANCE_MS
    && now - at <= MICROPULLBACK_MAX_AGE_MS && event.symbol.trim().length > 0
    && Number.isFinite(event.price) && event.price > 0;
}

/** Called from an effect, never during render. The precise timestamp and
 * notification identity are deliberate. A still-fresh event can qualify
 * when its momentum arrives later, but never announces twice.
 * Memory is bounded by the retained feed plus arrivals in the TTL window,
 * rather than by a tiny FIFO cap that would reannounce fresh replay. */
export function reduceMicropullbackDelivery(
  previous: ReadonlyMap<string, number>,
  events: ConsolidationEvent[],
  momentum: Map<string, MomentumUpdate>,
  now: number,
) {
  const seen = new Map(previous);
  const retained = new Set(events.map(micropullbackKey));
  const fresh: ConsolidationEvent[] = [];
  for (const event of events) {
    const key = micropullbackKey(event);
    if (seen.has(key)) continue;
    const eligible = isFreshMicropullback(event, now);
    if (eligible && confirmedMomentum(momentum.get(event.symbol))) {
      seen.set(key, now);
      fresh.push(event);
    } else if (!eligible) {
      seen.set(key, now);
    }
  }
  for (const [key, firstSeenAt] of seen) {
    if (!retained.has(key) && now - firstSeenAt > MICROPULLBACK_SEEN_TTL_MS) seen.delete(key);
  }
  return { seen, fresh };
}
