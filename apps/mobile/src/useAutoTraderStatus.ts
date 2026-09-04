// Auto-trader monitoring data -- ws-server's /auto-trader/status (reads
// the shared JSONL journal crates/auto-trader writes to and computes
// running stats + open positions + recent activity from it -- see that
// route's own doc comment). Same poll/disposed-flag shape as
// useMarketData.ts, just one endpoint instead of two.
import { useEffect, useState } from "react";
import { HTTP_URL } from "./config";
import type { AutoTraderStatus } from "./types";

const POLL_MS = 15_000;
const EMPTY_STATUS: AutoTraderStatus = { trades: 0, wins: 0, losses: 0, cumulativePnlUsd: 0, openPositions: [], recentEntries: [] };

export function useAutoTraderStatus() {
  const [status, setStatus] = useState<AutoTraderStatus>(EMPTY_STATUS);
  const [error, setError] = useState(false);

  useEffect(() => {
    let disposed = false;
    async function poll() {
      try {
        const response = await fetch(`${HTTP_URL}/auto-trader/status`);
        if (!response.ok) throw new Error("auto-trader status request failed");
        const fetched = (await response.json()) as AutoTraderStatus;
        if (!disposed) {
          setStatus(fetched);
          setError(false);
        }
      } catch {
        if (!disposed) setError(true);
      }
    }
    poll();
    const timer = setInterval(poll, POLL_MS);
    return () => {
      disposed = true;
      clearInterval(timer);
    };
  }, []);

  return { status, error };
}
