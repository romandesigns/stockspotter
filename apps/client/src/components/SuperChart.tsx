// The reusable chart component from the Artifact prototype
// (stockspotter-super-chart-prototype memory) — this is meant to BE that
// component, ported as directly as the prototype's own architecture
// allows, not a smaller React-flavored reimplementation of pieces of it.
// mountSuperChart()/CHART_PRESETS was always the actual reuse mechanism;
// this file (plus ChartPanel.tsx wiring real data into it) is that same
// idea in real code for the `scanner` context specifically, per Roman's
// explicit correction after the first two rounds only ported fragments
// (overlays/MACD/tooltip, then timeframe pills) without the header, full
// toolbar, or momentum panel that made it recognizably the same chart.
//
// Real per-piece notes:
// - Overlays (MA9/MA20/VWAP), the MACD pane, the Indicators popover, and
//   the crosshair OHLCV tooltip are ports of the prototype's already-
//   validated design and math (chartIndicators.ts).
// - 1m/5m/15m timeframe pills are wired (client-side resample — see
//   chartIndicators.ts's resample()). 1D is present but disabled: it
//   needs a new daily-bar data source (the live feed only ever sends
//   1-minute bars), real new backend scope rather than a resample of
//   what we already have — shown, not silently omitted, so the gap is
//   visible rather than hidden.
// - Settings popover: autoScale/scaleMode/fitIndicators are real,
//   wired the same way the prototype's own bug-fixed version does
//   (autoscaleInfoProvider as an *option*, not a method — see its own
//   comment below). Symbol markers, extended-hours filtering, and
//   session-highlight shading are NOT ported yet — real remaining scope,
//   deliberately deferred rather than the whole popover being skipped.
// - Momentum panel: real momentum_scorer::MomentumScore data (the
//   MomentumUpdate ScanEvent, already broadcasting live) — NOT the
//   prototype's version, which was static demo copy ("84 / Strong
//   Bullish", "Structure intact since 9:41" were never computed from a
//   formula, confirmed by reading its source). Our thresholds/labels are
//   our own (momentumLabel.ts), grounded in the one real tuned number
//   that exists (DEFAULT_QUALIFY_THRESHOLD=0.60).
// - Alerts button is a placeholder — matching the prototype's own status,
//   not a shortcut: no alert-line feature exists there either.
// - Still genuinely deferred: the MACD pane's drag-resize handle (fixed
//   0.78 boundary), and the `backtest`/`watchlist` CHART_PRESETS contexts
//   (only a live single-symbol view exists in the real app so far).
import { useEffect, useMemo, useRef, useState } from "react";
import {
  ColorType,
  CrosshairMode,
  PriceScaleMode,
  createChart,
  type IChartApi,
  type ISeriesApi,
  type MouseEventParams,
  type UTCTimestamp,
} from "lightweight-charts";
import type { MomentumUpdate } from "@stockspotter/shared-types";
import type { CandleBar } from "../lib/derive";
import { computeMACD, resample, sma, vwap } from "../lib/chartIndicators";
import { factorGood, momentumLabel } from "../lib/momentumLabel";

const TIMEFRAMES = [1, 5, 15] as const;
type Timeframe = (typeof TIMEFRAMES)[number];
type ScaleMode = "linear" | "percent" | "log";

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

