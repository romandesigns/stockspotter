// Mobile equivalent of SuperChart.tsx's Indicators Popover -- Radix
// Popover has no RN equivalent, so this becomes its own bottom sheet (RN's
// built-in Modal, transparent + slide-from-bottom + backdrop Pressable to
// dismiss -- no new dependency, matching this app's own "don't add a
// library unless needed" discipline), kept as a SEPARATE sheet from
// Settings (below) rather than merged into one, matching web's real
// structure: two separate popovers, not one combined menu.
//
// Same 4 rows, same order, same colors, same copy as web's real
// Indicators popover (MA9/MA20/VWAP/MACD). No RN Switch exists in this
// app's ui/ set yet, so each row's on/off control is the existing Button
// primitive in a two-state on/off form -- same semantics, adapted widget.
import { Modal, Pressable, StyleSheet, View } from "react-native";
import { Text } from "./ui/text";
import { Button } from "./ui/button";
import { colors } from "../theme";

export interface IndicatorVisibility {
  ma9: boolean;
  ma20: boolean;
  vwap: boolean;
  macd: boolean;
  rsi: boolean;
  bollinger: boolean;
}

// RSI/Bollinger appended, same colors as web's new --series-6/7 -- not
// from the prototype, borrowed from Robinhood's Advanced Charts per
// Roman's explicit ask.
const ROWS: { key: keyof IndicatorVisibility; label: string; color: string }[] = [
  { key: "ma9", label: "MA9", color: "#3987e5" },
  { key: "ma20", label: "MA20", color: "#d95926" },
  { key: "vwap", label: "VWAP", color: "#9085e9" },
  { key: "macd", label: "MACD", color: "#c98500" },
  { key: "rsi", label: "RSI", color: "#2ec4b6" },
  { key: "bollinger", label: "Bollinger Bands", color: "#7c93a8" },
];

export function ChartIndicatorsSheet(props: {
  visible: boolean;
  onClose: () => void;
  values: IndicatorVisibility;
  onToggle: (key: keyof IndicatorVisibility, next: boolean) => void;
}) {
  return (
    <Modal visible={props.visible} transparent animationType="slide" onRequestClose={props.onClose}>
      <Pressable style={styles.backdrop} onPress={props.onClose} accessibilityRole="button" accessibilityLabel="Close indicators" />
      <View style={styles.sheet}>
        <Text className="px-4 pt-3.5 pb-1 text-[13px] font-semibold">Indicators</Text>
        <View className="px-4 pb-5 pt-1">
          {ROWS.map((row) => {
            const on = props.values[row.key];
            return (
              <View key={row.key} className="flex-row items-center gap-2.5 py-2.5">
                <View style={[styles.swatch, { backgroundColor: row.color }]} />
                <Text className="flex-1 text-[13px]">{row.label}</Text>
                <Button variant={on ? "default" : "outline"} size="sm" onPress={() => props.onToggle(row.key, !on)}>
                  <Text variant={on ? "accent" : "muted"} className="text-[11px] font-semibold">
                    {on ? "On" : "Off"}
                  </Text>
                </Button>
              </View>
            );
          })}
        </View>
      </View>
    </Modal>
  );
}

const styles = StyleSheet.create({
  backdrop: { position: "absolute", left: 0, right: 0, top: 0, bottom: 0, backgroundColor: "rgba(0,0,0,0.5)" },
  sheet: { position: "absolute", left: 0, right: 0, bottom: 0, backgroundColor: colors.row, borderTopLeftRadius: 16, borderTopRightRadius: 16 },
  swatch: { width: 10, height: 10, borderRadius: 5 },
});
