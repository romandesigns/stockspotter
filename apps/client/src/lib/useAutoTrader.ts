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

// "momentum_deteriorated" (2026-09-04) -- the engine now exits a
// position early on real momentum breakdown (overall < 0.4, the same
// "critical" tier boundary MomentumScoreRow.tsx already uses), not just
// waiting for the trailing stop to eventually catch up.
export type ExitReason = "target_hit" | "stop_hit" | "timeout" | "momentum_deteriorated";
// "halt_risk_too_high" (2026-09-04) -- skips an entry when the symbol's
// latest known halt-proximity level is Amber or Red, real added risk a
// plain momentum reading doesn't capture.
export type SkipReason =
  | "momentum_gate_failed"
  | "outside_regular_hours"
  | "max_concurrent_positions"
  | "already_entered_today"
  | "zero_quantity"
  | "halt_risk_too_high";

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
      // Real context, not a gate -- empty if no catalyst is known for
      // this symbol yet (2026-09-04).
      catalystTags: string[];
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
    }
  | {
      // The trailing stop ratcheting up (2026-09-04) -- a real, visible
      // "something happened" line, not a win or a loss. Only emitted on
      // an actual increase, not every bar.
      type: "stop_adjusted";
      symbol: string;
      previousStopPrice: number;
      newStopPrice: number;
      triggerPrice: number;
      at: string;
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
