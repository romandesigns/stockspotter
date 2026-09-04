// Phase 3 of the mobile redesign: migrated onto the NativeWind-based
// RNR-style ui/ primitives (src/components/ui/*) and the new
// PressureGauge/Sparkline chart components, replacing the previous
// StyleSheet.create-only styling. Business logic/derivations are
// unchanged from before this pass -- only presentation moved.
import { useEffect, useMemo, useState } from "react";
import { ActivityIndicator, Pressable, ScrollView, View } from "react-native";
import { StatusBar } from "expo-status-bar";
import * as Notifications from "expo-notifications";
import { SafeAreaProvider, SafeAreaView, initialWindowMetrics } from "react-native-safe-area-context";
import type { BarUpdate, CatalystUpdate, HaltWarning } from "@stockspotter/shared-types";
import { useRealtimeFeed } from "./src/useRealtimeFeed";
import { useMarketData } from "./src/useMarketData";
import { useAutoTraderStatus } from "./src/useAutoTraderStatus";
import { useWatchlist } from "./src/useWatchlist";
import { usePriceAlerts } from "./src/usePriceAlerts";
import { useMicropullbackAlerts } from "./src/useMicropullbackAlerts";
import { useGainersForDate, previousSession } from "./src/useGainersForDate";
import { buildAlerts, buildFocusRows, buildWatchlistRows, haltRows, latestHaltRisk, topBullishMomentum, topHaltsByProximity } from "./src/derive";
import { useChartSettings } from "./src/useChartSettings";
import { ChartScreen } from "./src/ChartScreen";
import { UpdatedAgo } from "./src/UpdatedAgo";
import { CatalystFlag } from "./src/components/CatalystFlag";
import { HaltMiniCard } from "./src/components/HaltMiniCard";
import { PressureGauge } from "./src/components/PressureGauge";
import { Sparkline } from "./src/components/Sparkline";
import { Badge } from "./src/components/ui/badge";
import { Button } from "./src/components/ui/button";
import { Card, CardContent, CardHeader } from "./src/components/ui/card";
import { EmptyState } from "./src/components/ui/empty-state";
import { Text } from "./src/components/ui/text";
import { TabsBar, type TabBarItem } from "./src/components/ui/tabs-bar";
import { ToggleGroup, type ToggleGroupOption } from "./src/components/ui/toggle-group";
import { formatPrice, formatTime } from "./src/format";
import { colors } from "./src/theme";
import type { AppTab, AutoTraderStatus, FocusRow, JournalEntry, Mover, Strategy, WatchlistRow } from "./src/types";

const TABS: TabBarItem<AppTab>[] = [
  { key: "radar", label: "Radar", glyph: "⌁" }, { key: "alerts", label: "Alerts", glyph: "!" },
  { key: "markets", label: "Markets", glyph: "↗" }, { key: "watchlist", label: "Watchlist", glyph: "☆" },
  { key: "autotrader", label: "Bot", glyph: "⚙" },
];

