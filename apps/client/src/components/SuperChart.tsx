// The reusable chart component from the Artifact prototype
// (stockspotter-super-chart-prototype memory), ported to real code —
// deliberately just the plumbing pass. That prototype's whole design
// history is a list of bugs (MACD painting across the wrong band, volume
// nearly invisible, session-highlight shading rendering as one giant gray
// box...) that were only ever caught because Roman looked at a screenshot,
// not by inspecting the code. So this pass is scoped to "real live data
// renders as a correct candlestick+volume chart" — no toolbar, no
// indicators/MACD, no playback, no presets-by-context yet. Those come once
// we're iterating on this together with real screenshots, same as before.
import { useEffect, useRef } from "react";
import {
  ColorType,
  CrosshairMode,
  createChart,
  type IChartApi,
  type ISeriesApi,
  type UTCTimestamp,
} from "lightweight-charts";
import type { CandleBar } from "../lib/derive";

const CHART_HEIGHT = 420;

// Same semantic pair as the rest of the app's up/down coloring (fast-funnel
// panels etc.) — good/critical, CVD-safe per the prototype's validated
// palette (see stockspotter-super-chart-prototype memory).
const UP_COLOR = "#0ca30c";
const DOWN_COLOR = "#d03b3b";

export function SuperChart(props: { symbol: string; bars: CandleBar[] }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<IChartApi | null>(null);
  const candleSeriesRef = useRef<ISeriesApi<"Candlestick"> | null>(null);
  const volumeSeriesRef = useRef<ISeriesApi<"Histogram"> | null>(null);

  // Mount the chart once. Series/options changes below react to prop
  // changes without tearing the chart instance down and rebuilding it.
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const chart = createChart(container, {
      width: container.clientWidth,
      height: CHART_HEIGHT,
      layout: {
        background: { type: ColorType.Solid, color: "#1e1e1e" },
        textColor: "#d4d4d4",
      },
      grid: {
        vertLines: { color: "#2d2d2d" },
        horzLines: { color: "#2d2d2d" },
      },
      crosshair: { mode: CrosshairMode.Normal },
      timeScale: { timeVisible: true, secondsVisible: false },
    });

    const candleSeries = chart.addCandlestickSeries({
      upColor: UP_COLOR,
      downColor: DOWN_COLOR,
      borderVisible: false,
      wickUpColor: UP_COLOR,
      wickDownColor: DOWN_COLOR,
    });

    // Overlaid at the bottom of the same pane rather than its own band —
    // matches the prototype's later "volume overlaid" redesign, see that
    // memory's overlay/layout section, rather than reintroducing bug #4
    // from that history (volume nearly invisible from a stale fixed band).
    const volumeSeries = chart.addHistogramSeries({
      priceFormat: { type: "volume" },
      priceScaleId: "volume",
    });
    chart.priceScale("volume").applyOptions({
      scaleMargins: { top: 0.8, bottom: 0 },
    });

    chartRef.current = chart;
    candleSeriesRef.current = candleSeries;
    volumeSeriesRef.current = volumeSeries;

    const resizeObserver = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (!entry) return;
      chart.applyOptions({ width: entry.contentRect.width });
    });
    resizeObserver.observe(container);

    return () => {
      resizeObserver.disconnect();
      chart.remove();
      chartRef.current = null;
      candleSeriesRef.current = null;
      volumeSeriesRef.current = null;
    };
    // Intentionally mount-once: `symbol`/`bars` are handled by the effect
    // below via setData, not by remounting the whole chart on every tick.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Real data in. Re-setting full data on every update is simple and, at
  // the current per-symbol cap of 500 bars (see MAX_BARS_PER_SYMBOL),
  // cheap — a real per-tick `.update()` path for a still-forming candle
  // can come later once this needs to look "live" mid-bar rather than
  // only ever showing already-closed 1-minute bars.
  useEffect(() => {
    if (!candleSeriesRef.current || !volumeSeriesRef.current) return;
    candleSeriesRef.current.setData(
      props.bars.map((b) => ({
        time: b.time as UTCTimestamp,
        open: b.open,
        high: b.high,
        low: b.low,
        close: b.close,
      })),
    );
    volumeSeriesRef.current.setData(
      props.bars.map((b) => ({
        time: b.time as UTCTimestamp,
        value: b.volume,
        color: b.close >= b.open ? `${UP_COLOR}80` : `${DOWN_COLOR}80`,
      })),
    );
    chartRef.current?.timeScale().fitContent();
  }, [props.bars]);

  if (props.bars.length === 0) {
    return (
      <div className="super-chart-empty" style={{ height: CHART_HEIGHT }}>
        Waiting for bars for {props.symbol}…
      </div>
    );
  }

  return <div ref={containerRef} className="super-chart" />;
}
