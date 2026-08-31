// Labels for the Super Chart momentum panel — our own thresholds, not
// ported from the prototype (its "84 / Strong Bullish" text there was
// static demo copy, not computed from a real formula, confirmed by
// reading its source: no scoreLabel()-shaped function exists). Built
// around momentum_scorer::DEFAULT_QUALIFY_THRESHOLD (0.60), the one real
// number that already exists on the backend and is actually tuned
// (broad-sweep backtest, see stockspotter-open-tasks memory) — everything
// above/below it here is a reasonable default banding, not itself
// backtested.

export function momentumLabel(overall: number): string {
  if (overall >= 0.8) return "Strong Bullish";
  if (overall >= 0.6) return "Bullish";
  if (overall >= 0.4) return "Neutral";
  return "Weak";
}

/** Same 0.6 threshold as the qualify gate, reused per-factor: a factor
 * scoring below it is flagged the same way an unqualified overall score
 * would be, for a consistent "good" bar across the panel. */
export const FACTOR_GOOD_THRESHOLD = 0.6;

export function factorGood(score: number): boolean {
  return score >= FACTOR_GOOD_THRESHOLD;
}