// selectedSymbol is lifted here (not local to any one view) so any row's
// tap, anywhere in the app, can drive the chart overlay -- unchanged
// from before this pass.
export default function App() {
  const [tab, setTab] = useState<AppTab>("radar");
  const [selectedSymbol, setSelectedSymbol] = useState<string | null>(null);
  const { saved, toggleSaved } = useWatchlist();
  const feed = useRealtimeFeed();
  const market = useMarketData();
  const autoTrader = useAutoTraderStatus();
  // Owned here, not inside ChartScreen -- an alert has to keep monitoring
  // its symbol even after this chart is closed, so it needs the same
  // feed.barsBySymbol every symbol's live ticks already flow through at
  // this level, not a copy scoped to whatever chart happens to be open.
  const { alerts: priceAlerts, setAlert, toggleAlert, clearAlert } = usePriceAlerts(feed.barsBySymbol);
  // Real cross-symbol "grab my attention" mechanism for a high-confidence
  // micropullback formation (2026-09-03) -- see useMicropullbackAlerts.ts's
  // own header comment for the full reasoning. Owned here, same pattern
  // as usePriceAlerts above: fires regardless of which tab is active or
  // whether this chart is even open.
  useMicropullbackAlerts(feed.micropullbackEvents, feed.momentumBySymbol);
  const alertsForSelectedSymbol = useMemo(
    () => (selectedSymbol ? priceAlerts.filter((a) => a.symbol === selectedSymbol) : []),
    [priceAlerts, selectedSymbol],
  );

  // Tapping a delivered price-alert notification opens straight to that
  // symbol's chart -- the notification's own data.symbol (set in
  // usePriceAlerts.ts) is the only payload it carries.
  useEffect(() => {
    const sub = Notifications.addNotificationResponseReceivedListener((response) => {
      const symbol = response.notification.request.content.data?.symbol;
      if (typeof symbol === "string") setSelectedSymbol(symbol);
    });
    return () => sub.remove();
  }, []);
  const focus = useMemo(
    () => buildFocusRows(feed.events, market.movers.gainers, feed.funnelBySymbol, feed.momentumBySymbol),
    [feed.events, market.movers.gainers, feed.funnelBySymbol, feed.momentumBySymbol],
  );
  const alerts = useMemo(() => buildAlerts(feed.events, feed.catalystsBySymbol, feed.momentumBySymbol), [feed.events, feed.catalystsBySymbol, feed.momentumBySymbol]);
  const halts = useMemo(() => haltRows(feed.events), [feed.events]);
  const topHalts = useMemo(() => topHaltsByProximity(feed.events), [feed.events]);
  const haltRisk = useMemo(() => latestHaltRisk(feed.events), [feed.events]);
  // Chart Page quick-jump chips (2026-09-03) -- reuses halts (already
  // sorted, already non-calm) for the halt half; bullish half is the
  // one genuinely new ranking this ask needed (see derive.ts's own doc
  // comment on topBullishMomentum).
  const bullishTop = useMemo(() => topBullishMomentum(feed.momentumBySymbol, 3), [feed.momentumBySymbol]);
  // Bumped 2 -> 5 (2026-09-04, Roman's own ask) alongside ChartScreen's
  // new "compact" HaltMiniCard layout, which was narrowed specifically
  // to fit at least the top 4 in view at once -- 5 gives one more
  // reachable by a small scroll past that.
  const haltTop = useMemo(() => halts.slice(0, 5), [halts]);
  // Chart display settings (indicators/scale/etc.) -- owned here so they
  // survive both an in-place symbol swap and a full close/reopen, and
  // persist across app restarts (useChartSettings.ts, AsyncStorage).
  const chartSettings = useChartSettings();
  const catalysts = feed.catalystsBySymbol;
  const savedRows = useMemo(
    () => buildWatchlistRows(saved, focus, { gainers: market.movers.gainers, mostActive: market.movers.mostActive, indices: market.indices }),
    [saved, focus, market.movers.gainers, market.movers.mostActive, market.indices],
  );

  return (
    <SafeAreaProvider initialMetrics={initialWindowMetrics}>
      <SafeAreaView className="flex-1 bg-background" edges={["top", "right", "bottom", "left"]}>
        <StatusBar style="light" />
        <View className="flex-1 bg-background">
          <AppHeader status={feed.status} />
          {haltRisk && <RiskStrip reading={haltRisk} />}
          <ScrollView className="flex-1" contentContainerStyle={{ paddingHorizontal: 20, paddingTop: 20, paddingBottom: 28, gap: 22 }} showsVerticalScrollIndicator={false}>
            {tab === "radar" && (
              <RadarView focus={focus} saved={saved} onToggleSaved={toggleSaved} market={market} barsBySymbol={feed.barsBySymbol} halts={topHalts} catalysts={catalysts} onSelectSymbol={setSelectedSymbol} />
            )}
            {tab === "alerts" && <AlertsView alerts={alerts} halts={halts} onSelectSymbol={setSelectedSymbol} />}
            {tab === "markets" && (
              <MarketsView market={market} saved={saved} onToggleSaved={toggleSaved} barsBySymbol={feed.barsBySymbol} catalysts={catalysts} onSelectSymbol={setSelectedSymbol} />
            )}
            {tab === "watchlist" && <WatchlistView rows={savedRows} onToggleSaved={toggleSaved} catalysts={catalysts} onSelectSymbol={setSelectedSymbol} />}
            {tab === "autotrader" && <AutoTraderView status={autoTrader.status} onSelectSymbol={setSelectedSymbol} />}
          </ScrollView>
          <TabsBar
            items={TABS}
            active={tab}
            onChange={setTab}
            renderBadge={(key) => (key === "alerts" && alerts.length + halts.length > 0 ? <View className="absolute -right-1 top-0 h-1.5 w-1.5 rounded-full bg-warning" /> : null)}
          />
        </View>
        {selectedSymbol && (
          <ChartScreen
            symbol={selectedSymbol}
            liveBars={feed.barsBySymbol.get(selectedSymbol) ?? []}
            subMinuteLiveBars={feed.subMinuteBarsBySymbol.get(selectedSymbol) ?? []}
            momentum={feed.momentumBySymbol.get(selectedSymbol) ?? null}
            alerts={alertsForSelectedSymbol}
            onSetAlert={(direction, targetPrice) => setAlert(selectedSymbol, direction, targetPrice)}
            onToggleAlert={(direction, enabled) => toggleAlert(selectedSymbol, direction, enabled)}
            onClearAlert={(direction) => clearAlert(selectedSymbol, direction)}
            onClose={() => setSelectedSymbol(null)}
            onSelectSymbol={setSelectedSymbol}
            chartSettings={chartSettings.settings}
            onToggleIndicator={chartSettings.toggleIndicator}
            onAutoScaleChange={chartSettings.setAutoScale}
            onFitIndicatorsChange={chartSettings.setFitIndicators}
            onScaleModeChange={chartSettings.setScaleMode}
            onChartTypeChange={chartSettings.setChartType}
            bullishTop={bullishTop}
            haltTop={haltTop}
            catalysts={catalysts}
          />
        )}
      </SafeAreaView>
    </SafeAreaProvider>
  );
}

