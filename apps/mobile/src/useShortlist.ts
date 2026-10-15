// Today's Shortlist -- ws-server's GET /shortlist, polled every 5s, but only
// while the Shortlist tab's Today view is showing AND the app is in the
// foreground (AppState). Same authenticatedFetch + HTTP_URL as
// useMarketData.ts, and the same best-effort rule: a failed poll keeps the
// last good body (with `error` set) instead of blanking the list.
// Decoding lives in shortlist.ts so it is unit tested.
//
// Also home to the plan form's persisted risk amount (AsyncStorage, same
// versioned-key + loaded-guard pattern as useChartSettings.ts).
import { authenticatedFetch } from "@stockspotter/shared-types";
import type { ShortlistResponse } from "@stockspotter/shared-types";
import AsyncStorage from "@react-native-async-storage/async-storage";
import { useCallback, useEffect, useRef, useState } from "react";
import { AppState } from "react-native";
import { HTTP_URL } from "./config";
import { DEFAULT_RISK_DOLLARS, decodeShortlist } from "./shortlist";

const POLL_MS = 5_000;

/** True while the app is in the foreground. */
export function useAppActive(): boolean {
  const [active, setActive] = useState(() => AppState.currentState !== "background" && AppState.currentState !== "inactive");
  useEffect(() => {
    const sub = AppState.addEventListener("change", (next) => setActive(next === "active"));
    return () => sub.remove();
  }, []);
  return active;
}

export interface ShortlistState {
  body: ShortlistResponse | null;
  /** Why the most recent poll failed; null once one succeeds again. */
  error: string | null;
  /** True until the first poll (success or failure) settles. */
  loading: boolean;
}

export function useShortlist(enabled: boolean): ShortlistState {
  const appActive = useAppActive();
  const [state, setState] = useState<ShortlistState>({ body: null, error: null, loading: true });
  const polling = enabled && appActive;

  useEffect(() => {
    if (!polling) return;
    let cancelled = false;
    let inFlight = false;

    async function poll() {
      if (inFlight) return; // a slow response never stacks requests
      inFlight = true;
      try {
        const response = await authenticatedFetch(`${HTTP_URL}/shortlist`);
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        const body = decodeShortlist(await response.json());
        if (!body) throw new Error("unrecognised response");
        if (!cancelled) setState({ body, error: null, loading: false });
      } catch (err) {
        const message = err instanceof Error ? err.message : "request failed";
        if (!cancelled) setState((prev) => ({ ...prev, error: message, loading: false }));
      } finally {
        inFlight = false;
      }
    }

    // Becoming visible again polls immediately instead of waiting out the interval.
    void poll();
    const timer = setInterval(() => void poll(), POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [polling]);

  return state;
}

const RISK_KEY = "stockspotter.shortlist.riskDollars.v1";

/** The plan form's dollar risk, remembered across launches. */
export function useRiskDollars(): { risk: number; loaded: boolean; setRisk: (n: number) => void } {
  const [risk, setRiskState] = useState(DEFAULT_RISK_DOLLARS);
  const [loaded, setLoaded] = useState(false);
  const loadedRef = useRef(false);

  useEffect(() => {
    AsyncStorage.getItem(RISK_KEY)
      .then((raw) => {
        const n = raw == null ? Number.NaN : Number(raw);
        if (Number.isFinite(n) && n > 0) setRiskState(n);
      })
      .catch(() => {})
      .finally(() => {
        loadedRef.current = true;
        setLoaded(true);
      });
  }, []);

  const setRisk = useCallback((n: number) => {
    if (!Number.isFinite(n) || n <= 0) return;
    setRiskState(n);
    if (loadedRef.current) AsyncStorage.setItem(RISK_KEY, String(n)).catch(() => {});
  }, []);

  return { risk, loaded, setRisk };
}
