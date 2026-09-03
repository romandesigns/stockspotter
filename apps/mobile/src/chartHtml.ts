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
//
// RSI, Bollinger Bands, and the candlestick/line chart-type toggle (per
// Roman's "borrow from Robinhood" ask) are ported here from
// superChartEngine.ts's own additions of the same -- same paneMargins()
// generalization for RSI sharing the bottom zone with MACD, same
// always-create-both-hidden-one approach for chart type, same bars-
// array-lookup tooltip fix (not param.seriesData, which drops a
// visible:false series). Unlike web, this single fixed page has no
// per-instance ChartPreset -- MACD and RSI are simply always both
// created, so paneMargins() only ever needs the "split the zone in
// half" case, not web's "one alone gets the whole zone" branch too.

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

  /* Candle countdown -- time left until the current (still-forming)
     candle closes, TradingView-style. Sits top-right, opposite corner
     from where the crosshair tooltip appears, so the two never overlap. */
  .candle-countdown {
    position: absolute; top: 8px; right: 10px; z-index: 12;
    background: ${colors.row}; border-radius: 6px; padding: 3px 8px;
    font-family: -apple-system, sans-serif; font-size: 10px; font-weight: 600;
    color: ${colors.muted}; font-variant-numeric: tabular-nums;
    opacity: 0; transition: opacity 0.15s ease; pointer-events: none;
  }
  .candle-countdown.show { opacity: 1; }
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
  s1: "#3987e5", s2: "#d95926", s3: "#9085e9", s4: "#c98500", s5: "#d55181",
  s6: "#2ec4b6", s7: "#7c93a8"
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
// Ported verbatim from chartIndicators.ts's computeRSI -- standard
// 14-period RSI, Wilder's smoothing.
function computeRSI(bars, period) {
  period = period || 14;
  var out = [];
  if (bars.length < period + 1) return out;
  var avgGain = 0, avgLoss = 0;
  for (var i = 1; i <= period; i++) {
    var change = bars[i].close - bars[i - 1].close;
    if (change >= 0) avgGain += change; else avgLoss -= change;
  }
  avgGain /= period; avgLoss /= period;
  function rsiFrom(gain, loss) { return loss === 0 ? 100 : 100 - 100 / (1 + gain / loss); }
  out.push({ time: bars[period].time, value: +rsiFrom(avgGain, avgLoss).toFixed(2) });
  for (var j = period + 1; j < bars.length; j++) {
    var chg = bars[j].close - bars[j - 1].close;
    var gain = chg > 0 ? chg : 0, loss = chg < 0 ? -chg : 0;
    avgGain = (avgGain * (period - 1) + gain) / period;
    avgLoss = (avgLoss * (period - 1) + loss) / period;
    out.push({ time: bars[j].time, value: +rsiFrom(avgGain, avgLoss).toFixed(2) });
  }
  return out;
}
// Ported verbatim from chartIndicators.ts's computeBollingerBands --
// standard 20-period SMA ± 2 standard deviations.
function computeBollingerBands(bars, period, stdDevMultiplier) {
  period = period || 20; stdDevMultiplier = stdDevMultiplier || 2;
  var upper = [], lower = [];
  var sum = 0, sumSq = 0;
  for (var i = 0; i < bars.length; i++) {
    var close = bars[i].close;
    sum += close; sumSq += close * close;
    if (i >= period) {
      var dropped = bars[i - period].close;
      sum -= dropped; sumSq -= dropped * dropped;
    }
    if (i >= period - 1) {
      var mean = sum / period;
      var variance = Math.max(0, sumSq / period - mean * mean);
      var stdDev = Math.sqrt(variance);
      var time = bars[i].time;
      upper.push({ time: time, value: +(mean + stdDevMultiplier * stdDev).toFixed(3) });
      lower.push({ time: time, value: +(mean - stdDevMultiplier * stdDev).toFixed(3) });
    }
  }
  return { upper: upper, lower: lower };
}

// ---------- superChartEngine.ts's mountSuperChart('full'), ported ----------
var el = document.getElementById("chart");
var macdBoundary = 0.78;

