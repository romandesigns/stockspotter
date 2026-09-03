// Real, computed detail lines for the momentum panel — NOT ported from
// the prototype, whose factor detail text ("Up-volume 4.1× down-volume
// this leg", "Structure intact since 9:41", "One moderate upper wick at
// $7.35") was static demo copy for one specific SWVL session, never
// computed from a formula (confirmed by reading its source). Matching
// that *reads-like-a-sentence* style honestly means computing real
// sentences from real data, not copying placeholder text.
//
// Split into two trust tiers, deliberately:
// - Volume detail is a direct restatement of data already on the chart
//   (a volume sum) — safe to describe independently since there's
//   nothing to disagree with.
// - Structure and wick-rejection detail stay grounded in the real
//   backend score (momentum_scorer's own HH/HL and wick analysis)
//   instead of an independent client-side re-detection, specifically to
//   avoid a client-side guess disagreeing with the server's real
//   (backtested) analysis and showing a confusing mismatch between the
//   good/warning icon and the sentence next to it.
// - MA-slope detail was ORIGINALLY treated like volume (an independent
//   client-side restatement, on the theory that it's "just" describing
//   the same MA9/MA20 lines already drawn) -- that theory turned out to
//   be wrong. Found live 2026-09-03 (PPBT, real screenshot): the client
//   re-derives MA9/MA20 slope + price position from `sma()` over the
//   chart's own fetched bars, while the warning/good ICON next to it
//   comes from the server's own real momentum_scorer maSlope factor
//   score -- two independent computations that are NOT the same
//   calculation and can genuinely disagree (they did: the client's
//   quick re-check said "both sloping up, price above both", the real
//   server factor score said otherwise). maSlopeDetail now takes the
//   real factor's `good` verdict and only uses the fully-confident
//   wording when the client's own read agrees with it -- otherwise it
//   falls back to a more measured (still accurate) description rather
//   than asserting something the icon next to it contradicts.

import type { CandleBar } from "./derive";

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

/**
 * `good` is the SAME verdict driving the factor's own icon
 * (factorGood(m.maSlope), the real server-side score) — required here
 * specifically so this function can never describe a confidently
 * bullish/bearish MA picture that the icon next to it disagrees with.
 * See this file's header comment for the real PPBT case that found this.
 */
export function maSlopeDetail(ma9: number[], ma20: number[], price: number, good: boolean): string {
  if (ma9.length < 2 || ma20.length < 2) return "Not enough bars yet for MA9/MA20";
  const ma9Up = ma9[ma9.length - 1] > ma9[ma9.length - 2];
  const ma20Up = ma20[ma20.length - 1] > ma20[ma20.length - 2];
  const aboveBoth = price > ma9[ma9.length - 1] && price > ma20[ma20.length - 1];
  const belowBoth = price < ma9[ma9.length - 1] && price < ma20[ma20.length - 1];
  if (good && ma9Up && ma20Up && aboveBoth) return "MA9 & MA20 both sloping up, price above both";
  if (!good && !ma9Up && !ma20Up && belowBoth) return "MA9 & MA20 both sloping down, price below both";
  if (ma9Up !== ma20Up) return "MA9/MA20 slopes disagree — no clean trend";
  return aboveBoth ? "Sloping, price above both MAs" : belowBoth ? "Sloping, price below both MAs" : "Price sitting between MA9 and MA20";
}

/** Grounded in the real server score, not an independent client-side
 * HH/HL re-detection — see this file's header comment. */
export function structureDetail(score: number): string {
  return score >= 0.6 ? "Higher-highs/higher-lows structure holding" : "No clear higher-highs/higher-lows structure right now";
}

/** Same reasoning as structureDetail -- grounded in the real server
 * score, not an independent client-side wick re-detection. */
export function wickRejectionDetail(score: number): string {
  return score >= 0.6 ? "Clean candles, no meaningful rejection wicks" : "Rejection wicks present — upper-wick pressure recently";
}
