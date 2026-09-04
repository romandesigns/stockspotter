// Consolidated Indicators + Display settings (2026-09-03, Roman's
// explicit ask: "Consolidate indicators and settings into one modal or
// bottom sheet page"). Opened via the gear icon in the toolbar (see
// ChartScreen.tsx) -- split back out from the original one-sheet-does-
// everything ChartMenuSheet after a real follow-up correction: "the
// mobile settings consolidation looks and works as expected but...
// [it] should display this menu only after clicking on the gear icon.
// Pressing and holding on the chart... should then show the alarm
// widget popover" (ChartAlertsSheet.tsx, its own separate trigger now).
// Content itself isn't re-derived -- same rows/copy/controls this
// sheet already had when alerts lived in it too.
import { Pressable, View } from "react-native";
import { Text } from "./ui/text";
import { Button } from "./ui/button";
import { Separator } from "./ui/separator";
import { BottomSheet } from "./ui/bottom-sheet";
import { colors } from "../theme";
import type { ChartType, IndicatorVisibility, ScaleMode } from "../useChartSettings";

const INDICATOR_ROWS: { key: keyof IndicatorVisibility; label: string; color: string }[] = [
  { key: "ma9", label: "MA9", color: "#3987e5" },
  { key: "ma20", label: "MA20", color: "#d95926" },
  { key: "vwap", label: "VWAP", color: "#9085e9" },
  { key: "macd", label: "MACD", color: "#c98500" },
  { key: "rsi", label: "RSI", color: "#2ec4b6" },
  { key: "bollinger", label: "Bollinger Bands", color: "#7c93a8" },
];

const CHART_TYPE_OPTIONS: { value: ChartType; label: string }[] = [
  { value: "candles", label: "Candlestick" },
  { value: "line", label: "Line" },
];

const SCALE_OPTIONS: { value: ScaleMode; label: string }[] = [
  { value: "linear", label: "Linear (Price)" },
  { value: "percent", label: "Linear (Percentage)" },
  { value: "log", label: "Logarithmic (Price)" },
];

export function ChartSettingsSheet(props: {
  visible: boolean;
  onClose: () => void;
  indicators: IndicatorVisibility;
  onToggleIndicator: (key: keyof IndicatorVisibility, next: boolean) => void;
  chartType: ChartType;
  onChartTypeChange: (v: ChartType) => void;
  autoScale: boolean;
  onAutoScaleChange: (v: boolean) => void;
  fitIndicators: boolean;
  onFitIndicatorsChange: (v: boolean) => void;
  scaleMode: ScaleMode;
  onScaleModeChange: (v: ScaleMode) => void;
}) {
  return (
    <BottomSheet visible={props.visible} onClose={props.onClose} closeLabel="Close chart settings">
      <Text className="px-4 pt-1 pb-1 text-[13px] font-semibold">Indicators</Text>
      <View className="px-4 pb-1 pt-1">
        {INDICATOR_ROWS.map((row) => {
          const on = props.indicators[row.key];
          return (
            <View key={row.key} className="flex-row items-center gap-2.5 py-2.5">
              <View style={{ width: 10, height: 10, borderRadius: 5, backgroundColor: row.color }} />
              <Text className="flex-1 text-[13px]">{row.label}</Text>
              <Button variant={on ? "default" : "outline"} size="sm" onPress={() => props.onToggleIndicator(row.key, !on)} accessibilityLabel={`${row.label} ${on ? "on" : "off"}`}>
                <Text variant={on ? "accent" : "muted"} className="text-[11px] font-semibold">{on ? "On" : "Off"}</Text>
              </Button>
            </View>
          );
        })}
      </View>

      <Separator className="mx-4" />
      <Text className="px-4 pt-3 pb-1 text-[13px] font-semibold">Chart type</Text>
      <View className="px-4 pb-1 pt-1">
        {CHART_TYPE_OPTIONS.map((opt) => (
          <RadioRow key={opt.value} label={opt.label} selected={opt.value === props.chartType} onPress={() => props.onChartTypeChange(opt.value)} />
        ))}
      </View>

      <Separator className="mx-4" />
      <Text className="px-4 pt-3 pb-1 text-[13px] font-semibold">Display</Text>
      <ToggleRow label="Auto-scale price axis" value={props.autoScale} onChange={props.onAutoScaleChange} />
      <ToggleRow label="Fit all indicators" value={props.fitIndicators} onChange={props.onFitIndicatorsChange} />

      <Separator className="mx-4" />
      <Text className="px-4 pt-3 pb-1 text-[13px] font-semibold">Scaling</Text>
      <View className="px-4 pb-6 pt-1">
        {SCALE_OPTIONS.map((opt) => (
          <RadioRow key={opt.value} label={opt.label} selected={opt.value === props.scaleMode} onPress={() => props.onScaleModeChange(opt.value)} />
        ))}
      </View>
    </BottomSheet>
  );
}

function RadioRow(props: { label: string; selected: boolean; onPress: () => void }) {
  return (
    <Pressable onPress={props.onPress} className="flex-row items-center gap-2.5 py-2.5" accessibilityRole="radio" accessibilityState={{ selected: props.selected }}>
      <View style={{ width: 16, height: 16, borderRadius: 8, borderWidth: 1.5, borderColor: props.selected ? colors.accent : colors.divider, alignItems: "center", justifyContent: "center" }}>
        {props.selected && <View style={{ width: 8, height: 8, borderRadius: 4, backgroundColor: colors.accent }} />}
      </View>
      <Text className="text-[13px]">{props.label}</Text>
    </Pressable>
  );
}

function ToggleRow(props: { label: string; value: boolean; onChange: (v: boolean) => void }) {
  return (
    <View className="flex-row items-center gap-2.5 px-4 py-2.5">
      <Text className="flex-1 text-[13px]">{props.label}</Text>
      <Button variant={props.value ? "default" : "outline"} size="sm" onPress={() => props.onChange(!props.value)} accessibilityLabel={`${props.label} ${props.value ? "on" : "off"}`}>
        <Text variant={props.value ? "accent" : "muted"} className="text-[11px] font-semibold">{props.value ? "On" : "Off"}</Text>
      </Button>
    </View>
  );
}
