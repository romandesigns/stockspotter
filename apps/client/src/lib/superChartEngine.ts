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
import { computeBollingerBands, computeMACD, computeRSI, sma, vwap } from "./chartIndicators";

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
    s6: tok("--series-6") || "#2ec4b6",
    s7: tok("--series-7") || "#7c93a8",
  };
}

export interface ChartPreset {
  mode: "full" | "compact";
  showVolume: boolean;
  overlays: boolean;
  macd: boolean;
  /** New oscillator pane, alongside MACD -- shares the same bottom zone
   * (see paneMargins()'s own comment for how the split works when both
   * are on). Not from the prototype; added per Roman's ask to borrow
   * from Robinhood's Advanced Charts. */
  rsi: boolean;
  /** New price-pane overlay (upper/lower bands around price), same
   * scale as MA9/MA20/VWAP -- no pane-margin implications at all,
   * unlike rsi. Also not from the prototype. */
  bollinger: boolean;
  resizable: boolean;
  height: number;
}

// Same three contexts the prototype defined. `backtest`/`watchlist` are
// carried over for when those contexts get wired to real data (see
// stockspotter-open-tasks memory) -- only `scanner` is used in the app
// today. rsi/bollinger follow macd/overlays' own lead: on for scanner,
// off for the two not-yet-wired presets (kept minimal there, same as
// macd already was).
export const CHART_PRESETS: Record<string, ChartPreset> = {
  scanner: { mode: "full", showVolume: true, overlays: true, macd: true, rsi: true, bollinger: true, resizable: true, height: 380 },
  backtest: { mode: "full", showVolume: true, overlays: true, macd: false, rsi: false, bollinger: false, resizable: false, height: 340 },
  watchlist: { mode: "compact", showVolume: false, overlays: false, macd: false, rsi: false, bollinger: false, resizable: false, height: 52 },
};

export type ChartType = "candles" | "line";

