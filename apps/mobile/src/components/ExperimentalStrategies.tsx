// The mobile experimental lane's two panels (IAC / Extreme Momentum).
//
// Built out of the primitives the rest of this app already uses --
// Section/Card/CardHeader/CardContent/Badge/Text/EmptyState, the exact
// composition AlertsView's own feed rows use -- rather than a ported
// copy of the web panel's markup. The web side's PanelShell/TickerButton
// are DOM components (<section>/<button>, CSS classes from
// experimentalStrategies.css) and genuinely cannot be reused here; the
// native equivalent of "tap a ticker to chart it" is this app's own
// established Pressable-wrapping-a-Card row, which is what every other
// symbol row already is.
//
// Deliberately colourless: no `good`/green treatment anywhere, not on a
// candidate, not on an observed hold, not on a continuation. A green
// pill next to a ticker reads as "buy" in this app -- `good` is used for
// a positive change percentage and a confirmed catalyst -- and nothing
// in this lane is a recommendation. Kind labels are muted pills; the
// only coloured thing is a warning badge when the feed admits it is
// incomplete.
import { useEffect, useState } from "react";
import { Pressable, View } from "react-native";
import { Badge } from "./ui/badge";
import { Card, CardContent, CardHeader } from "./ui/card";
import { EmptyState } from "./ui/empty-state";
import { Section } from "./ui/section";
import { Text } from "./ui/text";
import { formatPrice, formatTime } from "../format";
import type { ExperimentalStrategyUpdate } from "../lib/experimentalStrategies";
import {
  EXPERIMENTAL_KIND_LABEL,
  EXPERIMENTAL_STRATEGY_NAME,
  experimentalCoverageNote,
  experimentalEvidenceLine,
  experimentalFeedIncomplete,
  experimentalFeedNote,
  experimentalObservationAgeSeconds,
  experimentalPanels,
} from "../lib/experimentalView";
import type { ExperimentalStrategiesFeed } from "../useExperimentalStrategies";

export function ExperimentalStrategies(props: { feed: ExperimentalStrategiesFeed; onSelectSymbol: (symbol: string) => void }) {
  const { enabled, rows, status, gap, connection } = props.feed;
  // One shared 1s tick for every row's observation age, only while the
  // lane is actually on -- with the flag off this interval never exists.
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!enabled) return;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [enabled]);

  if (!enabled) return null;
  const panels = experimentalPanels(status);
  // Reachable, but the server says both strategies are switched off:
  // render nothing at all. Two empty panels would read as "looked, found
  // no candidates", a claim nothing has actually made.
  if (status && panels.length === 0) return null;
  if (!status) {
    return (
      <Section title="Experimental research">
        <EmptyState label="Experimental strategy feed unavailable or connecting." />
      </Section>
    );
  }

  const incomplete = experimentalFeedIncomplete({ connection, status, gap, nowMs: now });
  return (
    <>
      <Section title="Experimental research">
        <Card>
          <CardHeader className="pb-1.5">
            <Text variant="muted" numberOfLines={2} className="flex-1 pr-2 text-xs">{experimentalFeedNote(status, incomplete)}</Text>
            {incomplete && <Badge variant="warning">incomplete</Badge>}
          </CardHeader>
          <CardContent>
            <Text mono variant="muted" numberOfLines={2} className="text-[10px]">{experimentalCoverageNote(status)}</Text>
          </CardContent>
        </Card>
      </Section>
      {panels.map((strategy) => {
        const strategyRows = rows.filter((row) => row.strategy === strategy);
        return (
          <Section key={strategy} title={EXPERIMENTAL_STRATEGY_NAME[strategy]} count={strategyRows.length}>
            {strategyRows.length === 0 ? (
              <EmptyState label="No candidates in the observed feed yet. Warmup and coverage may be incomplete." />
            ) : (
              <View className="gap-1.5">
                {strategyRows.map((row) => (
                  <ExperimentalRow key={row.lifecycleId} row={row} now={now} onPress={() => props.onSelectSymbol(row.symbol)} />
                ))}
              </View>
            )}
          </Section>
        );
      })}
    </>
  );
}

/** One immutable lifecycle observation. Tapping it selects the ticker
 * into the chart the rest of the app already uses (App.tsx's own
 * selectedSymbol -> ChartScreen), the same thing every other symbol row
 * here does -- no separate experimental chart, and no write of any kind
 * back into the research lane. */
function ExperimentalRow({ row, now, onPress }: { row: ExperimentalStrategyUpdate; now: number; onPress: () => void }) {
  return (
    <Pressable onPress={onPress} accessibilityRole="button" accessibilityLabel={`${row.symbol} ${EXPERIMENTAL_KIND_LABEL[row.kind]}, open chart`}>
      <Card>
        <CardHeader className="pb-1.5">
          <View className="flex-row items-baseline gap-2">
            <Text mono className="font-bold">{row.symbol}</Text>
            <Badge variant="muted">{EXPERIMENTAL_KIND_LABEL[row.kind]}</Badge>
            <Text mono className="text-xs">{formatPrice(row.observedPrice)}</Text>
          </View>
          <Text variant="muted" className="text-[10px]">{experimentalObservationAgeSeconds(now, row.eventTimeMs)}s</Text>
        </CardHeader>
        <CardContent>
          <Text mono numberOfLines={2} variant="muted" className="text-[10px]">{experimentalEvidenceLine(row)}</Text>
          {/* Market time vs. when this client could first act on it --
              the lane's own latency disclosure, kept visible rather than
              buried in a disclosure the way the web panel can afford to. */}
          <Text variant="muted" className="mt-0.5 text-[10px]">
            market {formatTime(new Date(row.eventTimeMs).toISOString())} · available {formatTime(new Date(row.decisionAtMs).toISOString())}
          </Text>
        </CardContent>
      </Card>
    </Pressable>
  );
}
