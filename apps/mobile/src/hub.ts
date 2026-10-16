// Home hub (2026-10-16 redesign, Hub.dc.html): one screen that answers
// "what is moving right now" across all seven dashboard panels, so the
// phone no longer needs a tab per panel. Pure on purpose -- no React, no
// React Native -- so bun:test runs it directly (hub.test.ts) and the
// selection rules below are pinned by tests rather than by eye.
//
// Every number comes from data the app already receives; this file only
// decides which rows each compact section shows, which symbol earns the
// "Most active signal" card, and which panels currently contain it.
import type { BarUpdate, CatalystUpdate, FunnelSignal, HaltWarning, IgnitionEvent, MomentumUpdate, SpeedRow } from "@stockspotter/shared-types";
import { MOVING_FLOOR_PCT } from "@stockspotter/shared-types";
import type { Mover } from "./types";

// ---------------------------------------------------------------- session

export type MarketSession = "premarket" | "regular" | "after_hours" | "closed";

const SESSION_TEXT: Record<MarketSession, string> = {
  premarket: "Premarket",
  regular: "Regular hours",
  after_hours: "After hours",
  closed: "Closed",
};

/** US equity session by New York wall clock: premarket 04:00-09:30,
 * regular 09:30-16:00, after hours 16:00-20:00, weekends closed.
 * Exchange holidays are not modelled (the app has no holiday calendar);
 * on a holiday this says the session the clock implies. */
export function marketSessionAt(now: Date): { session: MarketSession; time: string } {
  const parts = new Intl.DateTimeFormat("en-US", {
    timeZone: "America/New_York",
    weekday: "short",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  }).formatToParts(now);
  const get = (type: string) => parts.find((p) => p.type === type)?.value ?? "";
  // Some engines render midnight as "24" even with h23; normalise.
  const hour = Number(get("hour")) % 24;
  const minute = Number(get("minute"));
  const time = `${String(hour).padStart(2, "0")}:${String(minute).padStart(2, "0")}`;
  const weekday = get("weekday");
  const mins = hour * 60 + minute;
  let session: MarketSession = "closed";
  if (weekday !== "Sat" && weekday !== "Sun") {
    if (mins >= 240 && mins < 570) session = "premarket";
    else if (mins >= 570 && mins < 960) session = "regular";
    else if (mins >= 960 && mins < 1200) session = "after_hours";
  }
  return { session, time };
}

/** "Premarket · 08:42 ET · live". `feed` is the connection label the app
 * header already shows; omitted when null. */
export function sessionLine(now: Date, feed: string | null): string {
  const { session, time } = marketSessionAt(now);
  return [SESSION_TEXT[session], `${time} ET`, feed].filter(Boolean).join(" · ");
}

// ---------------------------------------------------------------- panels

export type PanelKey = "halt" | "momentum" | "gapgo" | "ignition" | "gainers" | "active" | "catalysts";

/** Fixed section order on Home -- never re-sorted by activity, so a
 * section does not jump under the user's thumb as data arrives. */
export const PANEL_ORDER: readonly PanelKey[] = ["halt", "momentum", "gapgo", "ignition", "gainers", "active", "catalysts"];

export const PANEL_TITLE: Record<PanelKey, string> = {
  halt: "Halt Early-Warning",
  momentum: "Bullish Momentum",
  gapgo: "Gap & Go",
  ignition: "Ignition",
  gainers: "Top Gainers",
  active: "Highly Trading",
  catalysts: "Catalysts",
};

/** A confirmed ignition counts as "new" for this long. Same window as
 * useIgnitionAlerts.ts's per-symbol cooldown: one alert's worth of time. */
export const IGNITION_FRESH_MS = 15 * 60_000;

export interface HubInput {
  /** speedRows(readings, tracker, "up"), fullest gauge first. */
  upRows: SpeedRow[];
  momentumBySymbol: Map<string, MomentumUpdate>;
  funnelBySymbol: Map<string, FunnelSignal>;
  /** Confirmed ignitions that pass the user-attention rule, newest first
   * (App.tsx's attentionIgnitions -- the same list that raises alerts). */
  ignitions: IgnitionEvent[];
  gainers: Mover[];
  mostActive: Mover[];
  catalystsBySymbol: Map<string, CatalystUpdate>;
  now: number;
}

export interface Hub {
  halt: {
    /** Every bullish reading, fullest first; the section shows the top few
     * unconditionally so it is not empty on a quiet tape (the calm-filter
     * bug topHaltsByProximity's doc comment describes). */
    rows: SpeedRow[];
    /** Rows at or above MOVING_FLOOR_PCT: what counts as "in" the panel
     * for the Most active signal card. */
    moving: SpeedRow[];
    fillingFast: number;
  };
  momentum: MomentumUpdate[];
  gapgo: FunnelSignal[];
  ignition: { fresh: IgnitionEvent[]; last: IgnitionEvent | null };
  gainers: Mover[];
  active: Mover[];
  catalysts: CatalystUpdate[];
  members: Record<PanelKey, Set<string>>;
}

