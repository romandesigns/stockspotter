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
  EXPERIMENTAL_CONCERN_LABEL,
  EXPERIMENTAL_KIND_LABEL,
  EXPERIMENTAL_RETENTION_NOTE,
  EXPERIMENTAL_STRATEGY_NAME,
  experimentalCapacityNote,
  experimentalCapacitySeen,
  experimentalCoverageNote,
  experimentalEmptyState,
  experimentalEvidenceLine,
  experimentalFeedIncomplete,
  experimentalFeedNote,
  experimentalLimitNote,
  experimentalMissedUpdatesNote,
  experimentalObservationAgeSeconds,
  experimentalPanelConcern,
  experimentalPanels,
  experimentalReadinessLine,
  experimentalReadySymbols,
  experimentalResetNote,
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
  // Sticky: see experimentalCapacitySeen. Every capacity field is
  // optional, so a later frame that just omits them must not retract a
  // rejection this client already observed.
  const [capacityLimited, setCapacityLimited] = useState(false);
  useEffect(() => {
    setCapacityLimited((previous) => experimentalCapacitySeen(previous, status));
  }, [status]);

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
  // Every counter below is server-global, not per-strategy, so it is
  // disclosed once here rather than repeated under both panels the way
  // the web panel's per-panel <details> does -- identical text twice on
  // a phone screen reads as two different claims. Only readiness is
  // per-strategy, and that stays in the panel it describes.
  const limitNote = experimentalLimitNote(status);
  // The sticky latch is passed in so a frame that omits every capacity
  // field degrades to the generic warning instead of retracting it.
  const capacityNote = experimentalCapacityNote(status, capacityLimited);
  const missedNote = experimentalMissedUpdatesNote(status);
  // Null on a server that reports none of the reset counters: no line at
  // all, rather than a row of manufactured zeroes.
  const resetNote = experimentalResetNote(status);
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
            {resetNote && (
              <Text mono variant="muted" numberOfLines={2} className="mt-0.5 text-[10px]">{resetNote}</Text>
            )}
            {capacityNote && (
              <Text mono variant="warning" numberOfLines={3} className="mt-1 text-[10px]">{capacityNote}</Text>
            )}
            {missedNote && (
              <Text mono variant="muted" numberOfLines={2} className="mt-0.5 text-[10px]">{missedNote}</Text>
            )}
            {limitNote && <Text variant="muted" numberOfLines={4} className="mt-1 text-[10px]">{limitNote}</Text>}
            <Text variant="muted" numberOfLines={4} className="mt-0.5 text-[10px]">{EXPERIMENTAL_RETENTION_NOTE}</Text>
          </CardContent>
        </Card>
      </Section>
      {panels.map((strategy) => {
        const strategyRows = rows.filter((row) => row.strategy === strategy);
        const ready = experimentalReadySymbols(status, strategy);
        const concern = experimentalPanelConcern({ status, incomplete, capacityLimited, ready });
        // An empty panel is the one place this lane could accidentally
        // make a research claim it has not earned, so which empty state
        // shows is decided by a tested pure function, not inline here.
        const empty = experimentalEmptyState({ status, incomplete, capacityLimited, ready });
        return (
          <Section
            key={strategy}
            title={EXPERIMENTAL_STRATEGY_NAME[strategy]}
            count={strategyRows.length}
            headerExtra={concern ? <Badge variant="warning">{EXPERIMENTAL_CONCERN_LABEL[concern]}</Badge> : undefined}
          >
            <Text mono variant="muted" numberOfLines={1} className="mb-1.5 text-[10px]">{experimentalReadinessLine(status, strategy)}</Text>
            {strategyRows.length === 0 ? (
              <EmptyState label={empty.label} />
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
