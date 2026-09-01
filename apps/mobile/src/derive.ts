import type { CatalystUpdate, FunnelSignal, HaltWarning, IgnitionEvent, MomentumUpdate } from "@stockspotter/shared-types";
import type { DetectionEvent, FocusRow, MarketReading, Mover, WatchlistRow } from "./types";
// Focus was only ever built by looping over Funnel signals (below),
// with momentum_update read solely as decoration on a Funnel row that
// already existed. That silently dropped every symbol confirmed on
// Bullish Momentum alone -- a real, separate detector from the Funnel/
// Gap-and-Go gate (different math path, see FunnelSignal/MomentumUpdate's
// own doc comments), which the web app shows as its own panel with no
// Funnel dependency at all (deriveConfirmedMomentum, apps/client/src/lib/
// derive.ts). Caught live: the web app's Bullish Momentum panel showed
// two real confirmed symbols (no Funnel signal behind either one) while
// mobile's Focus rendered "Waiting for the scanner's first signal…" for
// the exact same live broadcast -- not a connectivity gap (both platforms
// read the identical, unfiltered ws-server broadcast; see server.rs's own
// doc comment), a derivation gap. Fixed below by unioning in the latest
// qualifying momentum reading for any symbol a Funnel row doesn't already
// cover, using the same `momentum.qualifies` gate the web panel's edge-
// trigger is built on.
export function buildFocusRows(events: DetectionEvent[], gainers: Mover[]): FocusRow[] {
  const momentum = latestBySymbol(events.filter((e): e is MomentumUpdate => e.type === "momentum_update")); const ignition = latestBySymbol(events.filter((e): e is IgnitionEvent => e.type === "ignition_event")); const funnels = latestBySymbol(events.filter((e): e is FunnelSignal => e.type === "funnel_signal")); const moverBySymbol = new Map(gainers.map((m) => [m.symbol, m])); const rows: FocusRow[] = []; const covered = new Set<string>();
  for (const funnel of funnels.values()) { if (!funnel.passed) continue; covered.add(funnel.symbol); const score = momentum.get(funnel.symbol); const ignitionEvent = ignition.get(funnel.symbol); const parts = ["Funnel"]; if (score) parts.push(`momentum ${score.overall.toFixed(2)}`); if (ignitionEvent?.kind === "follow_through_confirmed") parts.push("ignition"); else if (ignitionEvent?.kind === "candidate_opened") parts.push("ignition candidate"); const mover = moverBySymbol.get(funnel.symbol); rows.push({ symbol: funnel.symbol, price: funnel.price, changePct: mover?.changePct ?? funnel.gapPct, timestamp: funnel.timestamp, detail: parts.join(" · "), strong: Boolean(score?.qualifies || ignitionEvent?.kind === "follow_through_confirmed") }); }
  for (const m of momentum.values()) { if (!m.qualifies || covered.has(m.symbol)) continue; const mover = moverBySymbol.get(m.symbol); if (!mover) continue; /* no real price to show without a movers-list match */ covered.add(m.symbol); rows.push({ symbol: m.symbol, price: mover.price, changePct: mover.changePct, timestamp: m.timestamp, detail: `Bullish momentum ${m.overall.toFixed(2)}`, strong: true }); }
  return rows.sort((a, b) => Number(b.strong) - Number(a.strong) || Date.parse(b.timestamp) - Date.parse(a.timestamp));
}
// catalysts is now a dedicated latest-per-symbol map (useRealtimeFeed's
// own catalystsBySymbol, matching the web app's -- see that file's doc
// comment) rather than scanned out of `events`: catalyst_update no longer
// lands in `events` at all, the same flood-eviction protection web's own
// catalyst_update handling has always had. Merged into this same combined
// alerts feed (not split into its own tab the way web's CatalystsPanel
// is) since that merge was mobile's own deliberate simplification, not
// something this fix should undo -- only *where* the catalyst rows come
// from changed, not that they still show up here.
export function buildAlerts(events: DetectionEvent[], catalysts: Map<string, CatalystUpdate>) {
  const fromEvents = events.flatMap((event, index) => { if (event.type === "ignition_event") { const labels = { candidate_opened: "Ignition candidate", follow_through_confirmed: "Ignition confirmed", follow_through_rejected: "Ignition rejected" }; return [{ id: `${event.type}-${event.symbol}-${event.timestamp}-${index}`, symbol: event.symbol, timestamp: event.timestamp, label: labels[event.kind], detail: `${event.kind === "follow_through_confirmed" ? "Follow-through held" : "Price"} at $${event.price.toFixed(2)}` }]; } if (event.type === "consolidation_event") { const labels = { surge_detected: "Surge detected", consolidation_confirmed: "Consolidating", entry_triggered: "Breakout entry" }; return [{ id: `${event.type}-${event.symbol}-${event.timestamp}-${index}`, symbol: event.symbol, timestamp: event.timestamp, label: labels[event.kind], detail: `Consolidation signal at $${event.price.toFixed(2)}` }]; } return []; });
  const fromCatalysts = Array.from(catalysts.values()).map((event) => ({ id: `catalyst_update-${event.symbol}-${event.timestamp}`, symbol: event.symbol, timestamp: event.timestamp, label: "Catalyst", detail: event.mostRecentHeadline ?? `${event.headlineCount} related headlines` }));
  return [...fromEvents, ...fromCatalysts].sort((a, b) => Date.parse(b.timestamp) - Date.parse(a.timestamp)).slice(0, 50);
}
export function latestHaltRisk(events: DetectionEvent[]): HaltWarning | null { const seen = new Set<string>(); let highest: HaltWarning | null = null; for (const event of events) { if (event.type !== "halt_warning" || seen.has(event.symbol)) continue; seen.add(event.symbol); if (!highest || event.proximityRatio > highest.proximityRatio) highest = event; } return highest && highest.level !== "calm" ? highest : null; }
/** Every currently at-risk symbol (not just the single highest one
 * latestHaltRisk's banner shows), ranked by proximity -- feeds the
 * Alerts tab's own "Halt risk" section specifically, which is deliberately
 * scoped to genuine risk only (level !== calm) so it stays an alert feed,
 * not a permanently-populated ranked list. */
