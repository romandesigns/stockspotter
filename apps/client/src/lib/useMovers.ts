// Top Gainers / Highly Trading data -- ws-server's /movers/today (live,
// polled) and /movers/gainers?date=... (one-off historical lookup for a
// picked past date). Same base-URL resolution + best-effort-on-failure
// pattern as useHistoricalBackfill.ts.

import { useEffect, useState } from "react";

export interface Mover {
  symbol: string;
  price: number;
  changePct: number;
  volume: number;
}

export interface TodayMovers {
  gainers: Mover[];
  mostActive: Mover[];
  /** When the last *successful* poll landed -- null until the first one
   * completes. A failed poll (best-effort, keeps showing stale data)
   * deliberately doesn't bump this, so UpdatedAgo correctly keeps
   * counting up from the last real refresh instead of lying about it. */
  lastUpdated: Date | null;
}

const DEFAULT_HTTP_URL = "http://localhost:8788";
/** Matches the backend's own movers-scan cadence (market_data::movers::
 * MOVERS_RESCAN_INTERVAL) -- polling faster than the data actually
 * refreshes would just be wasted requests. */
const POLL_MS = 60_000;

function resolveHttpUrl(): string {
  const fromEnv = (import.meta as { env?: Record<string, string | undefined> }).env?.VITE_HTTP_URL;
  return fromEnv && fromEnv.length > 0 ? fromEnv : DEFAULT_HTTP_URL;
}

/** Today's live Top Gainers + Highly Trading rankings, polled on an
 * interval. Used for Highly Trading always, and for Top Gainers whenever
 * no historical date is selected (the panel's own default). */
export function useTodayMovers(): TodayMovers {
  const [movers, setMovers] = useState<TodayMovers>({ gainers: [], mostActive: [], lastUpdated: null });

  useEffect(() => {
    let cancelled = false;

    function poll() {
      fetch(`${resolveHttpUrl()}/movers/today`)
        .then((r) => {
          if (!r.ok) throw new Error(`today movers request failed: ${r.status}`);
          // Server response has no timestamp of its own -- lastUpdated is
          // stamped client-side, right when this successful response
          // actually lands.
          return r.json() as Promise<Pick<TodayMovers, "gainers" | "mostActive">>;
        })
        .then((fetched) => {
          if (!cancelled) setMovers({ ...fetched, lastUpdated: new Date() });
        })
        .catch(() => {
          // Best-effort -- keep showing whatever was last fetched.
        });
    }

    poll();
    const id = setInterval(poll, POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, []);

  return movers;
}

/** Top Gainers for one specific past trading day (YYYY-MM-DD). `null`
 * date means "no historical date picked" -- the caller should show
 * `useTodayMovers().gainers` instead in that case rather than calling
 * this hook with a date at all. */
export function useGainersForDate(date: string | null): { rows: Mover[]; loading: boolean; error: boolean } {
  const [rows, setRows] = useState<Mover[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState(false);

  useEffect(() => {
    if (!date) {
      setRows([]);
      setLoading(false);
      setError(false);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(false);

    fetch(`${resolveHttpUrl()}/movers/gainers?date=${encodeURIComponent(date)}`)
      .then((r) => {
        if (!r.ok) throw new Error(`gainers-for-date request failed: ${r.status}`);
        return r.json() as Promise<Mover[]>;
      })
      .then((fetched) => {
        if (!cancelled) {
          setRows(fetched);
          setLoading(false);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setError(true);
          setLoading(false);
        }
      });

    return () => {
      cancelled = true;
    };
  }, [date]);

  return { rows, loading, error };
}
