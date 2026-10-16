// Home hub (2026-10-16 redesign, Hub.dc.html): replaces the Radar tab.
// "Now" header, the Most active signal card, then the seven dashboard
// panels as compact sections in a fixed order (hub.ts PANEL_ORDER), then
// this session's speed alerts. Selection rules live in hub.ts (pure,
// tested); this file only lays them out.
//
// Layout stability: each section shows a fixed number of rows (1-3) with
// fixed row heights, so a row changing symbol or value as the feed ticks
// does not move anything below it. No layout animations on pressable
// containers (LinearTransition there caused stale taps on Android).
import { useState } from "react";
import { Pressable, View } from "react-native";
import type { SpeedAlert, SpeedPoint, SpeedRow } from "@stockspotter/shared-types";
import { SPEED_HISTORY_WINDOW_MS } from "@stockspotter/shared-types";
import Svg, { Line as SvgLine, Polyline } from "react-native-svg";
import { Text } from "../ui/text";
import { RingGauge, BellIcon } from "../RingGauge";
import { colors } from "../../theme";
import { cn } from "../../lib/utils";
import { bandColor, bandOf } from "../../momentumLabel";
import {
  PANEL_ORDER,
  PANEL_TITLE,
  changeSince,
  etClock,
  formatQuote,
  formatShares,
  formatSigned,
  panelsContaining,
  type Hub,
  type MostActiveSignal,
  type PanelKey,
} from "../../hub";
import type { AppTab } from "../../types";

/** Where a section's "›" goes: an existing tab holding the full list, or
 * null to expand the section in place (no full screen exists for it). */
const FULL_LIST_TAB: Record<PanelKey, AppTab | null> = {
  halt: null,
  momentum: null,
  gapgo: null,
  ignition: "alerts", // ignition alerts are listed on the Alerts tab
  gainers: null, // expands to the Today/Yesterday list Radar used to show
  active: "markets", // Markets tab's Most active list
  catalysts: "alerts", // catalysts are listed on the Alerts tab
};

const COLLAPSED_ROWS: Record<PanelKey, number> = { halt: 3, momentum: 2, gapgo: 1, ignition: 3, gainers: 3, active: 3, catalysts: 2 };
const EXPANDED_ROWS: Record<PanelKey, number> = { halt: 8, momentum: 8, gapgo: 5, ignition: 3, gainers: 3, active: 3, catalysts: 2 };

export interface HomeTabProps {
  hub: Hub;
  signal: MostActiveSignal | null;
  recentAlerts: SpeedAlert[];
  priceOf: (symbol: string) => number | null;
  /** Speed alert threshold, drawn on the expanded Halt sparklines. */
  threshold: number;
  onSelectSymbol: (symbol: string) => void;
  onNavigate: (tab: AppTab) => void;
  /** Full Top Gainers list (with its Today/Yesterday toggle), shown when
   * that section is expanded. Owned by App.tsx, which has the row parts. */
  topGainersFull: React.ReactNode;
  /** Opens the panel scorecard (how each panel's past picks did vs random). */
  onOpenScorecard: () => void;
}

export function HomeHeader(props: { sessionText: string; feedColor: string; alertsOn: boolean; threshold: number; onOpenSpeedSettings: () => void; onOpenSettings: () => void }) {
  return (
    <View className="flex-row items-center gap-2 px-4 pb-2.5 pt-4">
      <View className="flex-1 gap-0.5">
        <Text className="text-[22px] font-bold">Now</Text>
        <View className="flex-row items-center gap-1.5">
          <View className="h-1.5 w-1.5 rounded-full" style={{ backgroundColor: props.feedColor }} />
          <Text variant="muted" className="text-xs">{props.sessionText}</Text>
        </View>
      </View>
      <Pressable
        onPress={props.onOpenSpeedSettings}
        className={cn("min-h-[44px] min-w-[44px] flex-row items-center justify-center gap-1 rounded-[10px] px-2.5", props.alertsOn ? "bg-accent-bg" : "bg-row")}
        accessibilityRole="button"
        accessibilityLabel={`Speed alerts ${props.alertsOn ? "on" : "off"} at ${props.threshold}%, open settings`}
      >
        <BellIcon color={props.alertsOn ? colors.accent : colors.muted} />
        <Text className={cn("font-mono text-xs", props.alertsOn ? "text-accent" : "text-muted")}>{props.threshold}%</Text>
      </Pressable>
      <Pressable onPress={props.onOpenSettings} className="min-h-[44px] min-w-[44px] items-center justify-center" accessibilityRole="button" accessibilityLabel="Settings">
        <Text variant="muted" className="text-base">⚙</Text>
      </Pressable>
    </View>
  );
}

