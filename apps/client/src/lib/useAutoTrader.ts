// Auto-trader monitoring data -- ws-server's /auto-trader/status, which
// reads the shared JSONL journal crates/auto-trader writes to and
// computes running stats + open positions + recent activity from it (see
// that route's own doc comment: no second internal service, no proxy,
// just the same "read shared state" shape /markets/today already uses).
// Same fetch/poll/cancelled-flag pattern as useMarketsToday.ts -- faster
// interval than the 60s movers cadence since this is meant to feel like
// watching something live.

import { useEffect, useState } from "react";
import { resolveHttpUrl } from "./config";

export type ExitReason = "target_hit" | "stop_hit" | "timeout";
export type SkipReason = "momentum_gate_failed" | "outside_regular_hours" | "max_concurrent_positions" | "already_entered_today" | "zero_quantity";

export interface OpenPosition {
  symbol: string;
  entryPrice: number;
  qty: number;
  enteredAt: string;
  targetPrice: number;
  stopPrice: number;
}

export type JournalEntry =
  | {
      type: "entered";
      symbol: string;
      entryPrice: number;
      qty: number;
      positionSizeUsd: number;
      targetPrice: number;
      stopPrice: number;
      enteredAt: string;
      momentumOverall: number;
      momentumVolumeConfirmation: number;
    }
  | {
      type: "exited";
      symbol: string;
      exitPrice: number;
      exitReason: ExitReason;
      pnlUsd: number;
      pnlPct: number;
      qty: number;
      enteredAt: string;
      exitedAt: string;
    }
  | {
      type: "skipped";
      symbol: string;
      reason: SkipReason;
      at: string;
      detail: string;
    };

export interface AutoTraderStatus {
  trades: number;
  wins: number;
  losses: number;
  cumulativePnlUsd: number;
  openPositions: OpenPosition[];
  recentEntries: JournalEntry[];
}

const POLL_MS = 15_000;
const EMPTY_STATUS: AutoTraderStatus = { trades: 0, wins: 0, losses: 0, cumulativePnlUsd: 0, openPositions: [], recentEntries: [] };

export function useAutoTrader(): AutoTraderStatus {
  const [status, setStatus] = useState<AutoTraderStatus>(EMPTY_STATUS);

  useEffect(() => {
    let cancelled = false;

    function poll() {
      fetch(`${resolveHttpUrl()}/auto-trader/status`)
        .then((r) => {
          if (!r.ok) throw new Error(`auto-trader status request failed: ${r.status}`);
          return r.json() as Promise<AutoTraderStatus>;
        })
        .then((fetched) => {
          if (!cancelled) setStatus(fetched);
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

  return status;
}
