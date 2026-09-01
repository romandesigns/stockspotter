// RNR-style Separator -- replaces StyleSheet.hairlineWidth + colors.divider
// duplicated across signalRow/dataRow/riskStrip/tabs in App.tsx's own
// StyleSheet block with one real primitive.
import * as React from "react";
import { View, type ViewProps } from "react-native";
import { cn } from "../../lib/utils";

export function Separator({ className, ...props }: ViewProps & { className?: string }) {
  return <View className={cn("h-px bg-border", className)} {...props} />;
}