// Same 3-state semantics/wording as the web app's own ConnectionStatus
// component. The dot's color is a real runtime value (not a fixed
// token), so it stays inline style -- NativeWind className can't carry
// an arbitrary JS-computed hex the way a static Tailwind class can.
const CONNECTION_LABEL: Record<"connecting" | "open" | "closed", string> = {
  connecting: "connecting…", open: "live", closed: "disconnected — retrying",
};
const CONNECTION_COLOR: Record<"connecting" | "open" | "closed", string> = {
  connecting: colors.warning, open: colors.good, closed: colors.critical,
};

function AppHeader({ status }: { status: "connecting" | "open" | "closed" }) {
  const dotColor = CONNECTION_COLOR[status];
  return (
    <View className="flex-row items-start px-5 pb-3.5 pt-4">
      <View>
        <Text className="text-xl font-bold tracking-tight text-accent">stockspotter</Text>
        <Text variant="muted" className="mt-0.5 text-[11px]">{marketSessionLabel()}</Text>
      </View>
      <View className="ml-auto flex-row items-center gap-1.5 pt-1" accessibilityLabel={`Market feed ${CONNECTION_LABEL[status]}`}>
        <View className="h-1.5 w-1.5 rounded-full" style={{ backgroundColor: dotColor }} />
        <Text className="text-[11px] font-semibold" style={{ color: dotColor }}>{CONNECTION_LABEL[status]}</Text>
      </View>
    </View>
  );
}

// Same amber/red escalation the web app's own Halt panel uses -- "calm"
// never reaches here (latestHaltRisk already filters it out). Now uses
// PressureGauge for the visual instead of plain percentage text -- the
// gauge's second real call site (HaltRow is the other), reading the
// exact same proximityRatio/level data.
function RiskStrip({ reading }: { reading: NonNullable<ReturnType<typeof latestHaltRisk>> }) {
  const escalationColor = reading.level === "red" ? colors.critical : colors.warning;
  return (
    <View className="mx-5 flex-row items-center gap-3 border-y border-border py-2.5" accessibilityRole="alert">
      <PressureGauge reading={reading} size={34} />
      <Text className="flex-1 text-xs text-text">
        <Text className="font-bold" style={{ color: escalationColor }}>{reading.symbol}</Text> halt risk
      </Text>
    </View>
  );
}

/** Shared star toggle for a saved-to-watchlist symbol -- a thin wrapper
 * around Button's ghost/icon variant instead of a bare Pressable. Reused
 * across Focus, Top Gainers, Markets, Most Active, and Watchlist rows --
 * every real save point in the app. */
function SaveStar({ symbol, saved, onToggleSaved }: { symbol: string; saved: boolean; onToggleSaved: (symbol: string) => void }) {
  return (
    <Button
      variant="ghost" size="icon" className="h-auto w-auto"
      onPress={() => onToggleSaved(symbol)} hitSlop={10}
      accessibilityRole="button" accessibilityLabel={`${saved ? "Remove" : "Add"} ${symbol} ${saved ? "from" : "to"} watchlist`}
    >
      <Text className={saved ? "text-lg text-accent" : "text-lg text-muted"}>{saved ? "★" : "☆"}</Text>
    </Button>
  );
}

