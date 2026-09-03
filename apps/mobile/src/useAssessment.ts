// Chart Page AI assessment (2026-09-03, Roman's own ask) -- real port
// of apps/client/src/lib/useAssessment.ts, same design (fetch effect
// depends only on `symbol`, momentum read from a ref at fetch time so a
// live-updating momentum object doesn't retrigger a real API call on
// every bar). See that file's own doc comment for the full reasoning,
// and python/app/assess.py for the real Claude-plus-web-search call and
// its server-side ~10 min cache this hook relies on for "once per
// symbol selection" to stay cheap.
import { useCallback, useEffect, useRef, useState } from "react";
import type { MomentumUpdate } from "@stockspotter/shared-types";
import { HTTP_URL } from "./config";

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
    fetch(`${HTTP_URL}/assess`, {
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
    if (!symbol || !hasMomentum) return;
    request(symbol, false);
  }, [symbol, hasMomentum, request]);

  const regenerate = useCallback(() => {
    if (symbol) request(symbol, true);
  }, [symbol, request]);

  return { assessment, loading, error, regenerate };
}
