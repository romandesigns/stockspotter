import { useMemo, useState } from "react";
import { ActivityIndicator, Pressable, ScrollView, StyleSheet, Text, View } from "react-native";
import { StatusBar } from "expo-status-bar";
import { SafeAreaProvider, SafeAreaView, initialWindowMetrics } from "react-native-safe-area-context";
import type { CatalystUpdate, HaltWarning } from "@stockspotter/shared-types";
import { useRealtimeFeed } from "./src/useRealtimeFeed";
import { useMarketData } from "./src/useMarketData";
import { useWatchlist } from "./src/useWatchlist";
import { useGainersForDate, previousSession } from "./src/useGainersForDate";
import { buildAlerts, buildFocusRows, buildWatchlistRows, haltRows, latestHaltRisk } from "./src/derive";
import { ChartScreen } from "./src/ChartScreen";
import { UpdatedAgo } from "./src/UpdatedAgo";
import { colors, monoFont } from "./src/theme";
import type { AppTab, FocusRow, Mover, WatchlistRow } from "./src/types";

const TABS: { key: AppTab; label: string; glyph: string }[] = [
  { key: "radar", label: "Radar", glyph: "⌁" }, { key: "alerts", label: "Alerts", glyph: "!" },
  { key: "markets", label: "Markets", glyph: "↗" }, { key: "watchlist", label: "Watchlist", glyph: "☆" },
];

export default function App() {
  const [tab, setTab] = useState<AppTab>("radar");
  const [selectedSymbol, setSelectedSymbol] = useState<string | null>(null);
  const { saved, toggleSaved } = useWatchlist();
  const feed = useRealtimeFeed();
  const market = useMarketData();
  const focus = useMemo(() => buildFocusRows(feed.events, market.movers.gainers), [feed.events, market.movers.gainers]);
  const alerts = useMemo(() => buildAlerts(feed.events, feed.catalystsBySymbol), [feed.events, feed.catalystsBySymbol]);
  const halts = useMemo(() => haltRows(feed.events), [feed.events]);
  const haltRisk = useMemo(() => latestHaltRisk(feed.events), [feed.events]);
  const catalysts = feed.catalystsBySymbol;
  const savedRows = useMemo(
    () => buildWatchlistRows(saved, focus, { gainers: market.movers.gainers, mostActive: market.movers.mostActive, indices: market.indices }),
    [saved, focus, market.movers.gainers, market.movers.mostActive, market.indices],
  );

  return <SafeAreaProvider initialMetrics={initialWindowMetrics}>
    <SafeAreaView style={styles.safeArea} edges={["top", "right", "bottom", "left"]}>
      <StatusBar style="light" />
      <View style={styles.app}>
        <AppHeader status={feed.status} />
        {haltRisk && <RiskStrip reading={haltRisk} />}
        <ScrollView style={styles.scroll} contentContainerStyle={styles.content} showsVerticalScrollIndicator={false}>
          {tab === "radar" && <RadarView focus={focus} saved={saved} onToggleSaved={toggleSaved} market={market} catalysts={catalysts} onSelectSymbol={setSelectedSymbol} />}
          {tab === "alerts" && <AlertsView alerts={alerts} halts={halts} onSelectSymbol={setSelectedSymbol} />}
          {tab === "markets" && <MarketsView market={market} saved={saved} onToggleSaved={toggleSaved} catalysts={catalysts} onSelectSymbol={setSelectedSymbol} />}
          {tab === "watchlist" && <WatchlistView rows={savedRows} onToggleSaved={toggleSaved} catalysts={catalysts} onSelectSymbol={setSelectedSymbol} />}
        </ScrollView>
        <BottomTabs active={tab} alertCount={alerts.length + halts.length} onChange={setTab} />
      </View>
      {selectedSymbol && (
        <ChartScreen symbol={selectedSymbol} liveBars={feed.barsBySymbol.get(selectedSymbol) ?? []} onClose={() => setSelectedSymbol(null)} />
      )}
    </SafeAreaView>
  </SafeAreaProvider>;
}

