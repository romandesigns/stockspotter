// Real Super Chart engine, ported into a self-contained HTML page for
// react-native-webview -- lightweight-charts has no native React Native
// binding at all (it's a canvas/DOM library), so a WebView hosting the
// real chart is the standard pattern for this exact situation, not a
// workaround. This is the SAME engine as apps/client/src/lib/
// superChartEngine.ts (mountSuperChart/wireChartTooltip) and
// chartIndicators.ts (sma/vwap), ported to plain JS since a WebView's
// `source={{ html }}` needs a real self-contained page, not a TS module
// graph -- same function bodies/formulas/margins, one more hop removed
// from the original Artifact prototype (which was itself plain JS in a
// single HTML file), not re-derived from memory. If this ever drifts
// from the web version, diff this against superChartEngine.ts's
// mountSuperChart, not the other way around.
//
// Scoped down from the web version's `scanner` context per Roman's own
// "keep in mind the mobile context" instruction: candles + volume +
// MA9/MA20/VWAP overlays + the real OHLCV tooltip, always on, no
// toolbar/indicators-popover/MACD/resize-handle -- a phone screen has no
// room for that chrome, and a mobile chart is for glancing at price
// action, not the full desktop workstation toolset.

import { colors } from "./theme";

const CDN = "https://unpkg.com/lightweight-charts@4.1.3/dist/lightweight-charts.standalone.production.js";

