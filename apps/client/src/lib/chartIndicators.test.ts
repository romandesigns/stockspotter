// Indicator math tests.
//
// These formulas drive what a trader sees on the chart and decides
// against, and they are the kind of code where an off-by-one in a
// rolling window produces plausible-looking output that is quietly
// wrong for weeks. Every expected value below is derived by hand from
// the definition, not captured from a previous run of this code — a
// snapshot of the implementation would pass even if the implementation
// were wrong.

import { describe, expect, test } from "bun:test";
import type { CandleBar } from "./derive";
import { computeBollingerBands, computeMACD, computeRSI, resample, sma, vwap } from "./chartIndicators";

/** Builds a bar at `minute` past an arbitrary epoch-aligned hour. */
function bar(minute: number, close: number, extra: Partial<CandleBar> = {}): CandleBar {
  return {
    time: 1_700_000_000 + minute * 60,
    open: close,
    high: close,
    low: close,
    close,
    volume: 100,
    ...extra,
  };
}

describe("sma", () => {
  test("averages exactly the trailing period", () => {
    const bars = [1, 2, 3, 4, 5].map((c, i) => bar(i, c));
    const out = sma(bars, 3);
    // First value can only exist once 3 bars are available.
    expect(out.length).toBe(3);
    expect(out[0].value).toBeCloseTo((1 + 2 + 3) / 3, 10);
    expect(out[1].value).toBeCloseTo((2 + 3 + 4) / 3, 10);
    expect(out[2].value).toBeCloseTo((3 + 4 + 5) / 3, 10);
  });

  test("aligns each point to its own bar's timestamp, not the window start", () => {
    const bars = [1, 2, 3].map((c, i) => bar(i, c));
    const out = sma(bars, 3);
    expect(out[0].time).toBe(bars[2].time);
  });

  test("returns nothing when there aren't enough bars", () => {
    expect(sma([bar(0, 1), bar(1, 2)], 3)).toEqual([]);
    expect(sma([], 3)).toEqual([]);
  });
});

describe("vwap", () => {
  test("weights price by volume, not by bar count", () => {
    // Typical price is (h+l+c)/3; with h=l=c it's just the close.
    const bars = [
      bar(0, 10, { volume: 1 }),
      bar(1, 20, { volume: 99 }),
    ];
    const out = vwap(bars);
    // A simple average would be 15. Volume weighting pulls it to ~19.9.
    expect(out[1].value).toBeCloseTo((10 * 1 + 20 * 99) / 100, 10);
    expect(out[1].value).toBeGreaterThan(19);
  });

  test("is cumulative across the session, not a rolling window", () => {
    const bars = [bar(0, 10), bar(1, 20), bar(2, 30)];
    const out = vwap(bars);
    expect(out[0].value).toBeCloseTo(10, 10);
    expect(out[1].value).toBeCloseTo(15, 10);
    expect(out[2].value).toBeCloseTo(20, 10);
  });

  test("uses the full typical price, not just the close", () => {
    const b: CandleBar = { time: 1, open: 10, high: 30, low: 20, close: 10, volume: 1 };
    expect(vwap([b])[0].value).toBeCloseTo((30 + 20 + 10) / 3, 10);
  });
});

describe("computeRSI", () => {
  test("a monotonically rising series pins to 100", () => {
    // No down moves at all -> average loss is zero -> RSI is 100 by
    // definition. Also the classic divide-by-zero trap in RSI code.
    const bars = Array.from({ length: 30 }, (_, i) => bar(i, 100 + i));
    const out = computeRSI(bars, 14);
    expect(out.length).toBeGreaterThan(0);
    expect(out[out.length - 1].value).toBeCloseTo(100, 6);
  });

  test("a monotonically falling series pins to 0", () => {
    const bars = Array.from({ length: 30 }, (_, i) => bar(i, 100 - i));
    const out = computeRSI(bars, 14);
    expect(out[out.length - 1].value).toBeCloseTo(0, 6);
  });

  test("a flat series sits at neither extreme's error state", () => {
    // Zero gain AND zero loss: whatever the implementation returns it
    // must be a finite number, never NaN.
    const bars = Array.from({ length: 30 }, (_, i) => bar(i, 100));
    for (const p of computeRSI(bars, 14)) {
      expect(Number.isFinite(p.value)).toBe(true);
    }
  });

  test("stays within 0..100 on real-shaped noisy data", () => {
    const closes = [44, 44.3, 44.1, 43.6, 44.3, 44.8, 45.1, 45.4, 45.4, 45.7, 46.2, 46.0, 46.0, 46.4, 46.2, 45.6, 46.2, 46.2, 46.0, 46.0];
    const out = computeRSI(closes.map((c, i) => bar(i, c)), 14);
    expect(out.length).toBeGreaterThan(0);
    for (const p of out) {
      expect(p.value).toBeGreaterThanOrEqual(0);
      expect(p.value).toBeLessThanOrEqual(100);
    }
  });

  test("returns nothing without enough history", () => {
    expect(computeRSI([bar(0, 1), bar(1, 2)], 14)).toEqual([]);
  });
});

