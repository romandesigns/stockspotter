// Decision card for one shortlist candidate, as a full-screen overlay --
// the same state-driven, position:absolute pattern ChartScreen uses (no
// navigation library in this app), one z-level below it so "Open chart"
// stacks the chart on top and closing the chart lands back here.
//
// Sections mirror the web DecisionCard: why it's here, can I trade it,
// key levels, plan, track record. Nothing on this screen is an entry
// signal; the plan sizes risk, and logging records it -- no order is placed.
import { useEffect, useMemo, useRef, useState } from "react";
import { Animated, KeyboardAvoidingView, Platform, Pressable, ScrollView, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import type { CatalystUpdate, FunnelSignal, JournalEntry, ShortlistCandidate } from "@stockspotter/shared-types";
import { assessTradability } from "@stockspotter/shared-types";
import { Badge } from "../ui/badge";
import { Text } from "../ui/text";
import { RossFivePillarsFlag } from "../RossFivePillarsFlag";
import { colors } from "../../theme";
import {
  buildJournalEntry,
  candidateCatalyst,
  candidateFunnel,
  formatAge,
  formatDollars,
  formatEtTime,
  formatRate,
  formatRvol,
  formatShares,
  formatShortlistPrice,
  formatSignedPct,
  formatTag,
  newJournalId,
  parsePositive,
  planDefaults,
  planFromForm,
  priceInput,
  type PlanForm,
} from "../../shortlist";
import type { SaveResult } from "../../useTradeJournal";
import { ActionButton, DetailSection, Fact, Field, Note, TradabilityPill, VERDICT_COLOR } from "./parts";

export function ShortlistDetail(props: {
  candidate: ShortlistCandidate;
  /** False when the symbol has dropped off the latest list -- the card then
   * shows the last reading it had. */
  onList: boolean;
  generatedAt: string;
  liveFunnel?: FunnelSignal;
  liveCatalyst?: CatalystUpdate;
  risk: number;
  riskLoaded: boolean;
  onRiskChange: (n: number) => void;
  onSave: (entry: JournalEntry) => Promise<SaveResult>;
  onOpenChart: (symbol: string) => void;
  onClose: () => void;
}) {
  const c = props.candidate;
  const assessment = assessTradability(c);

  // Opacity/offset only, native driver: the overlay eases in without
  // relayout of anything underneath.
  const enter = useRef(new Animated.Value(0)).current;
  useEffect(() => {
    Animated.timing(enter, { toValue: 1, duration: 180, useNativeDriver: true }).start();
  }, [enter]);

  return (
    <SafeAreaView style={{ position: "absolute", left: 0, right: 0, top: 0, bottom: 0, backgroundColor: colors.background, zIndex: 40 }} edges={["top", "left", "right", "bottom"]}>
      <Animated.View
        style={{ flex: 1, opacity: enter, transform: [{ translateY: enter.interpolate({ inputRange: [0, 1], outputRange: [16, 0] }) }] }}
      >
        <View className="flex-row items-center gap-2 px-3 pb-2 pt-2">
          <Pressable onPress={props.onClose} className="h-11 w-11 items-center justify-center" accessibilityRole="button" accessibilityLabel="Back to shortlist">
            <Text className="text-[30px] font-light leading-[32px]">‹</Text>
          </Pressable>
          <Text mono variant="muted" className="text-xs">#{c.rank}</Text>
          <Text mono className="text-lg font-bold">{c.symbol}</Text>
          <Text mono className="text-sm">{formatShortlistPrice(c.price)}</Text>
          <Text mono variant={c.gapPct >= 0 ? "good" : "critical"} className="text-xs font-semibold">{formatSignedPct(c.gapPct)}</Text>
          <View className="flex-1" />
          <Pressable
            onPress={() => props.onOpenChart(c.symbol)}
            className="min-h-[44px] items-center justify-center rounded-md bg-accent-bg px-3 active:opacity-70"
            accessibilityRole="button"
            accessibilityLabel={`Open ${c.symbol} chart`}
          >
            <Text variant="accent" className="text-[12px] font-semibold">Chart</Text>
          </Pressable>
        </View>
        <KeyboardAvoidingView style={{ flex: 1 }} behavior={Platform.OS === "ios" ? "padding" : undefined}>
          <ScrollView
            className="flex-1"
            contentContainerStyle={{ paddingHorizontal: 16, paddingTop: 4, paddingBottom: 32, gap: 20 }}
            keyboardShouldPersistTaps="handled"
            showsVerticalScrollIndicator={false}
          >
            {!props.onList && (
              <Note tone="warning">{c.symbol} is no longer on the latest shortlist — this is the last reading it had.</Note>
            )}
            <WhyItsHere c={c} generatedAt={props.generatedAt} liveFunnel={props.liveFunnel} liveCatalyst={props.liveCatalyst} />
            <CanITradeIt c={c} verdict={assessment.verdict} reasons={assessment.reasons} />
            <KeyLevels c={c} />
            <PlanSection
              c={c}
              risk={props.risk}
              riskLoaded={props.riskLoaded}
              onRiskChange={props.onRiskChange}
              onSave={props.onSave}
            />
            <TrackRecord c={c} />
            <ActionButton label={`Open ${c.symbol} chart`} tone="outline" onPress={() => props.onOpenChart(c.symbol)} />
            <Text variant="muted" className="text-center text-[11px]">A watchlist ranking, not an entry signal. Not financial advice.</Text>
          </ScrollView>
        </KeyboardAvoidingView>
      </Animated.View>
    </SafeAreaView>
  );
}

function WhyItsHere(props: { c: ShortlistCandidate; generatedAt: string; liveFunnel?: FunnelSignal; liveCatalyst?: CatalystUpdate }) {
  const { c } = props;
  const funnel = candidateFunnel(c, props.liveFunnel);
  const catalyst = candidateCatalyst(c, props.generatedAt || c.funnelAt, props.liveCatalyst);
  const tags = catalyst?.catalystTags ?? [];
  const headline = catalyst?.mostRecentHeadline;
  const age = formatAge(catalyst?.mostRecentPublishedAt);
  return (
    <DetailSection title="Why it's here" extra={<RossFivePillarsFlag funnel={funnel} catalyst={catalyst} />}>
      <Fact label="Day change (vs prior close)" valueColor={c.gapPct >= 0 ? colors.good : colors.critical}>{formatSignedPct(c.gapPct)}</Fact>
      <Fact label="Relative volume">{formatRvol(c.relativeVolume)}</Fact>
      <Fact label="Float">{formatShares(c.floatShares)}</Fact>
      <Fact label="Session volume">{formatShares(c.sessionVolume)}</Fact>
      <View className="flex-row flex-wrap gap-1.5 pt-0.5">
        <Badge variant={c.funnelPassed ? "good" : "muted"}>{c.funnelPassed ? "funnel passed" : "funnel not passed"}</Badge>
        {tags.map((tag) => (
          <Badge key={tag}>{formatTag(tag)}</Badge>
        ))}
      </View>
      {headline ? (
        <Text className="text-xs leading-4">
          {headline}
          {age ? <Text variant="muted" className="text-xs"> · {age}</Text> : null}
        </Text>
      ) : (
        <Note>No catalyst on record — that is not proof there is no news.</Note>
      )}
    </DetailSection>
  );
}

function CanITradeIt(props: { c: ShortlistCandidate; verdict: ReturnType<typeof assessTradability>["verdict"]; reasons: string[] }) {
  const { c } = props;
  const quote = c.quote;
  const halt = c.halt;
  const haltColor = halt?.level === "red" ? colors.critical : halt?.level === "amber" ? colors.warning : colors.muted;
  return (
    <DetailSection title="Can I trade it?" extra={<TradabilityPill verdict={props.verdict} />}>
      {props.reasons.length > 0 && (
        <View className="gap-0.5">
          {props.reasons.map((r) => (
            <Text key={r} className="text-xs" style={{ color: VERDICT_COLOR[props.verdict] }}>• {r}</Text>
          ))}
        </View>
      )}
      <Fact label="Bid / ask">{quote ? `${formatShortlistPrice(quote.bid)} / ${formatShortlistPrice(quote.ask)}` : "no quote"}</Fact>
      {quote && <Fact label="Spread · quote age">{`${quote.spreadPct.toFixed(2)}% · ${Math.round(quote.ageSecs)}s old`}</Fact>}
      <Fact label="Dollar volume today">{formatDollars(c.levels.dollarVolume)}</Fact>
      {halt ? (
        <>
          <Fact label="Halt level" valueColor={haltColor}>{halt.level}</Fact>
          <Fact label="Distance to upper band">{`${halt.distanceToUpperPct.toFixed(1)}%`}</Fact>
          {halt.estimated && <Note>Band is the server&apos;s own LULD estimate, not SIP-supplied.</Note>}
          {!halt.luldInEffect && <Note>Outside LULD hours (9:30–4:00 ET): no halt can trigger now, so the band doesn&apos;t count toward the verdict.</Note>}
        </>
      ) : (
        <Fact label="Halt level">no reading</Fact>
      )}
      <Note>Heuristic thresholds (spread, liquidity, halt proximity), not backtested.</Note>
    </DetailSection>
  );
}

function KeyLevels({ c }: { c: ShortlistCandidate }) {
  const { levels, halt } = c;
  const anchor = levels.vwapAnchor === "premarket" ? "from 4:00 AM ET" : levels.vwapAnchor === "regular" ? "from the 9:30 open" : undefined;
  return (
    <DetailSection title="Key levels">
      {!levels.barsComplete && (
        <Note tone="warning">
          Partial session data{levels.barsFrom ? ` — bars start at ${formatEtTime(levels.barsFrom)}` : ""}, so premarket levels and VWAP may be incomplete.
        </Note>
      )}
      <Fact label="Premarket high">{formatShortlistPrice(levels.premarketHigh)}</Fact>
      <Fact label="Premarket low">{formatShortlistPrice(levels.premarketLow)}</Fact>
      <Fact label={anchor ? `VWAP (${anchor})` : "VWAP"}>{formatShortlistPrice(levels.vwap)}</Fact>
      <Fact label="Day high">{formatShortlistPrice(levels.dayHigh)}</Fact>
      <Fact label="Halt upper band">{formatShortlistPrice(halt?.upperBand)}</Fact>
      <Fact label="Halt lower band">{formatShortlistPrice(halt?.lowerBand)}</Fact>
    </DetailSection>
  );
}

type LogState = { kind: "idle" } | { kind: "saving" } | { kind: "saved" } | { kind: "error"; message: string };

function PlanSection(props: {
  c: ShortlistCandidate;
  risk: number;
  riskLoaded: boolean;
  onRiskChange: (n: number) => void;
  onSave: (entry: JournalEntry) => Promise<SaveResult>;
}) {
  const { c } = props;
  // Suggestions are taken once, when the card opens: later polls must not
  // overwrite numbers the trader is typing. "Use suggestions" re-takes them.
  const [defaults, setDefaults] = useState(() => planDefaults(c));
  const [form, setForm] = useState<PlanForm>(() => ({
    entry: priceInput(defaults.entry),
    stop: priceInput(defaults.stop),
    target: "",
    risk: String(props.risk),
  }));
  const riskTouched = useRef(false);
  const [log, setLog] = useState<LogState>({ kind: "idle" });

  // The stored risk can arrive after the card opened; adopt it unless edited.
  useEffect(() => {
    if (props.riskLoaded && !riskTouched.current) setForm((f) => ({ ...f, risk: String(props.risk) }));
  }, [props.riskLoaded, props.risk]);

  const result = useMemo(() => planFromForm(c, form), [c, form]);

  function update(key: keyof PlanForm, value: string) {
    setForm((f) => ({ ...f, [key]: value }));
    if (log.kind === "saved" || log.kind === "error") setLog({ kind: "idle" });
    if (key === "risk") {
      riskTouched.current = true;
      const n = parsePositive(value);
      if (Number.isFinite(n)) props.onRiskChange(n);
    }
  }

  function applySuggestions() {
    const d = planDefaults(c);
    setDefaults(d);
    setForm((f) => ({ ...f, entry: priceInput(d.entry), stop: priceInput(d.stop) }));
    if (log.kind !== "saving") setLog({ kind: "idle" });
  }

  async function logTrade() {
    if (!result.ok || log.kind === "saving") return;
    const target = form.target.trim() === "" ? undefined : parsePositive(form.target);
    const entry = buildJournalEntry({
      id: newJournalId(),
      candidate: c,
      entry: parsePositive(form.entry),
      stop: parsePositive(form.stop),
      target,
      shares: result.plan.shares,
      now: new Date(),
    });
    setLog({ kind: "saving" });
    const saved = await props.onSave(entry);
    setLog(saved.ok ? { kind: "saved" } : { kind: "error", message: saved.error });
  }

  const entryCaption = form.entry === priceInput(defaults.entry) ? `suggested: ${defaults.entryBasis}` : undefined;
  const stopCaption = form.stop === priceInput(defaults.stop) ? `suggested: ${defaults.stopBasis}` : undefined;

  return (
    <DetailSection
      title="Plan"
      extra={
        <Pressable onPress={applySuggestions} className="min-h-[44px] justify-center px-1 -my-3" accessibilityRole="button" accessibilityLabel="Use suggested entry and stop">
          <Text variant="accent" className="text-[11px] font-semibold">Use suggestions</Text>
        </Pressable>
      }
    >
      <View className="flex-row gap-2.5">
        <Field className="flex-1" label="Entry" value={form.entry} onChangeText={(t) => update("entry", t)} caption={entryCaption} />
        <Field className="flex-1" label="Stop" value={form.stop} onChangeText={(t) => update("stop", t)} caption={stopCaption} />
      </View>
      <View className="flex-row gap-2.5">
        <Field className="flex-1" label="Target (optional)" value={form.target} onChangeText={(t) => update("target", t)} placeholder="none" />
        <Field className="flex-1" label="Risk ($)" value={form.risk} onChangeText={(t) => update("risk", t)} caption="remembered on this phone" />
      </View>

      {result.ok ? (
        <View className="gap-2">
          <Fact label="Shares">{String(result.plan.shares)}</Fact>
          <Fact label="Position">{formatDollars(result.plan.positionDollars)}</Fact>
          <Fact label="Risk per share">{formatShortlistPrice(result.plan.riskPerShare)}</Fact>
          <Fact label="Spread cost (in and out)">{`$${result.plan.spreadCostDollars.toFixed(2)}`}</Fact>
          <Fact label="Loss at the stop, with spread">{formatDollars(result.plan.totalRiskDollars)}</Fact>
          <Fact label="Reward : risk">{result.plan.rewardToRisk === undefined ? "—" : `${result.plan.rewardToRisk.toFixed(2)} : 1`}</Fact>
          {result.plan.warnings.map((w) => (
            <Note key={w} tone="warning">{w}</Note>
          ))}
        </View>
      ) : (
        <Note tone="critical">{result.error}</Note>
      )}

      <ActionButton
        label={log.kind === "saving" ? "Logging…" : log.kind === "saved" ? "Logged to Journal" : "Log trade"}
        disabled={!result.ok || log.kind === "saving" || log.kind === "saved"}
        onPress={() => void logTrade()}
        className="mt-1"
      />
      {log.kind === "error" ? (
        <Note tone="critical">{log.message}</Note>
      ) : (
        <Note>Logging records your plan; it does not place an order.</Note>
      )}
    </DetailSection>
  );
}

function TrackRecord({ c }: { c: ShortlistCandidate }) {
  const tr = c.trackRecord;
  if (!tr) {
    return (
      <DetailSection title="Track record">
        <Note>Historical base rate for this setup: not measured yet.</Note>
      </DetailSection>
    );
  }
  const close = tr.medianCloseReturnNetPct;
  return (
    <DetailSection title="Track record">
      <Text className="text-xs leading-4">
        {tr.profile}, bought at the ask at {formatDecisionTime(tr.decisionTimeEt)} ET, spread paid both ways.
      </Text>
      {tr.sufficient ? (
        <>
          <Fact label={`Reached +${tr.targetPct}% first`}>{formatRate(tr.targetRate)}</Fact>
          <Fact label={`Hit −${tr.stopPct}% first`}>{formatRate(tr.stopFirstRate)}</Fact>
          <Fact label="Median close, net" valueColor={close === undefined ? undefined : close < 0 ? colors.critical : colors.good}>
            {formatSignedPct(close)}
          </Fact>
          <Fact label="Median drawdown">{formatSignedPct(tr.medianMaxAdversePct)}</Fact>
          <Fact label="Past cases (n)">{String(tr.n)}</Fact>
        </>
      ) : (
        <Note tone="warning">Only {tr.n} past case{tr.n === 1 ? "" : "s"} like this — too few to state a rate.</Note>
      )}
      {tr.sourceNote ? <Note>{tr.sourceNote}</Note> : null}
    </DetailSection>
  );
}

/** "09:35" -> "09:35" as sent; the server already states it in ET. */
function formatDecisionTime(s: string): string {
  return s.trim() || "09:35";
}
