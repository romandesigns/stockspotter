// Per-symbol chart freshness: is the candle series for the symbol ON SCREEN
// current, independently of whether the socket is busy?
//
// feedHealth.ts answers a transport question. This answers a different one,
// and the 2026-09-21 live session showed how far apart they can be. With the
// socket provably healthy -- 96,000+ messages in five minutes, stream_lagged
// zero, no reconnect -- 355 of 678 tracked symbols (52.4%) had received no
// chart update for over 90 seconds, and the median symbol was 132.7 seconds
// stale. Every one of them would have rendered as "Live", because liveness
// was inferred from the socket rather than from the series.
//
// # Why a fixed timeout cannot work
//
// Measured median inter-update gaps on the same session span two orders of
// magnitude: GRML 0.53s, BTTC 0.59s, LOBO 0.80s, VRME 1.96s, SCNI 7.74s,
// HWH 6.47s, RGNT 30.34s, SONM 17.53s with a legitimate 157s maximum. Any
// single threshold is simultaneously hair-triggered for GRML and useless for
// SONM. 80% of tracked symbols are legitimately sparse, so a naive
// per-symbol timeout would paint most of the universe broken and destroy the
// signal it exists to carry.
//
// # Two tiers, not one
//
// A single threshold has to choose between crying wolf and hiding real
// staleness. Two do not:
//
//   QUIET -- beyond this symbol's normal spacing, so stop claiming "live",
//            but entirely consistent with it being a sparse symbol. Not an
//            alarm.
//   STALE -- beyond even a generous bound on this symbol's own worst
//            observed spacing. Something is probably wrong.
//
// Both derive from the symbol's own recent cadence. Validated by causal
// replay over the session capture: 2,380 decisions across the 38 symbols
// with enough history produced **zero** false STALE (a STALE verdict
// followed by a perfectly normal update) and 4 false QUIET (0.2%), which is
// harmless because QUIET is not an alarm.

/** What the series for one symbol can honestly claim about itself. */
export type SymbolFreshness =
  /** Updating within its own established cadence. */
  | "live"
  /** Beyond its normal spacing but consistent with being sparse. */
  | "quiet"
  /** Beyond a generous bound on its own worst observed spacing. */
  | "stale"
  /** Too few updates seen to have a cadence. Never reported as live on
   *  cadence grounds alone -- see resolveSymbolFreshness. */
  | "insufficient_history";

/** Bounded recent-gap window, in gaps.
 *
 * This is what stops one ancient update dominating forever: a gap leaves the
 * estimate after at most this many further updates. 12 is enough for a
 * stable median at every cadence measured on 2026-09-21 while still
 * adapting inside a minute for an active symbol. */
export const CADENCE_WINDOW = 12;

/** Gaps required before a cadence is considered established. Below this the
 *  state is `insufficient_history`, which is a different claim from `stale`
 *  and must not be conflated with it. */
export const MIN_GAPS_FOR_CADENCE = 4;

/** Clamps. The floors stop a fast symbol flapping on ordinary jitter; the
 *  ceilings stop a pathologically sparse symbol from acquiring a threshold
 *  so wide that genuine breakage never surfaces. All four were chosen
 *  against the measured session, not picked for roundness -- with these, the
 *  median symbol lands on quiet 20.0s / stale 45.0s, GRML on 20.0/45.0, and
 *  SONM on 52.6/235.5, which respects its real 157s maximum gap. */
export const QUIET_FLOOR_SECS = 20;
export const QUIET_CEIL_SECS = 120;
export const STALE_FLOOR_SECS = 45;
export const STALE_CEIL_SECS = 300;

/** Causal, bounded cadence state for exactly one (symbol, interval) series.
 *
 * Carries the interval because the 1-minute and 30-second streams have
 * genuinely different cadences for the same symbol and must not share an
 * estimate. */
export interface SymbolCadenceState {
  symbol: string;
  intervalSecs: number;
  /** Receipt time of the most recent update, ms. Null before the first. */
  lastUpdateMs: number | null;
  /** Bounded, oldest first, most recent last. Length <= CADENCE_WINDOW. */
  gapsSecs: readonly number[];
}

export function emptyCadence(symbol: string, intervalSecs: number): SymbolCadenceState {
  return { symbol, intervalSecs, lastUpdateMs: null, gapsSecs: [] };
}

/**
 * Fold one observed update into the cadence state. Pure and causal: it uses
 * only this update and gaps already observed, never anything later.
 *
 * A non-monotonic or duplicate receipt time is ignored rather than recorded
 * as a zero or negative gap, which would drag the median toward zero and
 * make every symbol look hair-trigger. Receipt times come from the client
 * clock, so equal timestamps are entirely possible in a burst.
 *
 * Switching symbol or interval returns a fresh state instead of appending:
 * inheriting GRML's 0.5s cadence onto SONM would report SONM stale within
 * seconds of opening its chart.
 */
