import { useEffect, useState } from "react";
import { HTTP_URL } from "./config";
import type { MarketReading, Mover } from "./types";
const POLL_MS = 60_000; const EMPTY_MOVERS = { gainers: [] as Mover[], mostActive: [] as Mover[] };
export function useMarketData() {
  const [indices, setIndices] = useState<MarketReading[]>([]); const [movers, setMovers] = useState(EMPTY_MOVERS); const [loading, setLoading] = useState(true); const [error, setError] = useState(false);
  useEffect(() => { let disposed = false; async function poll() { try { const [marketResponse, moversResponse] = await Promise.all([fetch(`${HTTP_URL}/markets/today`), fetch(`${HTTP_URL}/movers/today`)]); if (!marketResponse.ok || !moversResponse.ok) throw new Error("market request failed"); const [nextIndices, nextMovers] = await Promise.all([marketResponse.json() as Promise<MarketReading[]>, moversResponse.json() as Promise<typeof EMPTY_MOVERS>]); if (!disposed) { setIndices(nextIndices); setMovers(nextMovers); setError(false); } } catch { if (!disposed) setError(true); } finally { if (!disposed) setLoading(false); } }
    poll(); const timer = setInterval(poll, POLL_MS); return () => { disposed = true; clearInterval(timer); }; }, []);
  return { indices, movers, loading, error };
}
