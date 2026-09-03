// Chart Page AI assessment (2026-09-03, Roman's own ask): a brief,
// Claude-generated read on the currently-selected symbol, shown inside
// the momentum-score card (SuperChart.tsx's own MomentumScoreRow).
// Backed by ws-server's POST /assess, which proxies to the Python
// qualitative layer's real Claude-plus-web-search call (see
// python/app/assess.py's own doc comment for the real cost/cache
// policy: once per symbol selection, cached server-side for ~10 min).
//
// Real, deliberate design point: the fetch effect below depends ONLY on
// `symbol`, not on `momentum` (which updates on every live bar) -- a
// dependency on the live-updating momentum object would refetch on
// every tick, which is exactly the "spend a real API call every glance"
// behavior Roman's own cost-conscious cadence choice ruled out. The
// momentum snapshot used for the request is read from a ref at fetch
// time instead, so switching symbols is the only thing that triggers a
// new request (or a cache hit, server-side).

import { useCallback, useEffect, useRef, useState } from "react";
import type { MomentumUpdate } from "@stockspotter/shared-types";
import { resolveHttpUrl } from "./config";

export interface AssessmentResult {
  summary: string[];
  generatedAt: string;
}

export function useAssessment(symbol: string | null, momentum: MomentumUpdate | null) {
  const [assessment, setAssessment] = useState<AssessmentResult | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState(false);

  const momentumRef = useRef(momentum);
  momentumRef.current = momentum;

  const request = useCallback((sym: string, forceRefresh: boolean) => {
    const m = momentumRef.current;
    if (!m) return;
    setLoading(true);
    setError(false);
    fetch(`${resolveHttpUrl()}/assess`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        symbol: sym,
        overall: m.overall,
        volumeConfirmation: m.volumeConfirmation,
        structure: m.structure,
        maSlope: m.maSlope,
        wickRejection: m.wickRejection,
        forceRefresh,
      }),
    })
      .then((r) => {
        if (!r.ok) throw new Error(`assess request failed: ${r.status}`);
        return r.json() as Promise<AssessmentResult>;
      })
      .then((fetched) => {
        setAssessment(fetched);
        setLoading(false);
      })
      .catch(() => {
        setError(true);
        setLoading(false);
      });
  }, []);

  const hasMomentum = momentum != null;
  useEffect(() => {
    setAssessment(null);
    setError(false);
    if (!symbol) return;
    // Momentum may not have arrived yet the instant a symbol is
    // selected (same real gap MomentumScoreRow's own "No momentum
    // reading yet" state already tolerates) -- wait for it rather than
    // skip the assessment silently forever. Depending on `hasMomentum`
    // (a boolean) rather than `momentum` itself means a live-updating
    // momentum object doesn't retrigger this on every bar -- only the
    // symbol changing, or momentum going from absent to present for the
    // first time, does.
    if (!hasMomentum) return;
    request(symbol, false);
  }, [symbol, hasMomentum, request]);

  const regenerate = useCallback(() => {
    if (symbol) request(symbol, true);
  }, [symbol, request]);

  return { assessment, loading, error, regenerate };
}