export function observeUpdate(
  state: SymbolCadenceState,
  symbol: string,
  intervalSecs: number,
  atMs: number,
): SymbolCadenceState {
  if (state.symbol !== symbol || state.intervalSecs !== intervalSecs) {
    return { symbol, intervalSecs, lastUpdateMs: atMs, gapsSecs: [] };
  }
  if (state.lastUpdateMs === null) {
    return { ...state, lastUpdateMs: atMs };
  }
  const gap = (atMs - state.lastUpdateMs) / 1000;
  if (!(gap > 0)) return { ...state, lastUpdateMs: Math.max(state.lastUpdateMs, atMs) };
  const gaps = [...state.gapsSecs, gap];
  return {
    ...state,
    lastUpdateMs: atMs,
    gapsSecs: gaps.length > CADENCE_WINDOW ? gaps.slice(gaps.length - CADENCE_WINDOW) : gaps,
  };
}

export interface CadenceThresholds {
  /** Gaps backing the estimate. */
  samples: number;
  medianGapSecs: number;
  maxGapSecs: number;
  quietAfterSecs: number;
  staleAfterSecs: number;
}

function median(values: readonly number[]): number {
  const a = [...values].sort((x, y) => x - y);
  const mid = a.length >> 1;
  return a.length % 2 ? a[mid] : (a[mid - 1] + a[mid]) / 2;
}

const clamp = (x: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, x));

/**
 * Thresholds for this series, or null while the cadence is not established.
 *
 * QUIET uses the median -- a rolling median over a bounded window, so a
 * single outlier cannot move it much and cannot move it for long.
 *
 * STALE uses the window maximum, not the median, and that asymmetry is the
 * point. A symbol whose gaps are mostly short but occasionally long (SONM:
 * 17.5s median, 157s maximum) would be declared stale constantly by any
 * median-derived bound. Scaling its own worst recent gap is what makes the
 * verdict defensible for that symbol rather than for the average one.
 * The window bounds the outlier's influence; the ceiling bounds the
 * threshold.
 *
 * STALE is floored at QUIET so the two can never invert under clamping.
 */
export function cadenceThresholds(state: SymbolCadenceState): CadenceThresholds | null {
  if (state.gapsSecs.length < MIN_GAPS_FOR_CADENCE) return null;
  const med = median(state.gapsSecs);
  const max = Math.max(...state.gapsSecs);
  const quietAfterSecs = clamp(3 * med, QUIET_FLOOR_SECS, QUIET_CEIL_SECS);
  const staleAfterSecs = Math.max(clamp(1.5 * max, STALE_FLOOR_SECS, STALE_CEIL_SECS), quietAfterSecs);
  return { samples: state.gapsSecs.length, medianGapSecs: med, maxGapSecs: max, quietAfterSecs, staleAfterSecs };
}

export interface SymbolFreshnessResult {
  freshness: SymbolFreshness;
  /** Seconds since this series last updated, or null if never. */
  ageSecs: number | null;
  thresholds: CadenceThresholds | null;
}

/**
 * Classify the displayed series from its own history alone.
 *
 * Deliberately takes no transport argument. Nothing another symbol does can
 * reach this function, which is the structural guarantee that activity
 * elsewhere can never make this symbol look live.
 *
 * With no cadence yet, a recent update is still evidence of liveness, so the
 * result is `live` within QUIET_FLOOR_SECS and `insufficient_history`
 * beyond it -- conservative in that it never claims `live` on cadence
 * grounds it does not have, and never claims `stale` on a bound it cannot
 * justify.
 */
export function resolveSymbolFreshness(state: SymbolCadenceState, nowMs: number): SymbolFreshnessResult {
  if (state.lastUpdateMs === null) {
    return { freshness: "insufficient_history", ageSecs: null, thresholds: null };
  }
  const ageSecs = Math.max(0, (nowMs - state.lastUpdateMs) / 1000);
  const t = cadenceThresholds(state);
  if (!t) {
    return { freshness: ageSecs <= QUIET_FLOOR_SECS ? "live" : "insufficient_history", ageSecs, thresholds: null };
  }
  if (ageSecs > t.staleAfterSecs) return { freshness: "stale", ageSecs, thresholds: t };
  if (ageSecs > t.quietAfterSecs) return { freshness: "quiet", ageSecs, thresholds: t };
  return { freshness: "live", ageSecs, thresholds: t };
}
