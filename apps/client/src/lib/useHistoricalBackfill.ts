import { authenticatedFetch } from "@stockspotter/shared-types";
// Fetches real historical 1-minute bars from ws-server's new /bars/:symbol
// endpoint (crates/ws-server/src/http.rs) the moment a symbol is
// selected — without this, a freshly-selected symbol only has whatever's
// accumulated live since ws-server started tracking it this session,
// nowhere near the density of the Artifact prototype's own pre-fetched
// full-session demo data. Best-effort: if the fetch fails (backend down,
// symbol not covered, rate-limited), the chart still works off live data
// alone, just sparser until more bars arrive.

import { useEffect, useRef, useState } from "react";
import { resolveHttpUrl } from "./config";
import type { CandleBar } from "./derive";

/** ~4 hours -- enough to make a freshly-selected symbol's chart
 * genuinely readable without asking Alpaca for a full multi-day history
 * this component doesn't need. */
const BACKFILL_MINUTES = 240;

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
  const [bars, setBars] = useState<CandleBar[]>([]);
  // Which symbol the bars in state actually belong to. Lets one effect
  // serve both triggers without depending on effect declaration order:
  // a symbol change must blank immediately, a resync must not.
  const loadedFor = useRef<string | null>(null);

  useEffect(() => {
    // Resetting state to synchronize with an external resource (a fetch
    // keyed to `symbol`) on a prop change -- React's own documented
    // pattern for this exact case, not the redundant-setState smell the
    // linter's heuristic usually flags. There's no way to derive "no
    // data yet for this symbol" during render since the fetch is async.
    if (symbol !== loadedFor.current) {
      // Clear immediately on a genuine symbol change -- otherwise the
      // previous symbol's historical bars render merged with the new
      // symbol's live bars while the new fetch is still in flight.
      //
      // Deliberately NOT done for a resync, where the symbol is
      // unchanged: blanking the chart for a round trip would turn a
      // recoverable gap into a visibly broken chart, and the bars already
      // on screen remain the best available picture until better ones
      // land.
      setBars([]);
      loadedFor.current = symbol;
    }
    if (!symbol) return;
    let cancelled = false;

    authenticatedFetch(`${resolveHttpUrl()}/bars/${encodeURIComponent(symbol)}?minutes=${BACKFILL_MINUTES}`)
      .then((r) => {
        if (!r.ok) throw new Error(`backfill request failed: ${r.status}`);
        return r.json() as Promise<CandleBar[]>;
      })
      .then((fetched) => {
        // /bars/:symbol returns provider history, which covers whole
        // intervals by construction. Marking it authoritative is what stops
        // a partially observed live bar overwriting it in mergeBars -- the
        // 23,374-share DDC 15:37 loss measured on 2026-09-21.
        if (!cancelled) setBars(fetched.map((b) => ({ ...b, isFinal: true })));
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

  return bars;
}
