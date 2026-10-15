// Small building blocks shared by the Shortlist tab's views (ShortlistTab,
// ShortlistDetail, JournalView). Same tokens and ui/ primitives as the
// rest of the app; every tappable thing here is at least 44px tall.
import type { ReactNode } from "react";
import { Pressable, TextInput, View, type TextInputProps } from "react-native";
import type { TradabilityVerdict } from "@stockspotter/shared-types";
import { Text } from "../ui/text";
import { cn } from "../../lib/utils";
import { colors, monoFont } from "../../theme";
import { TRADABILITY_LABEL } from "../../shortlist";

export const VERDICT_COLOR: Record<TradabilityVerdict, string> = {
  good: colors.good,
  caution: colors.warning,
  poor: colors.critical,
  unknown: colors.muted,
};

/** Two-or-more option segmented control. ToggleGroup (ui/) is the app's
 * compact pill version; this one is full width with 44px targets, for a
 * view switch rather than a filter. */
export function Segmented<T extends string>(props: { options: { value: T; label: string }[]; value: T; onChange: (v: T) => void }) {
  return (
    <View className="flex-row rounded-lg bg-panel p-1" accessibilityRole="tablist">
      {props.options.map((opt) => {
        const active = opt.value === props.value;
        return (
          <Pressable
            key={opt.value}
            onPress={() => props.onChange(opt.value)}
            className={cn("min-h-[44px] flex-1 items-center justify-center rounded-md", active ? "bg-accent-bg" : "bg-transparent")}
            accessibilityRole="tab"
            accessibilityState={{ selected: active }}
          >
            <Text className={cn("text-[13px] font-semibold", active ? "text-accent" : "text-muted")}>{opt.label}</Text>
          </Pressable>
        );
      })}
    </View>
  );
}

export function TradabilityDot({ verdict, size = 10 }: { verdict: TradabilityVerdict; size?: number }) {
  return (
    <View
      style={{ width: size, height: size, borderRadius: size / 2, backgroundColor: VERDICT_COLOR[verdict] }}
      accessibilityLabel={`Tradability: ${TRADABILITY_LABEL[verdict]}`}
    />
  );
}

export function TradabilityPill({ verdict }: { verdict: TradabilityVerdict }) {
  return (
    <View className="flex-row items-center gap-1.5 self-start rounded-full bg-row px-2.5 py-1">
      <TradabilityDot verdict={verdict} size={8} />
      <Text className="font-mono text-[11px] font-semibold" style={{ color: VERDICT_COLOR[verdict] }}>
        {TRADABILITY_LABEL[verdict]}
      </Text>
    </View>
  );
}

/** Section block inside the detail screen: uppercase muted title (the
 * app's established Section header style), optional right-side extra. */
export function DetailSection(props: { title: string; extra?: ReactNode; children: ReactNode }) {
  return (
    <View className="gap-2">
      <View className="min-h-[20px] flex-row items-center justify-between">
        <Text variant="muted" className="text-[10px] font-semibold uppercase tracking-wider">{props.title}</Text>
        {props.extra}
      </View>
      <View className="gap-2 rounded-lg bg-row p-3">{props.children}</View>
    </View>
  );
}

/** One label/value line. */
export function Fact(props: { label: string; children: ReactNode; valueClassName?: string; valueColor?: string }) {
  return (
    <View className="flex-row items-start justify-between gap-3">
      <Text variant="muted" className="text-xs">{props.label}</Text>
      {typeof props.children === "string" ? (
        <Text mono className={cn("flex-shrink text-right text-xs", props.valueClassName)} style={props.valueColor ? { color: props.valueColor } : undefined}>
          {props.children}
        </Text>
      ) : (
        <View className="flex-shrink items-end">{props.children}</View>
      )}
    </View>
  );
}

export function Note(props: { children: ReactNode; tone?: "muted" | "warning" | "critical" }) {
  const variant = props.tone === "warning" ? "warning" : props.tone === "critical" ? "critical" : "muted";
  return <Text variant={variant} className="text-[11px] leading-4">{props.children}</Text>;
}

export function ActionButton(props: {
  label: string;
  onPress: () => void;
  disabled?: boolean;
  tone?: "primary" | "outline";
  className?: string;
  accessibilityLabel?: string;
}) {
  const primary = (props.tone ?? "primary") === "primary";
  return (
    <Pressable
      onPress={props.onPress}
      disabled={props.disabled}
      className={cn(
        "min-h-[44px] items-center justify-center rounded-md px-4 active:opacity-70",
        primary ? "bg-accent-bg" : "border border-border bg-transparent",
        props.disabled && "opacity-40",
        props.className,
      )}
      accessibilityRole="button"
      accessibilityState={{ disabled: !!props.disabled }}
      accessibilityLabel={props.accessibilityLabel ?? props.label}
    >
      <Text className={cn("text-[13px] font-semibold", primary ? "text-accent" : "text-text")}>{props.label}</Text>
    </Pressable>
  );
}

/** Numeric/text input matching ChartAlertsSheet's own TextInput styling. */
export function Field(props: TextInputProps & { label: string; caption?: string; className?: string }) {
  const { label, caption, className, style, ...input } = props;
  return (
    <View className={cn("gap-1", className)}>
      <Text variant="muted" className="text-[11px]">{label}</Text>
      <TextInput
        placeholderTextColor={colors.muted}
        keyboardType="decimal-pad"
        returnKeyType="done"
        accessibilityLabel={label}
        style={[
          {
            minHeight: 44,
            fontFamily: monoFont,
            fontSize: 14,
            color: colors.text,
            backgroundColor: colors.surface,
            borderRadius: 8,
            borderWidth: 1,
            borderColor: colors.divider,
            paddingHorizontal: 10,
            paddingVertical: 8,
          },
          style,
        ]}
        {...input}
      />
      {/* Always rendered (blank when absent) so a caption appearing never shifts the grid. */}
      <Text variant="muted" numberOfLines={1} className="min-h-[14px] text-[10px]">{caption ?? " "}</Text>
    </View>
  );
}