describe("computeMACD", () => {
  test("is zero everywhere on a perfectly flat series", () => {
    // Both EMAs converge to the same constant, so their difference and
    // the histogram must vanish. A window misalignment shows up here as
    // a non-zero drift.
    const bars = Array.from({ length: 60 }, (_, i) => bar(i, 50));
    const { macdLine: macd, signalLine: signal, hist: histogram } = computeMACD(bars);
    for (const p of macd) expect(p.value).toBeCloseTo(0, 8);
    for (const p of signal) expect(p.value).toBeCloseTo(0, 8);
    for (const p of histogram) expect(p.value).toBeCloseTo(0, 8);
  });

  test("goes positive on a sustained uptrend", () => {
    // Fast EMA leads slow EMA when price is rising.
    const bars = Array.from({ length: 60 }, (_, i) => bar(i, 50 + i));
    const { macdLine: macd } = computeMACD(bars);
    expect(macd[macd.length - 1].value).toBeGreaterThan(0);
  });

  test("goes negative on a sustained downtrend", () => {
    const bars = Array.from({ length: 60 }, (_, i) => bar(i, 200 - i));
    const { macdLine: macd } = computeMACD(bars);
    expect(macd[macd.length - 1].value).toBeLessThan(0);
  });

  test("histogram is the difference of the lines, to plotting precision", () => {
    // Not exactly `round(macd) - round(signal)`: the implementation
    // computes the difference from the UNROUNDED values and rounds once
    // at the end, which is the better order (it doesn't accumulate two
    // roundings). So the tolerance here is the 4-decimal plotting
    // granularity, not float epsilon -- asserting tighter would be
    // asserting a worse implementation.
    const bars = Array.from({ length: 80 }, (_, i) => bar(i, 50 + Math.sin(i / 5) * 10));
    const { macdLine: macd, signalLine: signal, hist: histogram } = computeMACD(bars);
    const macdAt = new Map(macd.map((p) => [p.time, p.value]));
    const signalAt = new Map(signal.map((p) => [p.time, p.value]));
    expect(histogram.length).toBeGreaterThan(0);
    for (const h of histogram) {
      expect(h.value).toBeCloseTo((macdAt.get(h.time) ?? 0) - (signalAt.get(h.time) ?? 0), 3);
    }
  });
});

describe("computeBollingerBands", () => {
  test("bands collapse onto the middle when price doesn't move", () => {
    // Zero standard deviation -> zero width. Catches a stddev
    // implementation that returns NaN on a constant series.
    const bars = Array.from({ length: 30 }, (_, i) => bar(i, 42));
    const { upper, lower } = computeBollingerBands(bars, 20, 2);
    expect(upper.length).toBeGreaterThan(0);
    for (let i = 0; i < upper.length; i++) {
      expect(upper[i].value).toBeCloseTo(42, 8);
      expect(lower[i].value).toBeCloseTo(42, 8);
    }
  });

  test("upper is always above lower on moving data", () => {
    const bars = Array.from({ length: 60 }, (_, i) => bar(i, 50 + Math.sin(i / 3) * 5));
    const { upper, lower } = computeBollingerBands(bars, 20, 2);
    expect(upper.length).toBeGreaterThan(0);
    for (let i = 0; i < upper.length; i++) {
      expect(upper[i].value).toBeGreaterThan(lower[i].value);
      expect(upper[i].time).toBe(lower[i].time);
    }
  });

  test("a wider multiplier widens the band", () => {
    const bars = Array.from({ length: 60 }, (_, i) => bar(i, 50 + Math.sin(i / 3) * 5));
    const narrow = computeBollingerBands(bars, 20, 1);
    const wide = computeBollingerBands(bars, 20, 3);
    const spread = (b: ReturnType<typeof computeBollingerBands>, i: number) => b.upper[i].value - b.lower[i].value;
    expect(spread(wide, 0)).toBeGreaterThan(spread(narrow, 0));
  });
});

describe("resample", () => {
  test("buckets by wall-clock time, not by array index", () => {
    // The real bug this function was rewritten to fix. Bars at minutes
    // 0, 1 and 7 with a 5-minute bucket must produce TWO candles
    // (0-4 and 5-9), not one-per-N-bars.
    const bars = [bar(0, 10), bar(1, 11), bar(7, 20)];
    const out = resample(bars, 5);
    expect(out.length).toBe(2);
    expect(out[0].close).toBe(11);
    expect(out[1].close).toBe(20);
  });

  test("aggregates OHLCV correctly within a bucket", () => {
    const bars: CandleBar[] = [
      { time: 1_700_000_000, open: 10, high: 12, low: 9, close: 11, volume: 100 },
      { time: 1_700_000_060, open: 11, high: 15, low: 8, close: 14, volume: 50 },
    ];
    const [candle] = resample(bars, 5);
    expect(candle.open).toBe(10); // first bar's open
    expect(candle.close).toBe(14); // last bar's close
    expect(candle.high).toBe(15); // max across the bucket
    expect(candle.low).toBe(8); // min across the bucket
    expect(candle.volume).toBe(150); // summed
  });

  test("is a pass-through at 1 minute", () => {
    const bars = [bar(0, 10), bar(1, 11)];
    expect(resample(bars, 1)).toEqual(bars);
  });

  test("handles an empty series", () => {
    expect(resample([], 5)).toEqual([]);
  });
});
