import { authenticatedFetch, decodeTodayMovers } from "@stockspotter/shared-types";
import type { TodayMovers, MoversResponse } from "@stockspotter/shared-types";
import { useEffect, useState } from "react";
import { HTTP_URL } from "./config";
import type { MarketReading } from "./types";
const POLL_MS = 60_000;
const EMPTY_MOVERS: TodayMovers = { gainers: [], mostActive: [], peakGainers: [], peakMostActive: [], currentAvailable: false, lastUpdated: null };
export function useMarketData() {
  const [indices, setIndices] = useState<MarketReading[]>([]);
  const [movers, setMovers] = useState(EMPTY_MOVERS);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(false);
  const [lastUpdated, setLastUpdated] = useState<Date | null>(null);
  useEffect(() => {
    let disposed = false;
    let busy = false;
    let controller: AbortController | null = null;
    async function poll() {
      if (busy) return;
      busy = true; controller = new AbortController();
      const timer = setTimeout(() => controller?.abort(), 15_000);
      await Promise.all([
        authenticatedFetch(`${HTTP_URL}/markets/today`, { signal: controller.signal }).then(async response => {
          if (!response.ok) throw new Error("market request failed");
          const value = await response.json() as MarketReading[];
          if (!Array.isArray(value)) throw new Error("invalid market response");
          if (!disposed) { setIndices(value); setError(false); setLastUpdated(new Date()); }
        }).catch(() => { if (!disposed) setError(true); }),
        authenticatedFetch(`${HTTP_URL}/movers/today`, { signal: controller.signal }).then(async response => {
          if (!response.ok) throw new Error("movers request failed");
          const value = await response.json() as MoversResponse;
          if (!value || !Array.isArray(value.gainers) || !Array.isArray(value.mostActive)) throw new Error("invalid movers response");
          if (!disposed) setMovers(previous => decodeTodayMovers(value, Date.now(), previous));
        }).catch(() => { if (!disposed) setMovers(previous => ({ ...previous, clientFetchFailed: true })); }),
      ]);
      clearTimeout(timer); busy = false;
      if (!disposed) setLoading(false);
    }
    void poll(); const timer = setInterval(() => { void poll(); }, POLL_MS);
    return () => { disposed = true; clearInterval(timer); controller?.abort(); };
  }, []);
  return { indices, movers, loading, error, lastUpdated };
}