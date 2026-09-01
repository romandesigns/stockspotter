// RNR-style Badge -- a real tinted-pill "chip" primitive, matching the
// web app's own .chip-good/.chip-bad/.chip-accent pattern (index.css)
// that mobile never had an equivalent for. Used for status/kind labels
// (halt escalation level, "2x band", alert kind) -- NOT for plain
// colored percentage deltas, which stay plain Text with a color variant
// (matching web's own real distinction between .chip -- a pill -- and
// .pct-up/.pct-down -- plain colored text, two different existing
// patterns, not one invented one).
import * as React from "react";
import { Text, View, type ViewProps } from "react-native";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "../../lib/utils";

const badgeVariants = cva("flex-row items-center self-start rounded-full px-2 py-0.5", {
  variants: {
    variant: {
      default: "bg-accent-bg",
      good: "bg-good-bg",
      warning: "bg-warning-bg",
      critical: "bg-critical-bg",
    },
  },
  defaultVariants: { variant: "default" },
});

const badgeTextVariants = cva("font-mono text-[10px] font-semibold", {
  variants: {
    variant: {
      default: "text-accent",
      good: "text-good",
      warning: "text-warning",
      critical: "text-critical",
    },
  },
  defaultVariants: { variant: "default" },
});

export interface BadgeProps extends ViewProps, VariantProps<typeof badgeVariants> {
  className?: string;
  textClassName?: string;
  children: React.ReactNode;
}

export function Badge({ className, textClassName, variant, children, ...props }: BadgeProps) {
  return (
    <View className={cn(badgeVariants({ variant }), className)} {...props}>
      <Text className={cn(badgeTextVariants({ variant }), textClassName)}>{children}</Text>
    </View>
  );
}
