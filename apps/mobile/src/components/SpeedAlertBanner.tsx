// In-app speed alert (2026-10-16), styled like the notification in
// Phone.dc.html: a floating card at the top of the screen, app mark on the
// left, "SYM ▲ speed 72% · filling fast" over "+19.6% in 5 min at $0.5310
// · rel vol 5.2x". Tap opens the chart. Slides in and fades out with the
// built-in Animated API on transform/opacity only, so nothing under it
// moves; the card is absolutely positioned and never pushes layout.
import { useEffect, useRef, useState } from "react";
import { Animated, Pressable, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import type { SpeedAlert } from "@stockspotter/shared-types";
import { Text } from "./ui/text";
import { speedAlertBody, speedAlertTitle } from "../useSpeedAlerts";

export function SpeedAlertBanner(props: { alert: SpeedAlert | null; onOpen: (symbol: string) => void; onDismiss: () => void }) {
  // Keep the last alert rendered while the exit animation runs.
  const [shown, setShown] = useState<SpeedAlert | null>(props.alert);
  const anim = useRef(new Animated.Value(0)).current;
  // Absolutely positioned children ignore the SafeAreaView padding (same
  // note as ChartScreen.tsx), so the status-bar inset is added here.
  const insets = useSafeAreaInsets();

  useEffect(() => {
    if (props.alert) {
      setShown(props.alert);
      anim.setValue(0);
      Animated.timing(anim, { toValue: 1, duration: 200, useNativeDriver: true }).start();
    } else {
      Animated.timing(anim, { toValue: 0, duration: 160, useNativeDriver: true }).start(({ finished }) => {
        if (finished) setShown(null);
      });
    }
  }, [props.alert, anim]);

  if (!shown) return null;
  return (
    <Animated.View
      pointerEvents={props.alert ? "auto" : "none"}
      style={{
        position: "absolute",
        top: insets.top + 8,
        zIndex: 60, // above ChartScreen (50) and ShortlistDetail (40)
        left: 10,
        right: 10,
        opacity: anim,
        transform: [{ translateY: anim.interpolate({ inputRange: [0, 1], outputRange: [-16, 0] }) }],
      }}
    >
      <Pressable
        onPress={() => {
          props.onOpen(shown.symbol);
          props.onDismiss();
        }}
        accessibilityRole="alert"
        accessibilityLabel={`${speedAlertTitle(shown)}. ${speedAlertBody(shown)} Opens the chart.`}
        className="flex-row gap-2.5 rounded-2xl px-3.5 py-3"
        style={{ backgroundColor: "rgba(43,45,54,0.97)", elevation: 8, shadowColor: "#000", shadowOpacity: 0.45, shadowRadius: 15, shadowOffset: { width: 0, height: 10 } }}
      >
        <View className="h-[34px] w-[34px] items-center justify-center rounded-[9px] bg-accent">
          <Text className="text-[15px] font-extrabold text-background">S</Text>
        </View>
        <View className="min-w-0 flex-1 gap-0.5">
          <View className="flex-row">
            <Text variant="muted" className="flex-1 text-xs">stockspotter</Text>
            <Text variant="muted" className="text-xs">now</Text>
          </View>
          <Text className="text-sm font-bold">{speedAlertTitle(shown)}</Text>
          <Text variant="muted" className="text-[13px]">{speedAlertBody(shown)} Tap to open the chart.</Text>
        </View>
        <Pressable onPress={props.onDismiss} hitSlop={12} className="-mr-1 h-11 w-8 items-center justify-center" accessibilityRole="button" accessibilityLabel="Dismiss alert">
          <Text variant="muted" className="text-base">×</Text>
        </Pressable>
      </Pressable>
    </Animated.View>
  );
}
