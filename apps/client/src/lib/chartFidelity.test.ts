import { expect, test } from "bun:test";
import { resample } from "./chartIndicators";
import { mergeBars, toChartBars } from "./derive";
import { reconcileBars } from "./reconcileBars";
import type { BarUpdate } from "@stockspotter/shared-types";

const event = (timestamp: string, close: number, isFinal = false): BarUpdate => ({
  type:"bar_update",symbol:"AUDIT",timestamp,open:10,high:Math.max(10,close),low:Math.min(10,close),close,volume:7,intervalSecs:60,isFinal,
});
test("premarket 5m OHLCV follows clock boundaries across gaps in summer and winter", () => {
  for (const boundary of ["2026-09-18T08:00:00Z","2026-01-09T09:00:00Z","2026-09-18T13:30:00Z","2026-01-09T14:30:00Z"]) {
    const t = Date.parse(boundary)/1000;
    const bars = [-60,0,60,240,300].map((offset,i)=>({time:t+offset,open:10+i,high:20+i,low:5+i,close:12+i,volume:i+1}));
    expect(resample(bars,5)).toEqual([
      {...bars[0],time:t-300},
      {time:t,open:11,high:23,low:6,close:15,volume:9},
      {...bars[4],time:t+300},
    ]);
    expect(resample(bars,1)).toEqual(bars);
  }
});
test("official correction survives delayed preview and updates 5m OHLCV", () => {
  const at = "2026-09-18T08:01:00Z";
  let bars = reconcileBars([],event(at,12),500);
  bars = reconcileBars(bars,event(at,15,true),500);
  bars = reconcileBars(bars,event(at,9),500);
  bars = reconcileBars(bars,event(at,16,true),500);
  expect(resample(toChartBars(bars),5)[0]).toMatchObject({open:10,high:16,low:10,close:16,volume:7});
});
// Characterization of an UNFIXED risk: CandleBar has no completeness/provenance.
test("characterize REST/live overlap: partial live volume currently wins", () => {
  const historical = [{time:1,open:8,high:15,low:7,close:12,volume:1000}];
  const live = [{time:1,open:12,high:13,low:12,close:13,volume:3}];
  expect(mergeBars(historical,live)[0].volume).toBe(3);
});
