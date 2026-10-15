// The Shortlist tab (default tab since 2026-10-15): a "Today | Journal"
// switch over the ranked list and the trader's own journal. Rendered
// inside App.tsx's shared tab ScrollView like every other tab; the
// candidate detail opens as its own full-screen overlay (ShortlistDetail),
// owned by App.tsx so the chart overlay can still stack on top of it.
//
// The list answers WHERE TO LOOK, not when to enter -- see docs/shortlist.md.
import { ActivityIndicator, Pressable, View } from "react-native";
import type { ShortlistCandidate, ShortlistResponse } from "@stockspotter/shared-types";
import { assessTradability } from "@stockspotter/shared-types";
import { Card } from "../ui/card";
import { Text } from "../ui/text";
import { UpdatedAgo } from "../../UpdatedAgo";
import { colors } from "../../theme";
import { SESSION_LABEL, formatRvol, formatShortlistPrice, formatSignedPct, shortlistObservedAt } from "../../shortlist";
import type { ShortlistState } from "../../useShortlist";
import type { TradeJournalState } from "../../useTradeJournal";
import { Note, Segmented, TradabilityDot } from "./parts";
import { JournalView } from "./JournalView";

export type ShortlistSegment = "today" | "journal";

const SEGMENTS: { value: ShortlistSegment; label: string }[] = [
  { value: "today", label: "Today" },
  { value: "journal", label: "Journal" },
];

export function ShortlistTab(props: {
  segment: ShortlistSegment;
  onSegmentChange: (s: ShortlistSegment) => void;
  shortlist: ShortlistState;
  journal: TradeJournalState;
  onOpenCandidate: (c: ShortlistCandidate) => void;
}) {
  return (
    <View className="gap-4">
      <Segmented options={SEGMENTS} value={props.segment} onChange={props.onSegmentChange} />
      {props.segment === "today" ? (
        <TodayView state={props.shortlist} onOpenCandidate={props.onOpenCandidate} />
      ) : (
        <JournalView journal={props.journal} />
      )}
    </View>
  );
}

function TodayView({ state, onOpenCandidate }: { state: ShortlistState; onOpenCandidate: (c: ShortlistCandidate) => void }) {
  const body = state.body;
  return (
    <View className="gap-3">
      <Header body={body} error={state.error} />
      {!body ? (
        state.loading ? (
          <ActivityIndicator className="py-7" color={colors.accent} />
        ) : (
          <Text variant="muted" className="py-7 text-center text-xs">Shortlist unavailable — retrying every 5 seconds.</Text>
        )
      ) : body.candidates.length === 0 ? (
        <Text variant="muted" className="py-7 text-center text-xs">
          Nothing qualifies right now: no symbol in the pool is $1–$20, up on the day, with relative volume.
        </Text>
      ) : (
        <View className="gap-1.5">
          <ColumnLabels />
          {body.candidates.map((c) => (
            <CandidateRow key={c.symbol} c={c} onPress={() => onOpenCandidate(c)} />
          ))}
          <Note>Dot: tradability from spread, liquidity and halt band — green fine, amber caution, red poor, grey unknown. Tap a row for the full card.</Note>
        </View>
      )}
      {body && <Excluded body={body} />}
      <Text variant="muted" className="pt-1 text-center text-[11px]">A watchlist ranking, not an entry signal. Not financial advice.</Text>
    </View>
  );
}

function Header({ body, error }: { body: ShortlistResponse | null; error: string | null }) {
  return (
    <View className="gap-1">
      <Text className="text-[13px] leading-5 text-text">Ranked by gap × relative volume — where to look, not when to enter</Text>
      <View className="min-h-[16px] flex-row flex-wrap items-center gap-x-2">
        {body && (
          <>
            <Text variant="muted" className="text-[11px]">{SESSION_LABEL[body.session]}</Text>
            <Text variant="muted" className="text-[11px]">·</Text>
            <Text variant="muted" className="text-[11px]">pool {body.poolSize}</Text>
            <Text variant="muted" className="text-[11px]">·</Text>
          </>
        )}
        <UpdatedAgo lastUpdated={shortlistObservedAt(body)} />
      </View>
      {/* Fixed-height slot: the error line comes and goes without moving the list. */}
      <Text variant="warning" numberOfLines={1} className="min-h-[15px] text-[10px] opacity-80">
        {error ? `Couldn't refresh (${error})${body ? " — showing the last list" : ""}` : " "}
      </Text>
    </View>
  );
}

// Fixed column widths so values changing every 5s never shift the row.
const COL = { rank: 28, price: 74, gap: 62, rvol: 54, dot: 18 };

function ColumnLabels() {
  return (
    <View className="flex-row items-center px-3">
      <Text variant="muted" style={{ width: COL.rank }} className="text-[9px] uppercase tracking-wider">#</Text>
      <Text variant="muted" className="flex-1 text-[9px] uppercase tracking-wider">Symbol</Text>
      <Text variant="muted" style={{ width: COL.price }} className="text-right text-[9px] uppercase tracking-wider">Price</Text>
      <Text variant="muted" style={{ width: COL.gap }} className="text-right text-[9px] uppercase tracking-wider">Day</Text>
      <Text variant="muted" style={{ width: COL.rvol }} className="text-right text-[9px] uppercase tracking-wider">RVOL</Text>
      <View style={{ width: COL.dot }} />
    </View>
  );
}

function CandidateRow({ c, onPress }: { c: ShortlistCandidate; onPress: () => void }) {
  const verdict = assessTradability(c).verdict;
  return (
    <Pressable
      onPress={onPress}
      className="active:opacity-70"
      accessibilityRole="button"
      accessibilityLabel={`Rank ${c.rank}, ${c.symbol}, ${formatShortlistPrice(c.price)}, ${formatSignedPct(c.gapPct)} on the day, relative volume ${formatRvol(c.relativeVolume)}. Open details.`}
    >
      <Card className="min-h-[52px] flex-row items-center px-3">
        <Text mono variant="muted" style={{ width: COL.rank }} className="text-xs">{c.rank}</Text>
        <Text mono numberOfLines={1} className="flex-1 text-[15px] font-bold">{c.symbol}</Text>
        <Text mono style={{ width: COL.price }} className="text-right text-xs">{formatShortlistPrice(c.price)}</Text>
        <Text mono variant={c.gapPct >= 0 ? "good" : "critical"} style={{ width: COL.gap }} className="text-right text-xs font-semibold">
          {formatSignedPct(c.gapPct)}
        </Text>
        <Text mono style={{ width: COL.rvol }} className="text-right text-xs">{formatRvol(c.relativeVolume)}</Text>
        <View style={{ width: COL.dot }} className="items-end">
          <TradabilityDot verdict={verdict} />
        </View>
      </Card>
    </Pressable>
  );
}

function Excluded({ body }: { body: ShortlistResponse }) {
  const { outsidePriceRange, notGappingUp, noRelativeVolume } = body.excluded;
  if (outsidePriceRange + notGappingUp + noRelativeVolume === 0) return null;
  return (
    <Note>
      Excluded from the pool: {outsidePriceRange} outside $1–$20 · {notGappingUp} not up on the day · {noRelativeVolume} without relative volume.
    </Note>
  );
}
