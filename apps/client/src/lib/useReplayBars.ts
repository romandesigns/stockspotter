// Fetches real multi-day 1-minute bars for the Backtest Replay dialog
// from ws-server's new /replay/bars/:symbol?start&end endpoint -- same
// resolveHttpUrl/best-effort pattern as useHistoricalBackfill.ts and
// useMarketsToday.ts, just keyed on a date range instead of a rolling
// "last N minutes" window.

import { useEffect, useState } from "react";
import type { CandleBar } from "./derive";

const DEFAULT_HTTP_URL = "http://localhost:8788";

function resolveHttpUrl(): string {
  const fromEnv = (import.meta as { env?: Record<string, string | undefined> }).env?.VITE_HTTP_URL;
  return fromEnv && fromEnv.length > 0 ? fromEnv : DEFAULT_HTTP_URL;
}

export function useReplayBars(
  symbol: string | null,
  start: string | null,
  end: string | null,
): { bars: CandleBar[]; loading: boolean; error: boolean } {
  const [bars, setBars] = useState<CandleBar[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState(false);

  useEffect(() => {
    if (!symbol || !start || !end) {
      setBars([]);
      setLoading(false);
      setError(false);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(false);

    fetch(`${resolveHttpUrl()}/replay/bars/${encodeURIComponent(symbol)}?start=${start}&end=${end}`)
      .then((r) => {
        if (!r.ok) throw new Error(`replay bars request failed: ${r.status}`);
        return r.json() as Promise<CandleBar[]>;
      })
      .then((fetched) => {
        if (!cancelled) {
          setBars(fetched);
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
  }, [symbol, start, end]);

  return { bars, loading, error };
}
