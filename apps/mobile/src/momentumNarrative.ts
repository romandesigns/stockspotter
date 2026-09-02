// Ported verbatim from apps/client/src/lib/momentumNarrative.ts -- real,
// computed detail sentences for the momentum panel, same two trust tiers
// (volume/MA-slope are direct restatements of data already on the chart;
// structure/wick-rejection stay grounded in the real backend score rather
// than an independent client-side re-detection, so the good/warning icon
// never disagrees with the sentence next to it). Not re-derived.

import type { CandleBar } from "./types";

const RECENT_WINDOW = 20; // bars considered "this leg" -- arbitrary but reasonable, not itself tuned

export function volumeConfirmationDetail(bars: CandleBar[]): string {
  const recent = bars.slice(-RECENT_WINDOW);
  let upVol = 0;
  let downVol = 0;
  for (let i = 1; i < recent.length; i++) {
    if (recent[i].close >= recent[i - 1].close) upVol += recent[i].volume;
    else downVol += recent[i].volume;
  }
  const n = Math.max(0, recent.length - 1);
  if (upVol === 0 && downVol === 0) return "No volume yet";
  if (downVol === 0) return `All up-volume, last ${n} bars`;
  if (upVol === 0) return `All down-volume, last ${n} bars`;
  const ratio = upVol / downVol;
  return ratio >= 1 ? `Up-volume ${ratio.toFixed(1)}× down-volume, last ${n} bars` : `Down-volume ${(1 / ratio).toFixed(1)}× up-volume, last ${n} bars`;
}

export function maSlopeDetail(ma9: number[], ma20: number[], price: number): string {
  if (ma9.length < 2 || ma20.length < 2) return "Not enough bars yet for MA9/MA20";
  const ma9Up = ma9[ma9.length - 1] > ma9[ma9.length - 2];
  const ma20Up = ma20[ma20.length - 1] > ma20[ma20.length - 2];
  const aboveBoth = price > ma9[ma9.length - 1] && price > ma20[ma20.length - 1];
  const belowBoth = price < ma9[ma9.length - 1] && price < ma20[ma20.length - 1];
  if (ma9Up && ma20Up && aboveBoth) return "MA9 & MA20 both sloping up, price above both";
  if (!ma9Up && !ma20Up && belowBoth) return "MA9 & MA20 both sloping down, price below both";
  if (ma9Up !== ma20Up) return "MA9/MA20 slopes disagree — no clean trend";
  return aboveBoth ? "Sloping, price above both MAs" : belowBoth ? "Sloping, price below both MAs" : "Price sitting between MA9 and MA20";
}

/** Grounded in the real server score, not an independent client-side
 * HH/HL re-detection -- see this file's header comment. */
export function structureDetail(score: number): string {
  return score >= 0.6 ? "Higher-highs/higher-lows structure holding" : "No clear higher-highs/higher-lows structure right now";
}

/** Same reasoning as structureDetail -- grounded in the real server
 * score, not an independent client-side wick re-detection. */
export function wickRejectionDetail(score: number): string {
  return score >= 0.6 ? "Clean candles, no meaningful rejection wicks" : "Rejection wicks present — upper-wick pressure recently";
}
