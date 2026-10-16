// Cards for the Bullish Momentum panel (MomentumPanel.tsx): one per symbol
// that crossed into qualifying momentum, showing its CURRENT reading (not
// the crossing moment's), with a strength band, the factors driving it,
// and the context needed to judge it (price, day change, session,
// shortlist rank). Symbols that drop back below the threshold stay a
// short while as "faded", then leave. Pure: everything is a function of
// the feed state and `nowMs`, so it is unit tested.

import {
  assessTradability,
  type FunnelSignal,
  type MomentumUpdate,
  type ShortlistResponse,
  type TradabilityAssessment,
} from "@stockspotter/shared-types";
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
export type MomentumStatus = "active" | "faded";
export type ReadingSession = "regular" | "pre" | "after";

export interface MomentumCard {
  symbol: string;
  status: MomentumStatus;
  /** 0..1 overall score of the CURRENT reading. */
  score: number;
  /** Band of the current score; null once it is below the 0.60 qualify line. */
  band: MomentumBand | null;
  /** Factors of the current reading. */
  factors: MomentumFactor[];
  /** The top factor, plus the runner-up when it is within 0.1 of it. */
  drivers: string[];
  /** When the latest crossing happened, and how strong it was then. */
  confirmedAt: string;
  confirmedScore: number;
  /** Band at the crossing -- the join key for the track record, which is
   * measured on crossings. */
  confirmedBand: MomentumBand;
  /** Timestamp of the current reading. */
  readingAt: string;
  /** For faded cards, the reading that dropped below the threshold. */
  fadedAt?: string;
  /** Minutes since the current reading, when that is over STALE_AFTER_MIN. */
  staleMinutes: number | null;
  /** Session of the current reading, by its New York time. */
  session: ReadingSession;
  /** Best available price: latest bar/reference price, else the latest funnel signal's. */
  price?: number;
  /** The day's % change, from the latest funnel signal (gapPct). */
  dayChangePct?: number;
  shortlist?: { rank: number; tradability: TradabilityAssessment };
}

export interface MomentumHidden {
  funds: number;
  noPrice: number;
  overTwenty: number;
}

export interface MomentumCardsInput {
  confirmations: readonly MomentumUpdate[];
  /** Latest reading per symbol (useRealtimeFeed momentumBySymbol). */
  current: ReadonlyMap<string, MomentumUpdate>;
  /** Latest below-threshold edge per symbol (useRealtimeFeed momentumFadesBySymbol). */
  fades: ReadonlyMap<string, MomentumUpdate>;
  /** Bar/reference prices (momentumPrices). */
  prices: ReadonlyMap<string, number>;
  funnelBySymbol: ReadonlyMap<string, FunnelSignal>;
  funds: ReadonlySet<string>;
  shortlist: ShortlistResponse | null;
  nowMs: number;
}

/** A faded card stays this long after the reading that faded it. */
export const FADED_VISIBLE_MIN = 15;
/** A current reading older than this is flagged "No update for N min". */
export const STALE_AFTER_MIN = 10;
/** An active card with no reading for longer than this is dropped. */
export const ACTIVE_MAX_SILENT_MIN = 60;

// Strength bands for the scorer's real range (scores cluster 0.60-0.70).
// The cut-offs MUST match the server's (backtest-metrics base_rates.rs
// MOMENTUM_BANDS): the track record is joined on the band name. One green
// ramp, dim to bright (validated as an ordinal scale on the panel
// surface: monotone lightness, single hue, every step distinguishable
// under colour-vision deficiency).
export const MOMENTUM_BANDS: ReadonlyArray<{ band: MomentumBand; min: number; range: string; color: string }> = [
  { band: "Mild", min: 0.6, range: "0.60–0.65", color: "#3d8a57" },
  { band: "Solid", min: 0.65, range: "0.65–0.70", color: "#3fb86c" },
  { band: "Strong", min: 0.7, range: "0.70+", color: "#86efac" },
];
/** Gauge colour for a score back below the qualify line. */
export const BELOW_BAND_COLOR = "#5b6070";

/** The band a score falls in; null below 0.60. */
export function bandOf(score: number): MomentumBand | null {
  for (let i = MOMENTUM_BANDS.length - 1; i >= 0; i--) if (score >= MOMENTUM_BANDS[i].min) return MOMENTUM_BANDS[i].band;
  return null;
}

export function bandColor(band: MomentumBand | null): string {
  return band ? MOMENTUM_BANDS.find((b) => b.band === band)!.color : BELOW_BAND_COLOR;
}

// Named for what the scorer actually measures (momentum-scorer scorer.rs).
const FACTORS: ReadonlyArray<[MomentumFactor["key"], string, string]> = [
  // Share of volume traded on up-candles over the last 30 minutes.
  ["volumeConfirmation", "Buying pressure", "Buying"],
  ["structure", "Higher highs", "Highs"],
  // Graded; needs 10 bars, reports 0 (and is left out of the score) before that.
  ["maSlope", "Above moving averages", "Trend"],
  ["wickRejection", "No selling wicks", "Wicks"],
];

export function factorsOf(m: MomentumUpdate): MomentumFactor[] {
  return FACTORS.map(([key, label, short]) => ({ key, label, short, value: clamp01(m[key]) }));
}

function driversOf(factors: MomentumFactor[]): string[] {
  const ranked = [...factors].sort((a, b) => b.value - a.value);
  return ranked[1].value >= ranked[0].value - 0.1 ? [ranked[0].label, ranked[1].label] : [ranked[0].label];
}

