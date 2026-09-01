// RNR-style segmented control -- collapses two near-identical hand-rolled
// implementations that used to live as separate StyleSheet blocks
// (App.tsx's Today/Yesterday datePreset pair, ChartScreen.tsx's
// 1D/1W/1M rangeTab row) into one real reusable primitive. A genuine
// simplification, not a re-skin: two copies of the same UI pattern
// become one.
import * as React from "react";
import { Pressable, Text, View } from "react-native";
import { cn } from "../../lib/utils";

export interface ToggleGroupOption<T extends string> {
  value: T;
  label: string;
}

export function ToggleGroup<T extends string>(props: {
  options: ToggleGroupOption<T>[];
  value: T;
  onChange: (value: T) => void;
  className?: string;
}) {
  return (
    <View className={cn("flex-row gap-1.5", props.className)}>
      {props.options.map((opt) => {
        const active = opt.value === props.value;
        return (
          <Pressable
            key={opt.value}
            onPress={() => props.onChange(opt.value)}
            className={cn("rounded-full px-2.5 py-1", active ? "bg-accent-bg" : "bg-panel")}
          >
            <Text className={cn("font-mono text-[10px] font-semibold", active ? "text-accent" : "text-muted")}>
              {opt.label}
            </Text>
          </Pressable>
        );
      })}
    </View>
  );
}
