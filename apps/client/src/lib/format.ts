export function formatPrice(n: number): string {
  return `$${n.toFixed(n < 1 ? 4 : 2)}`;
}

export function formatPct(n: number): string {
  const sign = n > 0 ? "+" : "";
  return `${sign}${n.toFixed(1)}%`;
}

export function formatVolume(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return `${n}`;
}

export function formatTime(iso: string): string {
  const d = new Date(iso);
  return d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

/** "offering_dilution" -> "offering dilution" -- underscores to spaces
 * only; visual title-casing is CSS's job (text-transform: capitalize),
 * same convention .feed-row-kind already uses for FunnelSignal's kind
 * label rather than doing it in JS. */
export function formatTag(tag: string): string {
  return tag.replace(/_/g, " ");
}