export function HomeTab(props: HomeTabProps) {
  const [expanded, setExpanded] = useState<Set<PanelKey>>(new Set());
  const toggle = (key: PanelKey) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  const onHeader = (key: PanelKey) => {
    const tab = FULL_LIST_TAB[key];
    if (tab) props.onNavigate(tab);
    else toggle(key);
  };
  const limit = (key: PanelKey) => (expanded.has(key) ? EXPANDED_ROWS[key] : COLLAPSED_ROWS[key]);
  const { hub } = props;

  const sections: Record<PanelKey, React.ReactNode> = {
    halt: (
      <Section key="halt" panel="halt" expanded={expanded.has("halt")} onHeader={onHeader}
        status={hub.halt.fillingFast > 0 ? `${hub.halt.fillingFast} filling fast` : hub.halt.moving.length > 0 ? `${hub.halt.moving.length} moving` : "quiet"}
        statusAccent={hub.halt.fillingFast > 0}
      >
        {hub.halt.rows.length === 0 ? (
          <Empty label="Waiting for the scanner's first trade…" />
        ) : (
          hub.halt.rows.slice(0, limit("halt")).map((row) => <HaltLine key={row.reading.symbol} row={row} detailed={expanded.has("halt")} threshold={props.threshold} onPress={props.onSelectSymbol} />)
        )}
      </Section>
    ),
    momentum: (
      <Section key="momentum" panel="momentum" expanded={expanded.has("momentum")} onHeader={onHeader} status={`${hub.momentum.length}`}>
        {hub.momentum.length === 0 ? (
          <Empty label="No stock qualifies right now." />
        ) : (
          hub.momentum.slice(0, limit("momentum")).map((m) => {
            const band = bandOf(m.overall);
            const color = bandColor(band);
            return (
              <Line key={m.symbol} symbol={m.symbol} onPress={props.onSelectSymbol} minHeight={40}>
                <View className="h-2 flex-1 overflow-hidden rounded-full bg-row">
                  <View className="h-2 rounded-full" style={{ width: `${Math.round(Math.min(1, Math.max(0, m.overall)) * 100)}%`, backgroundColor: color }} />
                </View>
                <Text mono className="w-[86px] text-right text-xs">{m.overall.toFixed(2)} {band ?? ""}</Text>
              </Line>
            );
          })
        )}
      </Section>
    ),
    gapgo: (
      <Section key="gapgo" panel="gapgo" expanded={expanded.has("gapgo")} onHeader={onHeader} status={`${hub.gapgo.length}`}>
        {hub.gapgo.length === 0 ? (
          <Empty label="Nothing has cleared all four checks yet." />
        ) : (
          hub.gapgo.slice(0, limit("gapgo")).map((f) => {
            const detail = [
              f.relativeVolume != null && Number.isFinite(f.relativeVolume) ? `${f.relativeVolume.toFixed(1)}x vol` : null,
              f.floatShares != null && Number.isFinite(f.floatShares) ? `float ${formatShares(f.floatShares)}` : null,
            ].filter(Boolean).join(" · ");
            return (
              <Line key={f.symbol} symbol={f.symbol} onPress={props.onSelectSymbol} minHeight={40}>
                <Text mono variant={f.gapPct >= 0 ? "good" : "critical"} className="text-[13px]">gap {formatSigned(f.gapPct, 0)}</Text>
                <Text mono variant="muted" numberOfLines={1} className="flex-1 text-right text-xs">{detail}</Text>
              </Line>
            );
          })
        )}
      </Section>
    ),
    ignition: (
      <Section key="ignition" panel="ignition" expanded={false} onHeader={onHeader} status={hub.ignition.fresh.length > 0 ? `${hub.ignition.fresh.length} new` : "nothing new"}>
        {hub.ignition.fresh.length > 0 ? (
          hub.ignition.fresh.slice(0, limit("ignition")).map((e) => {
            const change = changeSince(e.price, props.priceOf(e.symbol));
            return (
              <Line key={e.symbol} symbol={e.symbol} onPress={props.onSelectSymbol} minHeight={40}>
                <Text mono className="flex-1 text-xs" numberOfLines={1}>confirmed {formatQuote(e.price)} · {etClock(e.timestamp)}</Text>
                {change !== null && <Text mono variant={change >= 0 ? "good" : "critical"} className="text-xs">{formatSigned(change)} since</Text>}
              </Line>
            );
          })
        ) : hub.ignition.last ? (
          <Pressable onPress={() => props.onSelectSymbol(hub.ignition.last!.symbol)} className="min-h-[32px] justify-center" accessibilityRole="button" accessibilityLabel={`Open ${hub.ignition.last.symbol} chart`}>
            <Text variant="muted" className="text-xs">{lastIgnitionLine(hub, props.priceOf)}</Text>
          </Pressable>
        ) : (
          <Empty label="No confirmed ignition yet this session." />
        )}
      </Section>
    ),
    gainers: (
      <Section key="gainers" panel="gainers" expanded={expanded.has("gainers")} onHeader={onHeader} status={`${hub.gainers.length}`}>
        {expanded.has("gainers") ? (
          props.topGainersFull
        ) : hub.gainers.length === 0 ? (
          <Empty label="Waiting for the universe scan…" />
        ) : (
          <Tiles items={hub.gainers.slice(0, 3).map((m) => ({ symbol: m.symbol, value: formatSigned(m.changePct), variant: m.changePct >= 0 ? "good" : "critical" }))} onPress={props.onSelectSymbol} />
        )}
      </Section>
    ),
    active: (
      <Section key="active" panel="active" expanded={false} onHeader={onHeader} status={`${hub.active.length}`}>
        {hub.active.length === 0 ? (
          <Empty label="Waiting for the universe scan…" />
        ) : (
          <Tiles items={hub.active.slice(0, 3).map((m) => ({ symbol: m.symbol, value: formatShares(m.volume), variant: "default" }))} onPress={props.onSelectSymbol} />
        )}
      </Section>
    ),
    catalysts: (
      <Section key="catalysts" panel="catalysts" expanded={false} onHeader={onHeader} status={`${hub.catalysts.length}`}>
        {hub.catalysts.length === 0 ? (
          <Empty label="No catalysts yet today." />
        ) : (
          hub.catalysts.slice(0, limit("catalysts")).map((c) => (
            <Pressable key={c.symbol} onPress={() => props.onSelectSymbol(c.symbol)} className="min-h-[44px] justify-center gap-0.5" accessibilityRole="button" accessibilityLabel={`Open ${c.symbol} chart`}>
              <View className="flex-row items-center gap-2">
                <Text className="text-[13px] font-bold">{c.symbol}</Text>
                {c.catalystTags[0] ? (
                  <View className="rounded bg-accent-bg px-1.5 py-px">
                    <Text variant="accent" className="text-[10px]">{c.catalystTags[0].replace(/_/g, " ")}</Text>
                  </View>
                ) : null}
                <Text mono variant="muted" className="ml-auto text-[11px]">{etClock(c.mostRecentPublishedAt ?? c.timestamp)}</Text>
              </View>
              <Text variant="muted" numberOfLines={1} className="text-xs">{c.mostRecentHeadline ?? `${c.headlineCount} related headlines`}</Text>
            </Pressable>
          ))
        )}
      </Section>
    ),
  };

  return (
    <View className="gap-2.5">
      {props.signal && <SignalCard signal={props.signal} hub={hub} priceOf={props.priceOf} onPress={props.onSelectSymbol} />}
      {PANEL_ORDER.map((key) => sections[key])}
      <RecentAlerts alerts={props.recentAlerts} priceOf={props.priceOf} onPress={props.onSelectSymbol} />
      <Pressable
        onPress={props.onOpenScorecard}
        className="min-h-[52px] flex-row items-center gap-2 rounded-[14px] bg-panel px-3"
        accessibilityRole="button"
        accessibilityLabel="Panel scorecard: how each panel's past picks did against random. Open"
      >
        <View className="flex-1 gap-0.5">
          <Text className="text-sm font-semibold">Panel scorecard</Text>
          <Text variant="muted" className="text-xs">Do the panels beat random picks?</Text>
        </View>
        <Text variant="muted" className="text-base">›</Text>
      </Pressable>
    </View>
  );
}