// Same 3-state semantics and wording as the web app's own ConnectionStatus
// component (apps/client/src/components/ConnectionStatus.tsx) -- connecting
// is a real "still trying" state (warning color), not lumped in with a
// genuine disconnect (critical color) the way a plain open/not-open binary
// would have shown it.
const CONNECTION_LABEL: Record<"connecting" | "open" | "closed", string> = {
  connecting: "connecting…", open: "live", closed: "disconnected — retrying",
};
const CONNECTION_COLOR: Record<"connecting" | "open" | "closed", string> = {
  connecting: colors.warning, open: colors.good, closed: colors.critical,
};

function AppHeader({ status }: { status: "connecting" | "open" | "closed" }) {
  const dotColor = CONNECTION_COLOR[status];
  return <View style={styles.header}><View><Text style={styles.brand}>stockspotter</Text><Text style={styles.session}>{marketSessionLabel()}</Text></View>
    <View style={styles.connection} accessibilityLabel={`Market feed ${CONNECTION_LABEL[status]}`}><View style={[styles.connectionDot, { backgroundColor: dotColor }]} /><Text style={[styles.connectionText, { color: dotColor }]}>{CONNECTION_LABEL[status]}</Text></View>
  </View>;
}

// Same amber/red escalation the web app's own Halt panel uses
// (.halt-amber/.halt-red -> --warning/--critical) -- "calm" never reaches
// here at all (latestHaltRisk already filters it out), so this is a real
// two-way distinction, not a placeholder.
function RiskStrip({ reading }: { reading: NonNullable<ReturnType<typeof latestHaltRisk>> }) {
  const escalationColor = reading.level === "red" ? colors.critical : colors.warning;
  return <View style={[styles.riskStrip, { borderColor: escalationColor }]} accessibilityRole="alert"><Text style={[styles.riskIcon, { color: escalationColor }]}>△</Text><Text style={styles.riskLabel}><Text style={styles.riskTicker}>{reading.symbol}</Text> halt risk</Text><Text style={[styles.riskValue, { color: escalationColor }]}>{Math.round(reading.proximityRatio * 100)}%</Text></View>;
}

/** Small inline flag next to a ticker, real not decorative -- renders
 * nothing for a symbol with no catalyst record, same rule the web app's
 * CatalystBadge follows. */
function CatalystFlag({ symbol, catalysts }: { symbol: string; catalysts: Map<string, CatalystUpdate> }) {
  if (!catalysts.has(symbol)) return null;
  return <Text style={styles.catalystFlag} accessibilityLabel={`${symbol} has a catalyst`}>⚑</Text>;
}

function RadarView(props: { focus: FocusRow[]; saved: Set<string>; onToggleSaved: (symbol: string) => void; market: ReturnType<typeof useMarketData>; catalysts: Map<string, CatalystUpdate>; onSelectSymbol: (symbol: string) => void }) {
  return <>
    <Section title="Focus">{props.focus.length === 0 ? <Empty label="Waiting for the scanner's first signal…" /> : props.focus.slice(0, 6).map((row) => <SymbolRow key={row.symbol} row={row} saved={props.saved.has(row.symbol)} onToggleSaved={props.onToggleSaved} catalysts={props.catalysts} onPress={() => props.onSelectSymbol(row.symbol)} />)}</Section>
    <Section title="Market">{props.market.loading && props.market.indices.length === 0 ? <Loading /> : <View style={styles.marketStrip}>{props.market.indices.map((reading) => <Pressable key={reading.symbol} style={styles.marketStripItem} onPress={() => props.onSelectSymbol(reading.symbol)}><Text style={styles.marketSymbol}>{reading.symbol}</Text><Text style={reading.changePct >= 0 ? styles.positive : styles.negative}>{formatPct(reading.changePct)}</Text></Pressable>)}</View>}</Section>
    <TopGainersSection liveGainers={props.market.movers.gainers} lastUpdated={props.market.lastUpdated} saved={props.saved} onToggleSaved={props.onToggleSaved} catalysts={props.catalysts} onSelectSymbol={props.onSelectSymbol} />
  </>;
}

