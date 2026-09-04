// App-wide settings, first real content: the ignition push-notification
// toggle (2026-09-04, Roman: "I want to be able to turn this feature off
// on the phone if I want to"). Deliberately separate from
// ChartSettingsSheet.tsx -- that one is chart-display settings scoped to
// whichever symbol is open (indicators/scale/type), this one is real
// app-level preferences that apply regardless of what's on screen.
// Opened from the gear icon in AppHeader (App.tsx), not the chart's own
// gear (ChartScreen.tsx) -- two different gears, two different scopes,
// same real distinction Roman's own "the mobile settings consolidation"
// correction already drew between chart settings and everything else.
import { View } from "react-native";
import { Text } from "./ui/text";
import { Button } from "./ui/button";
import { BottomSheet } from "./ui/bottom-sheet";

export function AppSettingsSheet(props: { visible: boolean; onClose: () => void; ignitionPushEnabled: boolean; onIgnitionPushEnabledChange: (v: boolean) => void }) {
  return (
    <BottomSheet visible={props.visible} onClose={props.onClose} closeLabel="Close settings">
      <Text className="px-4 pt-1 pb-1 text-[13px] font-semibold">Notifications</Text>
      <View className="px-4 pb-4 pt-1">
        <View className="flex-row items-center gap-2.5 py-2.5">
          <View className="flex-1">
            <Text className="text-[13px]">Ignition push alerts</Text>
            <Text variant="muted" className="mt-0.5 text-[11px]">
              A real push notification for any symbol&apos;s confirmed ignition -- reaches this phone even locked or backgrounded, not just while
              the app is open.
            </Text>
          </View>
          <Button
            variant={props.ignitionPushEnabled ? "default" : "outline"}
            size="sm"
            onPress={() => props.onIgnitionPushEnabledChange(!props.ignitionPushEnabled)}
            accessibilityLabel={`Ignition push alerts ${props.ignitionPushEnabled ? "on" : "off"}`}
          >
            <Text variant={props.ignitionPushEnabled ? "accent" : "muted"} className="text-[11px] font-semibold">
              {props.ignitionPushEnabled ? "On" : "Off"}
            </Text>
          </Button>
        </View>
      </View>
    </BottomSheet>
  );
}
