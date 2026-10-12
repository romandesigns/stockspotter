import { MICROPULLBACK_SEEN_TTL_MS } from "@stockspotter/shared-types";
const COOLDOWN = 15 * 60_000;
export function decodeAlertMemory(raw: string | null, now: number) {
  const empty = { seen: new Map<string, number>(), cooldown: new Map<string, number>() };
  if (!raw || raw.length > 128 * 1024) return empty;
  try {
    const parsed = JSON.parse(raw);
    if (parsed.version !== 1 || !Array.isArray(parsed.seen) || !Array.isArray(parsed.cooldown)) return empty;
    function restore(rows: unknown[], ttl: number) {
      const result = new Map<string, number>();
      for (const row of rows) if (Array.isArray(row) && row.length === 2 && typeof row[0] === "string" && typeof row[1] === "number" && Number.isFinite(row[1]) && row[1] <= now + 30_000 && now - row[1] <= ttl) result.set(row[0], row[1]);
      return result;
    }
    return { seen: restore(parsed.seen, MICROPULLBACK_SEEN_TTL_MS), cooldown: restore(parsed.cooldown, COOLDOWN) };
  } catch { return empty; }
}