function RadarView(props: {
  focus: FocusRow[]; saved: Set<string>; onToggleSaved: (symbol: string) => void;
  market: ReturnType<typeof useMarketData>; barsBySymbol: Map<string, BarUpdate[]>;
  halts: HaltWarning[]; catalysts: Map<string, CatalystUpdate>; onSelectSymbol: (symbol: string) => void;
}) {
  return (
    <>
      <Section title="Focus">
        {props.focus.length === 0 ? (
          <EmptyState label="Waiting for the scanner's first signal…" />
        ) : (
          props.focus.slice(0, 6).map((row) => (
            <SymbolRow key={row.symbol} row={row} saved={props.saved.has(row.symbol)} onToggleSaved={props.onToggleSaved} catalysts={props.catalysts} onPress={() => props.onSelectSymbol(row.symbol)} />
          ))
        )}
      </Section>
      <Section title="Halt Early-Warning">
        {props.halts.length === 0 ? (
          <EmptyState label="Waiting for the scanner's first trade…" />
        ) : (
          <View className="flex-row flex-wrap justify-between gap-y-1.5">
            {props.halts.slice(0, 6).map((r) => (
              <HaltMiniCard key={r.symbol} reading={r} catalysts={props.catalysts} onPress={() => props.onSelectSymbol(r.symbol)} />
            ))}
          </View>
        )}
      </Section>
      <TopGainersSection
        liveGainers={props.market.movers.gainers} lastUpdated={props.market.lastUpdated}
        saved={props.saved} onToggleSaved={props.onToggleSaved} barsBySymbol={props.barsBySymbol}
        catalysts={props.catalysts} onSelectSymbol={props.onSelectSymbol}
      />
      {/* Moved to the very bottom of the home tab per Roman's explicit ask --
          Halt Early-Warning takes its old spot right under Focus instead. */}
      <Section title="Market">
        {props.market.loading && props.market.indices.length === 0 ? (
          <Loading />
        ) : (
          <View className="flex-row pt-1">
            {props.market.indices.map((reading) => (
              <Pressable key={reading.symbol} className="flex-1" onPress={() => props.onSelectSymbol(reading.symbol)}>
                <Text mono className="text-[11px] font-bold">{reading.symbol}</Text>
                <Text mono variant={reading.changePct >= 0 ? "good" : "critical"} className="mt-0.5 text-[11px]">{formatPct(reading.changePct)}</Text>
              </Pressable>
            ))}
          </View>
        )}
      </Section>
    </>
  );
}


const DATE_PRESET_OPTIONS: ToggleGroupOption<"today" | "yesterday">[] = [
  { value: "today", label: "Today" },
  { value: "yesterday", label: "Yesterday" },
];

/** Today/Yesterday toggle, not a full calendar -- a phone-sized card has
 * room for two quick options, not the web app's own date-range calendar
 * (SessionDatePicker.tsx). Same real GET /movers/gainers?date= endpoint. */
function TopGainersSection(props: {
  liveGainers: Mover[]; lastUpdated: Date | null; saved: Set<string>; onToggleSaved: (symbol: string) => void;
  barsBySymbol: Map<string, BarUpdate[]>; catalysts: Map<string, CatalystUpdate>; onSelectSymbol: (symbol: string) => void;
}) {
  const [date, setDate] = useState<string | null>(null);
  const historical = useGainersForDate(date);
  const rows = date ? historical.rows : props.liveGainers;
  const yesterday = useMemo(() => previousSession(new Date()), []);
  const preset = date === yesterday ? "yesterday" : "today";

  return (
    <View>
      <View className="mb-1.5 flex-row items-center justify-between">
        <Text variant="muted" className="text-[10px] font-semibold uppercase tracking-wider">Top gainers</Text>
        <View className="flex-row items-center gap-2">
          {/* Only meaningful for the live default -- a historical session
              is a one-off snapshot, not something that "updates". */}
          {!date && <UpdatedAgo lastUpdated={props.lastUpdated} />}
          <ToggleGroup options={DATE_PRESET_OPTIONS} value={preset} onChange={(v) => setDate(v === "yesterday" ? yesterday : null)} />
        </View>
      </View>
      {date && historical.loading ? (
        <Loading />
      ) : rows.length === 0 ? (
        <EmptyState label="Waiting for the universe scan…" />
      ) : (
        <View className="gap-1.5">
          {rows.slice(0, 5).map((mover) => (
            <MoverRow key={mover.symbol} mover={mover} bars={props.barsBySymbol.get(mover.symbol)} saved={props.saved.has(mover.symbol)} onToggleSaved={props.onToggleSaved} catalysts={props.catalysts} onPress={() => props.onSelectSymbol(mover.symbol)} detail={moverDetail(mover)} />
          ))}
        </View>
      )}
    </View>
  );
}

/** Shared card row for a ranked mover (Top Gainers / Most Active) --
 * ticker + optional sparkline + change%, with a secondary detail line
 * (volume) and the save star. Sparkline only renders when barsBySymbol
 * actually has live history for this symbol (see Sparkline's own doc
 * comment on why that's a real, deliberate scope decision, not a bug). */
