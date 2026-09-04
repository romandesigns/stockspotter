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
// "strategy_disabled" (2026-09-05, v4) -- this strategy's trigger is
// currently turned off by real, evidence-driven review (see the
// "strategy_config_changed" JournalEntry variant below for the decision
// that set this).
export type SkipReason =
  | "momentum_gate_failed"
  | "outside_regular_hours"
  | "max_concurrent_positions"
  | "already_entered_today"
  | "zero_quantity"
  | "halt_risk_too_high"
  | "strategy_disabled";

// Which real trigger opened a position (v3, 2026-09-04, Roman's own ask
// to broaden past Micropullback-only -- see auto-trader's engine.rs for
// which three and why the other two are deliberately left out). Matches
// backtest_metrics::Strategy's own Serialize output (PascalCase) exactly,
// not a re-cased copy.
export type Strategy = "FastFunnel" | "MomentumScorer" | "IgnitionDetector" | "ConsolidationBreakout" | "Micropullback";

export interface OpenPosition {
  symbol: string;
  strategy: Strategy;
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
      strategy: Strategy;
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
    }
  | {
      // Evidence-driven strategy selection (2026-09-05, v4) -- the
      // engine turning one of its own entry triggers on/off based on
      // real live-efficiency evidence (backtest_metrics::
      // decide_enabled_strategies), not a symbol-level event -- no
      // `symbol` field, unlike every other variant here.
      type: "strategy_config_changed";
      strategy: Strategy;
      enabled: boolean;
      sampleSize: number;
      expectancyPct: number | null;
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
