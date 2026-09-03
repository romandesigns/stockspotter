// Real port of SuperChart.tsx's MomentumScoreRow -- a genuinely
// established web feature this plan's first pass missed (confirmed by
// reading SuperChart.tsx in full). Real momentum_scorer::MomentumScore
// data (MomentumUpdate), not the Artifact prototype's static demo copy --
// same score badge + momentumLabel() caption + 4 real computed
// FactorRows as web, via momentumLabel.ts/momentumNarrative.ts (both
// ported verbatim, same files web itself uses).
//
// Trimmed 2026-09-03 (Roman's own ask: "make it more minimalist without
// losing its intuitiveness") -- tighter margins/padding/gaps, same
// content, nothing removed. Also gained the AI-assessment section below
// the factors (same card, per Roman's own suggestion, real Claude call
// via useAssessment.ts) -- part of the same "no vertical scroll on the
// Chart Page" redesign, so this card had to get denser to make room for
// the new quick-jump row and AI section without growing the page.
import { Pressable, View } from "react-native";
import { Card } from "./ui/card";
import { Text } from "./ui/text";
import { Separator } from "./ui/separator";
import type { MomentumUpdate } from "@stockspotter/shared-types";
import type { CandleBar } from "../types";
import { sma } from "../chartIndicators";
import { factorGood, momentumLabel } from "../momentumLabel";
import { maSlopeDetail, structureDetail, volumeConfirmationDetail, wickRejectionDetail } from "../momentumNarrative";
import { useAssessment } from "../useAssessment";
import { colors } from "../theme";

export function MomentumScoreRow(props: { symbol: string; momentum: MomentumUpdate | null; bars: CandleBar[] }) {
  const m = props.momentum;
  const { assessment, loading, error, regenerate } = useAssessment(props.symbol, m);

  if (!m) {
    return (
      <Card className="mx-3 mt-2 mb-2">
        <View className="px-3.5 py-3.5">
          <Text variant="muted" className="text-[12px]">
            No momentum reading yet for this symbol…
          </Text>
        </View>
      </Card>
    );
  }

  const scoreValue = Math.round(m.overall * 100);
  const scoreColor = m.overall >= 0.6 ? colors.good : m.overall >= 0.4 ? colors.warning : colors.critical;
  const bars = props.bars;
  const ma9Vals = sma(bars, 9).map((p) => p.value);
  const ma20Vals = sma(bars, 20).map((p) => p.value);
  const lastPrice = bars[bars.length - 1]?.close ?? 0;

  return (
    <Card className="mx-3 mt-2 mb-2">
      <View className="flex-row gap-3 p-3">
        <View className="w-[68px] items-center justify-center rounded-md bg-panel py-2.5">
          <Text mono className="text-[22px] font-bold" style={{ color: scoreColor }}>
            {scoreValue}
          </Text>
          <Text className="mt-0.5 text-center text-[9px] font-semibold">{momentumLabel(m.overall)}</Text>
          <Text variant="muted" className="text-[8px]">
            Momentum
          </Text>
        </View>
        <View className="flex-1 gap-1.5">
          <FactorRow label="Volume confirmation" score={m.volumeConfirmation} detail={volumeConfirmationDetail(bars)} />
          <FactorRow label="Higher highs / higher lows" score={m.structure} detail={structureDetail(m.structure)} />
          <FactorRow label="MA slope" score={m.maSlope} detail={maSlopeDetail(ma9Vals, ma20Vals, lastPrice, factorGood(m.maSlope))} />
          <FactorRow label="Rejection wicks" score={m.wickRejection} detail={wickRejectionDetail(m.wickRejection)} />
        </View>
      </View>

      <Separator className="mx-3" />
      <View className="gap-1 px-3.5 py-2">
        <View className="flex-row items-center gap-1.5">
          <Text variant="muted" className="text-[9px] font-semibold uppercase tracking-wide">
            AI read
          </Text>
          <Pressable onPress={regenerate} disabled={loading} hitSlop={8} className="ml-auto" accessibilityRole="button" accessibilityLabel="Regenerate AI assessment">
            <Text variant="accent" className="text-[10px]" style={loading ? { opacity: 0.4 } : undefined}>
              {loading ? "…" : "↻ Refresh"}
            </Text>
          </Pressable>
        </View>
        {loading && !assessment && (
          <Text variant="muted" className="text-[10px]">
            Reading the tape…
          </Text>
        )}
        {error && !assessment && (
          <Text variant="muted" className="text-[10px]">
            Couldn't reach the assessment service.
          </Text>
        )}
        {assessment?.summary.map((line, i) => (
          <Text key={i} variant="muted" className="text-[10px] leading-[14px]">
            • {line}
          </Text>
        ))}
      </View>
    </Card>
  );
}

function FactorRow(props: { label: string; score: number; detail: string }) {
  const good = factorGood(props.score);
  return (
    <View className="flex-row gap-1.5">
      <Text variant={good ? "good" : "warning"} className="text-[11px] font-semibold leading-[14px]">
        {good ? "✓" : "!"}
      </Text>
      <View className="flex-1">
        <Text className="text-[10px] font-semibold leading-[13px]">{props.label}</Text>
        <Text variant="muted" className="text-[9px] leading-[12px]">
          {props.detail}
        </Text>
      </View>
    </View>
  );
}
