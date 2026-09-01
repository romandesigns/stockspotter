// RNR-style Card -- replaces the ad-hoc bordered/padded View blocks
// (styles.section, styles.signalRow, styles.dataRow) that recur across
// every tab in App.tsx with one real, reusable primitive instead of a
// dozen near-duplicate StyleSheet entries.
import * as React from "react";
import { View, type ViewProps } from "react-native";
import { cn } from "../../lib/utils";

export function Card({ className, ...props }: ViewProps & { className?: string }) {
  return <View className={cn("rounded-lg bg-row", className)} {...props} />;
}

export function CardHeader({ className, ...props }: ViewProps & { className?: string }) {
  return <View className={cn("flex-row items-center justify-between p-3 pb-2", className)} {...props} />;
}

export function CardContent({ className, ...props }: ViewProps & { className?: string }) {
  return <View className={cn("px-3 pb-3", className)} {...props} />;
}
