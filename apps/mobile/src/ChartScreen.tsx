// Full-screen chart view -- opens over the tab content when a ticker is
// tapped anywhere in the app (Radar/Watchlist/Markets), the mobile
// equivalent of the web app's click-a-ticker-to-load-the-chart feature.
// A simple state-driven overlay (App.tsx sets/clears selectedSymbol),
// not a new navigation library -- consistent with how tabs themselves
// are already just local state in this app, not routes.
//
// Real chart engine (chartHtml.ts's own doc comment has the full story)
// hosted in a WebView, now running the FULL Super Chart -- candles,
// volume, MA9/MA20, VWAP, MACD, resize handle, real pinch/pan/zoom, and
// an OHLC crosshair tooltip.
//
// Redesigned 2026-09-03 per Roman's own real-usage friction list:
// - Quick-jump row (top 3 bullish momentum + top 2 halt-risk symbols)
//   right under the header, so switching to check another mover doesn't
//   mean leaving the chart -- mobile-only (web's dashboard already shows
//   the equivalent panels alongside the chart at the same time).
// - The old icon toolbar (Indicators/Settings/Alerts buttons) is gone.
//   A long-press anywhere on the chart opens one consolidated
//   ChartMenuSheet instead (see chartHtml.ts for where the long-press
//   is actually detected -- inside the WebView's own touch handling,
//   not an RN gesture wrapping it).
// - Chart settings (indicators/autoScale/fitIndicators/scaleMode/
//   chartType) are now owned by App.tsx (useChartSettings.ts,
//   AsyncStorage-persisted) instead of local useState here -- survives
//   both switching symbols via a quick-jump chip and a full close/
//   reopen, which local state to this component never could.
// - useSafeKeepAwake() for as long as this screen is mounted -- the
//   phone shouldn't sleep or lock while a live chart is open. Wraps
//   expo-keep-awake defensively (see useSafeKeepAwake.ts's own doc
//   comment) since it's a real native module an OTA update can't
//   deliver to an already-installed binary -- no-ops safely until a
//   real native rebuild ships it, rather than crashing on import.
//
// Rendered as position:"absolute" covering the full device bounds (see
// styles.screen below) rather than laid out inside App.tsx's own
// SafeAreaView -- an absolutely positioned child in RN is NOT affected
// by an ancestor's padding, which is why this screen wraps its own root
// in its OWN SafeAreaView instead.
import { useEffect, useMemo, useRef, useState } from "react";
import { ActivityIndicator, Pressable, ScrollView, StyleSheet, Text, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { WebView, type WebViewMessageEvent } from "react-native-webview";
import { useSafeKeepAwake } from "./useSafeKeepAwake";
import { buildChartHtml } from "./chartHtml";
import { useChartBars, RANGE_CONFIG, type ChartRange } from "./useChartBars";
import { resample } from "./chartIndicators";
import { colors, monoFont } from "./theme";
import { ToggleGroup } from "./components/ui/toggle-group";
import { ChartMenuSheet } from "./components/ChartMenuSheet";
import { MomentumScoreRow } from "./components/MomentumScoreRow";
import type { ChartSettings } from "./useChartSettings";
import type { AlertDirection, PriceAlert } from "./priceAlerts";
import type { BarUpdate, HaltWarning, MomentumUpdate } from "@stockspotter/shared-types";

const HTML = buildChartHtml();
const RANGE_OPTIONS: { value: ChartRange; label: string }[] = [
  { value: "1D", label: "1D" },
  { value: "1W", label: "1W" },
  { value: "1M", label: "1M" },
];
const TIMEFRAMES = [1, 5, 15] as const;
type Timeframe = (typeof TIMEFRAMES)[number];
const TIMEFRAME_OPTIONS: { value: string; label: string }[] = TIMEFRAMES.map((tf) => ({ value: String(tf), label: `${tf}m` }));

const CHART_HEIGHT = 320;

export function ChartScreen(props: {
  symbol: string;
  liveBars: BarUpdate[];
  momentum: MomentumUpdate | null;
  alerts: PriceAlert[]; // pre-filtered to this symbol -- at most one "above" + one "below"
  onSetAlert: (direction: AlertDirection, targetPrice: number) => void;
  onToggleAlert: (direction: AlertDirection, enabled: boolean) => void;
  onClearAlert: (direction: AlertDirection) => void;
  onClose: () => void;
  onSelectSymbol: (symbol: string) => void;
  chartSettings: ChartSettings;
  onToggleIndicator: (key: keyof ChartSettings["indicators"], next: boolean) => void;
  onAutoScaleChange: (v: boolean) => void;
  onFitIndicatorsChange: (v: boolean) => void;
  onScaleModeChange: (v: ChartSettings["scaleMode"]) => void;
  onChartTypeChange: (v: ChartSettings["chartType"]) => void;
  bullishTop: MomentumUpdate[];
  haltTop: HaltWarning[];
}) {
  useSafeKeepAwake();

  const [range, setRange] = useState<ChartRange>("1D");
  const [timeframe, setTimeframe] = useState<Timeframe>(1);
  const bars = useChartBars(props.symbol, props.liveBars, range);
  const bucketMinutes = range === "1D" ? timeframe : RANGE_CONFIG[range].bucketMinutes;
  const displayBars = useMemo(() => (range === "1D" ? resample(bars, timeframe) : bars), [bars, range, timeframe]);

  const { indicators, autoScale, fitIndicators, scaleMode, chartType } = props.chartSettings;
  const [menuOpen, setMenuOpen] = useState(false);

  const webviewRef = useRef<WebView>(null);
  const [ready, setReady] = useState(false);

  useEffect(() => {
    if (!ready || displayBars.length === 0) return;
    webviewRef.current?.injectJavaScript(`window.__setBars(${JSON.stringify(displayBars)}); true;`);
  }, [ready, displayBars]);

  useEffect(() => {
    if (!ready) return;
    webviewRef.current?.injectJavaScript(
      `window.__applySettings(${JSON.stringify({ indicators, autoScale, fitIndicators, scaleMode })}); true;`,
    );
  }, [ready, indicators, autoScale, fitIndicators, scaleMode]);

  useEffect(() => {
    if (!ready) return;
    webviewRef.current?.injectJavaScript(`window.__setTimeframe(${bucketMinutes}); true;`);
  }, [ready, bucketMinutes]);

  useEffect(() => {
    if (!ready) return;
    webviewRef.current?.injectJavaScript(`window.__setChartType(${JSON.stringify(chartType)}); true;`);
  }, [ready, chartType]);

  const armedAlerts = useMemo(() => props.alerts.filter((a) => a.enabled), [props.alerts]);
  useEffect(() => {
    if (!ready) return;
    const payload = armedAlerts.map((a) => ({ targetPrice: a.targetPrice, direction: a.direction }));
    webviewRef.current?.injectJavaScript(`window.__setAlerts(${JSON.stringify(payload)}); true;`);
  }, [ready, armedAlerts]);

  const onMessage = (e: WebViewMessageEvent) => {
    const raw = e.nativeEvent.data;
    if (raw === "ready") { setReady(true); return; } // tolerate the pre-JSON message shape too
    try {
      const msg = JSON.parse(raw) as { type: string };
      if (msg.type === "ready") setReady(true);
      // Opens the consolidated menu -- see chartHtml.ts for where this
      // is actually detected (a real touchstart/touchend timer inside
      // the WebView's own JS, not an RN gesture).
      else if (msg.type === "longpress") setMenuOpen(true);
    } catch { /* ignore malformed messages */ }
  };

  const last = bars[bars.length - 1];
  const first = bars[0];
  const displayPrice = last?.close;
  const changePct = displayPrice != null && first && first.open !== 0 ? ((displayPrice - first.open) / first.open) * 100 : 0;
  const up = changePct >= 0;

  // Combined quick-jump row -- one horizontal list, not two separate
  // rows, saving the vertical space the "no scrolling" redesign needs.
  // A tap swaps the chart's symbol IN PLACE (App.tsx's setSelectedSymbol
  // via onSelectSymbol), never routing through close+reopen, so chart
  // settings above survive the jump the same way they survive any
  // other symbol switch.
  const quickJump = useMemo(() => {
    const bullish = props.bullishTop.map((m) => ({ symbol: m.symbol, kind: "bullish" as const, value: m.overall }));
    const halt = props.haltTop.map((h) => ({ symbol: h.symbol, kind: "halt" as const, value: h.proximityRatio }));
    return [...bullish, ...halt].filter((q) => q.symbol !== props.symbol);
  }, [props.bullishTop, props.haltTop, props.symbol]);

  return (
    <SafeAreaView style={styles.screen} edges={["top", "left", "right"]}>
      <View style={styles.header}>
        <Pressable onPress={props.onClose} hitSlop={12} accessibilityRole="button" accessibilityLabel="Close chart">
          <Text style={styles.back}>‹</Text>
        </Pressable>
        <Text style={styles.symbol}>{props.symbol}</Text>
        <View style={styles.headerSpacer} />
        {displayPrice != null && (
          <>
            <Text style={styles.price}>${displayPrice.toFixed(displayPrice < 1 ? 4 : 2)}</Text>
            <Text style={[styles.change, up ? styles.up : styles.down]}>{up ? "▲" : "▼"} {up ? "+" : ""}{changePct.toFixed(1)}%</Text>
          </>
        )}
      </View>

      {quickJump.length > 0 && (
        <ScrollView horizontal showsHorizontalScrollIndicator={false} style={styles.quickJumpRow} contentContainerStyle={styles.quickJumpContent}>
          {quickJump.map((q) => (
            <Pressable
              key={`${q.kind}-${q.symbol}`}
              style={styles.chip}
              onPress={() => props.onSelectSymbol(q.symbol)}
              accessibilityRole="button"
              accessibilityLabel={`Jump to ${q.symbol}, ${q.kind === "bullish" ? "bullish momentum" : "halt risk"}`}
            >
              <View style={[styles.chipDot, q.kind === "bullish" ? styles.chipDotBullish : styles.chipDotHalt]} />
              <Text style={styles.chipSymbol}>{q.symbol}</Text>
              <Text style={styles.chipValue}>{q.kind === "bullish" ? Math.round(q.value * 100) : `${Math.round(q.value * 100)}%`}</Text>
            </Pressable>
          ))}
        </ScrollView>
      )}

      {/* Long-press anywhere on the chart opens the consolidated menu --
          no icon toolbar row here anymore, see ChartMenuSheet.tsx. */}
      <View style={[styles.chartWrap, { height: CHART_HEIGHT }]}>
        {bars.length === 0 && (
          <View style={styles.loading}>
            <ActivityIndicator color={colors.accent} />
            <Text style={styles.loadingText}>Loading bars for {props.symbol}…</Text>
          </View>
        )}
        <WebView
          ref={webviewRef}
          style={styles.webview}
          originWhitelist={["*"]}
          source={{ html: HTML }}
          onMessage={onMessage}
          javaScriptEnabled
          scrollEnabled={false}
          bounces={false}
        />
      </View>

      <View style={[styles.toolbarRow, styles.timeControlsRow]}>
        <ToggleGroup options={RANGE_OPTIONS} value={range} onChange={setRange} />
        {range === "1D" && <ToggleGroup options={TIMEFRAME_OPTIONS} value={String(timeframe)} onChange={(v) => setTimeframe(Number(v) as Timeframe)} />}
      </View>

      <ScrollView style={styles.momentumScroll} contentContainerStyle={styles.momentumContent}>
        <MomentumScoreRow symbol={props.symbol} momentum={props.momentum} bars={bars} />
      </ScrollView>

      <ChartMenuSheet
        visible={menuOpen}
        onClose={() => setMenuOpen(false)}
        symbol={props.symbol}
        indicators={indicators}
        onToggleIndicator={props.onToggleIndicator}
        chartType={chartType}
        onChartTypeChange={props.onChartTypeChange}
        autoScale={autoScale}
        onAutoScaleChange={props.onAutoScaleChange}
        fitIndicators={fitIndicators}
        onFitIndicatorsChange={props.onFitIndicatorsChange}
        scaleMode={scaleMode}
        onScaleModeChange={props.onScaleModeChange}
        currentPrice={displayPrice ?? null}
        alerts={props.alerts}
        onSetAlert={props.onSetAlert}
        onToggleAlert={props.onToggleAlert}
        onClearAlert={props.onClearAlert}
      />
    </SafeAreaView>
  );
}

const styles = StyleSheet.create({
  screen: { position: "absolute", left: 0, right: 0, top: 0, bottom: 0, backgroundColor: colors.background, zIndex: 50 },
  header: { flexDirection: "row", alignItems: "center", paddingHorizontal: 14, paddingTop: 8, paddingBottom: 8, gap: 10 },
  back: { color: colors.text, fontSize: 30, fontWeight: "300", lineHeight: 30, marginTop: -4 },
  symbol: { color: colors.text, fontFamily: monoFont, fontSize: 18, fontWeight: "700" },
  headerSpacer: { flex: 1 },
  price: { color: colors.text, fontFamily: monoFont, fontSize: 15, fontWeight: "600" },
  change: { fontFamily: monoFont, fontSize: 13, marginLeft: 8 },
  up: { color: colors.good }, down: { color: colors.critical },
  quickJumpRow: { flexGrow: 0, marginBottom: 6 },
  quickJumpContent: { paddingHorizontal: 14, gap: 6 },
  chip: { flexDirection: "row", alignItems: "center", gap: 5, paddingHorizontal: 8, paddingVertical: 5, borderRadius: 14, borderWidth: 1, borderColor: colors.divider },
  chipDot: { width: 6, height: 6, borderRadius: 3 },
  chipDotBullish: { backgroundColor: colors.good },
  chipDotHalt: { backgroundColor: colors.warning },
  chipSymbol: { color: colors.text, fontFamily: monoFont, fontSize: 11, fontWeight: "700" },
  chipValue: { color: colors.muted, fontFamily: monoFont, fontSize: 10 },
  toolbarRow: { flexDirection: "row", alignItems: "center", paddingHorizontal: 14, paddingBottom: 8, gap: 8 },
  timeControlsRow: { justifyContent: "flex-start", paddingTop: 8 },
  chartWrap: { position: "relative" },
  webview: { flex: 1, backgroundColor: colors.background },
  loading: { position: "absolute", left: 0, right: 0, top: 0, bottom: 0, alignItems: "center", justifyContent: "center", gap: 10, zIndex: 1 },
  loadingText: { color: colors.muted, fontSize: 12 },
  momentumScroll: { flex: 1 },
  momentumContent: { paddingBottom: 16 },
});
