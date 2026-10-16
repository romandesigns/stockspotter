// Ring gauge for the Home hub and speed alerts (2026-10-16, Hub.dc.html /
// Phone.dc.html). Drawn with react-native-svg -- already a direct
// dependency (gifted-charts renders through it) -- because the mockup's
// ring starts at the bottom and sweeps clockwise, which PressureGauge's
// gifted-charts PieChart cannot do (see that file's header). PressureGauge
// stays as is for the Alerts tab, RiskStrip and HaltMiniCard.
import * as React from "react";
import { Text, View } from "react-native";
import Svg, { Circle, Path } from "react-native-svg";
import { colors, monoFont } from "../theme";

const R = 24;
const C = 2 * Math.PI * R;

export function RingGauge(props: { pct: number; color: string; size?: number; strokeWidth?: number; label?: string; labelSize?: number; accessibilityLabel?: string }) {
  const size = props.size ?? 44;
  const stroke = props.strokeWidth ?? 7;
  const pct = Math.max(0, Math.min(100, props.pct));
  const label = props.label ?? `${Math.round(pct)}`;
  return (
    <View
      style={{ width: size, height: size }}
      accessible={props.accessibilityLabel !== undefined}
      accessibilityLabel={props.accessibilityLabel}
    >
      <Svg width={size} height={size} viewBox="0 0 58 58">
        <Circle cx={29} cy={29} r={R} fill="none" stroke={colors.background} strokeWidth={stroke} />
        {pct > 0 && (
          <Circle
            cx={29}
            cy={29}
            r={R}
            fill="none"
            stroke={props.color}
            strokeWidth={stroke}
            strokeLinecap="round"
            strokeDasharray={`${((C * pct) / 100).toFixed(1)} ${C.toFixed(1)}`}
            rotation={90}
            origin="29, 29"
          />
        )}
      </Svg>
      <View style={{ position: "absolute", top: 0, left: 0, right: 0, bottom: 0, alignItems: "center", justifyContent: "center" }}>
        <Text style={{ fontFamily: monoFont, fontSize: props.labelSize ?? Math.round(size * 0.25), fontWeight: "700", color: props.color }}>{label}</Text>
      </View>
    </View>
  );
}

/** Bell glyph (the mockup's lucide-style outline), for the speed-alert button. */
export function BellIcon(props: { size?: number; color: string }) {
  const size = props.size ?? 18;
  return (
    <Svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke={props.color} strokeWidth={2} strokeLinecap="round" strokeLinejoin="round">
      <Path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9" />
      <Path d="M10.3 21a1.94 1.94 0 0 0 3.4 0" />
    </Svg>
  );
}
