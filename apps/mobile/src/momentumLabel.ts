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

// Strength bands, ported verbatim from apps/client/src/lib/momentumChart.ts
// (MOMENTUM_BANDS / bandOf / bandColor). The cut-offs MUST match the
// server's (backtest-metrics base_rates.rs MOMENTUM_BANDS): web joins the
// track record on the band name. One green ramp, dim to bright, so the
// band reads as an ordinal scale without spending the accent colour.
export type MomentumBand = "Mild" | "Solid" | "Strong";
export const MOMENTUM_BANDS: ReadonlyArray<{ band: MomentumBand; min: number; color: string }> = [
  { band: "Mild", min: 0.6, color: "#3d8a57" },
  { band: "Solid", min: 0.65, color: "#3fb86c" },
  { band: "Strong", min: 0.7, color: "#86efac" },
];
/** Colour for a score back below the qualify line. */
export const BELOW_BAND_COLOR = "#5b6070";

/** The band a score falls in; null below 0.60. */
export function bandOf(score: number): MomentumBand | null {
  for (let i = MOMENTUM_BANDS.length - 1; i >= 0; i--) if (score >= MOMENTUM_BANDS[i].min) return MOMENTUM_BANDS[i].band;
  return null;
}

export function bandColor(band: MomentumBand | null): string {
  return band ? MOMENTUM_BANDS.find((b) => b.band === band)!.color : BELOW_BAND_COLOR;
}
