// This IS the Super Chart engine from the Artifact prototype
// (stockspotter-super-chart-prototype memory) — mountSuperChart(),
// wireChartTooltip(), and CHART_PRESETS, ported as close to verbatim as
// TypeScript requires. This is not a reimplementation: it's the same
// function bodies, same pane-margins formula (with its own bug-fix
// comments preserved from the prototype's real history), same series
// creation calls/options, same api.setBars() update path, same tooltip
// positioning/measurement. If you want to check this against the
// original, the prototype's own <script> block is the source of truth —
// diff this file's mountSuperChart against its mountSuperChart.
//
// Framework-agnostic on purpose, exactly like the original: operates
// directly on a DOM element via lightweight-charts' imperative API, no
// React state in this file at all. SuperChart.tsx is the thin wrapper
// that calls into this from a mount effect — React owns the toolbar/
// popover UI around the chart, this file owns the chart itself, same
// division the prototype had between mountSuperChart() and its
// page-level scenario wiring.
//
// sma/vwap/computeMACD/resample live in chartIndicators.ts (already a
// verbatim port of the same math, kept there rather than duplicated
// here) — everything else the engine itself needs is in this file.

import {
  ColorType,
  createChart,
  CrosshairMode,
  LineStyle,
  type IChartApi,
  type ISeriesApi,
  type UTCTimestamp,
} from "lightweight-charts";
import type { CandleBar } from "./derive";
import { computeMACD, sma, vwap } from "./chartIndicators";

// ---------- design tokens, read live from the real app's CSS custom
// properties -- same tok()-reads-getComputedStyle(documentElement)
// pattern the prototype used, pointed at our own :root tokens (index.css)
// instead of a second hardcoded copy of the same values. ----------------
function tok(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}
function readColors() {
  return {
    textSecondary: tok("--text") || "#9ca3af",
    textMuted: tok("--text") || "#9ca3af",
    border: tok("--border") || "#2e303a",
    good: tok("--good") || "#0ca30c",
    critical: tok("--critical") || "#d03b3b",
    s1: tok("--series-1") || "#3987e5",
    s2: tok("--series-2") || "#d95926",
    s3: tok("--series-3") || "#9085e9",
    s4: tok("--series-4") || "#c98500",
    s5: tok("--series-5") || "#d55181",
  };
}

export interface ChartPreset {
  mode: "full" | "compact";
  showVolume: boolean;
  overlays: boolean;
  macd: boolean;
  resizable: boolean;
  height: number;
}

// Same three contexts the prototype defined. `backtest`/`watchlist` are
// carried over for when those contexts get wired to real data (see
// stockspotter-open-tasks memory) -- only `scanner` is used in the app
// today.
export const CHART_PRESETS: Record<string, ChartPreset> = {
  scanner: { mode: "full", showVolume: true, overlays: true, macd: true, resizable: true, height: 380 },
  backtest: { mode: "full", showVolume: true, overlays: true, macd: false, resizable: false, height: 340 },
  watchlist: { mode: "compact", showVolume: false, overlays: false, macd: false, resizable: false, height: 52 },
};

export interface SuperChartApi {
  chart: IChartApi;
  series: {
    area?: ISeriesApi<"Area">;
    volume?: ISeriesApi<"Histogram">;
    candles?: ISeriesApi<"Candlestick">;
    ma9?: ISeriesApi<"Line">;
    ma20?: ISeriesApi<"Line">;
    vwap?: ISeriesApi<"Line">;
    macdHist?: ISeriesApi<"Histogram">;
    macdLine?: ISeriesApi<"Line">;
    macdSignal?: ISeriesApi<"Line">;
  };
  setBars: (bars: CandleBar[]) => void;
  /** Torn down by SuperChart.tsx's unmount cleanup -- disconnects the
   * ResizeObserver and removes the drag-handle/backdrop DOM nodes this
   * function created on `el`, which `chart.remove()` alone doesn't do. */
  destroy: () => void;
}