/** Today/Yesterday toggle, not a full calendar -- a phone-sized card has
 * room for two quick options, not the web app's own date-range calendar
 * (SessionDatePicker.tsx). Same real GET /movers/gainers?date= endpoint. */
function TopGainersSection(props: { liveGainers: Mover[]; lastUpdated: Date | null; saved: Set<string>; onToggleSaved: (symbol: string) => void; catalysts: Map<string, CatalystUpdate>; onSelectSymbol: (symbol: string) => void }) {
  const [date, setDate] = useState<string | null>(null);
  const historical = useGainersForDate(date);
  const rows = date ? historical.rows : props.liveGainers;
  const yesterday = useMemo(() => previousSession(new Date()), []);

  return <View style={styles.section}>
    <View style={styles.sectionHeadRow}>
      <Text style={styles.sectionTitle}>Top gainers</Text>
      <View style={styles.sectionHeadExtra}>
        {/* Only meaningful for the live default -- a historical session
            (Yesterday, or any picked date) is a one-off snapshot, not
            something that "updates". */}
        {!date && <UpdatedAgo lastUpdated={props.lastUpdated} />}
        <View style={styles.datePresets}>
          <Pressable onPress={() => setDate(null)} style={[styles.datePreset, !date && styles.datePresetActive]}><Text style={[styles.datePresetText, !date && styles.datePresetTextActive]}>Today</Text></Pressable>
          <Pressable onPress={() => setDate(yesterday)} style={[styles.datePreset, date === yesterday && styles.datePresetActive]}><Text style={[styles.datePresetText, date === yesterday && styles.datePresetTextActive]}>Yesterday</Text></Pressable>
        </View>
      </View>
    </View>
    {date && historical.loading ? <Loading /> : rows.length === 0 ? <Empty label="Waiting for the universe scan…" /> : rows.slice(0, 5).map((mover) => (
      <Pressable key={mover.symbol} style={styles.dataRow} onPress={() => props.onSelectSymbol(mover.symbol)}>
        <Text style={styles.symbol}>{mover.symbol}</Text><CatalystFlag symbol={mover.symbol} catalysts={props.catalysts} />
        <Text style={styles.secondary}>{formatVolume(mover.volume)} vol</Text>
        <Text style={mover.changePct >= 0 ? styles.positiveEnd : styles.negativeEnd}>{formatPct(mover.changePct)}</Text>
        <SaveStar symbol={mover.symbol} saved={props.saved.has(mover.symbol)} onToggleSaved={props.onToggleSaved} />
      </Pressable>
    ))}
  </View>;
}

/** Shared star toggle for a saved-to-watchlist symbol -- pulled out of
 * SymbolRow's own inline version so Top Gainers/Most Active/Markets rows
 * (none of which had any way to add a NEW symbol to the watchlist before
 * this -- only Focus and the Watchlist tab itself did, which meant a
 * quiet period with an empty Focus left no way to save anything) can
 * reuse the exact same toggle instead of a re-derived copy. */
function SaveStar({ symbol, saved, onToggleSaved }: { symbol: string; saved: boolean; onToggleSaved: (symbol: string) => void }) {
  return <Pressable onPress={() => onToggleSaved(symbol)} hitSlop={10} accessibilityRole="button" accessibilityLabel={`${saved ? "Remove" : "Add"} ${symbol} ${saved ? "from" : "to"} watchlist`}>
    <Text style={[styles.save, saved && styles.saveActive]}>{saved ? "★" : "☆"}</Text>
  </Pressable>;
}

