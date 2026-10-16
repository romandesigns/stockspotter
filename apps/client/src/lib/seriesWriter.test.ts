import { test, expect } from "bun:test";
import { barsChange, createSeriesWriter } from "./seriesWriter";

test("tail updates/append match full replacement; history corrections and trims replace", () => {
  type Point = { time: number; value: number };
  let actual: Point[] = []; let replacements = 0; let updates = 0;
  const write = createSeriesWriter<Point>({
    setData(data) { actual = [...data]; replacements++; },
    update(p) { if (actual.at(-1)?.time === p.time) actual[actual.length - 1] = p; else actual.push(p); updates++; },
  });
  const cases = [
    [{time:1,value:10},{time:2,value:20}],
    [{time:1,value:10},{time:2,value:25}],
    [{time:1,value:10},{time:2,value:26},{time:3,value:30}],
    [{time:1,value:11},{time:2,value:26},{time:3,value:30}],
    [{time:2,value:26},{time:3,value:30}],
    [], [{time:10,value:100}],
  ];
  for (const c of cases) { write(c); expect(actual).toEqual(c); }
  expect(replacements).toBe(5); expect(updates).toBe(3);
  write(cases.at(-1)!); expect(updates).toBe(3);
});

test("initial empty write clears pre-populated series and repeated empties are idle", () => {
  let replacements = 0;
  const write = createSeriesWriter<{ time: number; value: number }>({
    setData(data) { expect(data).toEqual([]); replacements++; },
    update() { throw new Error("Empty data must never append"); },
  });
  write([]);
  write([]);
  expect(replacements).toBe(1);
});

test("OHLC and style changes survive tail updates and historical replacement", () => {
  type Point = { time: number; open: number; high: number; low: number; close: number; color?: string };
  let actual: Point[] = [];
  const write = createSeriesWriter<Point>({
    setData(data) { actual = [...data]; },
    update(point) {
      if (point.time < actual.at(-1)!.time) throw new Error("Cannot update historical point");
      if (point.time === actual.at(-1)!.time) actual[actual.length - 1] = point;
      else actual.push(point);
    },
  });
  const first = { time: 1, open: 10, high: 12, low: 9, close: 11 };
  const last = { time: 2, open: 11, high: 13, low: 10, close: 12 };
  const cases = [
    [first, last],
    [first, { ...last, high: 14, low: 8, close: 9, color: "red" }],
    [first, last],
    [{ ...first, open: 8, low: 7 }, last],
    [{ ...first, time: 300 }, { ...last, time: 600 }],
    [first],
  ];
  for (const data of cases) {
    write(data);
    expect(actual).toEqual(data);
  }
});

const ohlc = (time: number, close: number) => ({ time, open: close, high: close, low: close, close, volume: 1 });

test("barsChange: same, tail (last bar / append) and full (history, trim, empty)", () => {
  const a = [ohlc(1, 1), ohlc(2, 2), ohlc(3, 3)];
  expect(barsChange(a, a.map((b) => ({ ...b })))).toEqual({ kind: "same" });
  expect(barsChange(a, [ohlc(1, 1), ohlc(2, 2), ohlc(3, 4)])).toEqual({ kind: "tail", unchangedBefore: 3 });
  expect(barsChange(a, [...a, ohlc(4, 4)])).toEqual({ kind: "tail", unchangedBefore: 4 });
  expect(barsChange(a, [ohlc(1, 1), ohlc(2, 2), ohlc(3, 9), ohlc(4, 4)])).toEqual({ kind: "tail", unchangedBefore: 3 });
  expect(barsChange(a, [ohlc(1, 1), ohlc(2, 5), ohlc(3, 3)])).toEqual({ kind: "full" });
  expect(barsChange(a, [ohlc(2, 2), ohlc(3, 3), ohlc(4, 4)])).toEqual({ kind: "full" });
  expect(barsChange(a, a.slice(1))).toEqual({ kind: "full" });
  expect(barsChange([], a)).toEqual({ kind: "full" });
});

test("a trusted hint skips the scan but writes exactly what the full check writes", () => {
  type Point = { time: number; value: number };
  const run = (useHint: boolean) => {
    let actual: Point[] = [];
    const calls: string[] = [];
    const write = createSeriesWriter<Point>({
      setData(data) { actual = [...data]; calls.push("set"); },
      update(p) { if (actual.at(-1)?.time === p.time) actual[actual.length - 1] = p; else actual.push(p); calls.push(`u${p.time}`); },
    });
    const steps: Point[][] = [
      [{ time: 1, value: 1 }, { time: 2, value: 2 }],
      [{ time: 1, value: 1 }, { time: 2, value: 3 }],
      [{ time: 1, value: 1 }, { time: 2, value: 3 }, { time: 3, value: 4 }],
    ];
    const hints = [undefined, 2, 3];
    steps.forEach((s, i) => { write(s, useHint ? hints[i] : undefined); expect(actual).toEqual(s); });
    // A hint that does not cover the history is ignored, so a real
    // correction still replaces everything.
    const corrected = [{ time: 1, value: 9 }, { time: 2, value: 3 }, { time: 3, value: 5 }];
    write(corrected, useHint ? 1 : undefined);
    expect(actual).toEqual(corrected);
    return calls;
  };
  expect(run(true)).toEqual(run(false));
});
