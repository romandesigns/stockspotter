// Indicator math, ported verbatim from the Artifact prototype
// (stockspotter-super-chart-prototype memory) rather than reimplemented —
// these formulas were already settled there, nothing about them needed
// re-deciding for the real port.

import type { CandleBar } from "./derive";

export interface SeriesPoint {
  time: number;
  value: number;
}

/**
 * Buckets 1-minute bars into wider candles by real wall-clock time
 * windows -- ported from the prototype, including its own real fix: an
 * earlier version bucketed by fixed array-index chunks (every N bars),
 * which silently mis-groups data once bars aren't perfectly one-per-
 * minute (real trade data has gaps where nothing printed for a stretch,
 * and our own tolerance-based watchlist can genuinely have a symbol drop
 * out and come back). Bucketing by `Math.floor(time / bucketSec)`
 * instead is correct regardless of gaps.
 */
export function resample(bars: CandleBar[], minutesPerBucket: number): CandleBar[] {
  if (minutesPerBucket === 1) return bars;
  const bucketSec = minutesPerBucket * 60;
  const out: CandleBar[] = [];
  let current: CandleBar | null = null;
  let currentBucketStart: number | null = null;
  for (const b of bars) {
    const bucketStart = Math.floor(b.time / bucketSec) * bucketSec;
    if (bucketStart !== currentBucketStart) {
      if (current) out.push(current);
      currentBucketStart = bucketStart;
      current = { time: bucketStart, open: b.open, high: b.high, low: b.low, close: b.close, volume: b.volume };
    } else if (current) {
      current.high = Math.max(current.high, b.high);
      current.low = Math.min(current.low, b.low);
      current.close = b.close;
      current.volume += b.volume;
    }
  }
  if (current) out.push(current);
  return out;
}

export function sma(bars: CandleBar[], period: number): SeriesPoint[] {
  const out: SeriesPoint[] = [];
  let sum = 0;
  for (let i = 0; i < bars.length; i++) {
    sum += bars[i].close;
    if (i >= period) sum -= bars[i - period].close;
    if (i >= period - 1) out.push({ time: bars[i].time, value: +(sum / period).toFixed(3) });
  }
  return out;
}

export function vwap(bars: CandleBar[]): SeriesPoint[] {
  const out: SeriesPoint[] = [];
  let cumPV = 0;
  let cumV = 0;
  for (const b of bars) {
    const tp = (b.high + b.low + b.close) / 3;
    cumPV += tp * b.volume;
    cumV += b.volume;
    // One deliberate deviation from the prototype's cumPV/cumV: real live
    // bars can have volume:0 (a tracked symbol with no trades that
    // minute), which the prototype's always-real-or-synthetic-with-volume
    // data never hit -- guarded here rather than emitting NaN into the
    // series.
    out.push({ time: b.time, value: cumV > 0 ? +(cumPV / cumV).toFixed(3) : 0 });
  }
  return out;
}

// Simplified EMA (seeds with the first value rather than an initial SMA
// window) — same prototype-grade approximation the Artifact used, not a
// trading-accurate implementation. Ported as-is rather than "fixed" since
// nothing about it was flagged as wrong, just approximate.
function emaSeries(values: number[], period: number): number[] {
  const k = 2 / (period + 1);
  const out = new Array<number>(values.length);
  let prev = 0;
  for (let i = 0; i < values.length; i++) {
    prev = i === 0 ? values[i] : values[i] * k + prev * (1 - k);
    out[i] = prev;
  }
  return out;
}

export interface MACDResult {
  macdLine: SeriesPoint[];
  signalLine: SeriesPoint[];
  hist: { time: number; value: number; color: string }[];
}

export function computeMACD(bars: CandleBar[]): MACDResult {
  const closes = bars.map((b) => b.close);
  const ema12 = emaSeries(closes, 12);
  const ema26 = emaSeries(closes, 26);
  const macdVals = closes.map((_, i) => ema12[i] - ema26[i]);
  const signalVals = emaSeries(macdVals, 9);
  const macdLine: SeriesPoint[] = [];
  const signalLine: SeriesPoint[] = [];
  const hist: { time: number; value: number; color: string }[] = [];
  for (let i = 0; i < bars.length; i++) {
    const h = macdVals[i] - signalVals[i];
    macdLine.push({ time: bars[i].time, value: +macdVals[i].toFixed(4) });
    signalLine.push({ time: bars[i].time, value: +signalVals[i].toFixed(4) });
    hist.push({ time: bars[i].time, value: +h.toFixed(4), color: h >= 0 ? "rgba(12,163,12,.55)" : "rgba(208,59,59,.55)" });
  }
  return { macdLine, signalLine, hist };
}
