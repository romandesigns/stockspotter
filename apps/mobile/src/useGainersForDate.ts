import { authenticatedFetch } from "@stockspotter/shared-types";
// Real historical Top Gainers lookup -- same GET /movers/gainers?date=
// endpoint the web app's SessionDatePicker uses, just with a simpler
// Today/Yesterday toggle instead of a full calendar (per Roman's own
// "keep in mind the mobile context" instruction -- a phone-sized Top
// Gainers card has room for two quick options, not a date-range picker).

import { useEffect, useState } from "react";
import { HTTP_URL } from "./config";
import type { Mover } from "./types";

function isoDate(d: Date): string {
  return d.toISOString().slice(0, 10);
}

/** Most recent weekday before `from` -- same weekend-skipping logic the
 * web app's tradingDays.ts uses, ported to the one case mobile needs. */
export function previousSession(from: Date): string {
  const parts = new Intl.DateTimeFormat("en-US", { timeZone: "America/New_York", year: "numeric", month: "2-digit", day: "2-digit" }).formatToParts(from);
  const part = (name: string) => Number(parts.find(value => value.type === name)?.value);
  const cur = new Date(Date.UTC(part("year"), part("month") - 1, part("day")));
  cur.setUTCDate(cur.getUTCDate() - 1);
  while (cur.getUTCDay() === 0 || cur.getUTCDay() === 6) cur.setUTCDate(cur.getUTCDate() - 1);
  return isoDate(cur);
}

export function useGainersForDate(date: string | null): { rows: Mover[]; loading: boolean; error: boolean } {
  const [result, setResult] = useState<{ date: string | null; rows: Mover[]; loading: boolean; error: boolean }>({ date: null, rows: [], loading: false, error: false });
  useEffect(() => {
    if (!date) { setResult({ date: null, rows: [], loading: false, error: false }); return; }
    let cancelled = false;
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 15_000);
    setResult({ date, rows: [], loading: true, error: false });
    authenticatedFetch(`${HTTP_URL}/movers/gainers?date=${date}`, { signal: controller.signal })
      .then(async response => {
        if (!response.ok) throw new Error("Historical gainers unavailable");
        const rows = await response.json() as Mover[];
        if (!Array.isArray(rows)) throw new Error("Invalid historical response");
        if (!cancelled) setResult({ date, rows, loading: false, error: false });
      })
      .catch(() => { if (!cancelled) setResult({ date, rows: [], loading: false, error: true }); })
      .finally(() => clearTimeout(timer));
    return () => { cancelled = true; clearTimeout(timer); controller.abort(); };
  }, [date]);
  return result.date === date ? result : { rows: [], loading: date !== null, error: false };
}
