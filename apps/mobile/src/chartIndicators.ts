// Ported verbatim from apps/client/src/lib/chartIndicators.ts -- same
// formulas (resample/sma/vwap/computeMACD), not re-derived. useChartBars.ts
// used to keep its own private copy of resample() only; this replaces
// that copy and adds sma/vwap/computeMACD so ChartScreen's new timeframe
// picker and MomentumScoreRow port can reuse the exact same math the web
// chart and its momentum panel already use, instead of a second
// hand-rolled version.

import type { CandleBar } from "./types";

export interface SeriesPoint {
  time: number;
  value: number;
}

/**
 * Buckets 1-minute bars into wider candles by real wall-clock time
 * windows -- bucketing by `Math.floor(time / bucketSec)` rather than
 * fixed array-index chunking, so it stays correct across gaps (a symbol
 * that drops out of the watchlist tolerance window and comes back).
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
    out.push({ time: b.time, value: cumV > 0 ? +(cumPV / cumV).toFixed(3) : 0 });
  }
  return out;
}

// Simplified EMA (seeds with the first value rather than an initial SMA
// window) -- same prototype-grade approximation web's port carries,
// ported as-is.
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

/** Ported verbatim from apps/client/src/lib/chartIndicators.ts -- standard
 * 14-period RSI, Wilder's smoothing. Nothing plotted for the first
 * `period` bars (insufficient history), same convention sma() follows. */
export function computeRSI(bars: CandleBar[], period = 14): SeriesPoint[] {
  const out: SeriesPoint[] = [];
  if (bars.length < period + 1) return out;

  let avgGain = 0;
  let avgLoss = 0;
  for (let i = 1; i <= period; i++) {
    const change = bars[i].close - bars[i - 1].close;
    if (change >= 0) avgGain += change;
    else avgLoss -= change;
  }
  avgGain /= period;
  avgLoss /= period;

  const rsiFrom = (gain: number, loss: number) => (loss === 0 ? 100 : 100 - 100 / (1 + gain / loss));
  out.push({ time: bars[period].time, value: +rsiFrom(avgGain, avgLoss).toFixed(2) });

  for (let i = period + 1; i < bars.length; i++) {
    const change = bars[i].close - bars[i - 1].close;
    const gain = change > 0 ? change : 0;
    const loss = change < 0 ? -change : 0;
    avgGain = (avgGain * (period - 1) + gain) / period;
    avgLoss = (avgLoss * (period - 1) + loss) / period;
    out.push({ time: bars[i].time, value: +rsiFrom(avgGain, avgLoss).toFixed(2) });
  }
  return out;
}

export interface BollingerBands {
  upper: SeriesPoint[];
  middle: SeriesPoint[];
  lower: SeriesPoint[];
}

/** Ported verbatim from apps/client/src/lib/chartIndicators.ts -- standard
 * 20-period SMA ± 2 standard deviations, same O(n) rolling-sum idiom
 * sma() already uses. */
export function computeBollingerBands(bars: CandleBar[], period = 20, stdDevMultiplier = 2): BollingerBands {
  const upper: SeriesPoint[] = [];
  const middle: SeriesPoint[] = [];
  const lower: SeriesPoint[] = [];
  let sum = 0;
  let sumSq = 0;
  for (let i = 0; i < bars.length; i++) {
    const close = bars[i].close;
    sum += close;
    sumSq += close * close;
    if (i >= period) {
      const dropped = bars[i - period].close;
      sum -= dropped;
      sumSq -= dropped * dropped;
    }
    if (i >= period - 1) {
      const mean = sum / period;
      const variance = Math.max(0, sumSq / period - mean * mean);
      const stdDev = Math.sqrt(variance);
      const time = bars[i].time;
      middle.push({ time, value: +mean.toFixed(3) });
      upper.push({ time, value: +(mean + stdDevMultiplier * stdDev).toFixed(3) });
      lower.push({ time, value: +(mean - stdDevMultiplier * stdDev).toFixed(3) });
    }
  }
  return { upper, middle, lower };
}
