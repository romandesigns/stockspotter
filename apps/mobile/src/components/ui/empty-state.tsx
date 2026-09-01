// RNR-style empty-state placeholder -- replaces App.tsx's own local
// `Empty` helper, reused ~4 times across tabs.
import * as React from "react";
import { Text } from "react-native";
import { cn } from "../../lib/utils";

export function EmptyState(props: { label: string; className?: string }) {
  return <Text className={cn("py-7 text-center text-xs text-muted", props.className)}>{props.label}</Text>;
}
