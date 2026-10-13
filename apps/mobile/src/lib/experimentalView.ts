// Pure presentation/transport arithmetic for the mobile experimental
// lane. Deliberately separate from experimentalStrategies.ts: that file
// is a strict byte-for-byte mirror of the web decoder and is guarded by
// a parity test, so mobile-only helpers must not live in it or that test
// would have to start asserting non-parity.
//
// Everything here is a pure function of its arguments so it can be unit
// tested under bun without Metro, React, or a socket (see
// apps/client/src/lib/experimentalParity.test.ts -- apps/mobile still has
// no test runner of its own, same as attentionParity.test.ts's own note).
import type { ExperimentalStrategy, ExperimentalStrategyStatus, ExperimentalStrategyUpdate } from "./experimentalStrategies";
import type { FeedStatus } from "../types";

/** Same 90s window the normal mobile feed uses to call itself "stale"
 * (useRealtimeFeed.ts), applied here to the server's own last source
 * timestamp rather than to this socket's last message -- the research
 * lane can legitimately be silent for minutes with nothing wrong, so
 * "no rows lately" is not evidence of a problem and must not be shown
 * as one. The server's sourceAtMs is. */
export const EXPERIMENTAL_STALE_AFTER_MS = 90_000;
/** Same 15s/45s heartbeat pair as the normal feed and as the web
 * experimental socket: ping every 15s, give up on a socket that has
 * produced no traffic at all for 45s. */
export const EXPERIMENTAL_HEARTBEAT_MS = 15_000;
export const EXPERIMENTAL_TRANSPORT_DEAD_MS = 45_000;
/** Backoff floor/ceiling, matching the web experimental socket exactly.
 * The normal mobile feed uses a flat 3s retry forever; this lane does
 * not, because it is an opt-in research socket that competes for the
 * server's bounded research connection pool (ws-server's own
 * `research_connections` semaphore) -- a phone that reconnects every 3s
 * through a capacity rejection would spend that pool on itself. */
export const EXPERIMENTAL_RECONNECT_BASE_MS = 3_000;
export const EXPERIMENTAL_RECONNECT_CEILING_MS = 60_000;

/** One backoff step: how long to wait now, and what the next step's base
 * delay becomes. Jitter is ±20%, so a whole fleet of phones waking from
 * background at once does not retry in lockstep. `random` is injected
 * rather than read from Math.random() inside so this is testable. */
export function experimentalBackoff(delayMs: number, random: number): { waitMs: number; nextDelayMs: number } {
  // Clamped, not validated: an out-of-range delay is pulled to the
  // nearest end of [base, ceiling] rather than reset to the floor, so a
  // nonsense-large value can never become the *shortest* retry. Only NaN
  // has no nearest end and falls back to the floor.
  const base = Number.isNaN(delayMs)
    ? EXPERIMENTAL_RECONNECT_BASE_MS
    : Math.min(EXPERIMENTAL_RECONNECT_CEILING_MS, Math.max(EXPERIMENTAL_RECONNECT_BASE_MS, delayMs));
  const spread = Number.isNaN(random) ? 0.5 : Math.min(1, Math.max(0, random));
  return {
    waitMs: Math.round(base * (0.8 + spread * 0.4)),
    nextDelayMs: Math.min(EXPERIMENTAL_RECONNECT_CEILING_MS, base * 2),
  };
}

/** Which of the two panels the server says are actually running. An
 * empty result means the lane is reachable but both strategies are off,
 * which is a "render nothing" case, not an empty-list case -- showing
 * two empty panels for a disabled strategy would read as "no candidates
 * found", a claim nothing has made. */
export function experimentalPanels(status: ExperimentalStrategyStatus | null): ExperimentalStrategy[] {
  if (!status) return [];
  const panels: ExperimentalStrategy[] = [];
  if (status.iacEnabled) panels.push("iac");
  if (status.extremeMomentumEnabled) panels.push("extreme_momentum");
  return panels;
}

/** Whether what is on screen may be missing observations. Any of: this
 * socket is not open, a gap was recorded, the server reports unhealthy,
 * or the server's own source timestamp is missing/old. Deliberately
 * pessimistic -- the lane's whole point is research honesty, so the
 * default when anything is unknown is "incomplete". */
