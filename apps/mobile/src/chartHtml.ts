// Real Super Chart engine, ported into a self-contained HTML page for
// react-native-webview -- lightweight-charts has no native React Native
// binding at all (it's a canvas/DOM library), so a WebView hosting the
// real chart is the standard pattern for this exact situation, not a
// workaround. This is the SAME engine as apps/client/src/lib/
// superChartEngine.ts's mountSuperChart() run in its real `full` mode
// (candles + volume + MA9/MA20 + VWAP + MACD + resize handle + tooltip),
// plus chartIndicators.ts's sma/vwap/computeMACD -- ported to plain JS
// since a WebView's `source={{ html }}` needs a real self-contained page,
// not a TS module graph. Same function bodies, same pane-margins formula
// (with its own bug-fix comments preserved), same series creation calls/
// options, same tooltip positioning/measurement -- one more hop removed
// from the original Artifact prototype, not re-derived from memory.
//
// Per Roman's explicit correction ("the design and features should be
// consistent to what we have already established"), this replaces the
// earlier Robinhood-style compact/area-mode version this file used to
// build -- that redesign is superseded. The real adaptations RN genuinely
// needs (not present on web) are: (1) handleScroll/handleScale left true
// (mode "full" already sets both -- real pinch/pan/zoom comes free from
// lightweight-charts' own canvas touch handling); (2) the resize handle
// also wires touchstart/touchmove/touchend alongside web's mouse events,
// since those never fire on a touch-only device; (3) a
// window.__applySettings() entry point (mirrors the existing
// window.__setBars) so RN's new Indicators/Settings sheets can drive
// indicator visibility/autoscale/scale-mode the same way SuperChart.tsx's
// toolbar drives the real api object on web. The old `scrub` postMessage
// is gone -- it only ever existed for the superseded area-mode's header-
// follows-your-finger behavior; the real OHLC tooltip below replaces it,
// matching web, where the header doesn't track the crosshair either.

import { colors } from "./theme";

const CDN = "https://unpkg.com/lightweight-charts@4.1.3/dist/lightweight-charts.standalone.production.js";

