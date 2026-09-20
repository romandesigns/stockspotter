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