export function experimentalFeedIncomplete(input: {
  connection: FeedStatus;
  status: ExperimentalStrategyStatus | null;
  gap: boolean;
  nowMs: number;
}): boolean {
  if (input.connection !== "open" || input.gap || !input.status) return true;
  return !input.status.healthy
    || input.status.sourceAtMs === null
    || input.nowMs - input.status.sourceAtMs > EXPERIMENTAL_STALE_AFTER_MS;
}

/** Human-readable reason strings for the server's own `reason` field
 * (ws-server's experimental_strategies.rs emits exactly these). Anything
 * unrecognised is shown verbatim rather than swallowed -- a new server
 * reason should surface as itself, not silently vanish. */
const REASON_NOTE: Record<string, string> = {
  warming_up: "warming up, baseline incomplete",
  observing: "",
  disabled: "strategies disabled",
  processing_budget_exceeded: "research feed paused",
  upstream_feed_gap: "upstream feed gap",
  source_closed: "source closed",
};

/** The lane's standing disclaimer line. Same wording as the web panel's
 * own header so the two cannot describe the same feed differently. */
export function experimentalFeedNote(status: ExperimentalStrategyStatus, incomplete: boolean): string {
  const parts = ["Experimental", "completed minute bars"];
  parts.push(status.coverage === "observed_subscribed_symbols" ? "observed symbols only" : status.coverage);
  if (incomplete) parts.push("feed incomplete or stale");
  const reason = REASON_NOTE[status.reason] ?? status.reason;
  if (reason) parts.push(reason);
  return parts.join(" · ");
}

/** Coverage/health counters, shown rather than hidden: a research panel
 * that silently drops observations is worse than one that admits it. */
export function experimentalCoverageNote(status: ExperimentalStrategyStatus): string {
  const parts = [
    `${status.activeSymbols} observed`,
    `${status.laggedEvents} lagged`,
    `${status.evictions} evicted`,
    `${status.processingWarnings} warnings`,
  ];
  if (status.budgetBreaches !== undefined) parts.push(`${status.budgetBreaches} budget breaches`);
  if (status.ignoredProvisional !== undefined) parts.push(`${status.ignoredProvisional} provisional ignored`);
  return parts.join(" · ");
}

export const EXPERIMENTAL_STRATEGY_NAME: Record<ExperimentalStrategy, string> = {
  // No 🔥 on the native side, unlike the web panel's own label: a Section
  // title here renders uppercase and letter-spaced, where an emoji reads
  // as endorsement decoration. This lane describes observations, not
  // opportunities.
  iac: "Ignition Anchor Candle",
  extreme_momentum: "Extreme Momentum",
};

/** Same six lifecycle kinds, same wording as the web panel. Every one of
 * them is an observation about an anchor candle, not an instruction --
 * "held"/"continued" are explicitly "Observed ...", so a glance can't
 * read them as an entry signal. */
export const EXPERIMENTAL_KIND_LABEL: Record<ExperimentalStrategyUpdate["kind"], string> = {
  candidate: "Candidate",
  held: "Observed hold",
  continued: "Observed continuation",
  failed: "Anchor low broken",
  expired: "Observation ended",
  invalidated: "Evidence incomplete",
};

/** Seconds since the observed price, floored at 0 -- a clock skew between
 * phone and server must not render a negative age. */
export function experimentalObservationAgeSeconds(nowMs: number, eventTimeMs: number): number {
  return Math.max(0, Math.floor((nowMs - eventTimeMs) / 1000));
}

/** The row's evidence line: the same three ratios the web panel's "Why
 * this candidate" disclosure shows, plus the two caveats that have to
 * travel with every row (float evidence is genuinely unavailable, and
 * none of this predicts a profitable trade). Flattened to one line
 * because a phone row has no room for a collapsible <details>. */
export function experimentalEvidenceLine(row: ExperimentalStrategyUpdate): string {
  const parts = [
    `expansion ${row.evidence.expansionRatio.toFixed(2)}×`,
    `participation ${row.evidence.participationRatio.toFixed(2)}×`,
  ];
  if (row.evidence.compressionRatio !== null) parts.push(`compression ${row.evidence.compressionRatio.toFixed(2)}×`);
  parts.push("float unavailable", "not a prediction");
  return parts.join(" · ");
}