function SymbolRow({ row, saved, onToggleSaved, catalysts, onPress }: { row: FocusRow; saved: boolean; onToggleSaved: (symbol: string) => void; catalysts: Map<string, CatalystUpdate>; onPress: () => void }) {
  return <Pressable style={styles.signalRow} onPress={onPress}><View style={styles.dataRowNoBorder}><Text style={styles.symbol}>{row.symbol}</Text><CatalystFlag symbol={row.symbol} catalysts={catalysts} /><Text style={styles.price}>{formatPrice(row.price)}</Text><Text style={row.changePct >= 0 ? styles.positiveEnd : styles.negativeEnd}>{formatPct(row.changePct)}</Text><Text style={styles.time}>{formatTime(row.timestamp)}</Text><SaveStar symbol={row.symbol} saved={saved} onToggleSaved={onToggleSaved} /></View><Text numberOfLines={1} style={[styles.signalDetail, row.strong && styles.signalDetailStrong]}>{row.detail}</Text></Pressable>;
}

function AlertsView({ alerts, halts, onSelectSymbol }: { alerts: ReturnType<typeof buildAlerts>; halts: HaltWarning[]; onSelectSymbol: (symbol: string) => void }) {
  return <>
    {halts.length > 0 && <Section title="Halt risk">{halts.map((r) => <HaltRow key={r.symbol} reading={r} onPress={() => onSelectSymbol(r.symbol)} />)}</Section>}
    <Section title="Latest alerts">{alerts.length === 0 ? <Empty label="No ignition, breakout, or catalyst alerts yet." /> : alerts.map((alert) => <Pressable key={alert.id} style={styles.signalRow} onPress={() => onSelectSymbol(alert.symbol)}><View style={styles.dataRowNoBorder}><Text style={styles.symbol}>{alert.symbol}</Text><Text style={styles.alertKind}>{alert.label}</Text><Text style={styles.timeEnd}>{formatTime(alert.timestamp)}</Text></View><Text numberOfLines={1} style={styles.signalDetail}>{alert.detail}</Text></Pressable>)}</Section>
  </>;
}

/** One row per currently at-risk symbol -- the mobile equivalent of the
 * web app's Halt Early-Warning panel (a card per tracked symbol), not
 * just RiskStrip's single-worst-symbol banner. */
function HaltRow({ reading, onPress }: { reading: HaltWarning; onPress: () => void }) {
  const escalationColor = reading.level === "red" ? colors.critical : colors.warning;
  return <Pressable style={[styles.signalRow, { borderLeftWidth: 3, borderLeftColor: escalationColor, paddingLeft: 8 }]} onPress={onPress}>
    <View style={styles.dataRowNoBorder}>
      <Text style={styles.symbol}>{reading.symbol}</Text>
      <Text style={styles.price}>{formatPrice(reading.currentPrice)}</Text>
      <Text style={[styles.riskValueEnd, { color: escalationColor }]}>{Math.round(reading.proximityRatio * 100)}% of band</Text>
    </View>
    <Text style={styles.signalDetail}>rel vol {reading.relativeVolume === null ? "—" : `${reading.relativeVolume.toFixed(1)}x`}{reading.bandDoubled ? " · 2x band" : ""}</Text>
  </Pressable>;
}