function MoverRow(props: {
  mover: Mover; bars?: BarUpdate[]; saved: boolean; onToggleSaved: (symbol: string) => void;
  catalysts: Map<string, CatalystUpdate>; onPress: () => void; detail: string;
}) {
  return (
    <Pressable onPress={props.onPress}>
      <Card>
        <CardHeader className="pb-1.5">
          <View className="flex-row items-center gap-2">
            <Text mono className="font-bold">{props.mover.symbol}</Text>
            <CatalystFlag symbol={props.mover.symbol} catalysts={props.catalysts} />
            {props.bars && <Sparkline bars={props.bars} />}
          </View>
          <View className="flex-row items-center gap-2">
            <Text mono variant={props.mover.changePct >= 0 ? "good" : "critical"} className="font-semibold">{formatPct(props.mover.changePct)}</Text>
            <SaveStar symbol={props.mover.symbol} saved={props.saved} onToggleSaved={props.onToggleSaved} />
          </View>
        </CardHeader>
        <CardContent>
          <Text variant="muted" className="text-xs">{props.detail}</Text>
        </CardContent>
      </Card>
    </Pressable>
  );
}

function SymbolRow({ row, saved, onToggleSaved, catalysts, onPress }: { row: FocusRow; saved: boolean; onToggleSaved: (symbol: string) => void; catalysts: Map<string, CatalystUpdate>; onPress: () => void }) {
  return (
    <Pressable onPress={onPress}>
      <Card className="mb-1.5">
        <CardHeader className="pb-1.5">
          <View className="flex-row items-baseline gap-2">
            <Text mono className="font-bold">{row.symbol}</Text>
            <CatalystFlag symbol={row.symbol} catalysts={catalysts} />
            <Text mono className="text-xs">{formatPrice(row.price)}</Text>
          </View>
          <View className="flex-row items-center gap-2">
            <Text mono variant={row.changePct >= 0 ? "good" : "critical"} className="font-semibold">{formatPct(row.changePct)}</Text>
            <Text variant="muted" className="text-[10px]">{formatTime(row.timestamp)}</Text>
            <SaveStar symbol={row.symbol} saved={saved} onToggleSaved={onToggleSaved} />
          </View>
        </CardHeader>
        <CardContent>
          <Text numberOfLines={1} variant={row.strong ? "good" : "muted"} className="text-xs">{row.detail}</Text>
        </CardContent>
      </Card>
    </Pressable>
  );
}

function AlertsView({ alerts, halts, onSelectSymbol }: { alerts: ReturnType<typeof buildAlerts>; halts: HaltWarning[]; onSelectSymbol: (symbol: string) => void }) {
  return (
    <>
      {halts.length > 0 && (
        <Section title="Halt risk">
          {halts.map((r) => <HaltRow key={r.symbol} reading={r} onPress={() => onSelectSymbol(r.symbol)} />)}
        </Section>
      )}
      <Section title="Latest alerts">
        {alerts.length === 0 ? (
          <EmptyState label="No ignition, breakout, or catalyst alerts yet." />
        ) : (
          <View className="gap-1.5">
            {alerts.map((alert) => (
              <Pressable key={alert.id} onPress={() => onSelectSymbol(alert.symbol)}>
                <Card>
                  <CardHeader className="pb-1.5">
                    <View className="flex-row items-center gap-2">
                      <Text mono className="font-bold">{alert.symbol}</Text>
                      <Badge>{alert.label}</Badge>
                      {/* Same "act fast" urgency treatment as web's chip-warning
                          MPB chip (IgnitionPanel.tsx) -- a micropullback entry
                          fires on thinner, faster evidence than a standard
                          consolidation breakout and shouldn't read identically
                          to it. See ConsolidationStrategy's own doc comment. */}
                      {alert.micropullback && <Badge variant="warning">MPB</Badge>}
                      {/* Whether real momentum currently backs this catalyst,
                          not just that it was tagged -- derive.ts's
                          catalystConfirmation(), same real momentum_scorer
                          reading every other panel uses. "pending" (no
                          reading yet) renders nothing rather than a third,
                          duller badge. */}
                      {alert.confirmation === "confirmed" && <Badge variant="good">Confirmed</Badge>}
                      {alert.confirmation === "unconfirmed" && <Badge variant="muted">Unconfirmed</Badge>}
                    </View>
                    <Text variant="muted" className="text-[10px]">{formatTime(alert.timestamp)}</Text>
                  </CardHeader>
                  <CardContent>
                    <Text numberOfLines={1} variant="muted" className="text-xs">{alert.detail}</Text>
                  </CardContent>
                </Card>
              </Pressable>
            ))}
          </View>
        )}
      </Section>
    </>
  );
}