function catalystTime(c: CatalystUpdate): number {
  const published = c.mostRecentPublishedAt ? Date.parse(c.mostRecentPublishedAt) : NaN;
  return Number.isFinite(published) ? published : Date.parse(c.timestamp);
}

export function buildHub(input: HubInput): Hub {
  const moving = input.upRows.filter((r) => r.pct >= MOVING_FLOOR_PCT);
  // Same ranking as topBullishMomentum (derive.ts), all qualifiers.
  const momentum = [...input.momentumBySymbol.values()].filter((m) => m.qualifies).sort((a, b) => b.overall - a.overall);
  const gapgo = [...input.funnelBySymbol.values()].filter((f) => f.passed).sort((a, b) => b.gapPct - a.gapPct || a.symbol.localeCompare(b.symbol));
  const seen = new Set<string>();
  const fresh: IgnitionEvent[] = [];
  for (const e of input.ignitions) {
    const at = Date.parse(e.timestamp);
    if (!Number.isFinite(at) || input.now - at > IGNITION_FRESH_MS || seen.has(e.symbol)) continue;
    seen.add(e.symbol);
    fresh.push(e);
  }
  const catalysts = [...input.catalystsBySymbol.values()].sort((a, b) => catalystTime(b) - catalystTime(a));
  const symbols = (list: { symbol: string }[]) => new Set(list.map((x) => x.symbol));
  return {
    halt: { rows: input.upRows, moving, fillingFast: moving.filter((r) => r.fillingFast).length },
    momentum,
    gapgo,
    ignition: { fresh, last: input.ignitions[0] ?? null },
    gainers: input.gainers,
    active: input.mostActive,
    catalysts,
    members: {
      halt: new Set(moving.map((r) => r.reading.symbol)),
      momentum: symbols(momentum),
      gapgo: symbols(gapgo),
      ignition: symbols(fresh),
      gainers: symbols(input.gainers),
      active: symbols(input.mostActive),
      catalysts: symbols(catalysts),
    },
  };
}

/** Which of the seven panels currently contain `symbol`, in Home order. */
export function panelsContaining(hub: Hub, symbol: string): PanelKey[] {
  return PANEL_ORDER.filter((key) => hub.members[key].has(symbol));
}

export type MostActiveSignal =
  | { kind: "speed"; symbol: string; row: SpeedRow }
  | { kind: "momentum"; symbol: string; reading: MomentumUpdate };

/** The symbol with the fullest bullish speed gauge, provided it is
 * actually moving (>= MOVING_FLOOR_PCT -- a 3% gauge is not a signal);
 * otherwise the strongest qualifying momentum reading; otherwise null and
 * the card is hidden. Labelled "Most active signal", never a trade call. */
export function mostActiveSignal(hub: Hub): MostActiveSignal | null {
  const row = hub.halt.moving[0];
  if (row) return { kind: "speed", symbol: row.reading.symbol, row };
  const reading = hub.momentum[0];
  if (reading) return { kind: "momentum", symbol: reading.symbol, reading };
  return null;
}

// ---------------------------------------------------------------- prices

/** Freshest known price: the halt reading (per trade), else the newest
 * 1-minute bar close, else the movers poll (60 s). Null when unknown. */
export function latestPrice(
  symbol: string,
  haltBySymbol: Map<string, HaltWarning>,
  barsBySymbol: Map<string, BarUpdate[]>,
  movers: Mover[],
): number | null {
  const halt = haltBySymbol.get(symbol);
  if (halt && halt.currentPrice > 0) return halt.currentPrice;
  const bars = barsBySymbol.get(symbol);
  const last = bars && bars.length > 0 ? bars[bars.length - 1] : undefined;
  if (last && last.close > 0) return last.close;
  const mover = movers.find((m) => m.symbol === symbol);
  return mover && mover.price > 0 ? mover.price : null;
}

/** Percent change from `from` to `to`; null when either is unusable. */
export function changeSince(from: number, to: number | null): number | null {
  if (to === null || !(from > 0) || !Number.isFinite(to)) return null;
  return ((to - from) / from) * 100;
}

/** Sub-dollar names need four decimals to show a move at all. */
export function formatQuote(price: number): string {
  return `$${price.toFixed(price < 1 ? 4 : 2)}`;
}

export function formatSigned(pct: number, digits = 1): string {
  return `${pct >= 0 ? "+" : "−"}${Math.abs(pct).toFixed(digits)}%`;
}

/** Compact share volume: 1.59B / 278.6M / 812K. */
export function formatShares(value: number): string {
  if (value >= 1e9) return `${(value / 1e9).toFixed(2)}B`;
  if (value >= 1e6) return `${(value / 1e6).toFixed(1)}M`;
  if (value >= 1e3) return `${(value / 1e3).toFixed(0)}K`;
  return String(Math.round(value));
}

/** New York HH:MM for an epoch-ms or ISO time. */
export function etClock(at: number | string): string {
  const d = new Date(at);
  if (!Number.isFinite(d.getTime())) return "—";
  return marketSessionAt(d).time;
}