export function SuperChart(props: { symbol: string; bars: CandleBar[]; momentum: MomentumUpdate | null }) {
  const panelRef = useRef<HTMLDivElement>(null);
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
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [tooltip, setTooltip] = useState<Tooltip | null>(null);
  const [autoScale, setAutoScale] = useState(true);
  const [scaleMode, setScaleMode] = useState<ScaleMode>("linear");
  const [fitIndicators, setFitIndicators] = useState(true);
  const [isFullscreen, setIsFullscreen] = useState(false);
  // Where the "instrument zone" backdrop starts (fraction of chart height,
  // 0..1) -- tracked as its own state rather than derived fresh from
  // `visible.macd` each render, because mount-time margins and the
  // Indicators popover's runtime toggle use two different formulas (see
  // MACD_TOGGLE_MARGINS's doc comment) ported faithfully from the
  // prototype, not unified into one.
  const [instrumentZoneTop, setInstrumentZoneTop] = useState(paneMargins(true).vol.top);
  const [timeframe, setTimeframe] = useState<Timeframe>(1);

  // Resampled once per bars/timeframe change, not inline in the data
  // effect below -- keeps that effect's own dependency list honest (it
  // reacts to the resampled bars, not the raw 1-minute ones) and avoids
  // recomputing resample() again on every unrelated re-render (indicator
  // toggles, popover open/close).
  const displayBars = useMemo(() => resample(props.bars, timeframe), [props.bars, timeframe]);

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
  // the current (possibly resampled) bars. Cheap at the per-symbol cap of
  // 500 1-minute bars (see useRealtimeFeed's MAX_BARS_PER_SYMBOL), and
  // resampling only shrinks that count further; a real per-tick
  // `.update()` path for a still-forming candle can come later.
  useEffect(() => {
    const series = seriesRef.current;
    if (!series) return;
    const bars = displayBars;

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
  }, [displayBars]);

  // Settings popover — real behavior, ported the same way the prototype's
  // own (bug-fixed) version does it.
  useEffect(() => {
    chartRef.current?.priceScale("right").applyOptions({ autoScale });
  }, [autoScale]);

  useEffect(() => {
    const mode =
      scaleMode === "log" ? PriceScaleMode.Logarithmic : scaleMode === "percent" ? PriceScaleMode.Percentage : PriceScaleMode.Normal;
    chartRef.current?.priceScale("right").applyOptions({ mode });
  }, [scaleMode]);

  useEffect(() => {
    // "Fit all indicators" — only the overlays sharing the main price
    // scale (MA9/MA20/VWAP); MACD lives on its own pane/scale and always
    // fits itself, that's not optional the way sharing the price axis is.
    // `autoscaleInfoProvider` is a series *option* (set via applyOptions),
    // not a method — the prototype's own history found
    // `series.setAutoscaleInfoProvider()` isn't real on v4.1.3's API and
    // throws, which (since it ran synchronously during setup there) took
    // out every wire-up after it.
    const series = seriesRef.current;
    if (!series) return;
    for (const s of [series.ma9, series.ma20, series.vwap]) {
      s.applyOptions({
        autoscaleInfoProvider: (original: () => unknown) => (fitIndicators ? original() : null),
      });
    }
    if (autoScale) chartRef.current?.priceScale("right").applyOptions({ autoScale: true });
  }, [fitIndicators, autoScale]);

  useEffect(() => {
    function onFullscreenChange() {
      setIsFullscreen(document.fullscreenElement === panelRef.current);
    }
    document.addEventListener("fullscreenchange", onFullscreenChange);
    return () => document.removeEventListener("fullscreenchange", onFullscreenChange);
  }, []);

  // Close either popover on an outside click -- registered once (mount-
  // time deps), always closes both unconditionally on an outside click
  // rather than reading current open-state, which is a harmless no-op
  // when already closed and avoids re-registering the listener on every
  // popover toggle.
  useEffect(() => {
    function onDocumentMouseDown(e: MouseEvent) {
      const target = e.target as HTMLElement;
      if (target.closest(".chart-popover-anchor")) return;
      setPopoverOpen(false);
      setSettingsOpen(false);
    }
    document.addEventListener("mousedown", onDocumentMouseDown);
    return () => document.removeEventListener("mousedown", onDocumentMouseDown);
  }, []);

  function toggleFullscreen() {
    if (document.fullscreenElement) {
      document.exitFullscreen();
    } else {
      panelRef.current?.requestFullscreen();
    }
  }

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

  // Header price/change -- from the full raw bar history, not whatever
  // timeframe pill is currently selected, so switching 1m/5m/15m doesn't
  // make the header number jump around. Matches the prototype's own
  // convention (it used the full un-resampled dataset for this too).
  const firstBar = props.bars[0];
  const lastBar = props.bars[props.bars.length - 1];
  const headerPrice = lastBar.close;
  const headerChangePct = firstBar.open !== 0 ? ((lastBar.close - firstBar.open) / firstBar.open) * 100 : 0;
  const headerUp = headerChangePct >= 0;

  return (
    <div className="super-chart-panel" ref={panelRef}>
      <div className="chart-header">
        <div className="ticker-head">
          <span className="ticker chart-ticker-symbol">{props.symbol}</span>
        </div>
        <div className="chart-header-spacer" />
        <span className="price chart-ticker-price">${headerPrice.toFixed(headerPrice < 1 ? 4 : 2)}</span>
        <span className={headerUp ? "pct-up" : "pct-down"}>
          {headerUp ? "▲" : "▼"} {headerUp ? "+" : ""}
          {headerChangePct.toFixed(1)}%
        </span>
      </div>

      <div className="chart-toolbar">
        <div className="chart-pill-group">
          {TIMEFRAMES.map((tf) => (
            <button
              key={tf}
              type="button"
              className="chart-pill"
              aria-pressed={timeframe === tf}
              onClick={() => setTimeframe(tf)}
            >
              {tf}m
            </button>
          ))}
          <button
            type="button"
            className="chart-pill"
            disabled
            title="Needs daily-bar data — the live feed only sends 1-minute bars, not built yet"
          >
            1D
          </button>
        </div>
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
        <div className="chart-toolbar-spacer" />
        <div className="chart-popover-anchor">
          <button
            type="button"
            className="chart-icon-btn"
            aria-haspopup="true"
            aria-expanded={settingsOpen}
            onClick={() => setSettingsOpen((v) => !v)}
          >
            Settings
          </button>
          {settingsOpen && (
            <div className="chart-popover chart-settings-popover">
              <div className="chart-popover-title">Auto-scale</div>
              <SettingSwitch label="Auto-scale price axis" checked={autoScale} onToggle={() => setAutoScale((v) => !v)} />
              <div className="chart-popover-divider" />
              <div className="chart-popover-title">Fit to chart</div>
              <SettingSwitch label="Fit all indicators" checked={fitIndicators} onToggle={() => setFitIndicators((v) => !v)} />
              <div className="chart-popover-divider" />
              <div className="chart-popover-title">Scaling</div>
              <ScaleRadio label="Linear (Price)" active={scaleMode === "linear"} onSelect={() => setScaleMode("linear")} />
              <ScaleRadio label="Linear (Percentage)" active={scaleMode === "percent"} onSelect={() => setScaleMode("percent")} />
              <ScaleRadio label="Logarithmic (Price)" active={scaleMode === "log"} onSelect={() => setScaleMode("log")} />
            </div>
          )}
        </div>
        <button type="button" className="chart-icon-btn" disabled title="Coming soon — no alert-line feature exists yet">
          Alerts
        </button>
        <button type="button" className="chart-icon-btn" onClick={toggleFullscreen}>
          {isFullscreen ? "Exit full screen" : "Full screen"}
        </button>
      </div>

      <div className="super-chart-mount">
        <div ref={containerRef} className="super-chart" />
        <div className="chart-instrument-bg" style={{ top: `${instrumentZoneTop * 100}%` }} />
        {tooltip && <ChartTooltip tooltip={tooltip} />}
      </div>

      <MomentumScoreRow momentum={props.momentum} />
    </div>
  );
}

