// "Today's Shortlist" -- ws-server's GET /shortlist (bearer-auth like every
// other route). Mirrors the Rust response struct field-for-field; treat
// that side as the source of truth if these two ever seem to disagree.
//
// The ranking is gapPct x relativeVolume (research finding R3). It is a
// WATCHLIST ordering -- where to look -- not an entry signal: no detector
// in this project has yet shown an edge after costs. Nothing in this file
// (or anything rendered from it) should be read as "buy this".

export type ShortlistSession = "premarket" | "regular" | "afterhours" | "overnight";

export interface ShortlistLevels {
  premarketHigh?: number;
  premarketLow?: number;
  dayHigh?: number;
  vwap?: number;
  /** Which session's first bar the VWAP is anchored to. */
  vwapAnchor?: "premarket" | "regular";
  /** Price x volume traded so far today, from the same bars as the levels. */
  dollarVolume?: number;
  /** ISO 8601 -- first bar the levels were computed from. */
  barsFrom?: string;
  /** False when the server's bar history doesn't cover the whole session
   * yet (a symbol promoted mid-morning) -- every level is partial then. */
  barsComplete: boolean;
}

export interface ShortlistQuote {
  bid: number;
  ask: number;
  spreadPct: number;
  ageSecs: number;
}

export interface ShortlistHalt {
  level: "calm" | "amber" | "red";
  proximityRatio: number;
  upperBand: number;
  lowerBand: number;
  distanceToUpperPct: number;
  /** True when the band is the server's own LULD estimate, not SIP-supplied. */
  estimated: boolean;
  /** False outside 9:30-16:00 ET -- no halt can trigger then. */
  luldInEffect: boolean;
  ageSecs: number;
}

export interface ShortlistCatalyst {
  tags: string[];
  headlineCount: number;
  headline?: string;
  publishedAt?: string; // ISO 8601
}

export interface ShortlistCandidate {
  rank: number;
  symbol: string;
  score: number;
  price: number;
  /** Last price vs prior close -- the day's % change, not just the open gap. */
  gapPct: number;
  relativeVolume: number;
  floatShares?: number;
  sessionVolume: number;
  funnelPassed: boolean;
  funnelAt: string; // ISO 8601
  levels: ShortlistLevels;
  quote?: ShortlistQuote;
  halt?: ShortlistHalt;
  catalyst?: ShortlistCatalyst;
}

export interface ShortlistResponse {
  generatedAt: string; // ISO 8601
  marketDay: string | null;
  session: ShortlistSession;
  rankedBy: string;
  poolSize: number;
  excluded: { outsidePriceRange: number; notGappingUp: number; noRelativeVolume: number };
  candidates: ShortlistCandidate[];
}

export type TradabilityVerdict = "good" | "caution" | "poor" | "unknown";

export interface TradabilityAssessment {
  verdict: TradabilityVerdict;
  /** Human-readable reasons behind every non-good contribution. */
  reasons: string[];
}

// HEURISTIC thresholds -- chosen as reasonable small-cap day-trading
// rules of thumb, NOT backtested or fitted against this project's data.
// Revisit them once phase 2 measures real fill quality.
/** A quote older than this is treated as missing. */
export const TRADABILITY_QUOTE_MAX_AGE_SECS = 60;
/** Spread (% of mid) at or below which a spread reads as good. */
export const TRADABILITY_SPREAD_GOOD_PCT = 0.5;
/** Spread at or below which it is only a caution; wider is poor. */
export const TRADABILITY_SPREAD_CAUTION_PCT = 1.5;
/** Dollar volume traded today below which a name is too thin to trade. */
export const TRADABILITY_THIN_DOLLAR_VOLUME = 500_000;
/** Dollar volume below which liquidity is only a caution. */
export const TRADABILITY_CAUTION_DOLLAR_VOLUME = 2_000_000;
/** LULD proximity ratio at/above which a halt is close enough to be poor. */
export const TRADABILITY_HALT_POOR_RATIO = 0.8;
/** LULD proximity ratio at/above which a halt is worth a caution. */
export const TRADABILITY_HALT_CAUTION_RATIO = 0.5;

function formatThousands(dollars: number): string {
  return `$${Math.round(dollars / 1000).toLocaleString("en-US")}K`;
}

/**
 * Practical "can I actually get in and out of this" read for one
 * candidate: spread, liquidity, and (during LULD hours only) halt-band
 * proximity. The verdict is the worst contribution -- any poor -> poor,
 * else any caution -> caution, else any unknown -> unknown, else good --
 * so a single missing input can never be papered over by the others.
 *
 * `nowMs` is accepted for signature parity with assessRossFivePillars;
 * every freshness input here is a server-computed age (`ageSecs`), so it
 * is currently unused.
 */
export function assessTradability(c: ShortlistCandidate, _nowMs = Date.now()): TradabilityAssessment {
  const contributions: TradabilityVerdict[] = [];
  const reasons: string[] = [];

  const quote = c.quote;
  if (!quote || !Number.isFinite(quote.spreadPct) || !(quote.ageSecs <= TRADABILITY_QUOTE_MAX_AGE_SECS)) {
    contributions.push("unknown");
    reasons.push("no fresh quote");
  } else if (quote.spreadPct <= TRADABILITY_SPREAD_GOOD_PCT) {
    contributions.push("good");
  } else if (quote.spreadPct <= TRADABILITY_SPREAD_CAUTION_PCT) {
    contributions.push("caution");
    reasons.push(`spread ${quote.spreadPct.toFixed(1)}%`);
  } else {
    contributions.push("poor");
    reasons.push(`wide spread ${quote.spreadPct.toFixed(1)}%`);
  }

  const dollarVolume = c.levels?.dollarVolume;
  if (dollarVolume == null || !Number.isFinite(dollarVolume)) {
    contributions.push("unknown");
    reasons.push("dollar volume unknown");
  } else if (dollarVolume < TRADABILITY_THIN_DOLLAR_VOLUME) {
    contributions.push("poor");
    reasons.push(`thin: ${formatThousands(dollarVolume)} traded`);
  } else if (dollarVolume < TRADABILITY_CAUTION_DOLLAR_VOLUME) {
    contributions.push("caution");
    reasons.push(`light: ${formatThousands(dollarVolume)} traded`);
  } else {
    contributions.push("good");
  }

  // Outside LULD hours (or with no reading at all) a halt can't trigger,
  // so the halt band contributes nothing either way.
  const halt = c.halt;
  if (halt && halt.luldInEffect) {
    if (halt.proximityRatio >= TRADABILITY_HALT_POOR_RATIO) {
      contributions.push("poor");
      reasons.push("near halt band");
    } else if (halt.proximityRatio >= TRADABILITY_HALT_CAUTION_RATIO) {
      contributions.push("caution");
      reasons.push("approaching halt band");
    } else {
      contributions.push("good");
    }
  }

  const verdict: TradabilityVerdict = contributions.includes("poor")
    ? "poor"
    : contributions.includes("caution")
      ? "caution"
      : contributions.includes("unknown")
        ? "unknown"
        : "good";
  return { verdict, reasons };
}