export function mountSuperChart(
  el: HTMLElement,
  context: keyof typeof CHART_PRESETS,
  instanceOpts: { bars: CandleBar[]; height?: number },
): SuperChartApi {
  const preset = CHART_PRESETS[context];
  if (!preset) throw new Error(`mountSuperChart: unknown context "${context}" — add it to CHART_PRESETS`);
  const opts = { ...preset, ...instanceOpts };
  const mode = opts.mode || "full";
  const COLOR = readColors();

  // Volume is an overlay, not its own band: price fills (almost) the
  // whole pane, and volume is confined to a thin translucent strip at
  // the same bottom edge, sharing space with the lower candles rather
  // than claiming exclusive height. MACD can't share the price scale
  // (its values are nowhere near price's magnitude) so it keeps a real
  // separate band — macdBoundary is the one remaining resizable split.
  let macdBoundary = 0.78;
  function paneMargins() {
    const priceBottom = opts.macd ? 1 - macdBoundary : 0.05;
    // Volume's top was previously a hardcoded 0.78 — a leftover from the
    // old fixed-band design that didn't account for where the price
    // region actually ends now. Since price's plot area runs from 5% to
    // (1-priceBottom), a fixed 0.78 either collapsed the volume band to
    // near-zero height (when priceBottom made 1-priceBottom ≈ 0.78) or
    // put it in the wrong spot entirely. Anchor it relative to the
    // price region's actual bottom edge instead: a fixed-height strip
    // (20% of total chart height) sitting right at that edge.
    const volBand = 0.2;
    const volTop = Math.max(0.05, 1 - priceBottom - volBand);
    return {
      price: { top: 0.05, bottom: priceBottom },
      vol: { top: volTop, bottom: priceBottom },
      macd: { top: macdBoundary, bottom: 0.02 },
    };
  }
  function applyPaneMargins() {
    const m = paneMargins();
    chart.priceScale("right").applyOptions({ scaleMargins: m.price });
    if (opts.showVolume) chart.priceScale("vol").applyOptions({ scaleMargins: m.vol });
    if (opts.macd) chart.priceScale("macd").applyOptions({ scaleMargins: m.macd });
    renderInstrumentBg();
  }
  let positionHandles: (() => void) | null = null; // set below only when opts.resizable
  let handleEl: HTMLDivElement | null = null;
  let activeDragCleanup: (() => void) | null = null; // set only while a drag is in progress

  // A subtle backdrop behind the volume+MACD zone (from wherever volume's
  // band starts down to the bottom). Fixing the margin math alone wasn't
  // enough on wide-range days — nothing told the eye "this is the
  // instrument zone" vs. "this is price," so when a candle wick dipped
  // down that far everything still read as one tangled cluster. A color
  // change at the boundary makes that read clearly regardless of where
  // price's own range happens to reach.
  let instrumentBg: HTMLDivElement | null = null;
  function renderInstrumentBg() {
    if (!opts.showVolume) return;
    if (!instrumentBg) {
      instrumentBg = document.createElement("div");
      instrumentBg.className = "chart-instrument-bg";
      el.appendChild(instrumentBg);
    }
    instrumentBg.style.top = `${paneMargins().vol.top * el.clientHeight}px`;
  }

  const chart = createChart(el, {
    width: el.clientWidth,
    height: opts.height || el.clientHeight,
    layout: {
      background: { type: ColorType.Solid, color: "transparent" },
      textColor: COLOR.textSecondary,
      fontFamily: getComputedStyle(document.body).fontFamily,
      fontSize: 11,
    },
    grid: {
      vertLines: { visible: false },
      horzLines: { visible: mode === "full", color: COLOR.border },
    },
    rightPriceScale: { visible: mode === "full", borderVisible: false, scaleMargins: paneMargins().price },
    timeScale: { visible: mode === "full", borderVisible: false, timeVisible: true, secondsVisible: false, rightOffset: 2 },
    crosshair: {
      mode: mode === "full" ? CrosshairMode.Normal : CrosshairMode.Magnet,
      vertLine: { visible: mode === "full", labelVisible: mode === "full", color: COLOR.textMuted, style: LineStyle.Solid, width: 1 },
      horzLine: { visible: mode === "full", labelVisible: mode === "full", color: COLOR.textMuted, style: LineStyle.Solid, width: 1 },
    },
    handleScroll: mode === "full",
    handleScale: mode === "full",
  });

  const api: SuperChartApi = { chart, series: {}, setBars: () => {}, destroy: () => {} };

  if (mode === "compact") {
    const dir = opts.bars[opts.bars.length - 1].close >= opts.bars[0].close;
    const lineColor = dir ? COLOR.good : COLOR.critical;
    const area = chart.addAreaSeries({
      lineColor,
      lineWidth: 2,
      topColor: dir ? "rgba(12,163,12,.22)" : "rgba(208,59,59,.22)",
      bottomColor: "rgba(0,0,0,0)",
      priceLineVisible: false,
      lastValueVisible: false,
      crosshairMarkerVisible: false,
    });
    area.setData(opts.bars.map((b) => ({ time: b.time as UTCTimestamp, value: b.close })));
    api.series.area = area;
  } else {
    // Volume added before candles so it draws *behind* them (lightweight-
    // charts layers series in add-order) — that's what makes it read as
    // an overlay wash under the candles instead of competing on top.
    if (opts.showVolume) {
      const vol = chart.addHistogramSeries({ priceFormat: { type: "volume" }, priceScaleId: "vol", color: COLOR.good });
      vol.setData(
        opts.bars.map((b, i) => {
          const up = i === 0 || b.close >= opts.bars[i - 1].close;
          return { time: b.time as UTCTimestamp, value: b.volume, color: up ? "rgba(12,163,12,.38)" : "rgba(208,59,59,.38)" };
        }),
      );
      api.series.volume = vol;
    }

    const candles = chart.addCandlestickSeries({
      upColor: COLOR.good,
      downColor: COLOR.critical,
      borderVisible: false,
      wickUpColor: COLOR.good,
      wickDownColor: COLOR.critical,
    });
    candles.setData(opts.bars.map((b) => ({ time: b.time as UTCTimestamp, open: b.open, high: b.high, low: b.low, close: b.close })));
    api.series.candles = candles;

    if (opts.showVolume) chart.priceScale("vol").applyOptions({ scaleMargins: paneMargins().vol });

    if (opts.overlays) {
      api.series.ma9 = chart.addLineSeries({ color: COLOR.s1, lineWidth: 2, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });
      api.series.ma9.setData(sma(opts.bars, 9).map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
      api.series.ma20 = chart.addLineSeries({ color: COLOR.s2, lineWidth: 2, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });
      api.series.ma20.setData(sma(opts.bars, 20).map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
      api.series.vwap = chart.addLineSeries({ color: COLOR.s3, lineWidth: 2, lineStyle: LineStyle.Dashed, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });
      api.series.vwap.setData(vwap(opts.bars).map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
    }

    if (opts.macd) {
      // Series must exist on the 'macd' scale BEFORE we can apply margins
      // to it — priceScale('macd') is created lazily by the first series
      // that references it. Doing this in the other order means the
      // applyOptions call silently no-ops on a scale that doesn't exist
      // yet, so MACD renders at default (full-height) margins instead of
      // its own band — exactly the overlap bug the prototype's own
      // history found.
      const macd0 = computeMACD(opts.bars);
      api.series.macdHist = chart.addHistogramSeries({ priceScaleId: "macd", priceLineVisible: false, lastValueVisible: false });
      api.series.macdHist.setData(macd0.hist.map((p) => ({ time: p.time as UTCTimestamp, value: p.value, color: p.color })));
      api.series.macdLine = chart.addLineSeries({ priceScaleId: "macd", color: COLOR.s4, lineWidth: 1, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });
      api.series.macdLine.setData(macd0.macdLine.map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
      api.series.macdSignal = chart.addLineSeries({ priceScaleId: "macd", color: COLOR.s5, lineWidth: 1, lineStyle: LineStyle.Dashed, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });
      api.series.macdSignal.setData(macd0.signalLine.map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
      chart.priceScale("macd").applyOptions({ scaleMargins: paneMargins().macd });

      // Drag handle at the one remaining boundary (chart vs. MACD) — a
      // separate capability from macd itself, so it's its own preset flag.
      if (opts.resizable) {
        const handle = document.createElement("div");
        handle.className = "pane-handle";
        el.appendChild(handle);
        handleEl = handle;

        positionHandles = () => {
          handle.style.top = `${macdBoundary * el.clientHeight}px`;
        };
        positionHandles();

        handle.addEventListener("mousedown", (e) => {
          e.preventDefault();
          handle.classList.add("dragging");
          function onMove(ev: MouseEvent) {
            const rect = el.getBoundingClientRect();
            const frac = (ev.clientY - rect.top) / rect.height;
            macdBoundary = Math.max(0.35, Math.min(frac, 0.92));
            applyPaneMargins();
            positionHandles?.();
          }
          function onUp() {
            handle.classList.remove("dragging");
            document.removeEventListener("mousemove", onMove);
            document.removeEventListener("mouseup", onUp);
            activeDragCleanup = null;
          }
          document.addEventListener("mousemove", onMove);
          document.addEventListener("mouseup", onUp);
          // So destroy() can force-end an in-progress drag rather than
          // leaking these document-level listeners if the component
          // unmounts (symbol switch) mid-drag.
          activeDragCleanup = () => {
            document.removeEventListener("mousemove", onMove);
            document.removeEventListener("mouseup", onUp);
          };
        });
      }
    }
  }

  chart.timeScale().fitContent();
  renderInstrumentBg();

  api.setBars = (bars: CandleBar[]) => {
    if (mode === "compact") {
      api.series.area?.setData(bars.map((b) => ({ time: b.time as UTCTimestamp, value: b.close })));
    } else {
      api.series.candles?.setData(bars.map((b) => ({ time: b.time as UTCTimestamp, open: b.open, high: b.high, low: b.low, close: b.close })));
      if (api.series.volume) {
        api.series.volume.setData(
          bars.map((b, i) => {
            const up = i === 0 || b.close >= bars[i - 1].close;
            return { time: b.time as UTCTimestamp, value: b.volume, color: up ? "rgba(12,163,12,.38)" : "rgba(208,59,59,.38)" };
          }),
        );
      }
      if (api.series.ma9) api.series.ma9.setData(sma(bars, 9).map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
      if (api.series.ma20) api.series.ma20.setData(sma(bars, 20).map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
      if (api.series.vwap) api.series.vwap.setData(vwap(bars).map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
      if (api.series.macdLine && api.series.macdHist && api.series.macdSignal) {
        const macd1 = computeMACD(bars);
        api.series.macdHist.setData(macd1.hist.map((p) => ({ time: p.time as UTCTimestamp, value: p.value, color: p.color })));
        api.series.macdLine.setData(macd1.macdLine.map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
        api.series.macdSignal.setData(macd1.signalLine.map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
      }
    }
    chart.timeScale().fitContent();
  };

  const resizeObserver = new ResizeObserver(() => {
    // Both dimensions tracked -- not just width like the prototype's own
    // version needed. The prototype's demo pages never put a chart in a
    // height-constrained grid cell (only fixed-height sections), so its
    // own resize handling never had to react to a height change; this
    // app's real dashboard grid does.
    chart.applyOptions({ width: el.clientWidth, height: el.clientHeight });
    positionHandles?.();
    renderInstrumentBg();
  });
  resizeObserver.observe(el);

  api.destroy = () => {
    resizeObserver.disconnect();
    instrumentBg?.remove();
    handleEl?.remove();
    // Force-end an in-progress drag rather than leaking its document-
    // level mousemove/mouseup listeners if the component unmounts mid-
    // drag (a symbol switch while the user is dragging the boundary).
    activeDragCleanup?.();
  };

  return api;
}

const MONTH_NAMES = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
function pad2(n: number): string {
  return String(n).padStart(2, "0");
}
function fmtVol(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return `${n}`;
}

/**
 * Real OHLCV crosshair tooltip -- imperative DOM, exactly like the
 * prototype's own version (creates and positions one div, no React state
 * involved), including its real tooltip-size measurement
 * (tipEl.offsetWidth/offsetHeight) rather than the estimated size a
 * previous round of this port used as a simplification.
 */
export function wireChartTooltip(instance: SuperChartApi, container: HTMLElement, getBaseOpen?: () => number): () => void {
  const tipEl = document.createElement("div");
  tipEl.className = "chart-tip";
  container.appendChild(tipEl);
  function row(label: string, val: string, cls?: string): string {
    return `<div class="row"><span class="label">${label}</span><span class="val${cls ? ` ${cls}` : ""}">${val}</span></div>`;
  }
  const onMove = (param: Parameters<Parameters<IChartApi["subscribeCrosshairMove"]>[0]>[0]) => {
    if (!param.point || !param.time || !instance.series.candles) {
      tipEl.classList.remove("show");
      return;
    }
    const bar = param.seriesData.get(instance.series.candles) as
      | { open: number; high: number; low: number; close: number }
      | undefined;
    if (!bar) {
      tipEl.classList.remove("show");
      return;
    }
    const up = bar.close >= bar.open;
    const d = new Date((param.time as number) * 1000);
    const volBar = instance.series.volume ? (param.seriesData.get(instance.series.volume) as { value: number } | undefined) : undefined;
    let chgRow = "";
    if (getBaseOpen) {
      const baseOpen = getBaseOpen();
      const chg = ((bar.close - baseOpen) / baseOpen) * 100;
      chgRow = row("Chg", `${chg >= 0 ? "+" : ""}${chg.toFixed(1)}%`, chg >= 0 ? "up" : "down");
    }
    tipEl.innerHTML =
      `<div class="time">${MONTH_NAMES[d.getUTCMonth()]} ${d.getUTCDate()} · ${pad2(d.getUTCHours())}:${pad2(d.getUTCMinutes())} UTC</div>` +
      row("O", bar.open.toFixed(2)) +
      row("H", bar.high.toFixed(2)) +
      row("L", bar.low.toFixed(2)) +
      row("C", bar.close.toFixed(2), up ? "up" : "down") +
      chgRow +
      (volBar ? row("Vol", fmtVol(volBar.value)) : "");
    tipEl.classList.add("show");
    const cw = container.clientWidth;
    const ch = container.clientHeight;
    const tw = tipEl.offsetWidth;
    const th = tipEl.offsetHeight;
    let x = param.point.x + 14;
    let y = param.point.y + 14;
    if (x + tw > cw) x = param.point.x - tw - 14;
    if (y + th > ch) y = param.point.y - th - 14;
    tipEl.style.left = `${Math.max(4, x)}px`;
    tipEl.style.top = `${Math.max(4, y)}px`;
  };
  instance.chart.subscribeCrosshairMove(onMove);
  const onLeave = () => tipEl.classList.remove("show");
  container.addEventListener("mouseleave", onLeave);

  return () => {
    instance.chart.unsubscribeCrosshairMove(onMove);
    container.removeEventListener("mouseleave", onLeave);
    tipEl.remove();
  };
}
