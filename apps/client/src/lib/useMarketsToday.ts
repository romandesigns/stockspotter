// Markets Today panel's data -- ws-server's /markets/today (4 index-proxy
// ETFs, polled) and the existing /bars/:symbol backfill endpoint reused
// for each one's sparkline history (no new backend surface needed for
// that part at all).

import { useEffect, useMemo, useState } from "react";

export interface MarketIndexReading {
  symbol: string;
  name: string;
  price: number;
  changePct: number;
}

export interface SparkPoint {
  time: number;
  price: number;
}

const DEFAULT_HTTP_URL = "http://localhost:8788";
const POLL_MS = 60_000;
/** Sparklines show a trend shape, not a precision readout -- refreshed
 * far less often than price/%change so 4 fixed symbols' full bars series
 * aren't re-fetched every 60s poll tick for no visible benefit. */
const SPARKLINE_REFRESH_MS = 5 * 60_000;
const SPARKLINE_MINUTES = 240;

interface BarOut {
  time: number;
  close: number;
}

function resolveHttpUrl(): string {
  const fromEnv = (import.meta as { env?: Record<string, string | undefined> }).env?.VITE_HTTP_URL;
  return fromEnv && fromEnv.length > 0 ? fromEnv : DEFAULT_HTTP_URL;
}

export function useMarketsToday(): { readings: MarketIndexReading[]; sparklines: Map<string, SparkPoint[]> } {
  const [readings, setReadings] = useState<MarketIndexReading[]>([]);
  const [sparklines, setSparklines] = useState<Map<string, SparkPoint[]>>(new Map());

  useEffect(() => {
    let cancelled = false;

    function poll() {
      fetch(`${resolveHttpUrl()}/markets/today`)
        .then((r) => {
          if (!r.ok) throw new Error(`markets-today request failed: ${r.status}`);
          return r.json() as Promise<MarketIndexReading[]>;
        })
        .then((fetched) => {
          if (!cancelled) setReadings(fetched);
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

  // A stable dependency that only changes when the actual SET of symbols
  // changes (never, in practice -- the 4 proxies are fixed server-side),
  // not on every 60s price poll tick the way depending on `readings`
  // directly would.
  const symbolsKey = readings.map((r) => r.symbol).join(",");
  const symbols = useMemo(() => symbolsKey.split(",").filter(Boolean), [symbolsKey]);

  useEffect(() => {
    if (symbols.length === 0) return;
    let cancelled = false;

    function fetchSparklines() {
      Promise.all(
        symbols.map((symbol) =>
          fetch(`${resolveHttpUrl()}/bars/${encodeURIComponent(symbol)}?minutes=${SPARKLINE_MINUTES}`)
            .then((res) => (res.ok ? (res.json() as Promise<BarOut[]>) : []))
            .then((bars) => [symbol, bars.map((b) => ({ time: b.time, price: b.close }))] as [string, SparkPoint[]])
            .catch(() => [symbol, []] as [string, SparkPoint[]]),
        ),
      ).then((entries) => {
        if (!cancelled) setSparklines(new Map(entries));
      });
    }

    fetchSparklines();
    const id = setInterval(fetchSparklines, SPARKLINE_REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [symbols]);

  return { readings, sparklines };
}
