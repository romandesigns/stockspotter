// Speed alert settings (2026-10-16, Alerts.dc.html), opened from the bell
// on Home. Threshold plus four switches; persisted by useSpeedAlerts.ts.
// The phone push is labelled with its real threshold: ws-server pushes at
// a fixed 70% for now, whatever the in-app threshold says.
import { Pressable, Switch, View } from "react-native";
import type { SpeedAlertSettings } from "@stockspotter/shared-types";
import { SPEED_ALERT_THRESHOLDS } from "@stockspotter/shared-types";
import { BottomSheet } from "./ui/bottom-sheet";
import { Text } from "./ui/text";
import { cn } from "../lib/utils";
import { colors } from "../theme";
import { SPEED_PUSH_THRESHOLD } from "../useSpeedAlerts";

type ToggleKey = "upOnly" | "premarket" | "sound" | "push";
const TOGGLES: { key: ToggleKey; label: string; hint: string }[] = [
  { key: "upOnly", label: "Up moves only", hint: "" },
  { key: "premarket", label: "Premarket", hint: "from 4:00" },
  { key: "sound", label: "Sound", hint: "and vibration" },
  { key: "push", label: `Phone push (at ${SPEED_PUSH_THRESHOLD}%)`, hint: "via the server" },
];

export function SpeedAlertSettingsSheet(props: { visible: boolean; onClose: () => void; settings: SpeedAlertSettings; onChange: (next: SpeedAlertSettings) => void }) {
  const s = props.settings;
  return (
    <BottomSheet visible={props.visible} onClose={props.onClose} closeLabel="Close speed alert settings">
      <View className="gap-3.5 px-4 pb-6 pt-2">
        <Text className="text-sm font-semibold">Speed alerts</Text>
        <View className="gap-1.5">
          <Text className="text-xs">Alert when the gauge passes</Text>
          <View className="flex-row gap-1 rounded-lg bg-background p-[3px]" accessibilityRole="radiogroup" accessibilityLabel="Alert threshold">
            {SPEED_ALERT_THRESHOLDS.map((t) => {
              const active = s.threshold === t;
              return (
                <Pressable
                  key={t}
                  onPress={() => props.onChange({ ...s, threshold: t })}
                  className={cn("min-h-[44px] flex-1 items-center justify-center rounded-md", active && "bg-row")}
                  accessibilityRole="radio"
                  accessibilityState={{ selected: active }}
                >
                  <Text className={cn("font-mono text-xs", active ? "font-semibold text-text" : "text-muted")}>{t}%</Text>
                </Pressable>
              );
            })}
          </View>
          <Text variant="muted" className="text-[11px]">Only while it is still rising. Starting values, not yet tested.</Text>
        </View>
        {TOGGLES.map((t) => (
          <View key={t.key} className="min-h-[44px] flex-row items-center gap-2.5">
            <Text className="flex-1 text-[13px]">{t.label}</Text>
            {t.hint ? <Text variant="muted" className="text-[11px]">{t.hint}</Text> : null}
            <Switch
              value={s[t.key]}
              onValueChange={(v) => props.onChange({ ...s, [t.key]: v })}
              trackColor={{ false: colors.divider, true: colors.accentBg }}
              thumbColor={s[t.key] ? colors.accent : colors.muted}
              accessibilityLabel={t.label}
            />
          </View>
        ))}
        <Text variant="muted" className="text-[11px]">Once per stock every 10 min. In the app, an alert shows as a banner at the top of the screen.</Text>
      </View>
    </BottomSheet>
  );
}
