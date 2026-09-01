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
  const cur = new Date(Date.UTC(from.getUTCFullYear(), from.getUTCMonth(), from.getUTCDate()));
  cur.setUTCDate(cur.getUTCDate() - 1);
  while (cur.getUTCDay() === 0 || cur.getUTCDay() === 6) cur.setUTCDate(cur.getUTCDate() - 1);
  return isoDate(cur);
}

export function useGainersForDate(date: string | null): { rows: Mover[]; loading: boolean } {
  const [rows, setRows] = useState<Mover[]>([]);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!date) { setRows([]); setLoading(false); return; }
    let cancelled = false;
    setLoading(true);
    fetch(`${HTTP_URL}/movers/gainers?date=${date}`)
      .then((r) => { if (!r.ok) throw new Error(`gainers-for-date failed: ${r.status}`); return r.json() as Promise<Mover[]>; })
      .then((fetched) => { if (!cancelled) { setRows(fetched); setLoading(false); } })
      .catch(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [date]);

  return { rows, loading };
}
