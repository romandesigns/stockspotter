// Real port of SuperChart.tsx's MomentumScoreRow -- a genuinely
// established web feature this plan's first pass missed (confirmed by
// reading SuperChart.tsx in full). Real momentum_scorer::MomentumScore
// data (MomentumUpdate), not the Artifact prototype's static demo copy --
// same score badge + momentumLabel() caption + 4 real computed
// FactorRows as web, via momentumLabel.ts/momentumNarrative.ts (both
// ported verbatim, same files web itself uses).
import { View } from "react-native";
import { Card } from "./ui/card";
import { Text } from "./ui/text";
import type { MomentumUpdate } from "@stockspotter/shared-types";
import type { CandleBar } from "../types";
import { sma } from "../chartIndicators";
import { factorGood, momentumLabel } from "../momentumLabel";
import { maSlopeDetail, structureDetail, volumeConfirmationDetail, wickRejectionDetail } from "../momentumNarrative";
import { colors } from "../theme";

export function MomentumScoreRow(props: { momentum: MomentumUpdate | null; bars: CandleBar[] }) {
  const m = props.momentum;
  if (!m) {
    return (
      <Card className="mx-3.5 mt-3 mb-3">
        <View className="px-3.5 py-4">
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
    <Card className="mx-3.5 mt-3 mb-3">
      <View className="flex-row gap-3.5 p-3.5">
        <View className="w-[76px] items-center justify-center rounded-md bg-panel py-3">
          <Text mono className="text-[26px] font-bold" style={{ color: scoreColor }}>
            {scoreValue}
          </Text>
          <Text className="mt-0.5 text-center text-[10px] font-semibold">{momentumLabel(m.overall)}</Text>
          <Text variant="muted" className="text-[9px]">
            Momentum score
          </Text>
        </View>
        <View className="flex-1 gap-2">
          <FactorRow label="Volume confirmation" score={m.volumeConfirmation} detail={volumeConfirmationDetail(bars)} />
          <FactorRow label="Higher highs / higher lows" score={m.structure} detail={structureDetail(m.structure)} />
          <FactorRow label="MA slope" score={m.maSlope} detail={maSlopeDetail(ma9Vals, ma20Vals, lastPrice)} />
          <FactorRow label="Rejection wicks" score={m.wickRejection} detail={wickRejectionDetail(m.wickRejection)} />
        </View>
      </View>
    </Card>
  );
}

function FactorRow(props: { label: string; score: number; detail: string }) {
  const good = factorGood(props.score);
  return (
    <View className="flex-row gap-1.5">
      <Text variant={good ? "good" : "warning"} className="text-[12px] font-semibold leading-[16px]">
        {good ? "✓" : "!"}
      </Text>
      <View className="flex-1">
        <Text className="text-[11px] font-semibold leading-[14px]">{props.label}</Text>
        <Text variant="muted" className="text-[10px] leading-[13px]">
          {props.detail}
        </Text>
      </View>
    </View>
  );
}
