// RNR-style themed Text -- the shared "colored text" primitive that
// replaces the scattered styles.symbol/secondary/muted/positive/negative
// StyleSheet entries across App.tsx. variant maps directly onto
// theme.ts's own token names (kept in sync with tailwind.config.js's
// color block); mono maps onto the same system-monospace fallback
// theme.ts's own monoFont constant already documents as real, not
// aspirational.
import * as React from "react";
import { Text as RNText, type TextProps } from "react-native";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "../../lib/utils";

const textVariants = cva("", {
  variants: {
    variant: {
      default: "text-text",
      muted: "text-muted",
      accent: "text-accent",
      good: "text-good",
      warning: "text-warning",
      critical: "text-critical",
    },
    mono: {
      true: "font-mono",
      false: "",
    },
  },
  defaultVariants: { variant: "default", mono: false },
});

export interface UiTextProps extends TextProps, VariantProps<typeof textVariants> {
  className?: string;
}

export const Text = React.forwardRef<RNText, UiTextProps>(({ className, variant, mono, ...props }, ref) => (
  <RNText ref={ref} className={cn(textVariants({ variant, mono }), className)} {...props} />
));
Text.displayName = "Text";