// The page loads once per symbol with zero bars, then RN pushes real data
// in via injectJavaScript(window.__setBars(...)) once the page reports
// itself ready -- reloading the whole WebView (a new `source.html`) on
// every live tick would flash/reset zoom-pan state, so bars are never
// baked into the initial HTML string.
export function buildChartHtml(): string {
  return `<!doctype html>
<html>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, maximum-scale=1, user-scalable=no">
<style>
  html, body { margin: 0; padding: 0; background: ${colors.background}; overflow: hidden; height: 100%; }
  #chart { position: absolute; inset: 0; }
  .chart-tip {
    position: absolute; z-index: 20; pointer-events: none; opacity: 0; transition: opacity .1s;
    background: ${colors.surface}; border-radius: 8px; padding: 8px 10px; font: 11px -apple-system, sans-serif;
    color: ${colors.text}; box-shadow: 0 4px 14px rgba(0,0,0,.4); min-width: 130px;
  }
  .chart-tip.show { opacity: 1; }
  .chart-tip .time { color: ${colors.muted}; font-size: 10px; margin-bottom: 4px; }
  .chart-tip .row { display: flex; justify-content: space-between; gap: 12px; }
  .chart-tip .label { color: ${colors.muted}; }
  .chart-tip .val { font-family: monospace; }
  .chart-tip .val.up { color: ${colors.good}; }
  .chart-tip .val.down { color: ${colors.critical}; }
</style>
</head>
<body>
<div id="chart"></div>
<script src="${CDN}"></script>
<script>
// ---------- chartIndicators.ts, ported verbatim (sma/vwap only -- MACD
// isn't shown on the mobile chart, no need to port it here). ----------
function sma(bars, period) {
  var out = [], sum = 0;
  for (var i = 0; i < bars.length; i++) {
    sum += bars[i].close;
    if (i >= period) sum -= bars[i - period].close;
    if (i >= period - 1) out.push({ time: bars[i].time, value: +(sum / period).toFixed(3) });
  }
  return out;
}
function vwap(bars) {
  var out = [], cumPV = 0, cumV = 0;
  for (var i = 0; i < bars.length; i++) {
    var b = bars[i];
    var tp = (b.high + b.low + b.close) / 3;
    cumPV += tp * b.volume; cumV += b.volume;
    out.push({ time: b.time, value: cumV > 0 ? +(cumPV / cumV).toFixed(3) : 0 });
  }
  return out;
}

// ---------- superChartEngine.ts's mountSuperChart, ported verbatim,
// scoped to the shape apps/client calls the "backtest" preset (candles +
// volume + overlays, no MACD, no resize handle) since that's the real
// shape this mobile view uses. ----------
var LWC = LightweightCharts;
var COLOR = {
  textSecondary: "${colors.muted}", textMuted: "${colors.muted}", border: "${colors.divider}",
  good: "${colors.good}", critical: "${colors.critical}",
  s1: "#3987e5", s2: "#d95926", s3: "#9085e9"
};

function paneMargins() {
  var priceBottom = 0.05, volBand = 0.2;
  var volTop = Math.max(0.05, 1 - priceBottom - volBand);
  return { price: { top: 0.05, bottom: priceBottom }, vol: { top: volTop, bottom: priceBottom } };
}

var el = document.getElementById("chart");
var chart = LWC.createChart(el, {
  width: el.clientWidth, height: el.clientHeight,
  layout: { background: { type: "solid", color: "transparent" }, textColor: COLOR.textSecondary, fontFamily: "-apple-system, sans-serif", fontSize: 11 },
  grid: { vertLines: { visible: false }, horzLines: { visible: true, color: COLOR.border } },
  rightPriceScale: { visible: true, borderVisible: false, scaleMargins: paneMargins().price },
  timeScale: { visible: true, borderVisible: false, timeVisible: true, secondsVisible: false, rightOffset: 2 },
  crosshair: {
    mode: LWC.CrosshairMode.Normal,
    vertLine: { visible: true, labelVisible: true, color: COLOR.textMuted, width: 1 },
    horzLine: { visible: true, labelVisible: true, color: COLOR.textMuted, width: 1 }
  },
  handleScroll: true, handleScale: true
});

var series = {};
var vol = chart.addHistogramSeries({ priceFormat: { type: "volume" }, priceScaleId: "vol", color: COLOR.good });
series.volume = vol;
var candles = chart.addCandlestickSeries({ upColor: COLOR.good, downColor: COLOR.critical, borderVisible: false, wickUpColor: COLOR.good, wickDownColor: COLOR.critical });
series.candles = candles;
chart.priceScale("vol").applyOptions({ scaleMargins: paneMargins().vol });
series.ma9 = chart.addLineSeries({ color: COLOR.s1, lineWidth: 2, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });
series.ma20 = chart.addLineSeries({ color: COLOR.s2, lineWidth: 2, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });
series.vwap = chart.addLineSeries({ color: COLOR.s3, lineWidth: 2, lineStyle: LWC.LineStyle.Dashed, priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: false });

function setBars(bars) {
  if (!bars || bars.length === 0) return;
  candles.setData(bars.map(function (b) { return { time: b.time, open: b.open, high: b.high, low: b.low, close: b.close }; }));
  vol.setData(bars.map(function (b, i) {
    var up = i === 0 || b.close >= bars[i - 1].close;
    return { time: b.time, value: b.volume, color: up ? "rgba(12,163,12,.38)" : "rgba(208,59,59,.38)" };
  }));
  series.ma9.setData(sma(bars, 9));
  series.ma20.setData(sma(bars, 20));
  series.vwap.setData(vwap(bars));
  chart.timeScale().fitContent();
}
window.__setBars = setBars;

new ResizeObserver(function () { chart.applyOptions({ width: el.clientWidth, height: el.clientHeight }); }).observe(el);

if (window.ReactNativeWebView) window.ReactNativeWebView.postMessage("ready");

// ---------- superChartEngine.ts's wireChartTooltip, ported verbatim. ----------
var tipEl = document.createElement("div");
tipEl.className = "chart-tip";
document.body.appendChild(tipEl);
function fmtVol(n) {
  if (n >= 1000000) return (n / 1000000).toFixed(2) + "M";
  if (n >= 1000) return (n / 1000).toFixed(1) + "K";
  return String(n);
}
function row(label, val, cls) {
  return '<div class="row"><span class="label">' + label + '</span><span class="val' + (cls ? " " + cls : "") + '">' + val + "</span></div>";
}
chart.subscribeCrosshairMove(function (param) {
  if (!param.point || !param.time) { tipEl.classList.remove("show"); return; }
  var bar = param.seriesData.get(candles);
  if (!bar) { tipEl.classList.remove("show"); return; }
  var up = bar.close >= bar.open;
  var d = new Date(param.time * 1000);
  var volBar = param.seriesData.get(vol);
  tipEl.innerHTML =
    '<div class="time">' + d.toUTCString().slice(5, 22) + ' UTC</div>' +
    row("O", bar.open.toFixed(2)) + row("H", bar.high.toFixed(2)) + row("L", bar.low.toFixed(2)) +
    row("C", bar.close.toFixed(2), up ? "up" : "down") +
    (volBar ? row("Vol", fmtVol(volBar.value)) : "");
  tipEl.classList.add("show");
  var cw = el.clientWidth, ch = el.clientHeight, tw = tipEl.offsetWidth, th = tipEl.offsetHeight;
  var x = param.point.x + 14, y = param.point.y + 14;
  if (x + tw > cw) x = param.point.x - tw - 14;
  if (y + th > ch) y = param.point.y - th - 14;
  tipEl.style.left = Math.max(4, x) + "px";
  tipEl.style.top = Math.max(4, y) + "px";
});
</script>
</body>
</html>`;
}
