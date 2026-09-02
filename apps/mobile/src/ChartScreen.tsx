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
// an OHLC crosshair tooltip -- per Roman's explicit "consistent with what
// we've already established" correction (web's real SuperChart.tsx/
// superChartEngine.ts), superseding the earlier Robinhood-style compact/
// area-mode chart this file used to host.
//
// Toolbar mirrors web's real controls where RN has an equivalent, and
// substitutes only where it genuinely doesn't: Radix's two Popovers
// (Indicators, Settings) become two separate RN Modal sheets (see
// ChartIndicatorsSheet.tsx / ChartSettingsSheet.tsx) -- kept separate,
// not merged, matching web's real two-popover structure. Mobile's own
// 1D/1W/1M range picker (already real, already working, and something web
// itself doesn't have) stays as-is; a NEW 1m/5m/15m timeframe picker
// (real port of web's TIMEFRAMES) is layered in only for 1D, the one
// range with native 1-minute source data to usefully re-bucket. Web's
// disabled "1D" toolbar button and disabled "Create alert" button are
// NOT ported -- both are inert placeholders on web itself (no
// functionality either way), and web's own "1D" button is a different,
// not-yet-built concept (daily-resolution candles) from mobile's own
// real 1D/1W/1M range. Fullscreen isn't ported either -- this screen is
// already always full-screen, so the control has no meaning here.
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
import { buildChartHtml } from "./chartHtml";
import { useChartBars, type ChartRange } from "./useChartBars";
import { resample } from "./chartIndicators";
import { colors, monoFont } from "./theme";
import { ToggleGroup } from "./components/ui/toggle-group";
import { ChartIndicatorsSheet, type IndicatorVisibility } from "./components/ChartIndicatorsSheet";
import { ChartSettingsSheet, type ScaleMode } from "./components/ChartSettingsSheet";
import { MomentumScoreRow } from "./components/MomentumScoreRow";
import type { BarUpdate, MomentumUpdate } from "@stockspotter/shared-types";

const HTML = buildChartHtml();
const RANGE_OPTIONS: { value: ChartRange; label: string }[] = [
  { value: "1D", label: "1D" },
  { value: "1W", label: "1W" },
  { value: "1M", label: "1M" },
];
const TIMEFRAMES = [1, 5, 15] as const;
type Timeframe = (typeof TIMEFRAMES)[number];
const TIMEFRAME_OPTIONS: { value: string; label: string }[] = TIMEFRAMES.map((tf) => ({ value: String(tf), label: `${tf}m` }));

const CHART_HEIGHT = 360;