function lastIgnitionLine(hub: Hub, priceOf: (symbol: string) => number | null): string {
  const last = hub.ignition.last!;
  const change = changeSince(last.price, priceOf(last.symbol));
  return `Last: ${last.symbol} at ${etClock(last.timestamp)}${change !== null ? `, ${formatSigned(change)} since` : ""}.`;
}

function Section(props: { panel: PanelKey; status: string; statusAccent?: boolean; expanded: boolean; onHeader: (key: PanelKey) => void; children: React.ReactNode }) {
  const goesToTab = FULL_LIST_TAB[props.panel] !== null;
  return (
    <View className="gap-2 rounded-[14px] bg-panel p-3">
      <Pressable
        onPress={() => props.onHeader(props.panel)}
        className="min-h-[32px] flex-row items-center"
        accessibilityRole="button"
        accessibilityLabel={`${PANEL_TITLE[props.panel]}, ${props.status}. ${goesToTab ? "Open the full list" : props.expanded ? "Show less" : "Show more"}`}
      >
        <Text className="flex-1 text-sm font-semibold">{PANEL_TITLE[props.panel]}</Text>
        <Text className={cn("text-xs", props.statusAccent ? "text-accent" : "text-muted")}>
          {props.status} {!goesToTab && props.expanded ? "‹" : "›"}
        </Text>
      </Pressable>
      {props.children}
    </View>
  );
}