function MarketsView({ market, saved, onToggleSaved, catalysts, onSelectSymbol }: { market: ReturnType<typeof useMarketData>; saved: Set<string>; onToggleSaved: (symbol: string) => void; catalysts: Map<string, CatalystUpdate>; onSelectSymbol: (symbol: string) => void }) {
  return <><Section title="Markets today">{market.loading && market.indices.length === 0 ? <Loading /> : market.indices.map((reading) => <Pressable key={reading.symbol} style={styles.signalRow} onPress={() => onSelectSymbol(reading.symbol)}><View style={styles.dataRowNoBorder}><Text style={styles.symbol}>{reading.symbol}</Text><Text style={styles.price}>{formatPrice(reading.price)}</Text><Text style={reading.changePct >= 0 ? styles.positiveEnd : styles.negativeEnd}>{formatPct(reading.changePct)}</Text><SaveStar symbol={reading.symbol} saved={saved.has(reading.symbol)} onToggleSaved={onToggleSaved} /></View><Text style={styles.signalDetail}>{reading.name}</Text></Pressable>)}{market.error && market.indices.length === 0 && <Empty label="Market service is unavailable." />}</Section>
    <Section title="Most active" headerExtra={<UpdatedAgo lastUpdated={market.lastUpdated} />}>{market.movers.mostActive.slice(0, 8).map((mover) => <Pressable key={mover.symbol} style={styles.dataRow} onPress={() => onSelectSymbol(mover.symbol)}><Text style={styles.symbol}>{mover.symbol}</Text><CatalystFlag symbol={mover.symbol} catalysts={catalysts} /><Text style={styles.secondary}>{formatPrice(mover.price)}</Text><Text style={styles.volumeEnd}>{formatVolume(mover.volume)}</Text><SaveStar symbol={mover.symbol} saved={saved.has(mover.symbol)} onToggleSaved={onToggleSaved} /></Pressable>)}</Section></>;
}

function WatchlistView(props: { rows: WatchlistRow[]; onToggleSaved: (symbol: string) => void; catalysts: Map<string, CatalystUpdate>; onSelectSymbol: (symbol: string) => void }) {
  return <Section title="Watchlist">{props.rows.length === 0 ? <Empty label="Star a ticker anywhere to save it here." /> : props.rows.map((row) => <WatchlistRowView key={row.symbol} row={row} onToggleSaved={props.onToggleSaved} catalysts={props.catalysts} onPress={() => props.onSelectSymbol(row.symbol)} />)}</Section>;
}

/** Every row here is by definition saved (it's the Watchlist tab), and
 * price/changePct/timestamp are all nullable -- unlike SymbolRow (a
 * Focus row, which always has real live numbers), a saved symbol might
 * currently have none of that (see buildWatchlistRows's own doc
 * comment), so each is only rendered when actually present rather than
 * formatting a null into a fake "$0.00". */
function WatchlistRowView({ row, onToggleSaved, catalysts, onPress }: { row: WatchlistRow; onToggleSaved: (symbol: string) => void; catalysts: Map<string, CatalystUpdate>; onPress: () => void }) {
  return <Pressable style={styles.signalRow} onPress={onPress}>
    <View style={styles.dataRowNoBorder}>
      <Text style={styles.symbol}>{row.symbol}</Text>
      <CatalystFlag symbol={row.symbol} catalysts={catalysts} />
      {row.price != null && <Text style={styles.price}>{formatPrice(row.price)}</Text>}
      {row.changePct != null && <Text style={row.changePct >= 0 ? styles.positiveEnd : styles.negativeEnd}>{formatPct(row.changePct)}</Text>}
      {row.timestamp && <Text style={styles.time}>{formatTime(row.timestamp)}</Text>}
      <SaveStar symbol={row.symbol} saved onToggleSaved={onToggleSaved} />
    </View>
    <Text numberOfLines={1} style={[styles.signalDetail, row.strong && styles.signalDetailStrong]}>{row.detail}</Text>
  </Pressable>;
}

function Section({ title, headerExtra, children }: { title: string; headerExtra?: React.ReactNode; children: React.ReactNode }) {
  return <View style={styles.section}>
    {headerExtra ? <View style={styles.sectionHeadRow}><Text style={styles.sectionTitle}>{title}</Text><View style={styles.sectionHeadExtra}>{headerExtra}</View></View> : <Text style={styles.sectionTitle}>{title}</Text>}
    {children}
  </View>;
}
function Empty({ label }: { label: string }) { return <Text style={styles.empty}>{label}</Text>; }
function Loading() { return <ActivityIndicator style={styles.loading} color={colors.accent} />; }

