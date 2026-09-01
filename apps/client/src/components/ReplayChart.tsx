// Thin React wrapper around the real Super Chart engine's `backtest`
// context (../lib/superChartEngine.ts's CHART_PRESETS.backtest --
// already existed, "carried over for when [it] gets wired to real
// data", per that file's own doc comment; this is that wiring). Same
// division as SuperChart.tsx (Scanner Detail): the engine owns the
// actual chart, this component just mounts it and feeds it bars.
//
// Simpler than SuperChart.tsx on purpose, matching the prototype's own
// real Backtest Replay tab: MA9/MA20/VWAP always show (no Indicators
// popover there in the original either -- "Backtest Replay has no
// Indicators popover of its own", stockspotter-super-chart-prototype
// memory), no MACD (`backtest` preset has macd:false), no resize handle
// (`resizable:false`).
//
// Mounts once per (symbol, date range) "chart identity" with the FULL
// fetched range; playback (ReplayLauncher.tsx) reveals it progressively
// by calling api.setBars() with a growing prefix on every tick, the same
// update path a timeframe-pill switch uses in SuperChart.tsx -- not a
// remount per tick.

import { useEffect, useRef } from "react";
import type { CandleBar } from "../lib/derive";
import { mountSuperChart, wireChartTooltip, type SuperChartApi } from "../lib/superChartEngine";

export function ReplayChart(props: { chartKey: string; bars: CandleBar[]; visibleCount: number; height?: number }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const apiRef = useRef<SuperChartApi | null>(null);
  const barsRef = useRef<CandleBar[]>(props.bars);
  barsRef.current = props.bars;

  // Mount fresh per chart identity (symbol + date range) -- same model
  // SuperChart.tsx uses for a symbol switch, not one instance whose data
  // gets destructively swapped across an unrelated replay selection.
  useEffect(() => {
    const container = containerRef.current;
    if (!container || barsRef.current.length === 0) return;

    const api = mountSuperChart(container, "backtest", {
      bars: barsRef.current.slice(0, props.visibleCount),
      height: props.height ?? (container.clientHeight || undefined),
    });
    apiRef.current = api;
    const unwireTooltip = wireChartTooltip(api, container, () => barsRef.current[0]?.open ?? 0);

    return () => {
      unwireTooltip();
      api.destroy();
      api.chart.remove();
      apiRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.chartKey]);

  // Playback ticks / scrub -- update the already-mounted instance in
  // place rather than remounting, matching the prototype's own
  // btChart.series.*.setData() progressive-reveal path.
  useEffect(() => {
    apiRef.current?.setBars(barsRef.current.slice(0, props.visibleCount));
  }, [props.visibleCount, props.bars]);

  return <div ref={containerRef} className="super-chart-mount replay-chart-mount" />;
}