function SettingSwitch(props: { label: string; checked: boolean; onToggle: () => void }) {
  return (
    <button type="button" className="chart-switch-row" role="switch" aria-checked={props.checked} onClick={props.onToggle}>
      <span>{props.label}</span>
      <span className="switch-track">
        <span className="switch-thumb" />
      </span>
    </button>
  );
}

function ScaleRadio(props: { label: string; active: boolean; onSelect: () => void }) {
  return (
    <button type="button" className="chart-radio-row" role="radio" aria-checked={props.active} onClick={props.onSelect}>
      <span className="chart-radio-dot" />
      {props.label}
    </button>
  );
}

/**
 * Real momentum_scorer::MomentumScore data (the MomentumUpdate ScanEvent),
 * NOT the prototype's version -- that was static demo copy ("84 / Strong
 * Bullish", "Structure intact since 9:41" were never computed from a
 * formula, confirmed by reading its source). Detail lines show the real
 * per-factor score since we don't have the prototype's narrative-text
 * generation and won't fabricate one.
 */
function MomentumScoreRow(props: { momentum: MomentumUpdate | null }) {
  const m = props.momentum;
  if (!m) {
    return (
      <div className="score-row">
        <div className="score-empty">No momentum reading yet for this symbol…</div>
      </div>
    );
  }
  const scoreValue = Math.round(m.overall * 100);
  const scoreColor = m.overall >= 0.6 ? "#0ca30c" : m.overall >= 0.4 ? "#fab219" : "#d03b3b";
  return (
    <div className="score-row">
      <div className="score-badge" title="Composite of volume confirmation, HH/HL structure, MA slope and wick rejection — weighted toward volume, the strongest signal">
        <div className="score-value" style={{ color: scoreColor }}>{scoreValue}</div>
        <div className="score-caption">{momentumLabel(m.overall)}</div>
        <div className="score-sub">Momentum score</div>
      </div>
      <div className="factors-list">
        <FactorRow label="Volume confirmation" score={m.volumeConfirmation} />
        <FactorRow label="Higher highs / higher lows" score={m.structure} />
        <FactorRow label="MA slope" score={m.maSlope} />
        <FactorRow label="Rejection wicks" score={m.wickRejection} />
      </div>
    </div>
  );
}

function FactorRow(props: { label: string; score: number }) {
  const good = factorGood(props.score);
  return (
    <div className={`factor-row ${good ? "factor-row-good" : "factor-row-warning"}`}>
      <span className="factor-row-icon">{good ? "✓" : "!"}</span>
      <div className="factor-row-body">
        <div className="factor-row-title">{props.label}</div>
        <div className="factor-row-detail">score {props.score.toFixed(2)}</div>
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