function BottomTabs({ active, alertCount, onChange }: { active: AppTab; alertCount: number; onChange: (tab: AppTab) => void }) {
  return <View style={styles.tabs} accessibilityRole="tablist">{TABS.map((tab) => { const selected = active === tab.key; return <Pressable key={tab.key} style={styles.tab} onPress={() => onChange(tab.key)} accessibilityRole="tab" accessibilityState={{ selected }}><View><Text style={[styles.tabGlyph, selected && styles.tabActive]}>{tab.glyph}</Text>{tab.key === "alerts" && alertCount > 0 && <View style={styles.alertDot} />}</View><Text style={[styles.tabLabel, selected && styles.tabActive]}>{tab.label}</Text></Pressable>; })}</View>;
}

function marketSessionLabel(): string {
  const now = new Date(); const parts = new Intl.DateTimeFormat("en-US", { timeZone: "America/New_York", hour: "numeric", minute: "2-digit", hour12: false, weekday: "short" }).formatToParts(now);
  const weekday = parts.find((part) => part.type === "weekday")?.value ?? ""; const hour = Number(parts.find((part) => part.type === "hour")?.value ?? 0); const minute = Number(parts.find((part) => part.type === "minute")?.value ?? 0);
  const open = weekday !== "Sat" && weekday !== "Sun" && hour * 60 + minute >= 570 && hour * 60 + minute < 960;
  const time = new Intl.DateTimeFormat("en-US", { timeZone: "America/New_York", hour: "numeric", minute: "2-digit" }).format(now);
  return `${open ? "Market open" : "Market closed"} · ${time} ET`;
}
const formatPct = (value: number) => `${value >= 0 ? "+" : ""}${value.toFixed(1)}%`;
const formatPrice = (value: number) => `$${value.toFixed(2)}`;
const formatTime = (value: string) => new Intl.DateTimeFormat("en-US", { timeZone: "America/New_York", hour: "numeric", minute: "2-digit" }).format(new Date(value));
const formatVolume = (value: number) => value >= 1_000_000 ? `${(value / 1_000_000).toFixed(1)}M` : value >= 1_000 ? `${(value / 1_000).toFixed(0)}K` : String(value);

