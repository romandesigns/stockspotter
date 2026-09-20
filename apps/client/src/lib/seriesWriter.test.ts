import { test, expect } from "bun:test";
import { createSeriesWriter } from "./seriesWriter";

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
