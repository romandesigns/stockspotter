// Real Super Chart engine, ported into a self-contained HTML page for
// react-native-webview -- lightweight-charts has no native React Native
// binding at all (it's a canvas/DOM library), so a WebView hosting the
// real chart is the standard pattern for this exact situation, not a
// workaround. This is the SAME engine as apps/client/src/lib/
// superChartEngine.ts's mountSuperChart, ported to plain JS since a
// WebView's `source={{ html }}` needs a real self-contained page, not a
// TS module graph -- same function bodies, one more hop removed from the
// original Artifact prototype (itself plain JS in a single HTML file),
// not re-derived from memory.
//
// Redesigned per Roman's explicit "behave no different than Robinhood"
// ask -- this now reuses mountSuperChart's `compact` mode (the real,
// already-built area-series path used elsewhere for watchlist
// sparklines: gradient-filled line, colored by direction, no volume/MA/
// candle chrome at all) instead of the `backtest` mode's full
// candles+volume+overlays shape the first version used. That's the
// actual real difference between "a dense trading-desk chart" and "a
// Robinhood-style price chart" -- not a new chart, the SAME engine's
// other already-existing preset, picked correctly this time. Axes are
// hidden entirely (Robinhood shows no price/time labels on the main
// chart at all -- price is conveyed by the header instead), and the
// crosshair posts back to RN on every move so the header can update
// live while scrubbing, exactly like Robinhood's own big-number-follows-
// your-finger behavior -- no floating OHLC tooltip box, that's a
// desktop-trading-app pattern, not what was asked for here.

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
</style>
</head>
<body>
<div id="chart"></div>
<script src="${CDN}"></script>
<script>
// ---------- superChartEngine.ts's mountSuperChart, 'compact' mode
// ported verbatim (same area-series options/colors as the real
// mode === "compact" branch -- see that function's own doc comment). ----------
var LWC = LightweightCharts;
var COLOR = { textMuted: "${colors.muted}", good: "${colors.good}", critical: "${colors.critical}" };

var el = document.getElementById("chart");
var chart = LWC.createChart(el, {
  width: el.clientWidth, height: el.clientHeight,
  layout: { background: { type: "solid", color: "transparent" }, textColor: COLOR.textMuted, fontFamily: "-apple-system, sans-serif" },
  grid: { vertLines: { visible: false }, horzLines: { visible: false } },
  // Robinhood shows no axis labels on the main chart at all -- price and
  // time are conveyed by the header (ChartScreen.tsx), which updates
  // live from this page's own crosshair postMessage below.
  rightPriceScale: { visible: false, scaleMargins: { top: 0.1, bottom: 0.1 } },
  timeScale: { visible: false, rightOffset: 0 },
  crosshair: {
    mode: LWC.CrosshairMode.Magnet,
    vertLine: { visible: true, labelVisible: false, color: COLOR.textMuted, width: 1, style: LWC.LineStyle.Solid },
    horzLine: { visible: false, labelVisible: false }
  },
  handleScroll: false, handleScale: false
});

var area = null;
var refLine = null;

function setBars(bars) {
  if (!bars || bars.length === 0) return;
  var dir = bars[bars.length - 1].close >= bars[0].open;
  var lineColor = dir ? COLOR.good : COLOR.critical;

  if (area) chart.removeSeries(area);
  area = chart.addAreaSeries({
    lineColor: lineColor, lineWidth: 2,
    topColor: dir ? "rgba(12,163,12,.25)" : "rgba(208,59,59,.25)",
    bottomColor: "rgba(0,0,0,0)",
    priceLineVisible: false, lastValueVisible: false, crosshairMarkerVisible: true,
    crosshairMarkerRadius: 5, crosshairMarkerBorderColor: lineColor, crosshairMarkerBackgroundColor: "${colors.background}",
    crosshairMarkerBorderWidth: 2
  });
  area.setData(bars.map(function (b) { return { time: b.time, value: b.close }; }));

  // Robinhood's own subtle "starting point" reference line -- the price
  // this view's range actually opened at, so the fill color and this
  // line agree on what "up" or "down" means for whatever range is
  // currently selected (1D/1W/1M), not always "since market open".
  if (refLine) area.removePriceLine(refLine);
  refLine = area.createPriceLine({
    price: bars[0].open, color: COLOR.textMuted, lineWidth: 1, lineStyle: LWC.LineStyle.Dashed,
    axisLabelVisible: false, title: ""
  });

  chart.timeScale().fitContent();
}
window.__setBars = setBars;

new ResizeObserver(function () { chart.applyOptions({ width: el.clientWidth, height: el.clientHeight }); }).observe(el);

if (window.ReactNativeWebView) window.ReactNativeWebView.postMessage(JSON.stringify({ type: "ready" }));

// Posts the scrubbed price/time back to RN on every crosshair move so
// ChartScreen's header can show it live -- Robinhood's own behavior (the
// big number itself follows your finger), not a separate floating
// tooltip box.
chart.subscribeCrosshairMove(function (param) {
  if (!window.ReactNativeWebView) return;
  if (!param.point || !param.time || !area) {
    window.ReactNativeWebView.postMessage(JSON.stringify({ type: "scrub", value: null }));
    return;
  }
  var point = param.seriesData.get(area);
  if (!point) { window.ReactNativeWebView.postMessage(JSON.stringify({ type: "scrub", value: null })); return; }
  window.ReactNativeWebView.postMessage(JSON.stringify({ type: "scrub", value: point.value, time: param.time }));
});
</script>
</body>
</html>`;
}
