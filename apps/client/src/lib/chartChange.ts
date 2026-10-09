import type { CandleBar } from "./derive";

export interface ChartReferenceQuote { price: number; changePct: number; timestamp: string; source?: "scanner" | "snapshot" }
export type ChartReferenceCache = Map<string, { day: string; base: number; source: string }>;
const dayFormat = new Intl.DateTimeFormat("en-CA", { timeZone: "America/New_York", year: "numeric", month: "2-digit", day: "2-digit" });
const timeFormat = new Intl.DateTimeFormat("en-US", { timeZone: "America/New_York", hour: "2-digit", minute: "2-digit", hour12: false });
const day = (ms: number) => dayFormat.format(ms);

/** Never infer a daily reference from a historical peak or an old session.
 * A coherent scanner/current-snapshot pair encodes its own reference close. */
export function chartChangeReference(bars: CandleBar[], quotes: ChartReferenceQuote[] = [], now = Date.now(), barTime = bars.at(-1)?.time, cache?: ChartReferenceCache, symbol = "", pin = true) {
  const today = day(now);
  if (cache && pin) for (const [key, entry] of cache) if (entry.day !== today) cache.delete(key);
  const pinned = cache?.get(symbol);
  if (pinned?.day === today && barTime !== undefined && day(barTime * 1000) === today) return { base: pinned.base, label: "Day", title: `Change vs prior close inferred from ${pinned.source}; reference held for this New York day` };
  const candidates = quotes.filter((q) => {
    const at = Date.parse(q.timestamp);
    return Number.isFinite(at) && now - at >= -30_000 && now - at <= 180_000
      && barTime !== undefined && day(at) === day(now) && day(at) === day(barTime * 1000)
      // Older snapshot code also encodes an invalid reference as gap=0.
      // Without explicit provenance, zero cannot prove a prior close.
      && Number.isFinite(q.price) && q.price > 0 && Number.isFinite(q.changePct) && q.changePct > -100 && q.changePct !== 0;
  }).sort((a, b) => Number(b.source === "scanner") - Number(a.source === "scanner") || Date.parse(b.timestamp) - Date.parse(a.timestamp));
  const q = candidates[0];
  if (q) {
    const base = q.price / (1 + q.changePct / 100);
    if (Number.isFinite(base) && base > 0) {
      if (pin) cache?.set(symbol, { day: today, base, source: q.source ?? "snapshot" });
      return { base, label: "Day", title: "Change vs prior close, from the current scanner/snapshot reference" };
    }
  }
  const first = bars[0];
  const start = first ? timeFormat.format(first.time * 1000) : "—";
  return { base: first?.open ?? 0, label: `Since ${start} ET`, title: "Change since the first loaded candle; prior-close reference unavailable" };
}
