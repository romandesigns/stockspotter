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
import type { ReplaySignal } from "../lib/useReplaySignals";

export function ReplayChart(props: {
  chartKey: string;
  bars: CandleBar[];
  visibleCount: number;
  /** Detection signals for this window (useReplaySignals). Revealed in
   * step with playback, not all at once -- the whole point of replaying
   * a signal is seeing it arrive at the moment it would have fired, so
   * showing every future marker up front would give away the answer. */
  signals?: ReplaySignal[];
  height?: number;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const apiRef = useRef<SuperChartApi | null>(null);
  const barsRef = useRef<CandleBar[]>(props.bars);
  barsRef.current = props.bars;
  // Tracks the live visibleCount so the tooltip's bars-getter (set up
  // once per mount, see wireChartTooltip below) always looks up against
  // whatever's actually plotted right now, not whatever slice existed
  // when the effect first ran.
  const visibleCountRef = useRef(props.visibleCount);
  visibleCountRef.current = props.visibleCount;

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
    const unwireTooltip = wireChartTooltip(api, container, () => barsRef.current.slice(0, visibleCountRef.current), () => barsRef.current[0]?.open ?? 0);

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
    // Replay keeps its existing progressive fit; live charts preserve user zoom.
    apiRef.current?.chart.timeScale().fitContent();
  }, [props.visibleCount, props.bars]);

  // Markers follow the same progressive reveal: only signals at or
  // before the last visible bar's timestamp. Recomputed on every tick
  // alongside setBars rather than tracked incrementally, so a backwards
  // scrub correctly REMOVES markers again instead of leaving stale ones
  // plotted ahead of the playhead.
  useEffect(() => {
    const api = apiRef.current;
    if (!api) return;
    const signals = props.signals;
    if (!signals || signals.length === 0) {
      api.setSignalMarkers([]);
      return;
    }
    const lastVisible = barsRef.current[Math.max(0, props.visibleCount - 1)];
    if (!lastVisible) {
      api.setSignalMarkers([]);
      return;
    }
    api.setSignalMarkers(
      signals
        .filter((s) => s.time <= lastVisible.time)
        .map((s) => ({ time: s.time, strategy: s.strategy })),
    );
  }, [props.visibleCount, props.signals, props.bars]);

  return <div ref={containerRef} className="super-chart-mount replay-chart-mount" />;
}
