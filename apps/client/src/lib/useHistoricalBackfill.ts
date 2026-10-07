import { authenticatedFetch } from "@stockspotter/shared-types";
// Fetches real historical 1-minute bars from ws-server's new /bars/:symbol
// endpoint (crates/ws-server/src/http.rs) the moment a symbol is
// selected — without this, a freshly-selected symbol only has whatever's
// accumulated live since ws-server started tracking it this session,
// nowhere near the density of the Artifact prototype's own pre-fetched
// full-session demo data. Best-effort: if the fetch fails (backend down,
// symbol not covered, rate-limited), the chart still works off live data
// alone, just sparser until more bars arrive.

import { useEffect, useState } from "react";
import { resolveHttpUrl } from "./config";
import type { CandleBar } from "./derive";

/** ~4 hours -- enough to make a freshly-selected symbol's chart
 * genuinely readable without asking Alpaca for a full multi-day history
 * this component doesn't need. */
const BACKFILL_MINUTES = 240;
const NO_BARS: CandleBar[] = [];

/**
 * @param resyncNonce Bumped by useRealtimeFeed whenever a gap is recorded
 *   (`stream_lagged` or a reconnect). This endpoint is the one
 *   authoritative repair mechanism the current protocol offers, so a gap
 *   re-runs the fetch and overwrites the suspect window with real server
 *   history rather than leaving the client to guess what it missed.
 *
 *   It covers 1-minute bars only. There is no sub-minute backfill (a real
 *   Alpaca constraint, see SuperChart.tsx), so a 30-second chart cannot be
 *   repaired this way and is surfaced as gapped instead -- see
 *   feedHealth.ts and §7 of the chart fidelity audit for the backend work
 *   that would be required to do better.
 */
export function useHistoricalBackfill(symbol: string | null, resyncNonce = 0): CandleBar[] {
  const [state, setState] = useState<{symbol: string | null; bars: CandleBar[]}>({symbol: null, bars: NO_BARS});

  useEffect(() => {
    if (!symbol) return;
    let cancelled = false;

    authenticatedFetch(`${resolveHttpUrl()}/bars/${encodeURIComponent(symbol)}?minutes=${BACKFILL_MINUTES}`)
      .then((r) => {
        if (!r.ok) throw new Error(`backfill request failed: ${r.status}`);
        return r.json() as Promise<CandleBar[]>;
      })
      .then((fetched) => {
        if (!cancelled) setState({ symbol, bars: fetched });
      })
      .catch(() => {
        // Best-effort -- live data alone still works, just sparser. On a
        // resync this means the gap stays flagged, which is the honest
        // outcome: we tried to repair it and could not.
      });

    return () => {
      cancelled = true;
    };
  }, [symbol, resyncNonce]);

  return state.symbol === symbol ? state.bars : NO_BARS;
}