export interface SuperChartApi {
  chart: IChartApi;
  series: {
    area?: ISeriesApi<"Area">;
    volume?: ISeriesApi<"Histogram">;
    candles?: ISeriesApi<"Candlestick">;
    ma9?: ISeriesApi<"Line">;
    ma20?: ISeriesApi<"Line">;
    vwap?: ISeriesApi<"Line">;
    bbUpper?: ISeriesApi<"Line">;
    bbLower?: ISeriesApi<"Line">;
    macdHist?: ISeriesApi<"Histogram">;
    macdLine?: ISeriesApi<"Line">;
    macdSignal?: ISeriesApi<"Line">;
    rsi?: ISeriesApi<"Line">;
  };
  setBars: (bars: CandleBar[]) => void;
  /** Candles and the line/area view are both created at mount (full mode
   * only) and swapped by visibility, not destroy/recreate -- keeps the
   * two series' z-order (and everything layered above them) stable
   * across a toggle instead of re-fighting draw order every time. */
  setChartType: (type: ChartType) => void;
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
  // than claiming exclusive height. MACD/RSI can't share the price
  // scale (their values are nowhere near price's magnitude) so they
  // keep a real separate zone below — macdBoundary is the one
  // remaining resizable split, governing the top edge of that WHOLE
  // zone regardless of whether one or both oscillators live in it.
  let macdBoundary = 0.78;
  function paneMargins() {
    const hasBottomZone = opts.macd || opts.rsi;
    const priceBottom = hasBottomZone ? 1 - macdBoundary : 0.05;
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

    // MACD and RSI split the same zone [macdBoundary, zoneBottom] evenly
    // (with a small gap) when BOTH are mounted; when only one is, it
    // gets the whole zone. This is a static split decided once at mount
    // from which oscillators are PRESENT (opts.macd/opts.rsi), not
    // recomputed when either is later hidden via the Indicators popover
    // -- same deliberately-not-dynamic quirk MACD's own visibility
    // toggle already has (see toggleIndicator's "macd" branch below):
    // hiding one leaves its half blank rather than growing the other to
    // fill it.
    const zoneTop = macdBoundary;
    const zoneBottom = 0.98;
    let macdMargins = { top: zoneTop, bottom: 1 - zoneBottom };
    let rsiMargins = { top: zoneTop, bottom: 1 - zoneBottom };
    if (opts.macd && opts.rsi) {
      const gap = 0.02;
      const half = (zoneBottom - zoneTop - gap) / 2;
      macdMargins = { top: zoneTop, bottom: 1 - (zoneTop + half) };
      rsiMargins = { top: zoneTop + half + gap, bottom: 1 - zoneBottom };
    }
    return {
      price: { top: 0.05, bottom: priceBottom },
      vol: { top: volTop, bottom: priceBottom },
      macd: macdMargins,
      rsi: rsiMargins,
    };
  }
  function applyPaneMargins() {
    const m = paneMargins();
    chart.priceScale("right").applyOptions({ scaleMargins: m.price });
    if (opts.showVolume) chart.priceScale("vol").applyOptions({ scaleMargins: m.vol });
    if (opts.macd) chart.priceScale("macd").applyOptions({ scaleMargins: m.macd });
    if (opts.rsi) chart.priceScale("rsi").applyOptions({ scaleMargins: m.rsi });
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

  const api: SuperChartApi = { chart, series: {}, setBars: () => {}, setChartType: () => {}, destroy: () => {} };

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

    // The line/area view of price -- always created alongside candles
    // (not lazily on first toggle) and always kept fully populated, just
    // hidden by default. Toggling chart type only ever flips `visible`
    // on these two (see api.setChartType below), never destroys/
    // recreates either -- that keeps both series' z-order (added right
    // here, before every overlay below) stable across any number of
    // toggles, rather than a later-recreated series jumping to the top
    // of the draw order and starting to cover the MA/VWAP lines.
    const dir = opts.bars[opts.bars.length - 1].close >= opts.bars[0].open;
    const area = chart.addAreaSeries({
      lineColor: dir ? COLOR.good : COLOR.critical,
      lineWidth: 2,
      topColor: dir ? "rgba(12,163,12,.22)" : "rgba(208,59,59,.22)",
      bottomColor: "rgba(0,0,0,0)",
      priceLineVisible: false,
      lastValueVisible: false,
      crosshairMarkerVisible: false,
      visible: false,
    });
    area.setData(opts.bars.map((b) => ({ time: b.time as UTCTimestamp, value: b.close })));
    api.series.area = area;

    if (opts.showVolume) chart.priceScale("vol").applyOptions({ scaleMargins: paneMargins().vol });

    if (opts.overlays) {
      api.series.ma9 = chart.addLineSeries({ color: COLOR.s1, lineWidth: 2, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });
      api.series.ma9.setData(sma(opts.bars, 9).map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
      api.series.ma20 = chart.addLineSeries({ color: COLOR.s2, lineWidth: 2, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });
      api.series.ma20.setData(sma(opts.bars, 20).map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
      api.series.vwap = chart.addLineSeries({ color: COLOR.s3, lineWidth: 2, lineStyle: LineStyle.Dashed, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });
      api.series.vwap.setData(vwap(opts.bars).map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
    }

    if (opts.bollinger) {
      // Same price scale as candles/MA/VWAP -- a band around price, not
      // its own pane, so it needs no paneMargins() entry at all. Upper/
      // lower only (middle is redundant with MA20 already on screen at
      // the same 20-period default) -- both share one color, dashed like
      // VWAP's own line, since a filled band-between-two-lines isn't a
      // lightweight-charts primitive without extra per-bar area series.
      const bb0 = computeBollingerBands(opts.bars);
      api.series.bbUpper = chart.addLineSeries({ color: COLOR.s7, lineWidth: 1, lineStyle: LineStyle.Dashed, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });
      api.series.bbUpper.setData(bb0.upper.map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
      api.series.bbLower = chart.addLineSeries({ color: COLOR.s7, lineWidth: 1, lineStyle: LineStyle.Dashed, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });
      api.series.bbLower.setData(bb0.lower.map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
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
    }

    if (opts.rsi) {
      // Same lazy-scale-creation ordering rule as MACD above. Fixed 0-100
      // range (not autoScale-to-data-range) via autoscaleInfoProvider --
      // the whole point of RSI's own 30/70 reference lines is a
      // consistent visual position across time, which a scale that
      // tightens around wherever RSI happens to be sitting would defeat.
      api.series.rsi = chart.addLineSeries({
        priceScaleId: "rsi",
        color: COLOR.s6,
        lineWidth: 2,
        priceLineVisible: false,
        lastValueVisible: false,
        crosshairMarkerVisible: false,
        autoscaleInfoProvider: () => ({ priceRange: { minValue: 0, maxValue: 100 } }),
      });
      api.series.rsi.setData(computeRSI(opts.bars).map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
      chart.priceScale("rsi").applyOptions({ scaleMargins: paneMargins().rsi });
      api.series.rsi.createPriceLine({ price: 70, color: COLOR.textMuted, lineWidth: 1, lineStyle: LineStyle.Dotted, axisLabelVisible: false, title: "" });
      api.series.rsi.createPriceLine({ price: 30, color: COLOR.textMuted, lineWidth: 1, lineStyle: LineStyle.Dotted, axisLabelVisible: false, title: "" });
    }

    // Drag handle at the one remaining boundary (chart vs. the MACD/RSI
    // zone) — a separate capability from either oscillator itself, so
    // it's its own preset flag, shared by whichever of the two are on.
    if (opts.resizable && (opts.macd || opts.rsi)) {
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

  chart.timeScale().fitContent();
  renderInstrumentBg();

  api.setBars = (bars: CandleBar[]) => {
    if (mode === "compact") {
      api.series.area?.setData(bars.map((b) => ({ time: b.time as UTCTimestamp, value: b.close })));
    } else {
      api.series.candles?.setData(bars.map((b) => ({ time: b.time as UTCTimestamp, open: b.open, high: b.high, low: b.low, close: b.close })));
      api.series.area?.setData(bars.map((b) => ({ time: b.time as UTCTimestamp, value: b.close })));
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
      if (api.series.bbUpper && api.series.bbLower) {
        const bb1 = computeBollingerBands(bars);
        api.series.bbUpper.setData(bb1.upper.map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
        api.series.bbLower.setData(bb1.lower.map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
      }
      if (api.series.macdLine && api.series.macdHist && api.series.macdSignal) {
        const macd1 = computeMACD(bars);
        api.series.macdHist.setData(macd1.hist.map((p) => ({ time: p.time as UTCTimestamp, value: p.value, color: p.color })));
        api.series.macdLine.setData(macd1.macdLine.map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
        api.series.macdSignal.setData(macd1.signalLine.map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
      }
      if (api.series.rsi) api.series.rsi.setData(computeRSI(bars).map((p) => ({ time: p.time as UTCTimestamp, value: p.value })));
    }
    chart.timeScale().fitContent();
  };

  // Both series stay populated at all times (see the creation comment
  // above) -- this just flips which one draws. `dir`'s already correct
  // for a fresh mount; a later toggle after live ticks have moved the
  // needle keeps whatever color it was set at rather than recomputing --
  // the area's own COLOR isn't data Roman asked this to track, just its
  // shape.
  api.setChartType = (type: ChartType) => {
    api.series.candles?.applyOptions({ visible: type === "candles" });
    api.series.area?.applyOptions({ visible: type === "line" });
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
 *
 * Looks OHLCV up from `getBars()` by `param.time`, not from
 * `param.seriesData.get(instance.series.candles)` the way this used to --
 * a deliberate change, not a style preference: lightweight-charts drops
 * a `visible:false` series from `seriesData` on crosshair events, so the
 * old series-lookup version would have gone silently blank the moment
 * chart type switches to "line" (candles hidden). Looking the bar up in
 * the data this engine already has works identically for either chart
 * type and no longer depends on which price series happens to be drawn.
 */
export function wireChartTooltip(instance: SuperChartApi, container: HTMLElement, getBars: () => CandleBar[], getBaseOpen?: () => number): () => void {
  const tipEl = document.createElement("div");
  tipEl.className = "chart-tip";
  container.appendChild(tipEl);
  function row(label: string, val: string, cls?: string): string {
    return `<div class="row"><span class="label">${label}</span><span class="val${cls ? ` ${cls}` : ""}">${val}</span></div>`;
  }
  const onMove = (param: Parameters<Parameters<IChartApi["subscribeCrosshairMove"]>[0]>[0]) => {
    if (!param.point || !param.time) {
      tipEl.classList.remove("show");
      return;
    }
    const bar = getBars().find((b) => b.time === param.time);
    if (!bar) {
      tipEl.classList.remove("show");
      return;
    }
    const up = bar.close >= bar.open;
    const d = new Date((param.time as number) * 1000);
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
      row("Vol", fmtVol(bar.volume));
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
