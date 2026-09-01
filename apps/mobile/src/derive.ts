import type { CatalystUpdate, FunnelSignal, HaltWarning, IgnitionEvent, MomentumUpdate } from "@stockspotter/shared-types";
import type { DetectionEvent, FocusRow, Mover } from "./types";
export function buildFocusRows(events: DetectionEvent[], gainers: Mover[]): FocusRow[] {
  const momentum = latestBySymbol(events.filter((e): e is MomentumUpdate => e.type === "momentum_update")); const ignition = latestBySymbol(events.filter((e): e is IgnitionEvent => e.type === "ignition_event")); const funnels = latestBySymbol(events.filter((e): e is FunnelSignal => e.type === "funnel_signal")); const moverBySymbol = new Map(gainers.map((m) => [m.symbol, m])); const rows: FocusRow[] = [];
  for (const funnel of funnels.values()) { if (!funnel.passed) continue; const score = momentum.get(funnel.symbol); const ignitionEvent = ignition.get(funnel.symbol); const parts = ["Funnel"]; if (score) parts.push(`momentum ${score.overall.toFixed(2)}`); if (ignitionEvent?.kind === "follow_through_confirmed") parts.push("ignition"); else if (ignitionEvent?.kind === "candidate_opened") parts.push("ignition candidate"); const mover = moverBySymbol.get(funnel.symbol); rows.push({ symbol: funnel.symbol, price: funnel.price, changePct: mover?.changePct ?? funnel.gapPct, timestamp: funnel.timestamp, detail: parts.join(" · "), strong: Boolean(score?.qualifies || ignitionEvent?.kind === "follow_through_confirmed") }); }
  return rows.sort((a, b) => Number(b.strong) - Number(a.strong) || Date.parse(b.timestamp) - Date.parse(a.timestamp));
}
export function buildAlerts(events: DetectionEvent[]) { return events.flatMap((event, index) => { if (event.type === "ignition_event") { const labels = { candidate_opened: "Ignition candidate", follow_through_confirmed: "Ignition confirmed", follow_through_rejected: "Ignition rejected" }; return [{ id: `${event.type}-${event.symbol}-${event.timestamp}-${index}`, symbol: event.symbol, timestamp: event.timestamp, label: labels[event.kind], detail: `${event.kind === "follow_through_confirmed" ? "Follow-through held" : "Price"} at $${event.price.toFixed(2)}` }]; } if (event.type === "consolidation_event") { const labels = { surge_detected: "Surge detected", consolidation_confirmed: "Consolidating", entry_triggered: "Breakout entry" }; return [{ id: `${event.type}-${event.symbol}-${event.timestamp}-${index}`, symbol: event.symbol, timestamp: event.timestamp, label: labels[event.kind], detail: `Consolidation signal at $${event.price.toFixed(2)}` }]; } if (event.type === "catalyst_update") return [{ id: `${event.type}-${event.symbol}-${event.timestamp}-${index}`, symbol: event.symbol, timestamp: event.timestamp, label: "Catalyst", detail: event.mostRecentHeadline ?? `${event.headlineCount} related headlines` }]; return []; }).slice(0, 50); }
export function latestHaltRisk(events: DetectionEvent[]): HaltWarning | null { const seen = new Set<string>(); let highest: HaltWarning | null = null; for (const event of events) { if (event.type !== "halt_warning" || seen.has(event.symbol)) continue; seen.add(event.symbol); if (!highest || event.proximityRatio > highest.proximityRatio) highest = event; } return highest && highest.level !== "calm" ? highest : null; }
/** Every currently at-risk symbol (not just the single highest one
 * latestHaltRisk's banner shows), ranked by proximity -- the mobile
 * equivalent of the web app's own Halt Early-Warning panel, which shows
 * a card per tracked symbol rather than collapsing to one summary. */
export function haltRows(events: DetectionEvent[]): HaltWarning[] {
  const latest = latestBySymbol(events.filter((e): e is HaltWarning => e.type === "halt_warning"));
  return [...latest.values()].filter((r) => r.level !== "calm").sort((a, b) => b.proximityRatio - a.proximityRatio);
}
/** Latest catalyst tags per symbol -- the mobile equivalent of the web
 * app's catalystsBySymbol map, used to show a real inline indicator next
 * to a ticker wherever one appears, not just inside the merged Alerts
 * feed. */
export function catalystsBySymbol(events: DetectionEvent[]): Map<string, CatalystUpdate> {
  return latestBySymbol(events.filter((e): e is CatalystUpdate => e.type === "catalyst_update"));
}
function latestBySymbol<T extends { symbol: string }>(events: T[]): Map<string, T> { const result = new Map<string, T>(); for (const event of events) if (!result.has(event.symbol)) result.set(event.symbol, event); return result; }
