// Small trend preview for Top Gainers / Most Active / Markets rows --
// react-native-gifted-charts' LineChart in its most minimal form (no
// axes, no rules, no data points), gradient-filled area matching the
// web app's own sparkline treatment (superChartEngine.ts's compact
// mode / MarketsTodayPanel's IndexCard).
//
// Real scope decision, not a silent gap: this renders ONLY from
// feed.barsBySymbol -- the live WS bar_update stream useRealtimeFeed
// already holds -- with zero new network calls. Fetching real history
// per-row for up to 25 Top-Gainers + 25 Most-Active rows on every 60s
// poll would mean up to ~50 new simultaneous REST calls, a real
// performance/API-load cost this pass doesn't take on. A symbol with no
// (or too little) live bar history renders NOTHING -- not a flat line,
// not a placeholder shape -- matching this project's own established
// "don't fabricate data" rule (see buildWatchlistRows's own doc comment
// in derive.ts making the identical argument for a different field).
// This means sparklines appear on whichever subset of rows the live
// universe scanner happens to be actively broadcasting bars for, and are
// visibly absent elsewhere -- accepted as this task's real scope.
import * as React from "react";
import { LineChart } from "react-native-gifted-charts";
import type { BarUpdate } from "@stockspotter/shared-types";
import { colors } from "../theme";

export function Sparkline(props: { bars: BarUpdate[]; width?: number; height?: number }) {
  if (props.bars.length < 2) return null; // honest fallback -- no chart, not a fake one

  const width = props.width ?? 56;
  const height = props.height ?? 22;
  const data = props.bars.map((b) => ({ value: b.close }));
  const up = props.bars[props.bars.length - 1].close >= props.bars[0].close;
  const color = up ? colors.good : colors.critical;

  return (
    <LineChart
      data={data}
      width={width}
      height={height}
      thickness={1.5}
      color={color}
      curved
      areaChart
      startFillColor={color}
      endFillColor={color}
      startOpacity={0.25}
      endOpacity={0}
      hideDataPoints
      hideAxesAndRules
      hideYAxisText
      disableScroll
      initialSpacing={0}
      endSpacing={0}
      adjustToWidth
    />
  );
}
