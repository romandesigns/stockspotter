import { authenticatedFetch } from "@stockspotter/shared-types";
// Top Gainers / Highly Trading data -- ws-server's /movers/today (live,
// polled) and /movers/gainers?date=... (one-off historical lookup for a
// picked past date). Same base-URL resolution + best-effort-on-failure
// pattern as useHistoricalBackfill.ts.

import { useEffect, useState } from "react";
import { resolveHttpUrl } from "./config";

export type TradingSession = "premarket" | "regular" | "after_hours" | "overnight";

export interface Mover {
  symbol: string;
  price: number;
  changePct: number;
  volume: number;
  /** Which trading session produced this reading -- ws-server now keeps a
   * rolling 24h "best observed" value per symbol (market_data::movers),
   * not just the current live snapshot, so an earlier session's real
   * mover stays on the list after it's no longer live-leading. `null`
   * for the historical date-lookup path (/movers/gainers?date=), which
   * only has daily-bar resolution and genuinely can't classify a
   * session -- render nothing rather than a fabricated label. */
  session: TradingSession | null;
  observedAt?: string | null;
  volumeSource?: "snapshot_daily_bar_current" | "minute_bars_since_open" | "unknown" | null;
  provenance?: {
    priceSource: "latest_trade" | "daily_bar";
    latestTradeAt: string | null;
    tradeTimeReason: string | null;
    snapshotReceivedAt: string | null;
    volumeRetrievedAt: string | null;
    dailyBarDate: string | null;
    dailyBarRelation: string;
  } | null;
}

export interface MoverScanProvenance {
  scanStartedAt: string;
  scanCompletedAt: string;
  classificationMarketDay: string;
  oldestBatchReceivedAt: string | null;
  requestedSymbolCount: number;
  returnedEntryCount: number;
  providerOmittedCount: number;
  convertedSnapshotCount: number;
}

export interface TodayMovers {
  gainers: Mover[];
  mostActive: Mover[];
  peakGainers: Mover[];
  peakMostActive: Mover[];
  currentAvailable: boolean;
  /** When the server last observed a successful snapshot -- null until one
   * completes. A failed poll (best-effort, keeps showing stale data)
   * deliberately doesn't bump this, so UpdatedAgo correctly keeps
   * counting up from the last real refresh instead of lying about it. */
  lastUpdated: Date | null;
  lastAttemptAt?: string | null;
  lastSuccessAt?: string | null;
  lastErrorAt?: string | null;
  refreshStatus?: "refreshing" | "success" | "failed" | null;
  scanProvenance?: MoverScanProvenance | null;
  clientFetchFailed?: boolean;
}

interface MoversResponse {
  gainers: Mover[];
  mostActive: Mover[];
  currentGainers?: Mover[];
  currentMostActive?: Mover[];
  observedAt?: string | null;
  lastAttemptAt?: string | null;
  lastSuccessAt?: string | null;
  lastErrorAt?: string | null;
  refreshStatus?: TodayMovers["refreshStatus"];
  scanProvenance?: MoverScanProvenance | null;
}

export function decodeTodayMovers(fetched: MoversResponse, receivedAt = Date.now(), previous?: TodayMovers): TodayMovers {
  const currentAvailable = Array.isArray(fetched.currentGainers) && Array.isArray(fetched.currentMostActive);
  const validDate = (value?: string | null) => value && Number.isFinite(Date.parse(value)) ? new Date(value) : null;
  const observed = validDate(fetched.observedAt);
  if (fetched.refreshStatus === "failed" && !observed && previous) return {
    ...previous, clientFetchFailed: false, refreshStatus: "failed",
    lastAttemptAt: fetched.lastAttemptAt, lastErrorAt: fetched.lastErrorAt,
  };
  const legacy = fetched.refreshStatus === undefined && fetched.lastAttemptAt === undefined && fetched.lastSuccessAt === undefined && fetched.lastErrorAt === undefined;
  return {
    gainers: currentAvailable ? fetched.currentGainers! : [],
    mostActive: currentAvailable ? fetched.currentMostActive! : [],
    peakGainers: fetched.gainers,
    peakMostActive: fetched.mostActive,
    currentAvailable,
    lastAttemptAt: fetched.lastAttemptAt,
    lastSuccessAt: fetched.lastSuccessAt,
    lastErrorAt: fetched.lastErrorAt,
    refreshStatus: fetched.refreshStatus,
    scanProvenance: fetched.scanProvenance,
    clientFetchFailed: false,
    // A legacy server reports collection refresh only, never a current quote.
    lastUpdated: observed ?? validDate(fetched.lastSuccessAt) ?? (!currentAvailable && legacy && fetched.observedAt == null ? new Date(receivedAt) : null),
  };
}

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