const styles = StyleSheet.create({
  safeArea: { flex: 1, backgroundColor: colors.background }, app: { flex: 1, backgroundColor: colors.background },
  // brand color matches the web app's own .app-wordmark (color: var(--accent))
  // -- the wordmark is a real brand-identity element, not plain body text.
  header: { paddingHorizontal: 20, paddingTop: 18, paddingBottom: 14, flexDirection: "row", alignItems: "flex-start" }, brand: { color: colors.accent, fontSize: 20, fontWeight: "700", letterSpacing: -0.8 }, session: { color: colors.muted, fontSize: 11, marginTop: 3 },
  connection: { marginLeft: "auto", flexDirection: "row", alignItems: "center", gap: 6, paddingTop: 3 }, connectionDot: { width: 6, height: 6, borderRadius: 3 }, connectionText: { fontSize: 11, fontWeight: "600" },
  riskStrip: { marginHorizontal: 20, paddingVertical: 11, borderTopWidth: StyleSheet.hairlineWidth, borderBottomWidth: StyleSheet.hairlineWidth, flexDirection: "row", alignItems: "center" }, riskIcon: { fontSize: 18, marginRight: 9 }, riskLabel: { color: colors.text, fontSize: 12, flex: 1 }, riskTicker: { fontWeight: "700" }, riskValue: { fontFamily: monoFont, fontSize: 12 }, riskValueEnd: { fontFamily: monoFont, fontSize: 12, marginLeft: "auto" },
  scroll: { flex: 1 }, content: { paddingHorizontal: 20, paddingTop: 20, paddingBottom: 28 }, section: { marginBottom: 26 }, sectionTitle: { color: colors.muted, fontSize: 10, fontWeight: "600", letterSpacing: 1.1, textTransform: "uppercase", marginBottom: 7 },
  sectionHeadRow: { flexDirection: "row", alignItems: "center", justifyContent: "space-between", marginBottom: 7 },
  sectionHeadExtra: { flexDirection: "row", alignItems: "center", gap: 8 },
  datePresets: { flexDirection: "row", gap: 6 }, datePreset: { paddingHorizontal: 8, paddingVertical: 3, borderRadius: 999, backgroundColor: colors.surface }, datePresetActive: { backgroundColor: colors.accentBg }, datePresetText: { color: colors.muted, fontSize: 10, fontWeight: "600" }, datePresetTextActive: { color: colors.accent },
  signalRow: { minHeight: 62, paddingVertical: 11, borderBottomWidth: StyleSheet.hairlineWidth, borderColor: colors.divider }, dataRow: { minHeight: 51, flexDirection: "row", alignItems: "center", borderBottomWidth: StyleSheet.hairlineWidth, borderColor: colors.divider }, dataRowNoBorder: { flexDirection: "row", alignItems: "baseline" },
  symbol: { color: colors.text, width: 58, fontFamily: monoFont, fontSize: 14, fontWeight: "700" }, catalystFlag: { color: colors.accent, fontSize: 11, marginRight: 8 }, price: { color: colors.text, fontFamily: monoFont, fontSize: 12 }, secondary: { color: colors.muted, fontSize: 11 }, positive: { color: colors.good, fontFamily: monoFont, fontSize: 11, marginTop: 3 }, negative: { color: colors.critical, fontFamily: monoFont, fontSize: 11, marginTop: 3 }, positiveEnd: { color: colors.good, fontFamily: monoFont, fontSize: 12, fontWeight: "600", marginLeft: "auto" }, negativeEnd: { color: colors.critical, fontFamily: monoFont, fontSize: 12, fontWeight: "600", marginLeft: "auto" }, volumeEnd: { color: colors.muted, fontFamily: monoFont, fontSize: 11, marginLeft: "auto" }, time: { color: colors.dim, fontFamily: monoFont, fontSize: 10, marginLeft: 10 }, timeEnd: { color: colors.dim, fontFamily: monoFont, fontSize: 10, marginLeft: "auto" },
  save: { color: colors.muted, fontSize: 19, marginLeft: 10 }, saveActive: { color: colors.accent }, signalDetail: { color: colors.muted, fontSize: 11, marginLeft: 58, marginTop: 6 }, signalDetailStrong: { color: colors.good }, alertKind: { color: colors.muted, fontSize: 11, marginLeft: 2 },
  marketStrip: { flexDirection: "row", paddingTop: 3 }, marketStripItem: { flex: 1 }, marketSymbol: { color: colors.text, fontFamily: monoFont, fontSize: 11, fontWeight: "700" }, empty: { color: colors.muted, fontSize: 12, paddingVertical: 28, textAlign: "center" }, loading: { paddingVertical: 28 },
  tabs: { minHeight: 70, paddingBottom: 8, flexDirection: "row", borderTopWidth: StyleSheet.hairlineWidth, borderColor: colors.divider, backgroundColor: colors.background }, tab: { flex: 1, minHeight: 60, alignItems: "center", justifyContent: "center" }, tabGlyph: { color: colors.muted, fontSize: 18, height: 22, textAlign: "center" }, tabLabel: { color: colors.muted, fontSize: 9, marginTop: 2 }, tabActive: { color: colors.text, fontWeight: "600" }, alertDot: { position: "absolute", width: 5, height: 5, borderRadius: 3, right: -4, top: 1, backgroundColor: colors.warning },
});