function Empty({ label }: { label: string }) {
  return <Text variant="muted" className="text-xs">{label}</Text>;
}

function Line(props: { symbol: string; onPress: (symbol: string) => void; minHeight: number; leading?: React.ReactNode; children: React.ReactNode }) {
  return (
    <Pressable
      onPress={() => props.onPress(props.symbol)}
      className="flex-row items-center gap-2.5"
      style={{ minHeight: Math.max(44, props.minHeight) }}
      accessibilityRole="button"
      accessibilityLabel={`Open ${props.symbol} chart`}
    >
      {props.leading}
      <Text numberOfLines={1} className="w-14 text-[15px] font-bold">{props.symbol}</Text>
      {props.children}
    </Pressable>
  );
}

function HaltLine({ row, detailed, threshold, onPress }: { row: SpeedRow; detailed: boolean; threshold: number; onPress: (symbol: string) => void }) {
  const color = row.bullish ? colors.good : colors.critical;
  return (
    <Line symbol={row.reading.symbol} onPress={onPress} minHeight={44} leading={<RingGauge pct={row.pct} color={color} size={36} strokeWidth={8} labelSize={10} />}>
      <View className="flex-1 flex-row items-center gap-2">
        <Text mono className="text-[13px]">{row.movePct === null ? "—" : `${formatSigned(row.movePct)} 5m`}</Text>
        {row.fillingFast && (
          <View className="rounded-full bg-accent-bg px-1.5 py-px">
            <Text variant="accent" className="text-[10px] font-semibold">filling fast</Text>
          </View>
        )}
      </View>
      {detailed ? (
        // Expanded: the last 10 minutes of the gauge against the alert
        // line, and points gained in the last minute (Phone.dc.html).
        <View className="items-end gap-0.5">
          <SpeedSparkline points={row.points} color={color} threshold={threshold} />
          <Text mono variant="muted" className="text-[10px]">{row.delta1m === null ? "—" : `${row.delta1m >= 0 ? "+" : ""}${row.delta1m}`}/1m</Text>
        </View>
      ) : (
        <Text mono className="text-[13px]" style={{ color }}>{formatQuote(row.reading.currentPrice)}</Text>
      )}
    </Line>
  );
}

const SPARK_W = 84;
const SPARK_H = 28;

/** Gauge history (0-100) over the stored 10-minute window, newest at the
 * right edge; dashed accent line at the alert threshold. */
function SpeedSparkline(props: { points: SpeedPoint[]; color: string; threshold: number }) {
  const y = (pct: number) => (SPARK_H - 2 - (Math.max(0, Math.min(100, pct)) / 100) * (SPARK_H - 4)).toFixed(1);
  const last = props.points[props.points.length - 1];
  const coords = last
    ? props.points.map((p) => `${(SPARK_W - ((last.at - p.at) / SPEED_HISTORY_WINDOW_MS) * SPARK_W).toFixed(1)},${y(p.pct)}`).join(" ")
    : "";
  return (
    <Svg width={SPARK_W} height={SPARK_H} accessibilityLabel="Gauge over the last 10 minutes">
      <SvgLine x1={0} y1={y(props.threshold)} x2={SPARK_W} y2={y(props.threshold)} stroke={colors.accent} strokeWidth={1} strokeDasharray="3 3" opacity={0.6} />
      {props.points.length > 1 && <Polyline points={coords} fill="none" stroke={props.color} strokeWidth={2} strokeLinejoin="round" strokeLinecap="round" />}
    </Svg>
  );
}

