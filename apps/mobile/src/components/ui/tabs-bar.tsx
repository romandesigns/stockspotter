// RNR-style bottom tab bar chrome -- App.tsx's own BottomTabs keeps its
// app-specific glyph/label/alert-dot logic, but the generic "row of
// equal-flex pressable tab items with an active-state color switch" is
// pulled out into this real, reusable primitive.
import * as React from "react";
import { Pressable, Text, View } from "react-native";
import { cn } from "../../lib/utils";

export interface TabBarItem<T extends string> {
  key: T;
  label: string;
  glyph: string;
}

export function TabsBar<T extends string>(props: {
  items: TabBarItem<T>[];
  active: T;
  onChange: (key: T) => void;
  renderBadge?: (key: T) => React.ReactNode;
  className?: string;
}) {
  return (
    <View className={cn("min-h-[70px] flex-row border-t border-border bg-background pb-2", props.className)} accessibilityRole="tablist">
      {props.items.map((item) => {
        const selected = props.active === item.key;
        return (
          <Pressable
            key={item.key}
            onPress={() => props.onChange(item.key)}
            className="min-h-[60px] flex-1 items-center justify-center"
            accessibilityRole="tab"
            accessibilityState={{ selected }}
          >
            <View>
              <Text className={cn("h-[22px] text-center text-lg", selected ? "font-semibold text-text" : "text-muted")}>
                {item.glyph}
              </Text>
              {props.renderBadge?.(item.key)}
            </View>
            <Text className={cn("mt-0.5 text-[9px]", selected ? "font-semibold text-text" : "text-muted")}>
              {item.label}
            </Text>
          </Pressable>
        );
      })}
    </View>
  );
}