/** Auto-trader monitoring tab (2026-09-04, Roman's own "how can we
 * monitor it" ask) -- same Section/Card/Badge/EmptyState composition as
 * AlertsView above, mobile's one established "chronological list of
 * discrete events" pattern. Skips are shown too, visually de-emphasized
 * (muted text) rather than hidden, matching the journal's own
 * "explain inaction, not just wins" design intent
 * (crates/auto-trader/src/journal.rs's doc comment). */
function AutoTraderView({ status, onSelectSymbol }: { status: AutoTraderStatus; onSelectSymbol: (symbol: string) => void }) {
  return (
    <>
      <Section title="Dry run">
        <Card>
          <CardContent className="flex-row items-center justify-around py-3">
            <View className="items-center gap-0.5">
              <Text mono className="font-bold">{status.trades}</Text>
              <Text variant="muted" className="text-[10px] uppercase">trades</Text>
            </View>
            <View className="items-center gap-0.5">
              <Text mono className="font-bold">{status.wins}/{status.losses}</Text>
              <Text variant="muted" className="text-[10px] uppercase">W/L</Text>
            </View>
            <View className="items-center gap-0.5">
              <Text mono className={`font-bold ${status.cumulativePnlUsd >= 0 ? "text-good" : "text-critical"}`}>
                {status.cumulativePnlUsd >= 0 ? "+" : ""}{status.cumulativePnlUsd.toFixed(2)}
              </Text>
              <Text variant="muted" className="text-[10px] uppercase">sim P&L</Text>
            </View>
          </CardContent>
        </Card>
      </Section>

      {status.openPositions.length > 0 && (
        <Section title="Open positions">
          <View className="gap-1.5">
            {status.openPositions.map((p) => (
              <Pressable key={p.symbol} onPress={() => onSelectSymbol(p.symbol)}>
                <Card className="flex-row items-center justify-between px-3 py-2.5">
                  <Text mono className="font-bold">{p.symbol}</Text>
                  <Text variant="muted" className="text-xs">{STRATEGY_LABEL[p.strategy]} · {formatPrice(p.entryPrice)} × {p.qty}</Text>
                </Card>
              </Pressable>
            ))}
          </View>
        </Section>
      )}

      <Section title="Recent activity">
        {status.recentEntries.length === 0 ? (
          <EmptyState label="Nothing yet -- dry-run only, watching for real Micropullback, Ignition, and Breakout signals." />
        ) : (
          <View className="gap-1.5">
            {status.recentEntries.map((entry, i) => (
              <Pressable key={i} onPress={() => onSelectSymbol(entry.symbol)}>
                <AutoTraderJournalRow entry={entry} />
              </Pressable>
            ))}
          </View>
        )}
      </Section>
    </>
  );
}

// Short, readable labels for the journal row's tight width -- the wire
// value itself (Strategy, PascalCase) stays the source of truth, this is
// purely display.
const STRATEGY_LABEL: Record<Strategy, string> = {
  FastFunnel: "Funnel",
  MomentumScorer: "Momentum",
  IgnitionDetector: "Ignition",
  ConsolidationBreakout: "Breakout",
  Micropullback: "Micropullback",
};

function AutoTraderJournalRow({ entry }: { entry: JournalEntry }) {
  if (entry.type === "entered") {
    return (
      <Card className="flex-row items-center justify-between px-3 py-2.5">
        <View className="flex-row items-center gap-2">
          <Text mono className="font-bold">{entry.symbol}</Text>
          <Badge>{STRATEGY_LABEL[entry.strategy]}</Badge>
          <Text variant="muted" className="text-xs">{formatPrice(entry.entryPrice)}</Text>
        </View>
        <Text variant="muted" className="text-[10px]">{formatTime(entry.enteredAt)}</Text>
      </Card>
    );
  }
  if (entry.type === "exited") {
    const win = entry.pnlUsd >= 0;
    return (
      <Card className="flex-row items-center justify-between px-3 py-2.5">
        <View className="flex-row items-center gap-2">
          <Text mono className="font-bold">{entry.symbol}</Text>
          <Badge variant={win ? "good" : "critical"}>{entry.exitReason.replace(/_/g, " ")}</Badge>
          <Text mono className={`text-xs ${win ? "text-good" : "text-critical"}`}>
            {win ? "+" : ""}{entry.pnlUsd.toFixed(2)}
          </Text>
        </View>
        <Text variant="muted" className="text-[10px]">{formatTime(entry.exitedAt)}</Text>
      </Card>
    );
  }
  if (entry.type === "stop_adjusted") {
    return (
      <Card className="flex-row items-center justify-between px-3 py-2.5">
        <View className="flex-row items-center gap-2">
          <Text mono className="font-bold">{entry.symbol}</Text>
          <Text variant="muted" className="text-xs">stop raised to {formatPrice(entry.newStopPrice)}</Text>
        </View>
        <Text variant="muted" className="text-[10px]">{formatTime(entry.at)}</Text>
      </Card>
    );
  }
  return (
    <Card className="flex-row items-center justify-between px-3 py-2.5 opacity-70">
      <View className="flex-row items-center gap-2">
        <Text mono className="font-bold">{entry.symbol}</Text>
        <Badge variant="muted">skipped</Badge>
        <Text variant="muted" className="text-xs">{entry.reason.replace(/_/g, " ")}</Text>
      </View>
      <Text variant="muted" className="text-[10px]">{formatTime(entry.at)}</Text>
    </Card>
  );
}

