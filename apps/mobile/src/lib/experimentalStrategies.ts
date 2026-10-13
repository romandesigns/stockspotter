/** Separate experimental lane; these are deliberately not RealtimeMessage or OI events.
 *
 * Hand-mirrored from apps/client/src/lib/experimentalStrategies.ts rather
 * than promoted into @stockspotter/shared-types. That package is the
 * frozen, shipped WS protocol both apps already depend on -- putting a
 * deliberately-isolated research contract in it would make every normal
 * client import the experimental lane's types, which is exactly the
 * coupling the lane exists to avoid (see cc03d36, which already removed
 * this contract from the mobile shared types once).
 *
 * The cost of the copy is drift, so it is paid for with a real test, not
 * with a comment: apps/client/src/lib/experimentalParity.test.ts imports
 * BOTH decoders and asserts they agree byte-for-byte on the same wire
 * vectors. Keep this file a strict mirror -- any validation change here
 * has to land on the web side too, or that test fails.
 *
 * Deliberately pure TypeScript, zero React Native in the import graph,
 * so the parity test above (which runs under bun, not Metro) can import
 * it directly. Do not add RN imports to this file.
 */
export type ExperimentalStrategy = "iac" | "extreme_momentum";
export type ExperimentalKind = "candidate" | "held" | "continued" | "failed" | "expired" | "invalidated";
export interface ExperimentalStrategyUpdate {
  type: "experimental_strategy_update";
  contractVersion: "iac-extreme-minute-v1";
  eventId: string;
  lifecycleId: string;
  supersedes: string | null;
  symbol: string;
  strategy: ExperimentalStrategy;
  kind: ExperimentalKind;
  marketDay: string;
  eventTimeMs: number;
  receivedAtMs: number;
  decisionAtMs: number;
  anchorAvailableAtMs: number;
  anchorEventTimeMs: number;
  anchorPrice: number;
  observedPrice: number;
  reason: string;
  evidence: {
    baselineBars: number;
    baselineRange: number;
    baselineVolume: number;
    compressionRatio: number | null;
    expansionRatio: number;
    participationRatio: number;
    moveFraction: number;
    pathEfficiency: number | null;
    floatEvidence: "unavailable";
  };
}
export interface ExperimentalStrategyStatus {
  type: "experimental_strategy_status";
  iacEnabled: boolean;
  extremeMomentumEnabled: boolean;
  healthy: boolean;
  sourceAtMs: number | null;
  laggedEvents: number;
  evictions: number;
  processingWarnings: number;
  budgetBreaches?: number;
  ignoredProvisional?: number;
  recentEvictedSymbols?: string[];
  activeSymbols: number;
  symbolLimit?: number;
  iacReadySymbols?: number;
  extremeReadySymbols?: number;
  capacityDroppedUpdates?: number;
  recentCapacityDroppedSymbols?: string[];
  idleReclaims?: number;
  idleResets?: number;
  acceptedBarUpdates?: number;
  warmupBarUpdates?: number;
  duplicateBarUpdates?: number;
  revisionResets?: number;
  missingMinuteResets?: number;
  invalidBarUpdates?: number;
  outputLaggedEvents?: number;
  reason: string;
  coverage: "observed_subscribed_symbols";
}
export type ExperimentalMessage = ExperimentalStrategyUpdate | ExperimentalStrategyStatus | { type: "experimental_strategy_gap" };
const object = (x: unknown): x is Record<string, unknown> => typeof x === "object" && x !== null && !Array.isArray(x);
const number = (x: unknown): x is number => typeof x === "number" && Number.isFinite(x);
const stamp = (x: unknown): x is number => number(x) && Number.isSafeInteger(x) && x >= 0;
const text = (x: unknown, max = 200): x is string => typeof x === "string" && x.length > 0 && x.length <= max;
const nullableNumber = (x: unknown) => x === null || number(x);

