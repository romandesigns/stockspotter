// Mobile equivalent of SuperChart.tsx's Settings Popover -- same real
// content/copy/order (Auto-scale price axis / Fit all indicators /
// Scaling), a separate sheet from Indicators (see ChartIndicatorsSheet.tsx
// for why). Web's RadioGroup renders as stacked labeled rows (Radix's
// default vertical orientation, not a horizontal pill row -- ToggleGroup's
// compact pill styling wouldn't fit "Logarithmic (Price)" on a phone
// anyway), so the scale-mode picker here is three selectable rows too,
// matching that real shape rather than mobile's own horizontal
// ToggleGroup primitive.
import { Modal, Pressable, StyleSheet, View } from "react-native";
import { Text } from "./ui/text";
import { Button } from "./ui/button";
import { Separator } from "./ui/separator";
import { colors } from "../theme";

export type ScaleMode = "linear" | "percent" | "log";
export type ChartType = "candles" | "line";

const SCALE_OPTIONS: { value: ScaleMode; label: string }[] = [
  { value: "linear", label: "Linear (Price)" },
  { value: "percent", label: "Linear (Percentage)" },
  { value: "log", label: "Logarithmic (Price)" },
];

const CHART_TYPE_OPTIONS: { value: ChartType; label: string }[] = [
  { value: "candles", label: "Candlestick" },
  { value: "line", label: "Line" },
];

export function ChartSettingsSheet(props: {
  visible: boolean;
  onClose: () => void;
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
    <Modal visible={props.visible} transparent animationType="slide" onRequestClose={props.onClose}>
      <Pressable style={styles.backdrop} onPress={props.onClose} accessibilityRole="button" accessibilityLabel="Close chart settings" />
      <View style={styles.sheet}>
        {/* Same placement as Robinhood's own chart-settings gear icon
            (their first option) -- borrowed per Roman's explicit ask,
            not a prototype carryover. */}
        <Text className="px-4 pt-3.5 pb-1 text-[13px] font-semibold">Chart type</Text>
        <View className="px-4 pb-1 pt-1">
          {CHART_TYPE_OPTIONS.map((opt) => {
            const selected = opt.value === props.chartType;
            return (
              <Pressable key={opt.value} onPress={() => props.onChartTypeChange(opt.value)} className="flex-row items-center gap-2.5 py-2.5">
                <View style={[styles.radioOuter, selected && styles.radioOuterOn]}>
                  {selected && <View style={styles.radioInner} />}
                </View>
                <Text className="text-[13px]">{opt.label}</Text>
              </Pressable>
            );
          })}
        </View>
        <Separator className="mx-4" />
        <Text className="px-4 pt-3 pb-1 text-[13px] font-semibold">Auto-scale</Text>
        <ToggleRow label="Auto-scale price axis" value={props.autoScale} onChange={props.onAutoScaleChange} />
        <Separator className="mx-4" />
        <Text className="px-4 pt-3 pb-1 text-[13px] font-semibold">Fit to chart</Text>
        <ToggleRow label="Fit all indicators" value={props.fitIndicators} onChange={props.onFitIndicatorsChange} />
        <Separator className="mx-4" />
        <Text className="px-4 pt-3 pb-1 text-[13px] font-semibold">Scaling</Text>
        <View className="px-4 pb-5 pt-1">
          {SCALE_OPTIONS.map((opt) => {
            const selected = opt.value === props.scaleMode;
            return (
              <Pressable key={opt.value} onPress={() => props.onScaleModeChange(opt.value)} className="flex-row items-center gap-2.5 py-2.5">
                <View style={[styles.radioOuter, selected && styles.radioOuterOn]}>
                  {selected && <View style={styles.radioInner} />}
                </View>
                <Text className="text-[13px]">{opt.label}</Text>
              </Pressable>
            );
          })}
        </View>
      </View>
    </Modal>
  );
}

function ToggleRow(props: { label: string; value: boolean; onChange: (v: boolean) => void }) {
  return (
    <View className="flex-row items-center gap-2.5 px-4 py-2.5">
      <Text className="flex-1 text-[13px]">{props.label}</Text>
      <Button variant={props.value ? "default" : "outline"} size="sm" onPress={() => props.onChange(!props.value)}>
        <Text variant={props.value ? "accent" : "muted"} className="text-[11px] font-semibold">
          {props.value ? "On" : "Off"}
        </Text>
      </Button>
    </View>
  );
}

const styles = StyleSheet.create({
  backdrop: { position: "absolute", left: 0, right: 0, top: 0, bottom: 0, backgroundColor: "rgba(0,0,0,0.5)" },
  sheet: { position: "absolute", left: 0, right: 0, bottom: 0, backgroundColor: colors.row, borderTopLeftRadius: 16, borderTopRightRadius: 16 },
  radioOuter: { width: 16, height: 16, borderRadius: 8, borderWidth: 1.5, borderColor: colors.divider, alignItems: "center", justifyContent: "center" },
  radioOuterOn: { borderColor: colors.accent },
  radioInner: { width: 8, height: 8, borderRadius: 4, backgroundColor: colors.accent },
});