/** One row per currently at-risk symbol -- the mobile equivalent of the
 * web app's Halt Early-Warning panel (a card per tracked symbol), not
 * just RiskStrip's single-worst-symbol banner. Uses PressureGauge for
 * the proximity visual (this gauge's first real call site) instead of
 * the plain "% of band" text this row used to show. */
function HaltRow({ reading, onPress }: { reading: HaltWarning; onPress: () => void }) {
  const escalationColor = reading.level === "red" ? colors.critical : colors.warning;
  return (
    <Pressable onPress={onPress}>
      <Card className="mb-1.5 flex-row items-center gap-3 border-l-[3px] pl-2" style={{ borderLeftColor: escalationColor }}>
        <PressureGauge reading={reading} size={40} />
        <View className="flex-1 py-2.5">
          <View className="flex-row items-center justify-between">
            <Text mono className="font-bold">{reading.symbol}</Text>
            <Text mono className="text-xs">{formatPrice(reading.currentPrice)}</Text>
          </View>
          <View className="mt-1 flex-row items-center gap-2">
            <Text variant="muted" className="text-[11px]">
              rel vol {reading.relativeVolume === null ? "—" : `${reading.relativeVolume.toFixed(1)}x`}
            </Text>
            {reading.bandDoubled && <Badge variant="critical">2x band</Badge>}
          </View>
        </View>
      </Card>
    </Pressable>
  );
}

function MarketsView({ market, saved, onToggleSaved, barsBySymbol, catalysts, onSelectSymbol }: {
  market: ReturnType<typeof useMarketData>; saved: Set<string>; onToggleSaved: (symbol: string) => void;
  barsBySymbol: Map<string, BarUpdate[]>; catalysts: Map<string, CatalystUpdate>; onSelectSymbol: (symbol: string) => void;
}) {
  return (
    <>
      <Section title="Markets today">
        {market.loading && market.indices.length === 0 ? (
          <Loading />
        ) : (
          <View className="gap-1.5">
            {market.indices.map((reading) => (
              <MoverRow
                key={reading.symbol}
                mover={{ symbol: reading.symbol, price: reading.price, changePct: reading.changePct, volume: 0, session: null }}
                bars={barsBySymbol.get(reading.symbol)}
                saved={saved.has(reading.symbol)} onToggleSaved={onToggleSaved} catalysts={catalysts}
                onPress={() => onSelectSymbol(reading.symbol)} detail={reading.name}
              />
            ))}
          </View>
        )}
        {market.error && market.indices.length === 0 && <EmptyState label="Market service is unavailable." />}
      </Section>
      <Section title="Most active" headerExtra={<UpdatedAgo lastUpdated={market.lastUpdated} />}>
        <View className="gap-1.5">
          {market.movers.mostActive.slice(0, 8).map((mover) => (
            <MoverRow key={mover.symbol} mover={mover} bars={barsBySymbol.get(mover.symbol)} saved={saved.has(mover.symbol)} onToggleSaved={onToggleSaved} catalysts={catalysts} onPress={() => onSelectSymbol(mover.symbol)} detail={moverDetail(mover)} />
          ))}
        </View>
      </Section>
    </>
  );
}

function WatchlistView(props: { rows: WatchlistRow[]; onToggleSaved: (symbol: string) => void; catalysts: Map<string, CatalystUpdate>; onSelectSymbol: (symbol: string) => void }) {
  return (
    <Section title="Watchlist">
      {props.rows.length === 0 ? (
        <EmptyState label="Star a ticker anywhere to save it here." />
      ) : (
        props.rows.map((row) => (
          <WatchlistRowView key={row.symbol} row={row} onToggleSaved={props.onToggleSaved} catalysts={props.catalysts} onPress={() => props.onSelectSymbol(row.symbol)} />
        ))
      )}
    </Section>
  );
}

