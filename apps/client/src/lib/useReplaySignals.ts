import { authenticatedFetch } from "@stockspotter/shared-types";
// Detection signals for the Backtest Replay window — what the scanner
// WOULD have fired, plotted on the same chart as the price it fired on.
//
// Architecture doc section 7 asks for exactly this: "indicators and any
// detection signals (ignition alerts, momentum panel qualifications,
// etc.) render on the chart at the exact moments they would have fired
// live." The replay dialog shipped playing bars back and nothing else,
// which showed price but never showed what the system would have DONE
// about that price — and watching a strategy fire against real history
// is the fastest way to build or destroy trust in it.
//
// Same resolveHttpUrl/best-effort shape as useReplayBars.ts (its
// sibling, fetched over the same date range), deliberately kept as a
// SEPARATE hook rather than folded into that one: signal replay needs
// tick data, so it is far slower and capped at a much narrower span
// server-side (MAX_SIGNAL_SPAN_DAYS = 3 vs bars' 45). Bars must keep
// rendering promptly on a wide range even when signals can't be
// computed for it — one failing must never blank the other.

import { useEffect, useState } from "react";
import { resolveHttpUrl } from "./config";

export interface ReplaySignal {
  /** Unix seconds, same base as CandleBar.time. */
  time: number;
  price: number;
  /** Rust `Strategy`'s Debug name, e.g. "IgnitionDetector". */
  strategy: string;
}

export function useReplaySignals(
  symbol: string | null,
  start: string | null,
  end: string | null,
): { signals: ReplaySignal[]; loading: boolean; error: boolean } {
  const [signals, setSignals] = useState<ReplaySignal[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState(false);

  useEffect(() => {
    if (!symbol || !start || !end) {
      setSignals([]);
      setLoading(false);
      setError(false);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(false);

    authenticatedFetch(`${resolveHttpUrl()}/replay/signals/${encodeURIComponent(symbol)}?start=${start}&end=${end}`)
      .then((r) => {
        if (!r.ok) throw new Error(`replay signals request failed: ${r.status}`);
        return r.json() as Promise<ReplaySignal[]>;
      })
      .then((fetched) => {
        if (!cancelled) {
          setSignals(fetched);
          setLoading(false);
        }
      })
      .catch(() => {
        if (!cancelled) {
          // A 400 here is the normal, expected answer for a range wider
          // than the server's tick-data cap — not an outage. Either way
          // the chart still has its bars; the caller shows a quiet note
          // rather than an error state.
          setSignals([]);
          setError(true);
          setLoading(false);
        }
      });

    return () => {
      cancelled = true;
    };
  }, [symbol, start, end]);

  return { signals, loading, error };
}
