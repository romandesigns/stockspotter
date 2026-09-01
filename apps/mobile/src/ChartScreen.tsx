// Full-screen chart view -- opens over the tab content when a ticker is
// tapped anywhere in the app (Radar/Watchlist/Markets), the mobile
// equivalent of the web app's click-a-ticker-to-load-the-chart feature.
// A simple state-driven overlay (App.tsx sets/clears selectedSymbol),
// not a new navigation library -- consistent with how tabs themselves
// are already just local state in this app, not routes.
//
// Real chart engine (chartHtml.ts's own doc comment has the full story)
// hosted in a WebView -- lightweight-charts has no native React Native
// binding, a WebView is the standard way to host a canvas/DOM charting
// library in RN, not a workaround.
//
// Rendered as position:"absolute" covering the full device bounds (see
// styles.screen below) rather than laid out inside App.tsx's own
// SafeAreaView -- an absolutely positioned child in RN is NOT affected
// by an ancestor's padding (it's sized/positioned against the parent's
// full border box, ignoring padding), which is exactly why the header
// used to draw under the status bar/notch: `top: 0` here really did mean
// the literal top pixel of the device, the outer SafeAreaView's own
// inset padding never applied to it at all. Wrapping this screen's own
// root in its OWN SafeAreaView (not double-applying an inset -- this
// box was never receiving one) is the real fix, confirmed against how
// RN absolute positioning actually works, not a guess.
import { useEffect, useRef, useState } from "react";
import { ActivityIndicator, Pressable, StyleSheet, Text, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { WebView, type WebViewMessageEvent } from "react-native-webview";
import { buildChartHtml } from "./chartHtml";
import { useChartBars, type ChartRange } from "./useChartBars";
import { colors, monoFont } from "./theme";
import { ToggleGroup } from "./components/ui/toggle-group";
import type { BarUpdate } from "@stockspotter/shared-types";

const HTML = buildChartHtml();
const RANGE_OPTIONS: { value: ChartRange; label: string }[] = [
  { value: "1D", label: "1D" },
  { value: "1W", label: "1W" },
  { value: "1M", label: "1M" },
];

export function ChartScreen(props: { symbol: string; liveBars: BarUpdate[]; onClose: () => void }) {
  const [range, setRange] = useState<ChartRange>("1D");
  const bars = useChartBars(props.symbol, props.liveBars, range);
  const webviewRef = useRef<WebView>(null);
  const [ready, setReady] = useState(false);
  // Robinhood's own behavior: the header's big price/% follows your
  // finger while scrubbing the chart, then reverts once you lift it --
  // no separate floating tooltip box. `scrub` holds the live value
  // pushed up from chartHtml.ts's crosshair handler; null means "not
  // currently scrubbing", i.e. show the real last price.
  const [scrub, setScrub] = useState<number | null>(null);

  useEffect(() => { setReady(false); setScrub(null); }, [props.symbol]);
  useEffect(() => { setScrub(null); }, [range]);

  useEffect(() => {
    if (!ready || bars.length === 0) return;
    webviewRef.current?.injectJavaScript(`window.__setBars(${JSON.stringify(bars)}); true;`);
  }, [ready, bars]);

  const onMessage = (e: WebViewMessageEvent) => {
    const raw = e.nativeEvent.data;
    if (raw === "ready") { setReady(true); return; } // tolerate the pre-JSON message shape too
    try {
      const msg = JSON.parse(raw) as { type: string; value?: number | null };
      if (msg.type === "ready") setReady(true);
      else if (msg.type === "scrub") setScrub(typeof msg.value === "number" ? msg.value : null);
    } catch { /* ignore malformed messages */ }
  };

  const last = bars[bars.length - 1];
  const first = bars[0];
  const displayPrice = scrub ?? last?.close;
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
      <ToggleGroup className="px-3.5 pb-2.5" options={RANGE_OPTIONS} value={range} onChange={setRange} />
      <View style={styles.chartWrap}>
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
  chartWrap: { flex: 1 },
  webview: { flex: 1, backgroundColor: colors.background },
  loading: { position: "absolute", left: 0, right: 0, top: 0, bottom: 0, alignItems: "center", justifyContent: "center", gap: 10, zIndex: 1 },
  loadingText: { color: colors.muted, fontSize: 12 },
});