function Tiles(props: { items: { symbol: string; value: string; variant: "good" | "critical" | "default" }[]; onPress: (symbol: string) => void }) {
  return (
    <View className="flex-row gap-1.5">
      {props.items.map((item) => (
        <Pressable
          key={item.symbol}
          onPress={() => props.onPress(item.symbol)}
          className="min-h-[44px] flex-1 gap-0.5 rounded-[10px] bg-row p-2"
          accessibilityRole="button"
          accessibilityLabel={`Open ${item.symbol} chart`}
        >
          <Text numberOfLines={1} className="text-[13px] font-bold">{item.symbol}</Text>
          <Text mono variant={item.variant} numberOfLines={1} className="text-xs">{item.value}</Text>
        </Pressable>
      ))}
      {/* Keep tile widths constant when fewer than three exist. */}
      {Array.from({ length: Math.max(0, 3 - props.items.length) }, (_, i) => <View key={`pad-${i}`} className="flex-1" />)}
    </View>
  );
}

function SignalCard(props: { signal: MostActiveSignal; hub: Hub; priceOf: (symbol: string) => number | null; onPress: (symbol: string) => void }) {
  const { signal } = props;
  const panels = panelsContaining(props.hub, signal.symbol);
  let gauge: { pct: number; color: string };
  let price: number | null;
  let line: string;
  let bullish = true;
  if (signal.kind === "speed") {
    const row = signal.row;
    bullish = row.bullish;
    gauge = { pct: row.pct, color: bullish ? colors.good : colors.critical };
    price = row.reading.currentPrice;
    line = [row.movePct === null ? null : `${formatSigned(row.movePct)} in 5 min`, row.fillingFast ? "filling fast" : null].filter(Boolean).join(" · ");
  } else {
    const band = bandOf(signal.reading.overall);
    gauge = { pct: Math.round(signal.reading.overall * 100), color: bandColor(band) };
    price = props.priceOf(signal.symbol);
    line = `Momentum ${signal.reading.overall.toFixed(2)}${band ? ` · ${band}` : ""}`;
  }
  return (
    <Pressable
      onPress={() => props.onPress(signal.symbol)}
      className="gap-2.5 rounded-2xl bg-panel p-3.5"
      accessibilityRole="button"
      accessibilityLabel={`Most active signal ${signal.symbol}, ${line}. In ${panels.length} of 7 panels. Open chart`}
    >
      <Text variant="accent" className="text-[11px] font-semibold tracking-wider">MOST ACTIVE SIGNAL</Text>
      <View className="flex-row items-center gap-3">
        <RingGauge pct={gauge.pct} color={gauge.color} size={52} strokeWidth={6} label={`${gauge.pct}%`} labelSize={12} />
        <View className="flex-1 gap-[3px]">
          <View className="flex-row items-baseline gap-2">
            <Text className="text-lg font-bold">{signal.symbol}</Text>
            {price !== null && (
              <Text mono className="text-sm" style={{ color: bullish ? colors.good : colors.critical }}>{bullish ? "▲" : "▼"} {formatQuote(price)}</Text>
            )}
          </View>
          {line ? <Text mono className="text-xs">{line}</Text> : null}
        </View>
      </View>
      <View className="flex-row flex-wrap items-center gap-1.5">
        {panels.map((key) => (
          <View key={key} className="rounded-full bg-row px-2 py-[3px]">
            <Text className="text-[11px]">{PANEL_TITLE[key]}</Text>
          </View>
        ))}
        <Text variant="muted" className="text-[11px]">{panels.length} of 7 panels</Text>
      </View>
    </Pressable>
  );
}

function RecentAlerts(props: { alerts: SpeedAlert[]; priceOf: (symbol: string) => number | null; onPress: (symbol: string) => void }) {
  return (
    <View className="gap-1 rounded-[14px] bg-panel p-3">
      <Text className="text-[13px] font-semibold">Recent alerts</Text>
      {props.alerts.length === 0 ? (
        <Empty label="No speed alerts yet this session." />
      ) : (
        props.alerts.slice(0, 5).map((a) => {
          const now = props.priceOf(a.symbol);
          const change = changeSince(a.price, now);
          return (
            <Pressable key={a.id} onPress={() => props.onPress(a.symbol)} className="min-h-[36px] flex-row items-center gap-2" accessibilityRole="button" accessibilityLabel={`Open ${a.symbol} chart`}>
              <Text mono variant="muted" className="text-xs">{etClock(a.at)}</Text>
              <Text className="text-xs font-semibold">{a.symbol}</Text>
              <Text mono variant="muted" numberOfLines={1} className="flex-1 text-xs">
                {formatQuote(a.price)} → {now === null ? "—" : formatQuote(now)}
              </Text>
              {change !== null && <Text mono variant={change >= 0 ? "good" : "critical"} className="text-xs font-semibold">{formatSigned(change)}</Text>}
            </Pressable>
          );
        })
      )}
    </View>
  );
}
