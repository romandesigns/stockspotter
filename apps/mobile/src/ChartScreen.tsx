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
// - The old three-icon toolbar (Indicators/Settings/Alerts buttons) is
//   gone, replaced by a single small gear icon in the header (opens
//   ChartSettingsSheet: indicators + chart type + display + scaling) and
//   a long-press anywhere on the chart (opens ChartAlertsSheet -- the
//   "alarm widget" -- see chartHtml.ts for where the long-press is
//   actually detected, inside the WebView's own touch handling, not an
//   RN gesture wrapping it). These two were originally ONE consolidated
//   sheet behind the long-press alone; split back into their own real
//   triggers 2026-09-03 per Roman's own follow-up correction once he'd
//   actually used it ("should display this menu only after clicking on
//   the gear icon... pressing and holding... should then show the alarm
//   widget popover").
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
import { useChartBars, useSubMinuteChartBars, RANGE_CONFIG, type ChartRange } from "./useChartBars";
import { resample } from "./chartIndicators";
import { colors, monoFont } from "./theme";
import { ToggleGroup } from "./components/ui/toggle-group";
import { ChartSettingsSheet } from "./components/ChartSettingsSheet";
import { ChartAlertsSheet } from "./components/ChartAlertsSheet";
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
/** "30s" is a real, distinct case, not a fifth minute-multiplier -- see
 * displayBars' own comment for why it can't go through resample(). */
type Timeframe = (typeof TIMEFRAMES)[number] | "30s";
const TIMEFRAME_OPTIONS: { value: string; label: string }[] = [
  // Real sub-minute (2026-09-03) -- live-only, no history below 1 minute
  // (confirmed live against Alpaca's own API), deliberately listed first/
  // most-granular rather than implying it's just another resampled
  // bucket like the rest.
  { value: "30s", label: "30s" },
  ...TIMEFRAMES.map((tf) => ({ value: String(tf), label: `${tf}m` })),
];

const CHART_HEIGHT = 320;

export function ChartScreen(props: {
  symbol: string;
  liveBars: BarUpdate[];
  subMinuteLiveBars: BarUpdate[];
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
  // Real sub-minute (30s) live-only bars -- a genuinely separate array
  // from `bars` above, not derivable from it (no history exists below 1
  // minute, confirmed live against Alpaca's own API). Only meaningful on
  // 1D (same reason the 1m/5m/15m pills themselves are 1D-only).
  const subMinuteBars = useSubMinuteChartBars(props.subMinuteLiveBars);
  const isSubMinute = range === "1D" && timeframe === "30s";
  const bucketMinutes = isSubMinute ? 0.5 : range === "1D" ? timeframe : RANGE_CONFIG[range].bucketMinutes;
  // "30s" bypasses resample() entirely -- that function can only ever
  // COARSEN already-1-minute-granular data, so the live-only
  // subMinuteBars array is used directly instead of being derived from
  // `bars`.
  const displayBars = useMemo(
    () => (isSubMinute ? subMinuteBars : range === "1D" ? resample(bars, timeframe as (typeof TIMEFRAMES)[number]) : bars),
    [bars, subMinuteBars, isSubMinute, range, timeframe],
  );

  const { indicators, autoScale, fitIndicators, scaleMode, chartType } = props.chartSettings;
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [alertsOpen, setAlertsOpen] = useState(false);

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
      // Opens the alarm widget (ChartAlertsSheet), not the settings menu
      // -- see chartHtml.ts for where this is actually detected (a real
      // touchstart/touchend timer inside the WebView's own JS, not an RN
      // gesture).
      else if (msg.type === "longpress") setAlertsOpen(true);
    } catch { /* ignore malformed messages */ }
  };

  const last = bars[bars.length - 1];
  const first = bars[0];
  const displayPrice = last?.close;
  const changePct = displayPrice != null && first && first.open !== 0 ? ((displayPrice - first.open) / first.open) * 100 : 0;
  const up = changePct >= 0;

  // Split into two separate rows (2026-09-04, Roman's own "their core
  // responsibilities aren't the same" call) -- these were one combined
  // row until now, but bullish momentum and halt risk aren't the same
  // kind of thing: bullish is a "here's what else looks strong, want to
  // jump there" discovery aid, which belongs right up top next to the
  // symbol/price it's comparing against. Halt risk is a "here's what's
  // under real pressure right now" read, which belongs right next to
  // this symbol's own scoring/assessment card (MomentumScoreRow) instead
  // -- the same place you're already looking to judge THIS symbol's own
  // condition. A tap on either still swaps the chart's symbol IN PLACE
  // (App.tsx's setSelectedSymbol via onSelectSymbol), same as before.
  const bullishJump = useMemo(
    () => props.bullishTop.map((m) => ({ symbol: m.symbol, value: m.overall })).filter((q) => q.symbol !== props.symbol),
    [props.bullishTop, props.symbol],
  );
  const haltJump = useMemo(
    () => props.haltTop.map((h) => ({ symbol: h.symbol, value: h.proximityRatio })).filter((q) => q.symbol !== props.symbol),
    [props.haltTop, props.symbol],
  );

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
        <Pressable
          style={styles.gearButton}
          onPress={() => setSettingsOpen(true)}
          hitSlop={8}
          accessibilityRole="button"
          accessibilityLabel="Chart settings"
        >
          <Text style={styles.gearGlyph}>⚙</Text>
        </Pressable>
      </View>

      {bullishJump.length > 0 && (
        <ScrollView horizontal showsHorizontalScrollIndicator={false} style={styles.quickJumpRow} contentContainerStyle={styles.quickJumpContent}>
          {bullishJump.map((q) => (
            <QuickJumpChip key={q.symbol} symbol={q.symbol} kind="bullish" value={q.value} onPress={() => props.onSelectSymbol(q.symbol)} />
          ))}
        </ScrollView>
      )}

      {/* Long-press anywhere on the chart opens the alarm widget
          (ChartAlertsSheet) -- the gear icon in the header above opens
          settings (ChartSettingsSheet) instead. No icon toolbar row
          here anymore. */}
      <View style={[styles.chartWrap, { height: CHART_HEIGHT }]}>
        {bars.length === 0 && (
          <View style={styles.loading}>
            <ActivityIndicator color={colors.accent} />
            <Text style={styles.loadingText}>Loading bars for {props.symbol}…</Text>
          </View>
        )}
        {bars.length > 0 && isSubMinute && subMinuteBars.length === 0 && (
          <View style={styles.loading}>
            <Text style={styles.loadingText}>Live — building 30s candles now, no history below 1 minute</Text>
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
        {range === "1D" && <ToggleGroup options={TIMEFRAME_OPTIONS} value={String(timeframe)} onChange={(v) => setTimeframe(v === "30s" ? "30s" : (Number(v) as Timeframe))} />}
      </View>

      <ScrollView style={styles.momentumScroll} contentContainerStyle={styles.momentumContent}>
        <MomentumScoreRow symbol={props.symbol} momentum={props.momentum} bars={bars} />
        {haltJump.length > 0 && (
          <ScrollView horizontal showsHorizontalScrollIndicator={false} style={styles.haltJumpRow} contentContainerStyle={styles.quickJumpContent}>
            {haltJump.map((q) => (
              <QuickJumpChip key={q.symbol} symbol={q.symbol} kind="halt" value={q.value} onPress={() => props.onSelectSymbol(q.symbol)} />
            ))}
          </ScrollView>
        )}
      </ScrollView>

      <ChartSettingsSheet
        visible={settingsOpen}
        onClose={() => setSettingsOpen(false)}
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
      />
      <ChartAlertsSheet
        visible={alertsOpen}
        onClose={() => setAlertsOpen(false)}
        symbol={props.symbol}
        currentPrice={displayPrice ?? null}
        alerts={props.alerts}
        onSet={props.onSetAlert}
        onToggle={props.onToggleAlert}
        onClear={props.onClearAlert}
      />
    </SafeAreaView>
  );
}

