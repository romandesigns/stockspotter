import type { IgnitionEvent } from "@stockspotter/shared-types";

export const ALERT_MAX_AGE_MS = 120_000;
export const ALERT_COOLDOWN_MS = 15 * 60_000;
export const MAX_VISIBLE_IGNITION_TOASTS = 3;
const MAX_SEEN = 2048;

export function createIgnitionDeliveryState() {
  return { seen: new Map<string, number>(), lastAlerted: new Map<string, number>() };
}

/** Snapshot replay and live frames share a wire shape. Age, dedup and the
 * per-symbol cooldown therefore apply to both, independent of React batches. */
export function collectFreshIgnitions(state: ReturnType<typeof createIgnitionDeliveryState>, events: IgnitionEvent[], now: number): IgnitionEvent[] {
  for (const [key, at] of state.seen) if (now - at > ALERT_COOLDOWN_MS) state.seen.delete(key);
  for (const [symbol, at] of state.lastAlerted) if (now - at > ALERT_COOLDOWN_MS) state.lastAlerted.delete(symbol);
  const fresh: IgnitionEvent[] = [];
  for (const event of [...events].sort((a, b) => Date.parse(a.timestamp) - Date.parse(b.timestamp))) {
    const at = Date.parse(event.timestamp);
    if (!Number.isFinite(at) || at > now + 30_000) continue;
    const key = `${event.symbol}-${event.timestamp}`;
    if (state.seen.has(key)) continue;
    state.seen.set(key, at);
    while (state.seen.size > MAX_SEEN) state.seen.delete(state.seen.keys().next().value!);
    const previous = state.lastAlerted.get(event.symbol);
    if (previous !== undefined && at - previous < ALERT_COOLDOWN_MS) continue;
    state.lastAlerted.set(event.symbol, at);
    while (state.lastAlerted.size > 16_384) state.lastAlerted.delete(state.lastAlerted.keys().next().value!);
    if (now - at <= ALERT_MAX_AGE_MS) fresh.push(event);
  }
  return fresh;
}
