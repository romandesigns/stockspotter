import { authenticatedFetch } from "@stockspotter/shared-types";
// Fetches real historical 1-minute bars from ws-server's /bars/:symbol
// endpoint (crates/ws-server/src/http.rs) the moment a symbol is
// selected — without this, a freshly-selected symbol only has whatever's
// accumulated live since ws-server started tracking it this session,
// nowhere near the density of the Artifact prototype's own pre-fetched
// full-session demo data. Best-effort: if the fetch fails (backend down,
// symbol not covered, rate-limited), the chart still works off live data
// alone, just sparser until more bars arrive.
//
// Fetched again, not just once (chart diagnosis 2026-10-16, root cause 5:
// "history is fetched once, never refreshed"):
//
// - **On the first live bar after the chart opened.** History ends at the
//   moment it was fetched and live bars start whenever the first one
//   arrives; the minutes in between were a permanent hole.
// - **Every REFRESH_MS while the chart is open** (skipped while the tab is
//   hidden, caught up when it comes back). This is what keeps a symbol the
//   server does not stream live -- or a server that predates
//   `subscribe_chart` -- moving at all, and it repairs minutes a quiet
//   live stream never finalised.
// - **After every recorded gap** (`resyncNonce`: stream_lagged, a
//   reconnect, or the server's upstream feed coming back), with `fresh=1`
//   so ws-server skips its in-memory copy and asks the provider: gap
//   repair must be authoritative.
//
// A refetch never blanks the chart: the previous bars stay until the new
// answer lands, and a failed or rate-limited (429) answer changes nothing.
// Answers that arrive out of order are dropped, so a slow early request
// cannot replace a newer one. Merging with live bars is the caller's job
// (derive.ts mergeHistoryAndLive): final bars win, the forming bar stays.
//
// Request budget: ws-server allows 120 requests a minute process-wide, so
// one refresh a minute per open chart slot (at most four) is a small share.

import { useEffect, useRef, useState } from "react";
import { resolveHttpUrl } from "./config";
import type { CandleBar } from "./derive";

/** ~4 hours -- enough to make a freshly-selected symbol's chart
 * genuinely readable without asking Alpaca for a full multi-day history
 * this component doesn't need. */
const BACKFILL_MINUTES = 240;
/** Periodic refresh while a chart is open. */
export const REFRESH_MS = 60_000;
const NO_BARS: CandleBar[] = [];

export interface HistoricalBackfill {
  bars: CandleBar[];
  /**
   * The highest `resyncNonce` whose gap this history is known to cover:
   * set when a fetch STARTED at or after that nonce succeeds. When it
   * equals the current nonce, the 1-minute series has been re-fetched
   * over every recorded gap and is no longer suspect (see SuperChart's
   * freshness badge). -1 until the first successful fetch.
   */
  repairedNonce: number;
}

const EMPTY: HistoricalBackfill = { bars: NO_BARS, repairedNonce: -1 };

/**
 * @param resyncNonce Bumped by useRealtimeFeed whenever a gap is recorded.
 *   Covers 1-minute bars only. There is no sub-minute backfill (a real
 *   Alpaca constraint, see SuperChart.tsx), so a 30-second chart cannot be
 *   repaired this way and is surfaced as gapped instead -- see
 *   feedHealth.ts and §7 of the chart fidelity audit.
 * @param hasLiveBars Whether any live bar has arrived for this symbol. Its
 *   first false->true after the chart opened triggers one refetch.
 */
export function useHistoricalBackfill(symbol: string | null, resyncNonce = 0, hasLiveBars = false): HistoricalBackfill {
  const [state, setState] = useState<{ symbol: string | null } & HistoricalBackfill>({ symbol: null, ...EMPTY });
  // The fetcher for the current symbol, set by the symbol effect and used
  // by the other triggers; null while no symbol is shown.
  const fetchRef = useRef<((fresh: boolean) => void) | null>(null);
  // Read by fetches to stamp which gaps they cover. Updated in an effect
  // declared before the others, so it is current by the time they run.
  const nonceRef = useRef(resyncNonce);
  useEffect(() => {
    nonceRef.current = resyncNonce;
  }, [resyncNonce]);

  useEffect(() => {
    if (!symbol) return;
    let cancelled = false;
    let requested = 0;
    let applied = 0;
    let lastFetchAt = 0;

    function fetchBars(fresh: boolean) {
      if (cancelled || !symbol) return;
      const seq = ++requested;
      const nonce = nonceRef.current;
      lastFetchAt = Date.now();
      const query = `minutes=${BACKFILL_MINUTES}${fresh ? "&fresh=1" : ""}`;
      authenticatedFetch(`${resolveHttpUrl()}/bars/${encodeURIComponent(symbol)}?${query}`)
        .then((r) => {
          if (!r.ok) throw new Error(`backfill request failed: ${r.status}`);
          return r.json() as Promise<CandleBar[]>;
        })
        .then((fetched) => {
          if (cancelled || seq < applied) return;
          applied = seq;
          setState((prev) => ({
            symbol,
            // An empty answer for a symbol that had history is a provider
            // hiccup, not "the history vanished": keep what is drawn.
            bars: fetched.length === 0 && prev.symbol === symbol ? prev.bars : fetched,
            repairedNonce: Math.max(prev.symbol === symbol ? prev.repairedNonce : -1, nonce),
          }));
        })
        .catch(() => {
          // Best-effort -- live data alone still works, just sparser. On a
          // resync this means the gap stays flagged, which is the honest
          // outcome: we tried to repair it and could not.
        });
    }
    fetchRef.current = fetchBars;
    fetchBars(false);

    const timer = setInterval(() => {
      if (typeof document !== "undefined" && document.hidden) return;
      fetchBars(false);
    }, REFRESH_MS);
    function onVisible() {
      if (!document.hidden && Date.now() - lastFetchAt >= REFRESH_MS) fetchBars(false);
    }
    document.addEventListener("visibilitychange", onVisible);

    return () => {
      cancelled = true;
      fetchRef.current = null;
      clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [symbol]);

  // A recorded gap: refetch authoritatively. Skips the value it mounted
  // with -- the symbol effect's own first fetch already covers that one.
  const mountedNonce = useRef(resyncNonce);
  useEffect(() => {
    if (resyncNonce === mountedNonce.current) return;
    mountedNonce.current = resyncNonce;
    fetchRef.current?.(true);
  }, [resyncNonce]);

  // The first live bar after this symbol was opened closes the hole
  // between the history snapshot and the live stream.
  const sawLive = useRef<{ symbol: string | null; live: boolean }>({ symbol, live: hasLiveBars });
  useEffect(() => {
    const prev = sawLive.current;
    sawLive.current = { symbol, live: hasLiveBars };
    if (prev.symbol === symbol && !prev.live && hasLiveBars) fetchRef.current?.(false);
  }, [symbol, hasLiveBars]);

  return state.symbol === symbol ? { bars: state.bars, repairedNonce: state.repairedNonce } : EMPTY;
}