export function haltRows(events: DetectionEvent[]): HaltWarning[] {
  const latest = latestBySymbol(events.filter((e): e is HaltWarning => e.type === "halt_warning"));
  return [...latest.values()].filter((r) => r.level !== "calm").sort((a, b) => b.proximityRatio - a.proximityRatio);
}
/** The mobile equivalent of the web app's own Halt Early-Warning panel
 * (deriveLatestHaltBySymbol, apps/client/src/lib/derive.ts) -- top
 * symbols by proximity, period, with NO calm-level filter. This is a
 * real, separate function from haltRows above, not a duplicate: haltRows'
 * own calm-filter is exactly why the home-tab card grid (built to mirror
 * web's panel per Roman's own ask) came up empty far more often than web
 * did for the identical live broadcast -- most of "the top N tracked
 * symbols by proximity" are legitimately calm most of the time (web's own
 * panel shows them anyway, e.g. a real 3%/11%/16% trio, none amber/red),
 * while haltRows' extra filter was throwing all of those away before this
 * function existed. Confirmed live: haltRows returned 0 for over 200 real
 * halt_warning events across 20s while web's identical-moment screenshot
 * showed 3 real non-empty cards for the same symbols. */
export function topHaltsByProximity(events: DetectionEvent[]): HaltWarning[] {
  const latest = latestBySymbol(events.filter((e): e is HaltWarning => e.type === "halt_warning"));
  return [...latest.values()].sort((a, b) => b.proximityRatio - a.proximityRatio);
}
// The Watchlist tab used to just filter Focus down to saved symbols --
// which meant a symbol saved from anywhere OTHER than a live Focus row
// (Top Gainers, Most Active, Markets -- all real save points now) never
// showed up on its own watchlist at all, since it has no Focus row to be
// filtered out of. Caught live: starred a real Top Gainer (HCWC), the
// Watchlist tab still rendered its empty state. Every saved symbol gets
// a row here regardless of source -- Focus data first when it exists
// (richest detail), otherwise whatever live price/change is available
// from movers/index readings, otherwise an honest "not currently
// tracked" rather than fabricating a number.
export function buildWatchlistRows(
  saved: Set<string>,
  focus: FocusRow[],
  market: { gainers: Mover[]; mostActive: Mover[]; indices: MarketReading[] },
): WatchlistRow[] {
  const focusBySymbol = new Map(focus.map((r) => [r.symbol, r]));
  const moverBySymbol = new Map([...market.gainers, ...market.mostActive].map((m) => [m.symbol, m]));
  const indexBySymbol = new Map(market.indices.map((r) => [r.symbol, r]));

  return [...saved].sort().map((symbol) => {
    const focusRow = focusBySymbol.get(symbol);
    if (focusRow) return { symbol, price: focusRow.price, changePct: focusRow.changePct, timestamp: focusRow.timestamp, detail: focusRow.detail, strong: focusRow.strong };
    const mover = moverBySymbol.get(symbol);
    if (mover) return { symbol, price: mover.price, changePct: mover.changePct, timestamp: null, detail: `${formatVolumeShort(mover.volume)} vol`, strong: false };
    const index = indexBySymbol.get(symbol);
    if (index) return { symbol, price: index.price, changePct: index.changePct, timestamp: null, detail: index.name, strong: false };
    return { symbol, price: null, changePct: null, timestamp: null, detail: "not currently tracked", strong: false };
  });
}

/** Local copy of App.tsx's own formatVolume -- derive.ts has no
 * dependency on App.tsx today and shouldn't gain one just for this. */
function formatVolumeShort(value: number): string {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(0)}K`;
  return String(value);
}

function latestBySymbol<T extends { symbol: string }>(events: T[]): Map<string, T> { const result = new Map<string, T>(); for (const event of events) if (!result.has(event.symbol)) result.set(event.symbol, event); return result; }
