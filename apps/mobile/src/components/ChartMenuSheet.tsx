// Consolidated Chart Page menu (2026-09-03, Roman's explicit ask:
// "Consolidate indicators and settings into one modal or bottom sheet
// page... arrange these sections in an intuitive manner, and clean UI
// design and accessibility") -- replaces the three separately-triggered
// ChartIndicatorsSheet/ChartSettingsSheet/ChartAlertsSheet with one
// sheet, sectioned (Indicators / Chart display / Price alerts, ordered
// by how often each is actually touched -- indicators and chart type
// first, alerts last since they're set-once-and-forget). Content itself
// isn't re-derived: each section's rows are the exact same
// controls/copy the three old sheets already had, just brought together
// under one BottomSheet instead of three separate Modals with their own
// duplicated backdrop/panel chrome. Opened via a long-press on the
// chart now, not an icon button (see ChartScreen.tsx/chartHtml.ts) --
// there's no dedicated trigger of its own here.
import { useState } from "react";
import { Pressable, Switch as RNSwitch, TextInput, View } from "react-native";
import * as Haptics from "expo-haptics";
import { Text } from "./ui/text";
import { Button } from "./ui/button";
import { Separator } from "./ui/separator";
import { BottomSheet } from "./ui/bottom-sheet";
import { colors, monoFont } from "../theme";
import type { ChartType, IndicatorVisibility, ScaleMode } from "../useChartSettings";
import type { AlertDirection, PriceAlert } from "../priceAlerts";

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

export function ChartMenuSheet(props: {
  visible: boolean;
  onClose: () => void;
  symbol: string;
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
  currentPrice: number | null;
  alerts: PriceAlert[]; // pre-filtered to this symbol
  onSetAlert: (direction: AlertDirection, targetPrice: number) => void;
  onToggleAlert: (direction: AlertDirection, enabled: boolean) => void;
  onClearAlert: (direction: AlertDirection) => void;
}) {
  const above = props.alerts.find((a) => a.direction === "above") ?? null;
  const below = props.alerts.find((a) => a.direction === "below") ?? null;

  return (
    <BottomSheet visible={props.visible} onClose={props.onClose} closeLabel="Close chart menu">
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
      <View className="px-4 pb-1 pt-1">
        {SCALE_OPTIONS.map((opt) => (
          <RadioRow key={opt.value} label={opt.label} selected={opt.value === props.scaleMode} onPress={() => props.onScaleModeChange(opt.value)} />
        ))}
      </View>

      <Separator className="mx-4" />
      <Text className="px-4 pt-3 pb-1 text-[13px] font-semibold">Price alerts · {props.symbol}</Text>
      {props.currentPrice != null && (
        <Text variant="muted" className="px-4 pb-1 text-[11px]">
          Current price ${props.currentPrice.toFixed(props.currentPrice < 1 ? 4 : 2)}
        </Text>
      )}
      <AlertRow
        label="Price moves above" direction="above" color={colors.good}
        alert={above} currentPrice={props.currentPrice}
        onSet={(price) => props.onSetAlert("above", price)}
        onToggle={(enabled) => props.onToggleAlert("above", enabled)}
        onClear={() => props.onClearAlert("above")}
      />
      <Separator className="mx-4" />
      <AlertRow
        label="Price moves below" direction="below" color={colors.critical}
        alert={below} currentPrice={props.currentPrice}
        onSet={(price) => props.onSetAlert("below", price)}
        onToggle={(enabled) => props.onToggleAlert("below", enabled)}
        onClear={() => props.onClearAlert("below")}
      />
      <Text variant="muted" className="px-4 pb-6 pt-3 text-[11px]">
        Fires once each way it's armed — works even if you've closed this chart.
      </Text>
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

function AlertRow(props: {
  label: string; direction: AlertDirection; color: string;
  alert: PriceAlert | null; currentPrice: number | null;
  onSet: (price: number) => void; onToggle: (enabled: boolean) => void; onClear: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const parsed = Number(draft);
  const valid = draft.trim() !== "" && Number.isFinite(parsed) && parsed > 0;

  function startEdit() {
    setDraft(props.alert ? String(props.alert.targetPrice) : props.currentPrice != null ? props.currentPrice.toFixed(2) : "");
    setEditing(true);
  }

  function save() {
    if (!valid) return;
    Haptics.impactAsync(Haptics.ImpactFeedbackStyle.Light);
    props.onSet(parsed);
    setEditing(false);
  }

  function onToggleSwitch(next: boolean) {
    if (next && !props.alert) { startEdit(); return; }
    Haptics.impactAsync(Haptics.ImpactFeedbackStyle.Light);
    props.onToggle(next);
  }

  return (
    <View className="px-4 py-3">
      <View className="flex-row items-center gap-2.5">
        <Text style={{ color: props.color }} className="text-xs">{props.direction === "above" ? "▲" : "▼"}</Text>
        <Text className="flex-1 text-[13px]">{props.label}</Text>
        <RNSwitch
          value={props.alert?.enabled ?? false}
          onValueChange={onToggleSwitch}
          trackColor={{ false: colors.divider, true: colors.accentBg }}
          thumbColor={props.alert?.enabled ? colors.accent : colors.muted}
        />
      </View>

      {editing ? (
        <View className="mt-2.5 flex-row items-center gap-2.5">
          <TextInput
            value={draft}
            onChangeText={setDraft}
            placeholder="Target price"
            placeholderTextColor={colors.muted}
            keyboardType="decimal-pad"
            style={{ flex: 1, fontFamily: monoFont, fontSize: 13, color: colors.text, backgroundColor: colors.surface, borderRadius: 8, borderWidth: 1, borderColor: colors.divider, paddingHorizontal: 10, paddingVertical: 8 }}
            autoFocus
            onSubmitEditing={save}
            returnKeyType="done"
          />
          <Button variant="default" size="sm" disabled={!valid} onPress={save} className={valid ? undefined : "opacity-40"}>
            <Text variant="accent" className="text-[12px] font-semibold">Save</Text>
          </Button>
          <Pressable onPress={() => setEditing(false)} hitSlop={10} accessibilityRole="button" accessibilityLabel="Cancel">
            <Text variant="muted" className="text-[12px]">Cancel</Text>
          </Pressable>
        </View>
      ) : (
        <Pressable
          className="mt-1.5 flex-row items-center justify-between"
          onPress={startEdit}
          accessibilityRole="button"
          accessibilityLabel={`Edit ${props.label.toLowerCase()} price`}
        >
          <Text mono variant={props.alert ? undefined : "muted"} className="text-[15px] font-semibold">
            {props.alert ? `$${props.alert.targetPrice.toFixed(props.alert.targetPrice < 1 ? 4 : 2)}` : "Not set"}
          </Text>
          <View className="flex-row items-center gap-3">
            {props.alert && (
              <Pressable onPress={props.onClear} hitSlop={10} accessibilityRole="button" accessibilityLabel={`Clear ${props.label.toLowerCase()} alert`}>
                <Text variant="muted" className="text-[11px]">Clear</Text>
              </Pressable>
            )}
            <Text variant="accent" className="text-[11px] font-semibold">Edit</Text>
          </View>
        </Pressable>
      )}
    </View>
  );
}
