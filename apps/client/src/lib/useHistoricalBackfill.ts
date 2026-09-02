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

export function useHistoricalBackfill(symbol: string | null): CandleBar[] {
  const [bars, setBars] = useState<CandleBar[]>([]);

  useEffect(() => {
    // Resetting state to synchronize with an external resource (a fetch
    // keyed to `symbol`) on a prop change -- React's own documented
    // pattern for this exact case, not the redundant-setState smell the
    // linter's heuristic usually flags. There's no way to derive "no
    // data yet for this symbol" during render since the fetch is async.
    if (!symbol) {
      setBars([]);
      return;
    }
    // Clear immediately on symbol change -- otherwise the previous
    // symbol's historical bars would briefly render merged with the new
    // symbol's live bars while the new fetch is still in flight.
    setBars([]);
    let cancelled = false;

    fetch(`${resolveHttpUrl()}/bars/${encodeURIComponent(symbol)}?minutes=${BACKFILL_MINUTES}`)
      .then((r) => {
        if (!r.ok) throw new Error(`backfill request failed: ${r.status}`);
        return r.json() as Promise<CandleBar[]>;
      })
      .then((fetched) => {
        if (!cancelled) setBars(fetched);
      })
      .catch(() => {
        // Best-effort -- live data alone still works, just sparser.
      });

    return () => {
      cancelled = true;
    };
  }, [symbol]);

  return bars;
}