// The page loads once per symbol with zero bars, then RN pushes real data
// in via injectJavaScript(window.__setBars(...)) once the page reports
// itself ready -- reloading the whole WebView (a new `source.html`) on
// every live tick or range switch would flash/reset state, so bars are
// never baked into the initial HTML string.
export function buildChartHtml(): string {
  return `<!doctype html>
<html>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, maximum-scale=1, user-scalable=no">
<style>
  html, body { margin: 0; padding: 0; background: ${colors.background}; overflow: hidden; height: 100%; }
  #chart { position: absolute; inset: 0; }

  /* Ported from apps/client/src/index.css -- same real DOM-managed nodes
     (not React-rendered), same box-shadow/padding/font, same .row/.time/
     .label/.val tooltip structure, same pane-handle grip. */
  .chart-instrument-bg {
    position: absolute; left: 0; right: 0; bottom: 0;
    background: rgba(255, 255, 255, 0.045);
    pointer-events: none; z-index: 1;
  }
  .pane-handle {
    position: absolute; left: 0; right: 0; height: 13px; margin-top: -6.5px;
    z-index: 15; touch-action: none;
    display: flex; align-items: center; justify-content: center;
  }
  .pane-handle::before {
    content: ""; position: absolute; left: 0; right: 0; top: 6px; height: 1px;
    background: ${colors.accent}; opacity: 0; transition: opacity 0.12s ease;
  }
  .pane-handle::after {
    content: ""; width: 34px; height: 5px; border-radius: 999px;
    background: ${colors.text}; opacity: 0.32;
    transition: opacity 0.12s ease, background-color 0.12s ease, width 0.12s ease;
  }
  .pane-handle.dragging::before { opacity: 0.4; }
  .pane-handle.dragging::after { opacity: 1; background: ${colors.accent}; width: 46px; }

  .chart-tip {
    position: absolute; z-index: 20; pointer-events: none;
    background: ${colors.row}; border-radius: 8px; padding: 8px 11px;
    font-family: -apple-system, sans-serif; font-size: 11px; color: ${colors.muted};
    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.5); min-width: 148px;
    opacity: 0; transition: opacity 0.1s ease;
  }
  .chart-tip.show { opacity: 1; }
  .chart-tip .time { font-size: 10px; color: ${colors.muted}; opacity: 0.7; margin-bottom: 6px; padding-bottom: 6px; }
  .chart-tip .row { display: flex; justify-content: space-between; gap: 16px; }
  .chart-tip .row + .row { margin-top: 2px; }
  .chart-tip .label { color: ${colors.muted}; opacity: 0.7; }
  .chart-tip .val { font-weight: 600; color: ${colors.text}; font-variant-numeric: tabular-nums; }
  .chart-tip .val.up { color: ${colors.good}; }
  .chart-tip .val.down { color: ${colors.critical}; }
</style>
</head>
<body>
<div id="chart"></div>
<script src="${CDN}"></script>
<script>
// ---------- superChartEngine.ts's mountSuperChart(), 'full' mode +
// wireChartTooltip(), ported verbatim (same series creation, same
// paneMargins()/applyPaneMargins()/renderInstrumentBg() formulas, same
// tooltip). chartIndicators.ts's sma/vwap/computeMACD ported alongside,
// same as chartHtml.ts already did for resample() before this rewrite. ----------
var LWC = LightweightCharts;
var COLOR = {
  textMuted: "${colors.muted}", border: "${colors.divider}",
  good: "${colors.good}", critical: "${colors.critical}",
  s1: "#3987e5", s2: "#d95926", s3: "#9085e9", s4: "#c98500", s5: "#d55181"
};

// ---------- chartIndicators.ts, ported verbatim ----------
function sma(bars, period) {
  var out = []; var sum = 0;
  for (var i = 0; i < bars.length; i++) {
    sum += bars[i].close;
    if (i >= period) sum -= bars[i - period].close;
    if (i >= period - 1) out.push({ time: bars[i].time, value: +(sum / period).toFixed(3) });
  }
  return out;
}
function vwap(bars) {
  var out = []; var cumPV = 0; var cumV = 0;
  for (var i = 0; i < bars.length; i++) {
    var b = bars[i];
    var tp = (b.high + b.low + b.close) / 3;
    cumPV += tp * b.volume; cumV += b.volume;
    out.push({ time: b.time, value: cumV > 0 ? +(cumPV / cumV).toFixed(3) : 0 });
  }
  return out;
}
function emaSeries(values, period) {
  var k = 2 / (period + 1);
  var out = new Array(values.length);
  var prev = 0;
  for (var i = 0; i < values.length; i++) {
    prev = i === 0 ? values[i] : values[i] * k + prev * (1 - k);
    out[i] = prev;
  }
  return out;
}
function computeMACD(bars) {
  var closes = bars.map(function (b) { return b.close; });
  var ema12 = emaSeries(closes, 12);
  var ema26 = emaSeries(closes, 26);
  var macdVals = closes.map(function (_, i) { return ema12[i] - ema26[i]; });
  var signalVals = emaSeries(macdVals, 9);
  var macdLine = [], signalLine = [], hist = [];
  for (var i = 0; i < bars.length; i++) {
    var h = macdVals[i] - signalVals[i];
    macdLine.push({ time: bars[i].time, value: +macdVals[i].toFixed(4) });
    signalLine.push({ time: bars[i].time, value: +signalVals[i].toFixed(4) });
    hist.push({ time: bars[i].time, value: +h.toFixed(4), color: h >= 0 ? "rgba(12,163,12,.55)" : "rgba(208,59,59,.55)" });
  }
  return { macdLine: macdLine, signalLine: signalLine, hist: hist };
}

// ---------- superChartEngine.ts's mountSuperChart('full'), ported ----------
var el = document.getElementById("chart");
var macdBoundary = 0.78;

function paneMargins() {
  var priceBottom = 1 - macdBoundary;
  var volBand = 0.2;
  var volTop = Math.max(0.05, 1 - priceBottom - volBand);
  return {
    price: { top: 0.05, bottom: priceBottom },
    vol: { top: volTop, bottom: priceBottom },
    macd: { top: macdBoundary, bottom: 0.02 }
  };
}
function applyPaneMargins() {
  var m = paneMargins();
  chart.priceScale("right").applyOptions({ scaleMargins: m.price });
  chart.priceScale("vol").applyOptions({ scaleMargins: m.vol });
  chart.priceScale("macd").applyOptions({ scaleMargins: m.macd });
  renderInstrumentBg();
}

var instrumentBg = null;
function renderInstrumentBg() {
  if (!instrumentBg) {
    instrumentBg = document.createElement("div");
    instrumentBg.className = "chart-instrument-bg";
    el.appendChild(instrumentBg);
  }
  instrumentBg.style.top = (paneMargins().vol.top * el.clientHeight) + "px";
}

var chart = LWC.createChart(el, {
  width: el.clientWidth, height: el.clientHeight,
  layout: {
    background: { type: "solid", color: "transparent" },
    textColor: COLOR.textMuted, fontFamily: "-apple-system, sans-serif", fontSize: 11
  },
  grid: { vertLines: { visible: false }, horzLines: { visible: true, color: COLOR.border } },
  rightPriceScale: { visible: true, borderVisible: false, scaleMargins: paneMargins().price },
  timeScale: { visible: true, borderVisible: false, timeVisible: true, secondsVisible: false, rightOffset: 2 },
  crosshair: {
    mode: LWC.CrosshairMode.Normal,
    vertLine: { visible: true, labelVisible: true, color: COLOR.textMuted, style: LWC.LineStyle.Solid, width: 1 },
    horzLine: { visible: true, labelVisible: true, color: COLOR.textMuted, style: LWC.LineStyle.Solid, width: 1 }
  },
  // Real pinch/pan/zoom -- the exact flags web's own "scanner" preset
  // already runs in production for mode "full". lightweight-charts
  // implements this itself via pointer/touch events on its own canvas, a
  // separate mechanism from the WebView's own scroll view (ChartScreen's
  // scrollEnabled={false} only blocks the page-level scroll).
  handleScroll: true, handleScale: true
});

var candles = null, vol = null, ma9 = null, ma20 = null, vwapSeries = null;
var macdHist = null, macdLine = null, macdSignal = null;
var handleEl = null, positionHandles = null, activeDragCleanup = null;
var seriesReady = false;

// The Indicators/Settings sheets' own MACD-toggle handler uses these
// exact fallback margins rather than recomputing from paneMargins() --
// ported verbatim from SuperChart.tsx's MACD_TOGGLE_MARGINS, the real
// quirk web's own toolbar carries (toggling MACD this way does NOT
// reposition the instrument backdrop, matching web exactly).
var MACD_TOGGLE_MARGINS = {
  on: { price: { top: 0.05, bottom: 0.4 }, vol: { top: 0.84, bottom: 0 } },
  off: { price: { top: 0.08, bottom: 0.28 }, vol: { top: 0.78, bottom: 0 } }
};

function ensureSeries() {
  if (seriesReady) return;
  seriesReady = true;

  // Volume added before candles so it draws *behind* them (lightweight-
  // charts layers series in add-order).
  vol = chart.addHistogramSeries({ priceFormat: { type: "volume" }, priceScaleId: "vol", color: COLOR.good });
  candles = chart.addCandlestickSeries({
    upColor: COLOR.good, downColor: COLOR.critical, borderVisible: false,
    wickUpColor: COLOR.good, wickDownColor: COLOR.critical
  });
  chart.priceScale("vol").applyOptions({ scaleMargins: paneMargins().vol });

  ma9 = chart.addLineSeries({ color: COLOR.s1, lineWidth: 2, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });
  ma20 = chart.addLineSeries({ color: COLOR.s2, lineWidth: 2, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });
  vwapSeries = chart.addLineSeries({ color: COLOR.s3, lineWidth: 2, lineStyle: LWC.LineStyle.Dashed, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });

  // Series must exist on the 'macd' scale BEFORE margins are applied to
  // it -- priceScale('macd') is created lazily by the first series that
  // references it, same real ordering bug-fix superChartEngine.ts's own
  // history documents.
  macdHist = chart.addHistogramSeries({ priceScaleId: "macd", priceLineVisible: false, lastValueVisible: false });
  macdLine = chart.addLineSeries({ priceScaleId: "macd", color: COLOR.s4, lineWidth: 1, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });
  macdSignal = chart.addLineSeries({ priceScaleId: "macd", color: COLOR.s5, lineWidth: 1, lineStyle: LWC.LineStyle.Dashed, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });
  chart.priceScale("macd").applyOptions({ scaleMargins: paneMargins().macd });

  setupResizeHandle();
  wireTooltip();
  renderInstrumentBg();
}

function setupResizeHandle() {
  var handle = document.createElement("div");
  handle.className = "pane-handle";
  el.appendChild(handle);
  handleEl = handle;

  positionHandles = function () { handle.style.top = (macdBoundary * el.clientHeight) + "px"; };
  positionHandles();

  function getY(ev) { return ev.touches && ev.touches.length ? ev.touches[0].clientY : ev.clientY; }

  function startDrag(e) {
    e.preventDefault();
    handle.classList.add("dragging");
    function onMove(ev) {
      if (ev.cancelable) ev.preventDefault();
      var rect = el.getBoundingClientRect();
      var frac = (getY(ev) - rect.top) / rect.height;
      macdBoundary = Math.max(0.35, Math.min(frac, 0.92));
      applyPaneMargins();
      positionHandles();
    }
    function onUp() {
      handle.classList.remove("dragging");
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      document.removeEventListener("touchmove", onMove);
      document.removeEventListener("touchend", onUp);
      activeDragCleanup = null;
    }
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
    // Web's drag only wires mouse events, which never fire on a
    // touch-only device -- real adaptation RN genuinely needs, same frac
    // math from the touch point instead of the mouse point.
    document.addEventListener("touchmove", onMove, { passive: false });
    document.addEventListener("touchend", onUp);
    activeDragCleanup = function () {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      document.removeEventListener("touchmove", onMove);
      document.removeEventListener("touchend", onUp);
    };
  }
  handle.addEventListener("mousedown", startDrag);
  handle.addEventListener("touchstart", startDrag, { passive: false });
}

// ---------- wireChartTooltip(), ported verbatim ----------
var MONTH_NAMES = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"];
function pad2(n) { return String(n).padStart(2, "0"); }
function fmtVol(n) {
  if (n >= 1000000) return (n / 1000000).toFixed(2) + "M";
  if (n >= 1000) return (n / 1000).toFixed(1) + "K";
  return String(n);
}
function wireTooltip() {
  var tipEl = document.createElement("div");
  tipEl.className = "chart-tip";
  el.appendChild(tipEl);
  function row(label, val, cls) {
    return '<div class="row"><span class="label">' + label + '</span><span class="val' + (cls ? " " + cls : "") + '">' + val + "</span></div>";
  }
  chart.subscribeCrosshairMove(function (param) {
    if (!param.point || !param.time || !candles) { tipEl.classList.remove("show"); return; }
    var bar = param.seriesData.get(candles);
    if (!bar) { tipEl.classList.remove("show"); return; }
    var up = bar.close >= bar.open;
    var d = new Date(param.time * 1000);
    var volBar = vol ? param.seriesData.get(vol) : undefined;
    var chgRow = "";
    if (latestBars && latestBars.length > 0) {
      var baseOpen = latestBars[0].open;
      var chg = ((bar.close - baseOpen) / baseOpen) * 100;
      chgRow = row("Chg", (chg >= 0 ? "+" : "") + chg.toFixed(1) + "%", chg >= 0 ? "up" : "down");
    }
    tipEl.innerHTML =
      '<div class="time">' + MONTH_NAMES[d.getUTCMonth()] + " " + d.getUTCDate() + " · " + pad2(d.getUTCHours()) + ":" + pad2(d.getUTCMinutes()) + ' UTC</div>' +
      row("O", bar.open.toFixed(2)) + row("H", bar.high.toFixed(2)) + row("L", bar.low.toFixed(2)) +
      row("C", bar.close.toFixed(2), up ? "up" : "down") + chgRow +
      (volBar ? row("Vol", fmtVol(volBar.value)) : "");
    tipEl.classList.add("show");
    var cw = el.clientWidth, ch = el.clientHeight;
    var tw = tipEl.offsetWidth, th = tipEl.offsetHeight;
    var x = param.point.x + 14, y = param.point.y + 14;
    if (x + tw > cw) x = param.point.x - tw - 14;
    if (y + th > ch) y = param.point.y - th - 14;
    tipEl.style.left = Math.max(4, x) + "px";
    tipEl.style.top = Math.max(4, y) + "px";
  });
  el.addEventListener("mouseleave", function () { tipEl.classList.remove("show"); });
}

var latestBars = [];

function setBars(bars) {
  if (!bars || bars.length === 0) return;
  latestBars = bars;
  ensureSeries();

  candles.setData(bars.map(function (b) { return { time: b.time, open: b.open, high: b.high, low: b.low, close: b.close }; }));
  vol.setData(bars.map(function (b, i) {
    var up = i === 0 || b.close >= bars[i - 1].close;
    return { time: b.time, value: b.volume, color: up ? "rgba(12,163,12,.38)" : "rgba(208,59,59,.38)" };
  }));
  ma9.setData(sma(bars, 9).map(function (p) { return { time: p.time, value: p.value }; }));
  ma20.setData(sma(bars, 20).map(function (p) { return { time: p.time, value: p.value }; }));
  vwapSeries.setData(vwap(bars).map(function (p) { return { time: p.time, value: p.value }; }));
  var macd = computeMACD(bars);
  macdHist.setData(macd.hist.map(function (p) { return { time: p.time, value: p.value, color: p.color }; }));
  macdLine.setData(macd.macdLine.map(function (p) { return { time: p.time, value: p.value }; }));
  macdSignal.setData(macd.signalLine.map(function (p) { return { time: p.time, value: p.value }; }));

  chart.timeScale().fitContent();
}
window.__setBars = setBars;

// New settings command channel -- mirrors window.__setBars. RN's
// Indicators/Settings sheets always send the FULL current settings
// snapshot on every toggle (not a partial patch), same shape
// SuperChart.tsx's own toolbar state already holds.
function applySettings(s) {
  if (!s || !seriesReady) return;
  if (s.indicators) {
    if (typeof s.indicators.ma9 === "boolean") ma9.applyOptions({ visible: s.indicators.ma9 });
    if (typeof s.indicators.ma20 === "boolean") ma20.applyOptions({ visible: s.indicators.ma20 });
    if (typeof s.indicators.vwap === "boolean") vwapSeries.applyOptions({ visible: s.indicators.vwap });
    if (typeof s.indicators.macd === "boolean") {
      var nowOn = s.indicators.macd;
      macdHist.applyOptions({ visible: nowOn });
      macdLine.applyOptions({ visible: nowOn });
      macdSignal.applyOptions({ visible: nowOn });
      var m = nowOn ? MACD_TOGGLE_MARGINS.on : MACD_TOGGLE_MARGINS.off;
      chart.priceScale("right").applyOptions({ scaleMargins: m.price });
      chart.priceScale("vol").applyOptions({ scaleMargins: m.vol });
    }
  }
  if (typeof s.autoScale === "boolean") {
    chart.priceScale("right").applyOptions({ autoScale: s.autoScale });
  }
  if (typeof s.fitIndicators === "boolean") {
    [ma9, ma20, vwapSeries].forEach(function (ser) {
      ser.applyOptions({ autoscaleInfoProvider: function (original) { return s.fitIndicators ? original() : null; } });
    });
    if (s.autoScale) chart.priceScale("right").applyOptions({ autoScale: true });
  }
  if (s.scaleMode) {
    var mode = s.scaleMode === "log" ? LWC.PriceScaleMode.Logarithmic : s.scaleMode === "percent" ? LWC.PriceScaleMode.Percentage : LWC.PriceScaleMode.Normal;
    chart.priceScale("right").applyOptions({ mode: mode });
  }
}
window.__applySettings = applySettings;

new ResizeObserver(function () {
  chart.applyOptions({ width: el.clientWidth, height: el.clientHeight });
  if (positionHandles) positionHandles();
  if (seriesReady) renderInstrumentBg();
}).observe(el);

if (window.ReactNativeWebView) window.ReactNativeWebView.postMessage(JSON.stringify({ type: "ready" }));
</script>
</body>
</html>`;
}
