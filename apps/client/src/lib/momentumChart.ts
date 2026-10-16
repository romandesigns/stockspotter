// Cards for the Bullish Momentum panel (MomentumPanel.tsx): one per symbol
// -- its latest confirmation only -- strongest score first, each with a
// strength band (Mild / Solid / Strong), the one or two factors driving it
// as badges, and all four factors in plain words for the hover card.

import type { MomentumUpdate } from "@stockspotter/shared-types";
import { isUnderTwenty } from "./momentumPrices";

export interface MomentumFactor {
  key: "volumeConfirmation" | "structure" | "maSlope" | "wickRejection";
  /** Full name, for badges. */
  label: string;
  /** One word, for the factor chart's axis. */
  short: string;
  /** 0..1 */
  value: number;
}

export type MomentumBand = "Mild" | "Solid" | "Strong";

export interface MomentumCard {
  symbol: string;
  /** 0..1 overall momentum score. */
  score: number;
  band: MomentumBand;
  timestamp: string;
  factors: MomentumFactor[];
  /** The top factor, plus the runner-up when it is within 0.1 of it. */
  drivers: string[];
}

// Display steps for reading strength at a glance -- chosen for legibility
// above the scorer's 0.60 qualify threshold, not backtested, like the
// score itself. One green ramp, dim to bright (validated as an ordinal
// scale on the panel surface: monotone lightness, single hue, every step
// distinguishable under colour-vision deficiency).
export const MOMENTUM_BANDS: ReadonlyArray<{ band: MomentumBand; min: number; range: string; color: string }> = [
  { band: "Mild", min: 0, range: "0.60–0.65", color: "#3d8a57" },
  { band: "Solid", min: 0.65, range: "0.65–0.75", color: "#3fb86c" },
  { band: "Strong", min: 0.75, range: "0.75+", color: "#86efac" },
];

export function bandOf(score: number): MomentumBand {
  return score >= 0.75 ? "Strong" : score >= 0.65 ? "Solid" : "Mild";
}

export function bandColor(band: MomentumBand): string {
  return MOMENTUM_BANDS.find((b) => b.band === band)!.color;
}

const FACTORS: ReadonlyArray<[MomentumFactor["key"], string, string]> = [
  ["volumeConfirmation", "Volume surge", "Volume"],
  ["structure", "Price structure", "Structure"],
  ["maSlope", "Rising trend", "Trend"],
  ["wickRejection", "Clean candles", "Candles"],
];

export function momentumCards(confirmations: readonly MomentumUpdate[], pricesBySymbol: Map<string, number>): MomentumCard[] {
  const latest = new Map<string, MomentumUpdate>();
  for (const m of confirmations) {
    if (!isUnderTwenty(pricesBySymbol.get(m.symbol))) continue;
    const seen = latest.get(m.symbol);
    if (!seen || Date.parse(m.timestamp) >= Date.parse(seen.timestamp)) latest.set(m.symbol, m);
  }
  return [...latest.values()]
    .map((m) => {
      const factors = FACTORS.map(([key, label, short]) => ({ key, label, short, value: clamp01(m[key]) }));
      const ranked = [...factors].sort((a, b) => b.value - a.value);
      const drivers = ranked[1].value >= ranked[0].value - 0.1 ? [ranked[0].label, ranked[1].label] : [ranked[0].label];
      const score = clamp01(m.overall);
      return { symbol: m.symbol, score, band: bandOf(score), timestamp: m.timestamp, factors, drivers };
    })
    .sort((a, b) => b.score - a.score || a.symbol.localeCompare(b.symbol));
}

function clamp01(n: number): number {
  return Number.isFinite(n) ? Math.min(1, Math.max(0, n)) : 0;
}
