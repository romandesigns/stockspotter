// The reusable chart component from the Artifact prototype
// (stockspotter-super-chart-prototype memory), ported into real code.
// Overlays (MA9/MA20/VWAP), the MACD pane, the Indicators popover, and
// the crosshair OHLCV tooltip are real ports of that prototype's already-
// validated design and math (chartIndicators.ts), not a reinvention.
//
// Still deliberately deferred, left for the next real design pass:
// timeframe pills (5m/15m/1D need either client-side resampling of the
// accumulated 1-minute bars, or a new daily-bar data source — genuine new
// scope, not just a port), the Settings popover (extended hours/session
// shading/scale mode), playback/replay controls (not applicable to a live
// view), and the MACD pane's drag-resize handle (kept at the prototype's
// fixed 0.78 boundary for now). Only one context exists so far too — the
// prototype's `scanner`/`backtest`/`watchlist` CHART_PRESETS split hasn't
// been ported since only a live single-symbol view exists in the real app
// yet.
import { useEffect, useRef, useState } from "react";
import {
  ColorType,
  CrosshairMode,
  createChart,
  type IChartApi,
  type ISeriesApi,
  type MouseEventParams,
  type UTCTimestamp,
} from "lightweight-charts";
import type { CandleBar } from "../lib/derive";
import { computeMACD, sma, vwap } from "../lib/chartIndicators";

const CHART_HEIGHT = 420;

// Same fixed order/colors as the prototype's --series-1..5 (CVD-validated
// there, see stockspotter-super-chart-prototype memory) — never reused for
// a different meaning elsewhere in the app.
const UP_COLOR = "#0ca30c";
const DOWN_COLOR = "#d03b3b";
const MA9_COLOR = "#3987e5";
const MA20_COLOR = "#d95926";
const VWAP_COLOR = "#9085e9";
const MACD_LINE_COLOR = "#c98500";
const MACD_SIGNAL_COLOR = "#d55181";
const TEXT_SECONDARY = "#9ca3af";
const GRID_COLOR = "#2e303a";

// MACD needs its own price scale (its values are nowhere near price's
// magnitude) so it keeps a real separate band at a fixed boundary — ported
// from the prototype's `macdBoundary`, minus the drag-resize interactivity
// (deferred, see the doc comment above).
const MACD_BOUNDARY = 0.78;

type IndicatorKey = "ma9" | "ma20" | "vwap" | "macd";

interface IndicatorSeries {
  candles: ISeriesApi<"Candlestick">;
  volume: ISeriesApi<"Histogram">;
  ma9: ISeriesApi<"Line">;
  ma20: ISeriesApi<"Line">;
  vwap: ISeriesApi<"Line">;
  macdHist: ISeriesApi<"Histogram">;
  macdLine: ISeriesApi<"Line">;
  macdSignal: ISeriesApi<"Line">;
}

interface Tooltip {
  x: number;
  y: number;
  time: number;
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number;
}

function paneMargins(macdOn: boolean) {
  const priceBottom = macdOn ? 1 - MACD_BOUNDARY : 0.05;
  // Volume is a thin translucent strip anchored to the price region's
  // actual bottom edge (20% of chart height), not a hardcoded fraction —
  // see the prototype's own bug-fix comment on why a fixed 0.78 either
  // collapsed or misplaced the band depending on whether MACD was on.
  const volBand = 0.2;
  const volTop = Math.max(0.05, 1 - priceBottom - volBand);
  return {
    price: { top: 0.05, bottom: priceBottom },
    vol: { top: volTop, bottom: priceBottom },
    macd: { top: MACD_BOUNDARY, bottom: 0.02 },
  };
}

