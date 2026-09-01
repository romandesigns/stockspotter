// Full-screen chart view -- opens over the tab content when a ticker is
// tapped anywhere in the app (Radar/Watchlist/Markets), the mobile
// equivalent of the web app's click-a-ticker-to-load-the-chart feature.
// A simple state-driven overlay (App.tsx sets/clears selectedSymbol),
// not a new navigation library -- consistent with how tabs themselves
// are already just local state in this app, not routes.
//
// Real chart engine (ChartScreen.tsx's own doc comment in chartHtml.ts
// has the full story) hosted in a WebView -- lightweight-charts has no
// native React Native binding, a WebView is the standard way to host a
// canvas/DOM charting library in RN, not a workaround.

import { useEffect, useRef, useState } from "react";
import { ActivityIndicator, Pressable, StyleSheet, Text, View } from "react-native";
import { WebView } from "react-native-webview";
import { buildChartHtml } from "./chartHtml";
import { useChartBars } from "./useChartBars";
import { colors, monoFont } from "./theme";
import type { BarUpdate } from "@stockspotter/shared-types";

const HTML = buildChartHtml();

export function ChartScreen(props: { symbol: string; liveBars: BarUpdate[]; onClose: () => void }) {
  const bars = useChartBars(props.symbol, props.liveBars);
  const webviewRef = useRef<WebView>(null);
  const [ready, setReady] = useState(false);

  useEffect(() => { setReady(false); }, [props.symbol]);

  useEffect(() => {
    if (!ready || bars.length === 0) return;
    webviewRef.current?.injectJavaScript(`window.__setBars(${JSON.stringify(bars)}); true;`);
  }, [ready, bars]);

  const last = bars[bars.length - 1];
  const first = bars[0];
  const changePct = last && first && first.open !== 0 ? ((last.close - first.open) / first.open) * 100 : 0;
  const up = changePct >= 0;

  return (
    <View style={styles.screen}>
      <View style={styles.header}>
        <Pressable onPress={props.onClose} hitSlop={12} accessibilityRole="button" accessibilityLabel="Close chart">
          <Text style={styles.back}>‹</Text>
        </Pressable>
        <Text style={styles.symbol}>{props.symbol}</Text>
        <View style={styles.headerSpacer} />
        {last && (
          <>
            <Text style={styles.price}>${last.close.toFixed(last.close < 1 ? 4 : 2)}</Text>
            <Text style={[styles.change, up ? styles.up : styles.down]}>{up ? "+" : ""}{changePct.toFixed(1)}%</Text>
          </>
        )}
      </View>
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
          onMessage={(e) => { if (e.nativeEvent.data === "ready") setReady(true); }}
          javaScriptEnabled
          scrollEnabled={false}
          bounces={false}
        />
      </View>
    </View>
  );
}

const styles = StyleSheet.create({
  screen: { position: "absolute", left: 0, right: 0, top: 0, bottom: 0, backgroundColor: colors.background, zIndex: 50 },
  header: { flexDirection: "row", alignItems: "center", paddingHorizontal: 14, paddingTop: 16, paddingBottom: 10, gap: 10 },
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
