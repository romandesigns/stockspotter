// Mobile equivalent of the web app's radial Halt Early-Warning gauge
// (apps/client/src/components/PressureGauge.tsx) -- ported via
// react-native-gifted-charts' PieChart in donut mode, the real
// Expo-Go-compatible chart library for this project (victory-native's
// current Skia-based rewrite needs a custom dev client, breaking Expo
// Go, which this project's whole mobile workflow depends on).
//
// This APPROXIMATES the web gauge's spirit -- filled-proportion ring,
// colored by direction (bullish good / bearish critical), dark neutral
// track, centered % label -- rather than pixel-matching its bottom-
// start, direction-mirrored sweep. gifted-charts' PieChart has no
// start-angle/mirrored-sweep primitive the way Recharts' RadialBarChart
// (used on web) does; `initialAngle` rotates the whole ring but doesn't
// give independent per-direction sweep control. Flagged here explicitly
// per this task's own scope, not silently shipped as equivalent.
import * as React from "react";
import { Text } from "react-native";
import { PieChart } from "react-native-gifted-charts";
import type { HaltWarning } from "@stockspotter/shared-types";
import { colors, monoFont } from "../theme";

export function PressureGauge(props: { reading: HaltWarning; size?: number }) {
  const size = props.size ?? 44;
  const bullish = props.reading.currentPrice >= props.reading.referencePrice; // same derivation as web's HaltPanel
  const pct = Math.min(100, Math.round(props.reading.proximityRatio * 100));
  const color = bullish ? colors.good : colors.critical;

  return (
    <PieChart
      data={[
        { value: pct, color },
        { value: 100 - pct, color: colors.background },
      ]}
      donut
      radius={size / 2}
      innerRadius={size / 2 - 6}
      initialAngle={90} // starts at the top, matching the web gauge's own pre-mockup default
      centerLabelComponent={() => (
        <Text style={{ fontFamily: monoFont, fontSize: size * 0.24, fontWeight: "700", color }}>
          {pct}%
        </Text>
      )}
    />
  );
}
