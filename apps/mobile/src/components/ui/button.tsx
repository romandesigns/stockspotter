// RNR-style Button -- SaveStar (App.tsx) becomes a thin wrapper around
// the ghost/icon variant instead of a bare inline Pressable; also the
// real primitive behind the chart-screen back button and any other
// icon-only tap target introduced during the App.tsx migration.
import * as React from "react";
import { Pressable, type PressableProps } from "react-native";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "../../lib/utils";

const buttonVariants = cva("flex-row items-center justify-center rounded-md active:opacity-70", {
  variants: {
    variant: {
      default: "bg-accent-bg",
      ghost: "bg-transparent",
      outline: "border border-border bg-transparent",
    },
    size: {
      default: "px-3 py-2",
      sm: "px-2 py-1",
      icon: "h-8 w-8",
    },
  },
  defaultVariants: { variant: "ghost", size: "default" },
});

export interface ButtonProps extends PressableProps, VariantProps<typeof buttonVariants> {
  className?: string;
}

export const Button = React.forwardRef<React.ComponentRef<typeof Pressable>, ButtonProps>(
  ({ className, variant, size, ...props }, ref) => (
    <Pressable ref={ref} className={cn(buttonVariants({ variant, size }), className)} {...props} />
  ),
);
Button.displayName = "Button";