export function ChartScreen(props: { symbol: string; liveBars: BarUpdate[]; momentum: MomentumUpdate | null; onClose: () => void }) {
  const [range, setRange] = useState<ChartRange>("1D");
  const [timeframe, setTimeframe] = useState<Timeframe>(1);
  const bars = useChartBars(props.symbol, props.liveBars, range);
  // Header price/change and the momentum panel read the FULL raw bars,
  // not whatever timeframe pill is selected -- matches SuperChart.tsx's
  // own explicit reasoning (doesn't jump around when switching
  // timeframes). Only the chart itself gets the resampled view.
  const displayBars = useMemo(() => (range === "1D" ? resample(bars, timeframe) : bars), [bars, range, timeframe]);

  const [indicators, setIndicators] = useState<IndicatorVisibility>({ ma9: true, ma20: true, vwap: true, macd: true });
  const [autoScale, setAutoScale] = useState(true);
  const [fitIndicators, setFitIndicators] = useState(true);
  const [scaleMode, setScaleMode] = useState<ScaleMode>("linear");
  const [indicatorsOpen, setIndicatorsOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);

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

  const onMessage = (e: WebViewMessageEvent) => {
    const raw = e.nativeEvent.data;
    if (raw === "ready") { setReady(true); return; } // tolerate the pre-JSON message shape too
    try {
      const msg = JSON.parse(raw) as { type: string };
      if (msg.type === "ready") setReady(true);
    } catch { /* ignore malformed messages */ }
  };

  function toggleIndicator(key: keyof IndicatorVisibility, next: boolean) {
    setIndicators((prev) => ({ ...prev, [key]: next }));
  }

  const last = bars[bars.length - 1];
  const first = bars[0];
  const displayPrice = last?.close;
  const changePct = displayPrice != null && first && first.open !== 0 ? ((displayPrice - first.open) / first.open) * 100 : 0;
  const up = changePct >= 0;

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
            <Text style={[styles.change, up ? styles.up : styles.down]}>{up ? "+" : ""}{changePct.toFixed(1)}%</Text>
          </>
        )}
      </View>

      <View style={styles.toolbarRow}>
        <ToggleGroup options={RANGE_OPTIONS} value={range} onChange={setRange} />
        <View style={styles.toolbarSpacer} />
        <Pressable style={styles.iconButton} onPress={() => setIndicatorsOpen(true)} accessibilityRole="button" accessibilityLabel="Indicators">
          <Text style={styles.iconGlyph}>▤</Text>
        </Pressable>
        <Pressable style={styles.iconButton} onPress={() => setSettingsOpen(true)} accessibilityRole="button" accessibilityLabel="Chart display settings">
          <Text style={styles.iconGlyph}>⚙</Text>
        </Pressable>
      </View>
      {range === "1D" && (
        <View style={styles.toolbarRow}>
          <ToggleGroup options={TIMEFRAME_OPTIONS} value={String(timeframe)} onChange={(v) => setTimeframe(Number(v) as Timeframe)} />
        </View>
      )}

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

      <ScrollView style={styles.momentumScroll} contentContainerStyle={styles.momentumContent}>
        <MomentumScoreRow momentum={props.momentum} bars={bars} />
      </ScrollView>

      <ChartIndicatorsSheet visible={indicatorsOpen} onClose={() => setIndicatorsOpen(false)} values={indicators} onToggle={toggleIndicator} />
      <ChartSettingsSheet
        visible={settingsOpen}
        onClose={() => setSettingsOpen(false)}
        autoScale={autoScale}
        onAutoScaleChange={setAutoScale}
        fitIndicators={fitIndicators}
        onFitIndicatorsChange={setFitIndicators}
        scaleMode={scaleMode}
        onScaleModeChange={setScaleMode}
      />
    </SafeAreaView>
  );
}

const styles = StyleSheet.create({
  screen: { position: "absolute", left: 0, right: 0, top: 0, bottom: 0, backgroundColor: colors.background, zIndex: 50 },
  header: { flexDirection: "row", alignItems: "center", paddingHorizontal: 14, paddingTop: 8, paddingBottom: 10, gap: 10 },
  back: { color: colors.text, fontSize: 30, fontWeight: "300", lineHeight: 30, marginTop: -4 },
  symbol: { color: colors.text, fontFamily: monoFont, fontSize: 18, fontWeight: "700" },
  headerSpacer: { flex: 1 },
  price: { color: colors.text, fontFamily: monoFont, fontSize: 15, fontWeight: "600" },
  change: { fontFamily: monoFont, fontSize: 13, marginLeft: 8 },
  up: { color: colors.good }, down: { color: colors.critical },
  toolbarRow: { flexDirection: "row", alignItems: "center", paddingHorizontal: 14, paddingBottom: 8, gap: 8 },
  toolbarSpacer: { flex: 1 },
  iconButton: { width: 30, height: 30, borderRadius: 8, borderWidth: 1, borderColor: colors.divider, alignItems: "center", justifyContent: "center" },
  iconGlyph: { color: colors.muted, fontSize: 14 },
  chartWrap: { position: "relative" },
  webview: { flex: 1, backgroundColor: colors.background },
  loading: { position: "absolute", left: 0, right: 0, top: 0, bottom: 0, alignItems: "center", justifyContent: "center", gap: 10, zIndex: 1 },
  loadingText: { color: colors.muted, fontSize: 12 },
  momentumScroll: { flex: 1 },
  momentumContent: { paddingBottom: 24 },
});