// Shared chip renderer for both rows (top bullish row, halt row under
// MomentumScoreRow) -- same visual, different tint/value formatting per
// kind, factored out once both rows needed it instead of duplicating
// the JSX a second time.
function QuickJumpChip(props: { symbol: string; kind: "bullish" | "halt"; value: number; onPress: () => void }) {
  const bullish = props.kind === "bullish";
  return (
    <Pressable
      style={[styles.chip, bullish ? styles.chipBullish : styles.chipHalt]}
      onPress={props.onPress}
      accessibilityRole="button"
      accessibilityLabel={`Jump to ${props.symbol}, ${bullish ? "bullish momentum" : "halt risk"}`}
    >
      <View style={[styles.chipDot, bullish ? styles.chipDotBullish : styles.chipDotHalt]} />
      <Text style={styles.chipSymbol}>{props.symbol}</Text>
      <Text style={[styles.chipValue, bullish ? styles.chipValueBullish : styles.chipValueHalt]}>
        {bullish ? Math.round(props.value * 100) : `${Math.round(props.value * 100)}%`}
      </Text>
    </Pressable>
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
  gearButton: { width: 28, height: 28, borderRadius: 8, borderWidth: 1, borderColor: colors.divider, alignItems: "center", justifyContent: "center", marginLeft: 10 },
  gearGlyph: { color: colors.muted, fontSize: 13 },
  quickJumpRow: { flexGrow: 0, marginBottom: 6 },
  haltJumpRow: { flexGrow: 0 },
  quickJumpContent: { paddingHorizontal: 14, gap: 6 },
  // Real, not-just-a-dot distinction between the two chip kinds (a 6px
  // dot alone was too subtle at this size to read at a glance -- same
  // tinted-background + colored-value language Badge.tsx already
  // establishes elsewhere in this app for good/warning, not a new one).
  chip: { flexDirection: "row", alignItems: "center", gap: 5, paddingHorizontal: 8, paddingVertical: 5, borderRadius: 14, borderWidth: 1 },
  chipBullish: { backgroundColor: colors.goodBg, borderColor: colors.good },
  chipHalt: { backgroundColor: colors.warningBg, borderColor: colors.warning },
  chipDot: { width: 6, height: 6, borderRadius: 3 },
  chipDotBullish: { backgroundColor: colors.good },
  chipDotHalt: { backgroundColor: colors.warning },
  chipSymbol: { color: colors.text, fontFamily: monoFont, fontSize: 11, fontWeight: "700" },
  chipValue: { fontFamily: monoFont, fontSize: 10, fontWeight: "700" },
  chipValueBullish: { color: colors.good },
  chipValueHalt: { color: colors.warning },
  toolbarRow: { flexDirection: "row", alignItems: "center", paddingHorizontal: 14, paddingBottom: 8, gap: 8 },
  timeControlsRow: { justifyContent: "flex-start", paddingTop: 8 },
  chartWrap: { position: "relative" },
  webview: { flex: 1, backgroundColor: colors.background },
  loading: { position: "absolute", left: 0, right: 0, top: 0, bottom: 0, alignItems: "center", justifyContent: "center", gap: 10, zIndex: 1 },
  loadingText: { color: colors.muted, fontSize: 12 },
  momentumScroll: { flex: 1 },
  momentumContent: { paddingBottom: 16 },
});
