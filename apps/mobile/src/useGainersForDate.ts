import { authenticatedFetch } from "@stockspotter/shared-types";
// Real historical Top Gainers lookup -- same GET /movers/gainers?date=
// endpoint the web app's SessionDatePicker uses, just with a simpler
// Today/Yesterday toggle instead of a full calendar (per Roman's own
// "keep in mind the mobile context" instruction -- a phone-sized Top
// Gainers card has room for two quick options, not a date-range picker).

import { useEffect, useState } from "react";
import { HTTP_URL } from "./config";
import type { Mover } from "./types";

export { previousSession } from "./tradingDays";

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
