// Moved out of App.tsx (2026-09-04, Roman's own ask: "use the same UI
// component used in the Home page since they provide more context and
// mounting pressure") -- ChartScreen's halt-risk quick-jump row used to
// be a plain pill chip (symbol + proximity %). This is the real
// Home-page (Radar tab) component instead: a PressureGauge (an actual
// visual read of how close to a halt band the symbol is, not just a
// number), the symbol with its real catalyst flag, and the current
// price -- genuinely more context per card, exactly what was asked for.
// Byte-for-byte the same component now used in both places, not a
// re-derived copy -- see PressureGauge.tsx/CatalystFlag.tsx for the
// pieces it composes.
//
// Original doc comment, still accurate (this is the same component,
// just relocated): minimalist home-tab equivalent of the fuller Alerts-
// tab HaltRow -- same PressureGauge (the "chart" showing the halt-
// proximity percentage), symbol, price, and catalyst flag if present.
// Deliberately drops rel-vol/2x-band/timestamp -- those stay on the
// fuller Alerts-tab HaltRow, this is the compact version. Fed by
// topHaltsByProximity (derive.ts), NOT haltRows -- shows the top symbols
// by proximity unconditionally, calm ones included, matching the web
// app's own Halt Early-Warning panel exactly (topHaltsByProximity's own
// doc comment has the real bug this fixed: haltRows' calm-filter left
// this section empty far more often than web's identical, unfiltered
// panel). Because calm readings show up here routinely, the escalation
// border uses a real 3-way color (calm gets the same neutral --border
// every other card already uses, not an alarming color).
import { Pressable, View } from "react-native";
import type { HaltWarning, CatalystUpdate } from "@stockspotter/shared-types";
import { Card } from "./ui/card";
import { Text } from "./ui/text";
import { PressureGauge } from "./PressureGauge";
import { CatalystFlag } from "./CatalystFlag";
import { formatPrice } from "../format";
import { colors } from "../theme";

const HALT_LEVEL_COLOR: Record<HaltWarning["level"], string> = { calm: colors.divider, amber: colors.warning, red: colors.critical };

export function HaltMiniCard({
  reading,
  onPress,
  catalysts,
  widthClassName = "w-[32%]",
  layout = "row",
}: {
  reading: HaltWarning;
  onPress: () => void;
  catalysts: Map<string, CatalystUpdate>;
  /** Default (`w-[32%]`) only resolves correctly inside a full-width
   * wrapping grid, which is all this component has ever lived in before
   * (RadarView's 3-per-row Halt Early-Warning grid) -- a percentage
   * width has no stable parent width to resolve against inside a
   * horizontally-scrolling row (ChartScreen's new halt-risk row), so
   * that caller passes a fixed width class instead. */
  widthClassName?: string;
  /** `"row"` (default, unchanged) -- gauge beside the symbol/price, the
   * shape this card has always had on the Home page's wider grid cells.
   * `"compact"` (2026-09-04, Roman's own ask: narrow the cards enough to
   * fit at least the top 4 in view) -- gauge centered on top, symbol/
   * price stacked underneath, a near-square card that fits a narrower
   * width than the row layout ever could without clipping. ChartScreen's
   * halt row is the only caller of this variant; the Home-page grid is
   * untouched. */
  layout?: "row" | "compact";
}) {
  const escalationColor = HALT_LEVEL_COLOR[reading.level];
  if (layout === "compact") {
    return (
      <Pressable className={widthClassName} onPress={onPress}>
        <Card className="items-center gap-1 border-t-[3px] px-1.5 py-2" style={{ borderTopColor: escalationColor }}>
          <PressureGauge reading={reading} size={28} />
          <View className="flex-row items-center">
            <Text mono className="text-[11px] font-bold">
              {reading.symbol}
            </Text>
            <CatalystFlag symbol={reading.symbol} catalysts={catalysts} />
          </View>
          <Text mono variant="muted" className="text-[10px]">
            {formatPrice(reading.currentPrice)}
          </Text>
        </Card>
      </Pressable>
    );
  }
  return (
    <Pressable className={widthClassName} onPress={onPress}>
      <Card className="flex-row items-center gap-2 border-t-[3px] px-2.5 py-2" style={{ borderTopColor: escalationColor }}>
        <PressureGauge reading={reading} size={32} />
        <View>
          <View className="flex-row items-center">
            <Text mono className="text-xs font-bold">
              {reading.symbol}
            </Text>
            <CatalystFlag symbol={reading.symbol} catalysts={catalysts} />
          </View>
          <Text mono variant="muted" className="text-[11px]">
            {formatPrice(reading.currentPrice)}
          </Text>
        </View>
      </Card>
    </Pressable>
  );
}