/** Every row here is by definition saved (it's the Watchlist tab), and
 * price/changePct/timestamp are all nullable -- unlike SymbolRow (a
 * Focus row, which always has real live numbers), a saved symbol might
 * currently have none of that (see buildWatchlistRows's own doc
 * comment), so each is only rendered when actually present rather than
 * formatting a null into a fake "$0.00". */
function WatchlistRowView({ row, onToggleSaved, catalysts, onPress }: { row: WatchlistRow; onToggleSaved: (symbol: string) => void; catalysts: Map<string, CatalystUpdate>; onPress: () => void }) {
  return (
    <Pressable onPress={onPress}>
      <Card className="mb-1.5">
        <CardHeader className="pb-1.5">
          <View className="flex-row items-baseline gap-2">
            <Text mono className="font-bold">{row.symbol}</Text>
            <CatalystFlag symbol={row.symbol} catalysts={catalysts} />
            {row.price != null && <Text mono className="text-xs">{formatPrice(row.price)}</Text>}
          </View>
          <View className="flex-row items-center gap-2">
            {row.changePct != null && <Text mono variant={row.changePct >= 0 ? "good" : "critical"} className="font-semibold">{formatPct(row.changePct)}</Text>}
            {row.timestamp && <Text variant="muted" className="text-[10px]">{formatTime(row.timestamp)}</Text>}
            <SaveStar symbol={row.symbol} saved onToggleSaved={onToggleSaved} />
          </View>
        </CardHeader>
        <CardContent>
          <Text numberOfLines={1} variant={row.strong ? "good" : "muted"} className="text-xs">{row.detail}</Text>
        </CardContent>
      </Card>
    </Pressable>
  );
}

function Section({ title, headerExtra, children }: { title: string; headerExtra?: React.ReactNode; children: React.ReactNode }) {
  return (
    <View>
      {headerExtra ? (
        <View className="mb-1.5 flex-row items-center justify-between">
          <Text variant="muted" className="text-[10px] font-semibold uppercase tracking-wider">{title}</Text>
          <View className="flex-row items-center gap-2">{headerExtra}</View>
        </View>
      ) : (
        <Text variant="muted" className="mb-1.5 text-[10px] font-semibold uppercase tracking-wider">{title}</Text>
      )}
      {children}
    </View>
  );
}

function Loading() {
  return <ActivityIndicator className="py-7" color={colors.accent} />;
}

function marketSessionLabel(): string {
  const now = new Date(); const parts = new Intl.DateTimeFormat("en-US", { timeZone: "America/New_York", hour: "numeric", minute: "2-digit", hour12: false, weekday: "short" }).formatToParts(now);
  const weekday = parts.find((part) => part.type === "weekday")?.value ?? ""; const hour = Number(parts.find((part) => part.type === "hour")?.value ?? 0); const minute = Number(parts.find((part) => part.type === "minute")?.value ?? 0);
  const open = weekday !== "Sat" && weekday !== "Sun" && hour * 60 + minute >= 570 && hour * 60 + minute < 960;
  const time = new Intl.DateTimeFormat("en-US", { timeZone: "America/New_York", hour: "numeric", minute: "2-digit" }).format(now);
  return `${open ? "Market open" : "Market closed"} · ${time} ET`;
}
const formatPct = (value: number) => `${value >= 0 ? "+" : ""}${value.toFixed(1)}%`;
// formatPrice/formatTime moved to ./src/format.ts (2026-09-04) so
// HaltMiniCard.tsx (now shared with ChartScreen) can use the same ones.
const formatVolume = (value: number) => value >= 1_000_000 ? `${(value / 1_000_000).toFixed(1)}M` : value >= 1_000 ? `${(value / 1_000).toFixed(0)}K` : String(value);

const SESSION_LABEL: Record<NonNullable<Mover["session"]>, string> = {
  premarket: "Premarket", regular: "Regular", after_hours: "After-Hours", overnight: "Overnight",
};
/** Volume + session label for a MoverRow's detail line -- omitted, not
 * defaulted, when `mover.session` is null (the historical date-lookup
 * path genuinely can't classify one; see Mover.session's own doc comment
 * in types.ts). Same real ws-server-computed session as web's own
 * MoversList.tsx, not re-derived client-side. */
const moverDetail = (mover: Mover) => {
  const base = `${formatVolume(mover.volume)} vol`;
  return mover.session ? `${base} · ${SESSION_LABEL[mover.session]}` : base;
};