// Unlike web (whose ChartPreset can turn either oscillator off per
// context), this page is a single fixed instance where MACD and RSI are
// ALWAYS both created -- so the zone below macdBoundary is always the
// "split evenly, small gap" case, never the "one alone gets the whole
// zone" case web's own paneMargins() has to handle.
function paneMargins() {
  var priceBottom = 1 - macdBoundary;
  var volBand = 0.2;
  var volTop = Math.max(0.05, 1 - priceBottom - volBand);
  var zoneTop = macdBoundary, zoneBottom = 0.98, gap = 0.02;
  var half = (zoneBottom - zoneTop - gap) / 2;
  return {
    price: { top: 0.05, bottom: priceBottom },
    vol: { top: volTop, bottom: priceBottom },
    macd: { top: zoneTop, bottom: 1 - (zoneTop + half) },
    rsi: { top: zoneTop + half + gap, bottom: 1 - zoneBottom }
  };
}
function applyPaneMargins() {
  var m = paneMargins();
  chart.priceScale("right").applyOptions({ scaleMargins: m.price });
  chart.priceScale("vol").applyOptions({ scaleMargins: m.vol });
  chart.priceScale("macd").applyOptions({ scaleMargins: m.macd });
  chart.priceScale("rsi").applyOptions({ scaleMargins: m.rsi });
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

var candles = null, area = null, vol = null, ma9 = null, ma20 = null, vwapSeries = null, bbUpper = null, bbLower = null;
var macdHist = null, macdLine = null, macdSignal = null, rsiSeries = null;
var handleEl = null, positionHandles = null, activeDragCleanup = null;
var seriesReady = false;
var chartType = "candles";

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
  // The line/area view of price -- always created alongside candles and
  // always kept fully populated, just hidden by default. Toggling chart
  // type only ever flips visible on these two (window.__setChartType
  // below), never destroys/recreates either -- keeps both series' (and
  // everything layered above them) z-order stable across any number of
  // toggles.
  area = chart.addAreaSeries({
    lineColor: COLOR.good, lineWidth: 2,
    topColor: "rgba(12,163,12,.22)", bottomColor: "rgba(0,0,0,0)",
    priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false,
    visible: false
  });
  chart.priceScale("vol").applyOptions({ scaleMargins: paneMargins().vol });

  ma9 = chart.addLineSeries({ color: COLOR.s1, lineWidth: 2, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });
  ma20 = chart.addLineSeries({ color: COLOR.s2, lineWidth: 2, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });
  vwapSeries = chart.addLineSeries({ color: COLOR.s3, lineWidth: 2, lineStyle: LWC.LineStyle.Dashed, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });

  // Bollinger Bands -- same price scale as candles/MA/VWAP, a band
  // around price rather than its own pane, so no paneMargins() entry.
  // Upper/lower only (middle is redundant with MA20 already on screen
  // at the same 20-period default).
  bbUpper = chart.addLineSeries({ color: COLOR.s7, lineWidth: 1, lineStyle: LWC.LineStyle.Dashed, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });
  bbLower = chart.addLineSeries({ color: COLOR.s7, lineWidth: 1, lineStyle: LWC.LineStyle.Dashed, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });

  // Series must exist on the 'macd'/'rsi' scales BEFORE margins are
  // applied to them -- priceScale(id) is created lazily by the first
  // series that references it, same real ordering bug-fix
  // superChartEngine.ts's own history documents.
  macdHist = chart.addHistogramSeries({ priceScaleId: "macd", priceLineVisible: false, lastValueVisible: false });
  macdLine = chart.addLineSeries({ priceScaleId: "macd", color: COLOR.s4, lineWidth: 1, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });
  macdSignal = chart.addLineSeries({ priceScaleId: "macd", color: COLOR.s5, lineWidth: 1, lineStyle: LWC.LineStyle.Dashed, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });
  chart.priceScale("macd").applyOptions({ scaleMargins: paneMargins().macd });

  // RSI -- fixed 0-100 range (not autoScale-to-data-range) via
  // autoscaleInfoProvider, so the 30/70 reference lines sit at a
  // consistent visual position across time rather than a scale that
  // tightens around wherever RSI happens to be sitting.
  rsiSeries = chart.addLineSeries({
    priceScaleId: "rsi", color: COLOR.s6, lineWidth: 2,
    priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false,
    autoscaleInfoProvider: function () { return { priceRange: { minValue: 0, maxValue: 100 } }; }
  });
  chart.priceScale("rsi").applyOptions({ scaleMargins: paneMargins().rsi });
  rsiSeries.createPriceLine({ price: 70, color: COLOR.textMuted, lineWidth: 1, lineStyle: LWC.LineStyle.Dotted, axisLabelVisible: false, title: "" });
  rsiSeries.createPriceLine({ price: 30, color: COLOR.textMuted, lineWidth: 1, lineStyle: LWC.LineStyle.Dotted, axisLabelVisible: false, title: "" });

  setupResizeHandle();
  wireTooltip();
  renderInstrumentBg();
  if (pendingAlerts) { applyAlerts(pendingAlerts); pendingAlerts = null; }
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
// Looks OHLCV up from latestBars by param.time, not from
// param.seriesData.get(candles/vol) the way this used to -- a
// deliberate change, not a style preference: lightweight-charts drops a
// visible:false series from seriesData on crosshair events, so the old
// series-lookup version would have gone silently blank the moment chart
// type switches to "line" (candles hidden). Same fix as
// superChartEngine.ts's own wireChartTooltip.
function wireTooltip() {
  var tipEl = document.createElement("div");
  tipEl.className = "chart-tip";
  el.appendChild(tipEl);
  function row(label, val, cls) {
    return '<div class="row"><span class="label">' + label + '</span><span class="val' + (cls ? " " + cls : "") + '">' + val + "</span></div>";
  }
  chart.subscribeCrosshairMove(function (param) {
    if (!param.point || !param.time) { tipEl.classList.remove("show"); return; }
    var bar = latestBars.find(function (b) { return b.time === param.time; });
    if (!bar) { tipEl.classList.remove("show"); return; }
    var up = bar.close >= bar.open;
    var d = new Date(param.time * 1000);
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
      row("Vol", fmtVol(bar.volume));
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
  area.setData(bars.map(function (b) { return { time: b.time, value: b.close }; }));
  vol.setData(bars.map(function (b, i) {
    var up = i === 0 || b.close >= bars[i - 1].close;
    return { time: b.time, value: b.volume, color: up ? "rgba(12,163,12,.38)" : "rgba(208,59,59,.38)" };
  }));
  ma9.setData(sma(bars, 9).map(function (p) { return { time: p.time, value: p.value }; }));
  ma20.setData(sma(bars, 20).map(function (p) { return { time: p.time, value: p.value }; }));
  vwapSeries.setData(vwap(bars).map(function (p) { return { time: p.time, value: p.value }; }));
  var bb = computeBollingerBands(bars);
  bbUpper.setData(bb.upper.map(function (p) { return { time: p.time, value: p.value }; }));
  bbLower.setData(bb.lower.map(function (p) { return { time: p.time, value: p.value }; }));
  var macd = computeMACD(bars);
  macdHist.setData(macd.hist.map(function (p) { return { time: p.time, value: p.value, color: p.color }; }));
  macdLine.setData(macd.macdLine.map(function (p) { return { time: p.time, value: p.value }; }));
  macdSignal.setData(macd.signalLine.map(function (p) { return { time: p.time, value: p.value }; }));
  rsiSeries.setData(computeRSI(bars).map(function (p) { return { time: p.time, value: p.value }; }));

  chart.timeScale().fitContent();
  updateCountdown(); // a fresh last bar can move the current bucket's boundary
}
window.__setBars = setBars;

// Candles and the line/area view are both created at mount and always
// kept fully populated -- toggling chart type just flips which one is
// visible, matching web's own api.setChartType exactly.
window.__setChartType = function (type) {
  chartType = type;
  if (!seriesReady) return; // ChartScreen.tsx always sends bars first, but fail safe rather than throw if that ever changes
  candles.applyOptions({ visible: type === "candles" });
  area.applyOptions({ visible: type === "line" });
};

// ---------- price-alert lines -- window.__setAlerts, mirrors
// window.__setBars/__applySettings. RN always sends the FULL current
// alert list for this symbol (not a diff), same "resend the whole
// snapshot" convention every other command channel here already uses. ----------
var alertPriceLines = [];
var pendingAlerts = null;
function applyAlerts(alerts) {
  if (!candles) { pendingAlerts = alerts; return; } // series not created yet -- ensureSeries() replays this once they are
  alertPriceLines.forEach(function (line) { candles.removePriceLine(line); });
  alertPriceLines = (alerts || []).map(function (a) {
    return candles.createPriceLine({
      price: a.targetPrice,
      color: a.direction === "above" ? COLOR.good : COLOR.critical,
      lineWidth: 1,
      lineStyle: LWC.LineStyle.Dashed,
      axisLabelVisible: true,
      title: "alert"
    });
  });
}
window.__setAlerts = applyAlerts;

// ---------- candle countdown -- window.__setTimeframe tells this page
// how many minutes wide the CURRENT bucket is (1D: whichever of 1/5/15
// is selected; 1W/1M: their own fixed bucket, see useChartBars.ts's
// RANGE_CONFIG) -- ChartScreen.tsx re-sends this on every range/
// timeframe change. The countdown itself just needs that one number
// plus the last bar's own timestamp (already in latestBars), so it
// ticks on its own setInterval rather than round-tripping to RN every
// second. ----------
var countdownEl = document.createElement("div");
countdownEl.className = "candle-countdown";
el.appendChild(countdownEl);
var bucketSeconds = 0;
function updateCountdown() {
  var last = latestBars[latestBars.length - 1];
  if (!bucketSeconds || !last) { countdownEl.classList.remove("show"); return; }
  var bucketEnd = last.time + bucketSeconds;
  var remaining = bucketEnd - Math.floor(Date.now() / 1000);
  // Hide rather than show a stuck "0:00" or a misleadingly-ticking
  // countdown once the feed has genuinely gone stale (market closed, no
  // new trades) -- a bar more than one whole bucket old is no longer
  // "the current candle", it's just the last one we have.
  if (remaining <= 0 || remaining > bucketSeconds) { countdownEl.classList.remove("show"); return; }
  var mins = Math.floor(remaining / 60), secs = remaining % 60;
  countdownEl.textContent = mins + ":" + pad2(secs);
  countdownEl.classList.add("show");
}
window.__setTimeframe = function (minutes) {
  bucketSeconds = minutes > 0 ? minutes * 60 : 0;
  updateCountdown();
};
setInterval(updateCountdown, 1000);

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
    // Toggling MACD/RSI here is a plain visibility flip with no margin
    // recompute, same as superChartEngine.ts's own decision -- the old
    // prototype-era "reclaim the whole bottom zone for price/volume"
    // hack assumed MACD was the only possible bottom oscillator, an
    // assumption RSI breaks (MACD off can't reclaim space RSI is still
    // using half of). Toggling one off leaves its half of the zone
    // blank rather than the other growing to fill it.
    if (typeof s.indicators.macd === "boolean") {
      macdHist.applyOptions({ visible: s.indicators.macd });
      macdLine.applyOptions({ visible: s.indicators.macd });
      macdSignal.applyOptions({ visible: s.indicators.macd });
    }
    if (typeof s.indicators.rsi === "boolean") {
      rsiSeries.applyOptions({ visible: s.indicators.rsi });
    }
    if (typeof s.indicators.bollinger === "boolean") {
      bbUpper.applyOptions({ visible: s.indicators.bollinger });
      bbLower.applyOptions({ visible: s.indicators.bollinger });
    }
  }
  if (typeof s.autoScale === "boolean") {
    chart.priceScale("right").applyOptions({ autoScale: s.autoScale });
  }
  if (typeof s.fitIndicators === "boolean") {
    [ma9, ma20, vwapSeries, bbUpper, bbLower].forEach(function (ser) {
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