export function decodeExperimentalMessage(value: unknown): ExperimentalMessage | null {
  if (!object(value)) return null;
  if (value.type === "experimental_strategy_gap") return { type: value.type };
  if (value.type === "experimental_strategy_status") {
    if (![value.iacEnabled, value.extremeMomentumEnabled, value.healthy].every(x => typeof x === "boolean")
      || !(value.sourceAtMs === null || stamp(value.sourceAtMs))
      || ![value.laggedEvents, value.evictions, value.processingWarnings, value.activeSymbols].every(stamp)
      || !(value.budgetBreaches === undefined || stamp(value.budgetBreaches))
      || !(value.ignoredProvisional === undefined || stamp(value.ignoredProvisional))
      || !(value.recentEvictedSymbols === undefined || (Array.isArray(value.recentEvictedSymbols) && value.recentEvictedSymbols.length <= 100 && value.recentEvictedSymbols.every(x=>text(x,16))))
      || !["symbolLimit", "iacReadySymbols", "extremeReadySymbols", "capacityDroppedUpdates", "idleReclaims", "idleResets",
        "acceptedBarUpdates", "warmupBarUpdates", "duplicateBarUpdates", "revisionResets", "missingMinuteResets", "invalidBarUpdates", "outputLaggedEvents"]
        .every(key => value[key] === undefined || stamp(value[key]))
      || !(value.recentCapacityDroppedSymbols === undefined || (Array.isArray(value.recentCapacityDroppedSymbols)
        && value.recentCapacityDroppedSymbols.length <= 100 && value.recentCapacityDroppedSymbols.every(x=>text(x,16))))
      || (number(value.iacReadySymbols) && number(value.activeSymbols) && value.iacReadySymbols > value.activeSymbols)
      || (number(value.extremeReadySymbols) && number(value.activeSymbols) && value.extremeReadySymbols > value.activeSymbols)
      || (number(value.symbolLimit) && number(value.activeSymbols) && (value.symbolLimit < 1 || value.activeSymbols > value.symbolLimit))
      || !text(value.reason) || value.coverage !== "observed_subscribed_symbols") return null;
    return value as unknown as ExperimentalStrategyStatus;
  }
  if (value.type !== "experimental_strategy_update" || value.contractVersion !== "iac-extreme-minute-v1"
    || !text(value.eventId) || !text(value.lifecycleId) || !(value.supersedes === null || text(value.supersedes))
    || !text(value.symbol, 16) || !/^[A-Za-z0-9.-]+$/.test(value.symbol)
    || !["iac", "extreme_momentum"].includes(String(value.strategy))
    || !["candidate", "held", "continued", "failed", "expired", "invalidated"].includes(String(value.kind))
    || !text(value.marketDay, 10) || !/^\d{4}-\d{2}-\d{2}$/.test(value.marketDay)
    || ![value.eventTimeMs, value.receivedAtMs, value.decisionAtMs, value.anchorAvailableAtMs, value.anchorEventTimeMs].every(stamp)
    || !number(value.anchorPrice) || value.anchorPrice <= 0 || !number(value.observedPrice) || value.observedPrice <= 0
    || !text(value.reason) || !object(value.evidence)) return null;
  const e = value.evidence;
  if (!stamp(e.baselineBars) || e.baselineBars < 2 || ![e.baselineRange,e.baselineVolume,e.expansionRatio,e.participationRatio,e.moveFraction].every(number)
    || !nullableNumber(e.compressionRatio) || !nullableNumber(e.pathEfficiency) || e.floatEvidence !== "unavailable") return null;
  if ((value.decisionAtMs as number) < (value.receivedAtMs as number)
    || (value.anchorAvailableAtMs as number) > (value.decisionAtMs as number)) return null;
  return value as unknown as ExperimentalStrategyUpdate;
}

/** Latest immutable observation per lifecycle, isolated from the generic event ring. */
export function reduceExperimentalRows(rows: ExperimentalStrategyUpdate[], event: ExperimentalStrategyUpdate, cap = 100): ExperimentalStrategyUpdate[] {
  const previous = rows.find(row => row.lifecycleId === event.lifecycleId);
  if (previous && (previous.eventId === event.eventId || event.decisionAtMs < previous.decisionAtMs
    || (event.decisionAtMs === previous.decisionAtMs && event.supersedes !== previous.eventId))) return rows;
  return [event, ...rows.filter(row => row.lifecycleId !== event.lifecycleId)].slice(0, Math.max(1, Math.min(100, cap)));
}