// The Indicators toggle handler's own fallback margins when MACD is
// switched on/off at runtime — ported verbatim from the prototype rather
// than recomputed via paneMargins(), since that's literally what was
// tuned there.
const MACD_TOGGLE_MARGINS = {
  on: { price: { top: 0.05, bottom: 0.4 }, vol: { top: 0.84, bottom: 0 } },
  off: { price: { top: 0.08, bottom: 0.28 }, vol: { top: 0.78, bottom: 0 } },
};

export function SuperChart(props: { symbol: string; bars: CandleBar[] }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<IChartApi | null>(null);
  const seriesRef = useRef<IndicatorSeries | null>(null);
  const [visible, setVisible] = useState<Record<IndicatorKey, boolean>>({
    ma9: true,
    ma20: true,
    vwap: true,
    macd: true,
  });
  const [popoverOpen, setPopoverOpen] = useState(false);
  const [tooltip, setTooltip] = useState<Tooltip | null>(null);
  // Where the "instrument zone" backdrop starts (fraction of chart height,
  // 0..1) -- tracked as its own state rather than derived fresh from
  // `visible.macd` each render, because mount-time margins and the
  // Indicators popover's runtime toggle use two different formulas (see
  // MACD_TOGGLE_MARGINS's doc comment) ported faithfully from the
  // prototype, not unified into one.
  const [instrumentZoneTop, setInstrumentZoneTop] = useState(paneMargins(true).vol.top);

  // Mount the chart once. Series/options changes below react to prop and
  // state changes without tearing the chart instance down and rebuilding.
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const chart = createChart(container, {
      width: container.clientWidth,
      height: CHART_HEIGHT,
      layout: {
        background: { type: ColorType.Solid, color: "transparent" },
        textColor: TEXT_SECONDARY,
        fontSize: 11,
      },
      grid: {
        vertLines: { visible: false },
        horzLines: { color: GRID_COLOR },
      },
      rightPriceScale: { borderVisible: false, scaleMargins: paneMargins(true).price },
      timeScale: { borderVisible: false, timeVisible: true, secondsVisible: false, rightOffset: 2 },
      crosshair: { mode: CrosshairMode.Normal },
    });

    // Volume added before candles so it draws *behind* them (lightweight-
    // charts layers series in add-order) -- an overlay wash under the
    // candles rather than competing on top of them.
    const volumeSeries = chart.addHistogramSeries({
      priceFormat: { type: "volume" },
      priceScaleId: "vol",
      priceLineVisible: false,
      lastValueVisible: false,
    });
    chart.priceScale("vol").applyOptions({ scaleMargins: paneMargins(true).vol });

    const candleSeries = chart.addCandlestickSeries({
      upColor: UP_COLOR,
      downColor: DOWN_COLOR,
      borderVisible: false,
      wickUpColor: UP_COLOR,
      wickDownColor: DOWN_COLOR,
    });

    const ma9Series = chart.addLineSeries({
      color: MA9_COLOR,
      lineWidth: 2,
      priceLineVisible: false,
      lastValueVisible: false,
      crosshairMarkerVisible: false,
    });
    const ma20Series = chart.addLineSeries({
      color: MA20_COLOR,
      lineWidth: 2,
      priceLineVisible: false,
      lastValueVisible: false,
      crosshairMarkerVisible: false,
    });
    const vwapSeries = chart.addLineSeries({
      color: VWAP_COLOR,
      lineWidth: 2,
      lineStyle: 2, // Dashed
      priceLineVisible: false,
      lastValueVisible: false,
      crosshairMarkerVisible: false,
    });

    // Series must exist on the 'macd' scale BEFORE margins can be applied
    // to it -- priceScale('macd') is created lazily by the first series
    // added to it. Getting this order backwards was a real bug in the
    // prototype's own history (MACD painting across the whole chart).
    const macdHistSeries = chart.addHistogramSeries({
      priceScaleId: "macd",
      priceLineVisible: false,
      lastValueVisible: false,
    });
    const macdLineSeries = chart.addLineSeries({
      priceScaleId: "macd",
      color: MACD_LINE_COLOR,
      lineWidth: 1,
      priceLineVisible: false,
      lastValueVisible: false,
      crosshairMarkerVisible: false,
    });
    const macdSignalSeries = chart.addLineSeries({
      priceScaleId: "macd",
      color: MACD_SIGNAL_COLOR,
      lineWidth: 1,
      lineStyle: 2, // Dashed
      priceLineVisible: false,
      lastValueVisible: false,
      crosshairMarkerVisible: false,
    });
    chart.priceScale("macd").applyOptions({ scaleMargins: paneMargins(true).macd });

    chartRef.current = chart;
    seriesRef.current = {
      candles: candleSeries,
      volume: volumeSeries,
      ma9: ma9Series,
      ma20: ma20Series,
      vwap: vwapSeries,
      macdHist: macdHistSeries,
      macdLine: macdLineSeries,
      macdSignal: macdSignalSeries,
    };

    chart.subscribeCrosshairMove((param: MouseEventParams) => {
      const series = seriesRef.current;
      if (!series || !param.point || !param.time) {
        setTooltip(null);
        return;
      }
      const bar = param.seriesData.get(series.candles) as
        | { open: number; high: number; low: number; close: number }
        | undefined;
      if (!bar) {
        setTooltip(null);
        return;
      }
      const volPoint = param.seriesData.get(series.volume) as { value: number } | undefined;

      // Offset from the cursor, flipped toward the opposite edge if it'd
      // overflow the container -- ported from the prototype's tooltip
      // positioning, simplified to an estimated tooltip size rather than
      // measuring the actual rendered DOM node (a real ref-based
      // measurement can replace this if the estimate ever looks wrong).
      const cw = container.clientWidth;
      const ch = container.clientHeight;
      const estW = 220;
      const estH = 30;
      let x = param.point.x + 14;
      let y = param.point.y + 14;
      if (x + estW > cw) x = param.point.x - estW - 14;
      if (y + estH > ch) y = param.point.y - estH - 14;

      setTooltip({
        x: Math.max(4, x),
        y: Math.max(4, y),
        time: param.time as number,
        open: bar.open,
        high: bar.high,
        low: bar.low,
        close: bar.close,
        volume: volPoint?.value ?? 0,
      });
    });

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
      seriesRef.current = null;
    };
    // Intentionally mount-once: bars/visibility are handled by the effects
    // below via setData/applyOptions, not by remounting the whole chart.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Real data in -- candles, volume, and every indicator recomputed from
  // the current bars. Cheap at the per-symbol cap of 500 bars (see
  // useRealtimeFeed's MAX_BARS_PER_SYMBOL); a real per-tick `.update()`
  // path for a still-forming candle can come later.
  useEffect(() => {
    const series = seriesRef.current;
    if (!series) return;
    const bars = props.bars;

    series.candles.setData(
      bars.map((b) => ({ time: b.time as UTCTimestamp, open: b.open, high: b.high, low: b.low, close: b.close })),
    );
    series.volume.setData(
      bars.map((b, i) => {
        const up = i === 0 || b.close >= bars[i - 1].close;
        return { time: b.time as UTCTimestamp, value: b.volume, color: up ? "rgba(12,163,12,.38)" : "rgba(208,59,59,.38)" };
      }),
    );
    series.ma9.setData(sma(bars, 9).map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
    series.ma20.setData(sma(bars, 20).map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
    series.vwap.setData(vwap(bars).map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
    const macd = computeMACD(bars);
    series.macdHist.setData(macd.hist.map((p) => ({ time: p.time as UTCTimestamp, value: p.value, color: p.color })));
    series.macdLine.setData(macd.macdLine.map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
    series.macdSignal.setData(macd.signalLine.map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));

    chartRef.current?.timeScale().fitContent();
  }, [props.bars]);

  function toggleIndicator(key: IndicatorKey) {
    const series = seriesRef.current;
    const chart = chartRef.current;
    if (!series || !chart) return;
    const nowOn = !visible[key];
    setVisible((prev) => ({ ...prev, [key]: nowOn }));

    if (key === "macd") {
      series.macdHist.applyOptions({ visible: nowOn });
      series.macdLine.applyOptions({ visible: nowOn });
      series.macdSignal.applyOptions({ visible: nowOn });
      const m = nowOn ? MACD_TOGGLE_MARGINS.on : MACD_TOGGLE_MARGINS.off;
      chart.priceScale("right").applyOptions({ scaleMargins: m.price });
      chart.priceScale("vol").applyOptions({ scaleMargins: m.vol });
      setInstrumentZoneTop(m.vol.top);
    } else {
      series[key].applyOptions({ visible: nowOn });
    }
  }

  if (props.bars.length === 0) {
    return (
      <div className="super-chart-empty" style={{ height: CHART_HEIGHT }}>
        Waiting for bars for {props.symbol}…
      </div>
    );
  }

  return (
    <div className="super-chart-panel">
      <div className="chart-toolbar">
        <div className="chart-popover-anchor">
          <button
            type="button"
            className="chart-icon-btn"
            aria-haspopup="true"
            aria-expanded={popoverOpen}
            onClick={() => setPopoverOpen((v) => !v)}
          >
            Indicators
          </button>
          {popoverOpen && (
            <div className="chart-popover">
              <IndicatorSwitch label="MA9" color={MA9_COLOR} checked={visible.ma9} onToggle={() => toggleIndicator("ma9")} />
              <IndicatorSwitch label="MA20" color={MA20_COLOR} checked={visible.ma20} onToggle={() => toggleIndicator("ma20")} />
              <IndicatorSwitch label="VWAP" color={VWAP_COLOR} checked={visible.vwap} onToggle={() => toggleIndicator("vwap")} />
              <IndicatorSwitch label="MACD" color={MACD_LINE_COLOR} checked={visible.macd} onToggle={() => toggleIndicator("macd")} />
            </div>
          )}
        </div>
      </div>
      <div className="super-chart-mount">
        <div ref={containerRef} className="super-chart" />
        <div className="chart-instrument-bg" style={{ top: `${instrumentZoneTop * 100}%` }} />
        {tooltip && <ChartTooltip tooltip={tooltip} />}
      </div>
    </div>
  );
}

function IndicatorSwitch(props: { label: string; color: string; checked: boolean; onToggle: () => void }) {
  return (
    <button type="button" className="chart-switch-row" role="switch" aria-checked={props.checked} onClick={props.onToggle}>
      <span className="swatch" style={{ background: props.color }} />
      <span>{props.label}</span>
      <span className="switch-track">
        <span className="switch-thumb" />
      </span>
    </button>
  );
}

function fmtVol(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return `${n}`;
}

function ChartTooltip(props: { tooltip: Tooltip }) {
  const { tooltip } = props;
  const up = tooltip.close >= tooltip.open;
  const d = new Date(tooltip.time * 1000);
  const hh = String(d.getUTCHours()).padStart(2, "0");
  const mm = String(d.getUTCMinutes()).padStart(2, "0");
  return (
    <div className="super-chart-tooltip" style={{ left: tooltip.x, top: tooltip.y }}>
      <span className="dim">
        {hh}:{mm} UTC
      </span>
      <span>
        O <b>{tooltip.open.toFixed(2)}</b>
      </span>
      <span>
        H <b>{tooltip.high.toFixed(2)}</b>
      </span>
      <span>
        L <b>{tooltip.low.toFixed(2)}</b>
      </span>
      <span style={{ color: up ? "#0ca30c" : "#d03b3b" }}>
        C <b>{tooltip.close.toFixed(2)}</b>
      </span>
      <span className="dim">Vol {fmtVol(tooltip.volume)}</span>
    </div>
  );
}
