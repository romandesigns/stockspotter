// Ported verbatim from apps/client/src/lib/momentumLabel.ts -- same
// thresholds, same reasoning (built around momentum_scorer::
// DEFAULT_QUALIFY_THRESHOLD, the one real backtested number; the rest is
// a reasonable default banding, not itself backtested). Not re-derived.

export function momentumLabel(overall: number): string {
  if (overall >= 0.8) return "Strong Bullish";
  if (overall >= 0.6) return "Bullish";
  if (overall >= 0.4) return "Neutral";
  return "Weak";
}

/** Same 0.6 threshold as the qualify gate, reused per-factor -- matches
 * web's own FACTOR_GOOD_THRESHOLD exactly. */
export const FACTOR_GOOD_THRESHOLD = 0.6;

export function factorGood(score: number): boolean {
  return score >= FACTOR_GOOD_THRESHOLD;
}
