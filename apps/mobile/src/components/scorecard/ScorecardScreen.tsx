// Panel scorecard, as a full-screen overlay opened from Home -- the same
// state-driven, position:absolute pattern as ShortlistDetail (no navigation
// library in this app). The app judging its own panels: for each one, the
// 30-minute return after costs against random moments in the same stock and
// session, with a verdict that is always a word, never colour alone. All
// numbers and wording come from shared-types' scorecard.ts, so this says
// exactly what the desktop Scorecard view says.
//
// Android back closes it (BackHandler). No layout animations on pressable
// containers (LinearTransition there caused stale taps on Android); the
// overlay and the rules text fade with the native driver only.
import { useEffect, useMemo, useRef, useState } from "react";
import { Animated, BackHandler, Pressable, ScrollView, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import Svg, { Line as SvgLine, Rect } from "react-native-svg";
import type { ScorecardDay, ScorecardPanel, ScorecardRow, ScorecardSampling, ScorecardSource, TrendPoint, VerdictTone } from "@stockspotter/shared-types";
import {
  differenceText,
  formatGeneratedAt,
  formatMarketDay,
  formatNetPct,
  formatRate,
  methodText,
  notRecordedText,
  panelTrend,
  primaryStats,
  sampleText,
  samplingText,
  scorecardRows,
  sourceText,
  trendText,
  verdictRules,
  verdictView,
  windowText,
} from "@stockspotter/shared-types";
import { Text } from "../ui/text";
import { colors } from "../../theme";
import { cn } from "../../lib/utils";
import { useScorecard, type ScorecardState } from "../../useScorecard";

const ROLLING = "rolling";

export function ScorecardScreen(props: { onClose: () => void }) {
  const sc = useScorecard(true);
  const [selection, setSelection] = useState<string>(ROLLING);
  const { onClose } = props;

  useEffect(() => {
    const sub = BackHandler.addEventListener("hardwareBackPress", () => {
      onClose();
      return true;
    });
    return () => sub.remove();
  }, [onClose]);

  const enter = useRef(new Animated.Value(0)).current;
  useEffect(() => {
    Animated.timing(enter, { toValue: 1, duration: 180, useNativeDriver: true }).start();
  }, [enter]);

  function choose(value: string) {
    setSelection(value);
    if (value !== ROLLING && value !== sc.latest?.latest.day) sc.loadDay(value);
  }

  return (
    <SafeAreaView style={{ position: "absolute", left: 0, right: 0, top: 0, bottom: 0, backgroundColor: colors.background, zIndex: 40 }} edges={["top", "left", "right", "bottom"]}>
      <Animated.View style={{ flex: 1, opacity: enter, transform: [{ translateY: enter.interpolate({ inputRange: [0, 1], outputRange: [16, 0] }) }] }}>
        <View className="flex-row items-center gap-2 px-3 pb-2 pt-2">
          <Pressable onPress={onClose} className="h-11 w-11 items-center justify-center" accessibilityRole="button" accessibilityLabel="Back to Home">
            <Text className="text-[30px] font-light leading-[32px]">‹</Text>
          </Pressable>
          <View className="flex-1">
            <Text className="text-lg font-bold">Panel scorecard</Text>
            <Text variant="muted" className="text-xs">Do the panels beat random picks?</Text>
          </View>
        </View>
        <ScrollView className="flex-1" contentContainerStyle={{ paddingHorizontal: 16, paddingTop: 4, paddingBottom: 32, gap: 12 }} showsVerticalScrollIndicator={false}>
          {sc.latest && sc.days.length > 0 && <DayPicker days={sc.days} rollingCount={sc.latest.rolling.days.length} value={selection} onChange={choose} />}
          <Body sc={sc} selection={selection} onBackToRolling={() => setSelection(ROLLING)} />
        </ScrollView>
      </Animated.View>
    </SafeAreaView>
  );
}

function DayPicker(props: { days: string[]; rollingCount: number; value: string; onChange: (v: string) => void }) {
  const options = [{ value: ROLLING, label: `Rolling (${props.rollingCount})` }, ...props.days.map((d) => ({ value: d, label: formatMarketDay(d) }))];
  return (
    <ScrollView horizontal showsHorizontalScrollIndicator={false} contentContainerStyle={{ gap: 6 }} accessibilityRole="tablist">
      {options.map((opt) => {
        const active = opt.value === props.value;
        return (
          <Pressable
            key={opt.value}
            onPress={() => props.onChange(opt.value)}
            className={cn("min-h-[44px] items-center justify-center rounded-full px-3.5", active ? "bg-accent-bg" : "bg-panel")}
            accessibilityRole="tab"
            accessibilityState={{ selected: active }}
            accessibilityLabel={opt.value === ROLLING ? `Rolling summary, ${props.rollingCount} sessions` : formatMarketDay(opt.value, true)}
          >
            <Text className={cn("text-[13px] font-semibold", active ? "text-accent" : "text-muted")}>{opt.label}</Text>
          </Pressable>
        );
      })}
    </ScrollView>
  );
}

function Body(props: { sc: ScorecardState; selection: string; onBackToRolling: () => void }) {
  const { sc, selection } = props;
  const latest = sc.latest;
  const trendDays = useMemo(
    () => (latest ? latest.rolling.days.map((d) => sc.dayFiles.get(d)).filter((d): d is ScorecardDay => d !== undefined) : []),
    [latest, sc.dayFiles],
  );

  if (!latest) {
    return (
      <View className="rounded-[14px] bg-panel p-4">
        <Text variant="muted" className="text-[13px]">
          {sc.status === "unavailable"
            ? "Scorecard not available on this server yet."
            : sc.status === "error"
              ? `Scorecard unavailable: ${sc.error ?? "request failed"}`
              : "Loading scorecard…"}
        </Text>
      </View>
    );
  }

  const rolling = selection === ROLLING;
  let panels: ScorecardPanel[] | null;
  let sampling: ScorecardSampling | null;
  let source: ScorecardSource | null;
  if (rolling || selection === latest.latest.day) {
    panels = rolling ? latest.rolling.panels : latest.latest.panels;
    sampling = latest.latest.sampling;
    source = latest.latest.source;
  } else {
    const file = sc.dayFiles.get(selection);
    panels = file?.panels ?? null;
    sampling = file?.sampling ?? null;
    source = file?.source ?? null;
  }
  const dayError = rolling ? undefined : sc.dayErrors.get(selection);
  const rows = panels ? scorecardRows(panels) : [];
  const sampled = samplingText(sampling, rolling ? latest.latest.day : selection);
  const sourceNote = source ? sourceText(source) : null;

  return (
    <>
      <View className="gap-1.5 rounded-[14px] bg-panel p-3">
        <Text className="text-sm font-semibold">{rolling ? windowText(latest.rolling.days) : `One session: ${formatMarketDay(selection, true)}`}</Text>
        <Text variant="muted" className="text-xs">Generated {formatGeneratedAt(latest.generatedAt)}</Text>
        <Text variant="muted" className="text-xs leading-4">{methodText(latest.method)}.</Text>
        {!rolling && (
          <View className="gap-1">
            <Text variant="muted" className="text-xs leading-4">A single day never gets a verdict — one day is one market mood.</Text>
            <Pressable onPress={props.onBackToRolling} className="min-h-[44px] justify-center self-start" accessibilityRole="button">
              <Text className="text-xs font-semibold underline">Back to the rolling summary</Text>
            </Pressable>
          </View>
        )}
        {sampled && <Text variant="muted" className="text-xs leading-4">{sampled}{rolling ? " Earlier days may be sampled too." : ""}</Text>}
        {sourceNote && <Text variant="muted" className="text-xs leading-4">{rolling ? `Latest day (${formatMarketDay(latest.latest.day)}): ` : ""}{sourceNote}</Text>}
        {sc.error && <Text variant="warning" className="text-xs">Couldn't refresh ({sc.error}) — showing the last scorecard received.</Text>}
        <Rules lines={verdictRules(latest.method)} />
      </View>

      {panels === null ? (
        <View className="rounded-[14px] bg-panel p-4">
          <Text variant="muted" className="text-[13px]">{dayError ? `Couldn't load ${formatMarketDay(selection, true)}: ${dayError}` : `Loading ${formatMarketDay(selection, true)}…`}</Text>
        </View>
      ) : (
        <>
          {rows.filter((r) => r.group === "main").map((r) => <PanelCard key={r.key} row={r} trend={rolling ? panelTrend(trendDays, r.key) : null} />)}
          <Text variant="muted" className="mt-2 text-[11px] font-semibold tracking-wider">OTHER DETECTORS</Text>
          {rows.filter((r) => r.group === "other").map((r) => <PanelCard key={r.key} row={r} trend={rolling ? panelTrend(trendDays, r.key) : null} />)}
        </>
      )}
      <Text variant="muted" className="text-center text-[11px]">Past detections only. A panel that beats random here is still not a trade signal.</Text>
    </>
  );
}

function Rules(props: { lines: string[] }) {
  const [open, setOpen] = useState(false);
  const fade = useRef(new Animated.Value(0)).current;
  useEffect(() => {
    if (open) {
      fade.setValue(0);
      Animated.timing(fade, { toValue: 1, duration: 160, useNativeDriver: true }).start();
    }
  }, [open, fade]);
  return (
    <View>
      <Pressable
        onPress={() => setOpen((v) => !v)}
        className="min-h-[44px] flex-row items-center"
        accessibilityRole="button"
        accessibilityState={{ expanded: open }}
        accessibilityLabel="How the verdict works"
      >
        <Text className="flex-1 text-[13px] font-semibold">How the verdict works</Text>
        <Text variant="muted" className="text-xs">{open ? "Hide" : "Show"}</Text>
      </Pressable>
      {open && (
        <Animated.View style={{ opacity: fade, gap: 6 }}>
          {props.lines.map((line) => (
            <Text key={line} variant="muted" className="text-xs leading-4">• {line}</Text>
          ))}
        </Animated.View>
      )}
    </View>
  );
}

const TONE_CLASS: Record<VerdictTone, { box: string; text: string }> = {
  good: { box: "bg-good-bg", text: "text-good" },
  critical: { box: "bg-critical-bg", text: "text-critical" },
  neutral: { box: "bg-row", text: "text-text" },
  muted: { box: "bg-row", text: "text-muted" },
};

function VerdictBadge(props: { label: string; tone: VerdictTone }) {
  const t = TONE_CLASS[props.tone];
  return (
    <View className={cn("rounded-full px-2.5 py-1", t.box)}>
      <Text className={cn("text-[11px] font-semibold", t.text)}>{props.label}</Text>
    </View>
  );
}

function PanelCard(props: { row: ScorecardRow; trend: TrendPoint[] | null }) {
  const { row } = props;
  const s = row.stats;
  if (!s) {
    return (
      <View className="gap-1.5 rounded-[14px] bg-panel p-3">
        <View className="flex-row items-center gap-2">
          <Text className="flex-1 text-sm font-semibold">{row.title}</Text>
          <VerdictBadge label="No data" tone="muted" />
        </View>
        <Text variant="muted" className="text-xs">{notRecordedText()}</Text>
      </View>
    );
  }
  const v = verdictView(s.verdict);
  const h = primaryStats(s);
  return (
    <View className="gap-2 rounded-[14px] bg-panel p-3" accessible accessibilityLabel={`${row.title}: ${v.label}. ${differenceText(h)}. Hit rate ${formatRate(h?.hitRate ?? null)}. ${sampleText(s)}.`}>
      <View className="flex-row items-center gap-2">
        <Text className="flex-1 text-sm font-semibold">{row.title}</Text>
        <VerdictBadge label={v.label} tone={v.tone} />
      </View>
      <Text mono className="text-[13px]">{differenceText(h)}</Text>
      <View className="flex-row flex-wrap gap-y-2">
        <Fact label={`${s.primaryHorizon} avg after costs`} value={formatNetPct(h?.meanNetPct ?? null)} />
        <Fact label="Random avg" value={formatNetPct(h?.baselineMeanNetPct ?? null)} />
        <Fact label="Hit rate" value={formatRate(h?.hitRate ?? null)} />
        <Fact label="Sample" value={sampleText(s)} />
      </View>
      {props.trend && props.trend.length > 1 && <TrendBars points={props.trend} />}
      <Text variant="muted" className="text-[11px] leading-4">{s.verdictReason}</Text>
    </View>
  );
}

function Fact(props: { label: string; value: string }) {
  return (
    <View className="w-1/2 gap-0.5 pr-2">
      <Text variant="muted" className="text-[10px]">{props.label}</Text>
      <Text mono className="text-xs">{props.value}</Text>
    </View>
  );
}

const TREND_W = 140;
const TREND_H = 30;

/** One bar per scored day (panel minus random), up or down from a zero
 * line, neutral colour -- direction carries the sign, the label the values. */
function TrendBars(props: { points: TrendPoint[] }) {
  const pts = props.points;
  const max = Math.max(0.5, ...pts.map((p) => Math.abs(p.diffPct ?? 0)));
  const slot = TREND_W / pts.length;
  const barW = Math.max(2, Math.min(10, slot - 2));
  const mid = TREND_H / 2;
  return (
    <View className="flex-row items-center gap-2.5" accessible accessibilityLabel={trendText(pts)}>
      <Text variant="muted" className="text-[10px]">Per day vs random</Text>
      <Svg width={TREND_W} height={TREND_H}>
        <SvgLine x1={0} x2={TREND_W} y1={mid} y2={mid} stroke={colors.divider} strokeWidth={1} />
        {pts.map((p, i) => {
          if (p.diffPct === null) return null;
          const hgt = Math.max(1, (Math.abs(p.diffPct) / max) * (mid - 1));
          return <Rect key={p.day} x={i * slot + (slot - barW) / 2} y={p.diffPct >= 0 ? mid - hgt : mid} width={barW} height={hgt} rx={1} fill={colors.muted} />;
        })}
      </Svg>
    </View>
  );
}
