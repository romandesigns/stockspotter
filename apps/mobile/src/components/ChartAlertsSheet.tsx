// Mobile equivalent of the toolbar's "Create alert" bolt -- unlike
// web's own button (still a genuine no-op placeholder there, see
// SuperChart.tsx), this one is real: opens this sheet to set/remove
// price alerts for the symbol currently on screen. A real OS
// notification fires (usePriceAlerts.ts) the instant a live bar crosses
// the armed level, even after this chart is closed or the app is
// backgrounded.
import { useState } from "react";
import { Modal, Pressable, StyleSheet, TextInput, View } from "react-native";
import * as Haptics from "expo-haptics";
import { Text } from "./ui/text";
import { Button } from "./ui/button";
import { Separator } from "./ui/separator";
import { colors, monoFont } from "../theme";
import type { PriceAlert } from "../priceAlerts";

export function ChartAlertsSheet(props: {
  visible: boolean;
  onClose: () => void;
  symbol: string;
  currentPrice: number | null;
  alerts: PriceAlert[];
  onAdd: (targetPrice: number) => void;
  onRemove: (id: string) => void;
}) {
  const [draft, setDraft] = useState("");
  const parsed = Number(draft);
  const valid = draft.trim() !== "" && Number.isFinite(parsed) && parsed > 0;

  function submit() {
    if (!valid) return;
    Haptics.impactAsync(Haptics.ImpactFeedbackStyle.Light);
    props.onAdd(parsed);
    setDraft("");
  }

  return (
    <Modal visible={props.visible} transparent animationType="slide" onRequestClose={props.onClose}>
      <Pressable style={styles.backdrop} onPress={props.onClose} accessibilityRole="button" accessibilityLabel="Close price alerts" />
      <View style={styles.sheet}>
        <Text className="px-4 pt-3.5 pb-1 text-[13px] font-semibold">Price alerts · {props.symbol}</Text>
        {props.currentPrice != null && (
          <Text variant="muted" className="px-4 pb-2 text-[11px]">
            Current price ${props.currentPrice.toFixed(props.currentPrice < 1 ? 4 : 2)}
          </Text>
        )}

        {props.alerts.length > 0 && (
          <>
            <View className="px-4 pb-1">
              {props.alerts.map((alert) => (
                <View key={alert.id} className="flex-row items-center gap-2.5 py-2">
                  <Text style={{ color: alert.direction === "above" ? colors.good : colors.critical }} className="text-xs">
                    {alert.direction === "above" ? "▲" : "▼"}
                  </Text>
                  <Text mono className="flex-1 text-[13px]">
                    ${alert.targetPrice.toFixed(alert.targetPrice < 1 ? 4 : 2)}
                  </Text>
                  <Pressable
                    onPress={() => { Haptics.impactAsync(Haptics.ImpactFeedbackStyle.Light); props.onRemove(alert.id); }}
                    hitSlop={10}
                    accessibilityRole="button"
                    accessibilityLabel={`Remove alert at $${alert.targetPrice}`}
                  >
                    <Text variant="muted" className="text-base">✕</Text>
                  </Pressable>
                </View>
              ))}
            </View>
            <Separator className="mx-4" />
          </>
        )}

        <View className="flex-row items-center gap-2.5 px-4 py-3.5">
          <TextInput
            value={draft}
            onChangeText={setDraft}
            placeholder="Target price"
            placeholderTextColor={colors.muted}
            keyboardType="decimal-pad"
            style={styles.input}
            onSubmitEditing={submit}
            returnKeyType="done"
          />
          <Button variant="default" size="sm" disabled={!valid} onPress={submit} className={valid ? undefined : "opacity-40"}>
            <Text variant="accent" className="text-[12px] font-semibold">Add</Text>
          </Button>
        </View>
        <Text variant="muted" className="px-4 pb-5 text-[11px]">
          {props.alerts.length === 0
            ? `Fires a notification the instant ${props.symbol} crosses this price — works even if you've closed this chart.`
            : "Direction (above/below) is set automatically from where price is right now."}
        </Text>
      </View>
    </Modal>
  );
}

const styles = StyleSheet.create({
  backdrop: { position: "absolute", left: 0, right: 0, top: 0, bottom: 0, backgroundColor: "rgba(0,0,0,0.5)" },
  sheet: { position: "absolute", left: 0, right: 0, bottom: 0, backgroundColor: colors.row, borderTopLeftRadius: 16, borderTopRightRadius: 16 },
  input: {
    flex: 1, fontFamily: monoFont, fontSize: 13, color: colors.text,
    backgroundColor: colors.surface, borderRadius: 8, borderWidth: 1, borderColor: colors.divider,
    paddingHorizontal: 10, paddingVertical: 8,
  },
});