const ET_HOUR_MINUTE = new Intl.DateTimeFormat("en-US", { timeZone: "America/New_York", hour: "2-digit", minute: "2-digit", hourCycle: "h23" });

/** 09:30-16:00 New York time is the regular session; earlier is pre-market, later after hours. */
export function readingSession(iso: string): ReadingSession {
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return "regular";
  const parts = ET_HOUR_MINUTE.formatToParts(new Date(t));
  const minutes = Number(parts.find((p) => p.type === "hour")?.value ?? "0") * 60 + Number(parts.find((p) => p.type === "minute")?.value ?? "0");
  if (minutes < 9 * 60 + 30) return "pre";
  if (minutes >= 16 * 60) return "after";
  return "regular";
}

export const SESSION_BADGE: Record<ReadingSession, string | null> = { regular: null, pre: "Pre-market", after: "After hours" };

export function momentumCards(input: MomentumCardsInput): { cards: MomentumCard[]; hidden: MomentumHidden } {
  const latest = new Map<string, MomentumUpdate>();
  for (const m of input.confirmations) {
    const seen = latest.get(m.symbol);
    if (!seen || Date.parse(m.timestamp) >= Date.parse(seen.timestamp)) latest.set(m.symbol, m);
  }
  const ranks = new Map((input.shortlist?.candidates ?? []).map((c) => [c.symbol, c]));
  const hidden: MomentumHidden = { funds: 0, noPrice: 0, overTwenty: 0 };
  const cards: MomentumCard[] = [];

  for (const [symbol, confirmation] of latest) {
    const reading = input.current.get(symbol);
    const current = reading && Date.parse(reading.timestamp) >= Date.parse(confirmation.timestamp) ? reading : confirmation;
    const status: MomentumStatus = current.qualifies ? "active" : "faded";
    let fadedAt: string | undefined;
    if (status === "faded") {
      const fade = input.fades.get(symbol);
      // A fade edge older than the crossing belongs to an earlier episode.
      fadedAt = fade && Date.parse(fade.timestamp) >= Date.parse(confirmation.timestamp) ? fade.timestamp : current.timestamp;
      if (input.nowMs - Date.parse(fadedAt) > FADED_VISIBLE_MIN * 60_000) continue;
    }

    if (input.funds.has(symbol)) { hidden.funds++; continue; }
    const funnel = input.funnelBySymbol.get(symbol);
    const price = input.prices.get(symbol) ?? funnel?.price;
    if (price === undefined || !Number.isFinite(price) || price <= 0) { hidden.noPrice++; continue; }
    if (!isUnderTwenty(price)) { hidden.overTwenty++; continue; }

    const factors = factorsOf(current);
    const score = clamp01(current.overall);
    const confirmedScore = clamp01(confirmation.overall);
    const ageMin = Math.floor((input.nowMs - Date.parse(current.timestamp)) / 60_000);
    // An active card with no new reading for an hour describes nothing
    // current (typically yesterday's after-hours crossings overnight).
    if (status === "active" && ageMin > ACTIVE_MAX_SILENT_MIN) continue;
    const candidate = ranks.get(symbol);
    cards.push({
      symbol,
      status,
      score,
      band: bandOf(score),
      factors,
      drivers: driversOf(factors),
      confirmedAt: confirmation.timestamp,
      confirmedScore,
      // A crossing qualifies by definition, so it always has a band.
      confirmedBand: bandOf(confirmedScore) ?? "Mild",
      readingAt: current.timestamp,
      fadedAt,
      staleMinutes: ageMin > STALE_AFTER_MIN ? ageMin : null,
      session: readingSession(current.timestamp),
      price,
      dayChangePct: funnel && Number.isFinite(funnel.gapPct) ? funnel.gapPct : undefined,
      shortlist: candidate ? { rank: candidate.rank, tradability: assessTradability(candidate, input.nowMs) } : undefined,
    });
  }

  // Active regular-session first, then active outside regular hours, then faded; strongest first within each.
  const group = (c: MomentumCard) => (c.status === "faded" ? 2 : c.session === "regular" ? 0 : 1);
  cards.sort((a, b) => group(a) - group(b) || b.score - a.score || a.symbol.localeCompare(b.symbol));
  return { cards, hidden };
}

/** "Hidden: 2 funds · 1 with no price yet · 1 at $20 or more"; null when nothing is hidden. */
export function hiddenSummary(h: MomentumHidden): string | null {
  const parts: string[] = [];
  if (h.funds > 0) parts.push(`${h.funds} ${h.funds === 1 ? "fund" : "funds"}`);
  if (h.noPrice > 0) parts.push(`${h.noPrice} with no price yet`);
  if (h.overTwenty > 0) parts.push(`${h.overTwenty} at $20 or more`);
  return parts.length > 0 ? `Hidden: ${parts.join(" · ")}` : null;
}

/** "12 min" / "2 h" / "2 h 5 min" -- whole minutes, never rounded up. */
export function formatMinutes(minutes: number): string {
  const m = Math.max(0, Math.floor(minutes));
  if (m < 60) return `${m} min`;
  const h = Math.floor(m / 60);
  return m % 60 === 0 ? `${h} h` : `${h} h ${m % 60} min`;
}

/** "confirmed 12 min ago" / "confirmed just now". */
export function confirmedAgo(iso: string, nowMs: number): string {
  const minutes = Math.floor((nowMs - Date.parse(iso)) / 60_000);
  return !Number.isFinite(minutes) || minutes < 1 ? "confirmed just now" : `confirmed ${formatMinutes(minutes)} ago`;
}

function clamp01(n: number): number {
  return Number.isFinite(n) ? Math.min(1, Math.max(0, n)) : 0;
}
