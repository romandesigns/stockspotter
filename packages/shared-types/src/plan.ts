// Trade plan arithmetic for a shortlisted candidate (2026-10-15). Turns
// "should I?" into numbers before the trader commits: how many shares a
// fixed dollar risk allows at a given stop, what the spread costs on top,
// and whether the target sits past the halt band. Pure, so web and mobile
// compute the same plan.
//
// This sizes risk; it does not pick trades. Nothing here is advice.

import type { ShortlistCandidate } from "./shortlist";

export interface PlanInput {
  entry: number;
  stop: number;
  /** Dollars the trader accepts losing if the stop is hit (before spread). */
  riskDollars: number;
  target?: number;
  /** Round-trip spread estimate, % of price (the candidate's quote). */
  spreadPct?: number;
  /** LULD upper band: a target above it needs a halt to clear first. */
  haltUpperBand?: number;
}

export interface Plan {
  shares: number;
  positionDollars: number;
  riskPerShare: number;
  /** Crossing the spread in and out on `shares`, at entry price. */
  spreadCostDollars: number;
  /** Loss at the stop including the spread. */
  totalRiskDollars: number;
  /** (target - entry) / (entry - stop), when a target is given. */
  rewardToRisk?: number;
  warnings: string[];
}

export type PlanResult = { ok: true; plan: Plan } | { ok: false; error: string };

/** Spread cost above this share of the planned risk is called out. */
export const SPREAD_RISK_WARNING_SHARE = 0.25;

export function computePlan(input: PlanInput): PlanResult {
  const { entry, stop, riskDollars, target, spreadPct, haltUpperBand } = input;
  if (![entry, stop, riskDollars].every((n) => Number.isFinite(n) && n > 0)) {
    return { ok: false, error: "Entry, stop and risk must be positive numbers." };
  }
  if (stop >= entry) return { ok: false, error: "The stop must be below the entry." };
  const riskPerShare = entry - stop;
  // The epsilon absorbs float error: 100 / (4.0 - 3.8) is 499.99999999999994.
  const shares = Math.floor(riskDollars / riskPerShare + 1e-9);
  if (shares < 1) return { ok: false, error: "The risk is smaller than one share's distance to the stop." };

  const spread = spreadPct !== undefined && Number.isFinite(spreadPct) && spreadPct > 0 ? spreadPct : undefined;
  const spreadCostDollars = spread === undefined ? 0 : shares * entry * (spread / 100);
  const warnings: string[] = [];
  if (spread === undefined) warnings.push("No fresh spread: the cost of crossing it is not included.");
  else if (spreadCostDollars > riskDollars * SPREAD_RISK_WARNING_SHARE) {
    warnings.push(`The spread alone costs ${Math.round((spreadCostDollars / riskDollars) * 100)}% of the planned risk.`);
  }
  if (spread !== undefined && riskPerShare < entry * (spread / 100)) {
    warnings.push("The stop sits inside the spread: ordinary quote noise can trigger it.");
  }

  let rewardToRisk: number | undefined;
  if (target !== undefined && Number.isFinite(target)) {
    if (target <= entry) warnings.push("The target is not above the entry.");
    else rewardToRisk = (target - entry) / riskPerShare;
    if (haltUpperBand !== undefined && target > haltUpperBand) {
      warnings.push("The target is beyond the halt band: price would have to halt or the band reset first.");
    }
  }

  return {
    ok: true,
    plan: {
      shares,
      positionDollars: shares * entry,
      riskPerShare,
      spreadCostDollars,
      totalRiskDollars: shares * riskPerShare + spreadCostDollars,
      rewardToRisk,
      warnings,
    },
  };
}

/** A starting stop for the plan form: the nearest key level below the
 * entry by more than one spread (VWAP, premarket high, premarket low, halt
 * lower band), else 5% below the entry. A suggestion to edit, not a call. */
export function suggestStop(c: ShortlistCandidate, entry: number): { price: number; basis: string } {
  const minGap = entry * ((c.quote?.spreadPct ?? 0) / 100);
  const levels: Array<[number | undefined, string]> = [
    [c.levels.vwap, "VWAP"],
    [c.levels.premarketHigh, "premarket high"],
    [c.levels.premarketLow, "premarket low"],
    [c.halt?.lowerBand, "halt lower band"],
  ];
  const below = levels
    .filter((l): l is [number, string] => l[0] !== undefined && Number.isFinite(l[0]) && l[0] < entry - minGap)
    .sort((a, b) => b[0] - a[0]);
  return below.length > 0 ? { price: below[0][0], basis: below[0][1] } : { price: entry * 0.95, basis: "5% below entry" };
}
