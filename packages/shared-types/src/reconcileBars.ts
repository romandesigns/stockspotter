import type { BarUpdate } from "./index";
import { mayReplace } from "./coverage";

/**
 * Fold an incoming bar into a symbol's retained series, honouring authority
 * precedence rather than letting the newest arrival always win.
 *
 * Previously this guarded only one case: a forming preview could not replace
 * a provider-final bar. That left the wider class open, and the 2026-09-21
 * audit measured it -- a partially observed live bar can overwrite a bar
 * that covered the whole interval. `mayReplace` generalises the guard over
 * provider finality AND coverage completeness, so the ordering is
 *
 *     provider final > locally complete > unknown > locally partial
 *
 * Equal authority still replaces, because that is ordinary live updating:
 * a forming bucket's newer state supersedes its older state.
 */
export function reconcileBars(existing: BarUpdate[], incoming: BarUpdate, limit: number): BarUpdate[] {
  const byTime = new Map(existing.map((bar) => [Date.parse(bar.timestamp), bar]));
  const time = Date.parse(incoming.timestamp);
  if (!Number.isFinite(time)) return existing;
  if (!mayReplace(byTime.get(time), incoming)) return existing;
  byTime.set(time, incoming);
  return [...byTime.entries()].sort(([a], [b]) => a - b).slice(-limit).map(([, bar]) => bar);
}
