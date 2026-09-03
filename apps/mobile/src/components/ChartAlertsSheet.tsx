// Mobile equivalent of the toolbar's "Create alert" bolt -- unlike
// web's own button (still a genuine no-op placeholder there, see
// SuperChart.tsx), this one is real, and modeled directly on Robinhood's
// own real flow per Roman's explicit ask: two independent rows, "Price
// moves above" / "Price moves below", each a toggle switch plus its own
// editable price -- not an open-ended list of arbitrary alerts. A real
// OS notification fires (usePriceAlerts.ts) the instant a live bar
// crosses an armed level, even after this chart is closed or the app is
// backgrounded.
import { useState } from "react";
import { Pressable, Modal, StyleSheet, Switch, TextInput, View } from "react-native";
import * as Haptics from "expo-haptics";
import { Text } from "./ui/text";
import { Button } from "./ui/button";
import { Separator } from "./ui/separator";
import { colors, monoFont } from "../theme";
import type { AlertDirection, PriceAlert } from "../priceAlerts";

export function ChartAlertsSheet(props: {
  visible: boolean;
  onClose: () => void;
  symbol: string;
  currentPrice: number | null;
  alerts: PriceAlert[]; // pre-filtered to this symbol -- at most one per direction
  onSet: (direction: AlertDirection, targetPrice: number) => void;
  onToggle: (direction: AlertDirection, enabled: boolean) => void;
  onClear: (direction: AlertDirection) => void;
}) {
  const above = props.alerts.find((a) => a.direction === "above") ?? null;
  const below = props.alerts.find((a) => a.direction === "below") ?? null;

  return (
    <Modal visible={props.visible} transparent animationType="slide" onRequestClose={props.onClose}>
      <Pressable style={styles.backdrop} onPress={props.onClose} accessibilityRole="button" accessibilityLabel="Close price alerts" />
      <View style={styles.sheet}>
        <Text className="px-4 pt-3.5 pb-1 text-[13px] font-semibold">Price alerts · {props.symbol}</Text>
        {props.currentPrice != null && (
          <Text variant="muted" className="px-4 pb-1 text-[11px]">
            Current price ${props.currentPrice.toFixed(props.currentPrice < 1 ? 4 : 2)}
          </Text>
        )}

        <AlertRow
          label="Price moves above" direction="above" color={colors.good}
          alert={above} currentPrice={props.currentPrice}
          onSet={(price) => props.onSet("above", price)}
          onToggle={(enabled) => props.onToggle("above", enabled)}
          onClear={() => props.onClear("above")}
        />
        <Separator className="mx-4" />
        <AlertRow
          label="Price moves below" direction="below" color={colors.critical}
          alert={below} currentPrice={props.currentPrice}
          onSet={(price) => props.onSet("below", price)}
          onToggle={(enabled) => props.onToggle("below", enabled)}
          onClear={() => props.onClear("below")}
        />

        <Text variant="muted" className="px-4 pb-5 pt-3 text-[11px]">
          Fires once each way it's armed — works even if you've closed this chart.
        </Text>
      </View>
    </Modal>
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

  // Toggling on with nothing configured yet has nothing to arm -- same
  // as Robinhood's own "switch it on, then Edit to give it a price"
  // two-step, collapsed into one tap here: flipping the switch on with
  // no price set just opens the price field directly.
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
        <Switch
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
            style={styles.input}
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

const styles = StyleSheet.create({
  backdrop: { position: "absolute", left: 0, right: 0, top: 0, bottom: 0, backgroundColor: "rgba(0,0,0,0.5)" },
  sheet: { position: "absolute", left: 0, right: 0, bottom: 0, backgroundColor: colors.row, borderTopLeftRadius: 16, borderTopRightRadius: 16 },
  input: {
    flex: 1, fontFamily: monoFont, fontSize: 13, color: colors.text,
    backgroundColor: colors.surface, borderRadius: 8, borderWidth: 1, borderColor: colors.divider,
    paddingHorizontal: 10, paddingVertical: 8,
  },
});
