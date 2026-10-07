import type { BarUpdate } from "./index";

/** Keep provider corrections without allowing a forming preview to replace a final bar. */
export function reconcileBars(existing: BarUpdate[], incoming: BarUpdate, limit: number): BarUpdate[] {
  const byTime = new Map(existing.map((bar) => [Date.parse(bar.timestamp), bar]));
  const time = Date.parse(incoming.timestamp);
  if (!Number.isFinite(time)) return existing;
  if (byTime.get(time)?.isFinal && !incoming.isFinal) return existing;
  byTime.set(time, incoming);
  return [...byTime.entries()].sort(([a], [b]) => a - b).slice(-limit).map(([, bar]) => bar);
}
