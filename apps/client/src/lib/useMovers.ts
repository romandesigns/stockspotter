import { authenticatedFetch } from "@stockspotter/shared-types";
// Top Gainers / Highly Trading data -- ws-server's /movers/today (live,
// polled) and /movers/gainers?date=... (one-off historical lookup for a
// picked past date). Same base-URL resolution + best-effort-on-failure
// pattern as useHistoricalBackfill.ts.

import { useEffect, useState } from "react";
import { resolveHttpUrl } from "./config";

import { decodeTodayMovers } from "@stockspotter/shared-types";
import type { Mover, TodayMovers, MoversResponse } from "@stockspotter/shared-types";
export { decodeTodayMovers } from "@stockspotter/shared-types";
export type { TradingSession, Mover, MoverScanProvenance, TodayMovers } from "@stockspotter/shared-types";

/** Matches the backend's own movers-scan cadence (market_data::movers::
 * MOVERS_RESCAN_INTERVAL) -- polling faster than the data actually
 * refreshes would just be wasted requests. */
const POLL_MS = 60_000;

/** Today's live Top Gainers + Highly Trading rankings, polled on an
 * interval. Used for Highly Trading always, and for Top Gainers whenever
 * no historical date is selected (the panel's own default). */
export function useTodayMovers(): TodayMovers {
  const [movers, setMovers] = useState<TodayMovers>({ gainers: [], mostActive: [], peakGainers: [], peakMostActive: [], currentAvailable: false, lastUpdated: null });

  useEffect(() => {
    let cancelled = false;

    function poll() {
      authenticatedFetch(`${resolveHttpUrl()}/movers/today`)
        .then((r) => {
          if (!r.ok) throw new Error(`today movers request failed: ${r.status}`);
          // Use the server observation timestamp so polling stale data cannot refresh its age.
          return r.json() as Promise<MoversResponse>;
        })
        .then((fetched) => {
          if (!cancelled) setMovers((previous) => decodeTodayMovers(fetched, Date.now(), previous));
        })
        .catch(() => {
          // Best-effort -- keep showing whatever was last fetched.
          if (!cancelled) setMovers((previous) => ({ ...previous, clientFetchFailed: true }));
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

    authenticatedFetch(`${resolveHttpUrl()}/movers/gainers?date=${encodeURIComponent(date)}`)
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

