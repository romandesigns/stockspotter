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

/** Per-strategy count of histories with enough continuous bars for the
 * NEXT evaluation (IAC needs 25 prior contiguous bars, Extreme 22; see
 * docs/IAC-EXTREME-COVERAGE-2026-10-13.md). `undefined` means this
 * server does not report readiness at all -- an older build, or the
 * field genuinely absent -- which is a different state from zero and
 * must not be rendered as "0 ready". */
export function experimentalReadySymbols(status: ExperimentalStrategyStatus, strategy: ExperimentalStrategy): number | undefined {
  return strategy === "iac" ? status.iacReadySymbols : status.extremeReadySymbols;
}

/** The panel's one coverage line. Readiness is a count of histories, not
 * of signals: saying "ready" about a symbol claims only that enough
 * contiguous minute bars exist to evaluate it next bar. The "observed
 * feed only" tail is not decoration -- the subscribed feed never covered
 * the whole market, so an unqualified count would overclaim. */
export function experimentalReadinessLine(status: ExperimentalStrategyStatus, strategy: ExperimentalStrategy): string {
  const ready = experimentalReadySymbols(status, strategy);
  const head = ready === undefined
    ? `${status.activeSymbols} observed`
    : `${ready} / ${status.activeSymbols} histories ready`;
  return `${head} · observed feed only`;
}

/** Whether capacity has interfered with coverage: either the new policy
 * rejected incoming updates at the symbol ceiling, or the old LRU
 * evicted a history. `evictions` stays zero under the 16,384-monitor
 * policy, so a nonzero value is itself worth surfacing. */
export function experimentalCapacityLimited(status: ExperimentalStrategyStatus | null): boolean {
  if (!status) return false;
  return (status.capacityDroppedUpdates ?? 0) > 0 || status.evictions > 0;
}

/** Sticky fold of the above. Every capacity field is OPTIONAL, so a later
 * status frame that simply omits `capacityDroppedUpdates` would otherwise
 * silently clear a warning that was true -- and the rows already on
 * screen were still gathered under a rejection. Once this client has seen
 * capacity interfere, it keeps saying so for the session, the same
 * one-way reasoning as the lane's own `gap` flag. */
export function experimentalCapacitySeen(previous: boolean, status: ExperimentalStrategyStatus | null): boolean {
  return previous || experimentalCapacityLimited(status);
}

/** The single most important thing wrong with a panel right now, or null
 * when nothing is. Ordered worst-first: an incomplete feed outranks a
 * capacity rejection, which outranks histories that are merely still
 * warming up. One badge, not three -- a row of warning pills on a phone
 * reads as alarm, and only the most severe one changes what the numbers
 * below it are worth. */
export type ExperimentalConcern = "incomplete" | "capacity" | "warming";
export function experimentalPanelConcern(input: {
  status: ExperimentalStrategyStatus | null;
  incomplete: boolean;
  capacityLimited: boolean;
  ready: number | undefined;
}): ExperimentalConcern | null {
  if (!input.status || input.incomplete) return "incomplete";
  if (input.capacityLimited) return "capacity";
  if (input.ready === 0) return "warming";
  return null;
}

export const EXPERIMENTAL_CONCERN_LABEL: Record<ExperimentalConcern, string> = {
  incomplete: "incomplete",
  capacity: "capacity",
  warming: "warming up",
};

/** Why a panel is empty. The distinction matters more here than almost
 * anywhere else in the app: "no candidates found" and "nothing could be
 * evaluated yet" look identical on screen but mean opposite things about
 * the research claim, and conflating them would let an empty panel be
 * read as evidence of absence. Same five-way split, and same ordering,
 * as the web panel's own empty state. */
export type ExperimentalEmptyReason = "connecting" | "incomplete" | "not_ready" | "ready_no_candidates" | "readiness_unknown";
export function experimentalEmptyState(input: {
  status: ExperimentalStrategyStatus | null;
  incomplete: boolean;
  capacityLimited: boolean;
  ready: number | undefined;
}): { reason: ExperimentalEmptyReason; label: string } {
  if (!input.status) return { reason: "connecting", label: "Waiting for the research connection." };
  if (input.incomplete || input.capacityLimited) {
    return { reason: "incomplete", label: "Results are incomplete; check feed coverage." };
  }
  if (input.ready === 0) return { reason: "not_ready", label: "Waiting for enough continuous minute bars." };
  if (input.ready !== undefined) return { reason: "ready_no_candidates", label: "No candidates among ready histories yet." };
  return { reason: "readiness_unknown", label: "No candidates in the observed feed yet. Histories may still be warming up." };
}

/** What the retained list is and is not. Both halves are load-bearing:
 * the cap is across BOTH strategies (so one busy strategy can crowd out
 * the other's history), and a reconnect replays only the server's
 * retained 100 updates, so this view is not a session log and must not
 * be read as one. */
export const EXPERIMENTAL_RETENTION_NOTE =
  "Holds up to 100 lifecycle observations across both strategies. A reconnect restores only the latest 100 updates, so this is not a complete session log.";

/** Thousands-separated explicitly rather than via `toLocaleString`, so
 * the separator is the same on every device regardless of locale. This
 * figure is a fixed engineering constant from the capacity amendment
 * (16,384 monitors), not a user-facing quantity to localise, and it is
 * quoted verbatim in `docs/IAC-EXTREME-NATIVE.md`; a separator that
 * varied by device would make the panel and the doc disagree, and would
 * make this helper's own test locale-dependent. */
function grouped(value: number): string {
  return String(value).replace(/\B(?=(\d{3})+(?!\d))/g, ",");
}

/** The symbol ceiling and what readiness does not mean. Null when the
 * server does not report a limit -- an invented ceiling would be a
 * capacity claim this client cannot support. */
export function experimentalLimitNote(status: ExperimentalStrategyStatus): string | null {
  if (status.symbolLimit === undefined) return null;
  return `History limit ${grouped(status.symbolLimit)} symbols. Readiness means enough continuous bars for the next evaluation, not a qualifying signal and not whole-market coverage.`;
}

/** The four reset/rejection totals, in one line. These are sticky server
 * totals, so they only ever grow; each one is a count of observations
 * this lane did NOT get to keep, which is exactly why they are shown
 * instead of hidden behind a healthy flag.
 *
 * Every field is optional, and a server that does not report one is NOT
 * a server reporting zero -- rendering `?? 0` would manufacture a clean
 * coverage figure out of an absent one, which is the precise overclaim
 * this lane exists to avoid. So: null when the server reports none of
 * them (nothing to disclose, and the caller renders no line at all),
 * "unavailable" for the individual fields that are absent, and the
 * literal number whenever one IS reported -- including a genuine 0,
 * which is a real and meaningful measurement. */
export function experimentalResetNote(status: ExperimentalStrategyStatus): string | null {
  const fields: [number | undefined, string][] = [
    [status.missingMinuteResets, "gap resets"],
    [status.revisionResets, "revised"],
    [status.invalidBarUpdates, "invalid"],
    [status.idleResets, "idle reset"],
  ];
  if (fields.every(([value]) => value === undefined)) return null;
  return fields.map(([value, label]) => (value === undefined ? `${label} unavailable` : `${value} ${label}`)).join(" · ");
}

/** Capacity's effect on coverage, with the bounded recent-symbol list the
 * server sends. Null when capacity never interfered and this session has
 * never seen it interfere.
 *
 * Same rule as the reset totals: an absent `capacityDroppedUpdates` is
 * not zero rejections. An old evictions-only server reports real
 * evictions and no rejection count at all, so this states the evictions
 * and stays silent about rejections rather than printing "0 updates
 * rejected" beside them -- which would read as "capacity turned nothing
 * away", the opposite of what that frame says.
 *
 * `capacitySeen` is the sticky latch. If capacity interfered earlier in
 * the session but the current frame omits every capacity field, there is
 * no count to show and the warning still has to stand, so it degrades to
 * the generic sentence instead of disappearing. */
export function experimentalCapacityNote(status: ExperimentalStrategyStatus, capacitySeen = false): string | null {
  // Not interfering right now: either nothing to say, or the latch says
  // it interfered earlier and there is no current count to report it
  // with. A frame that now reports a genuine 0 takes this path too --
  // Keep earlier coverage loss distinct from the current server's count.
  // A reported zero is available telemetry even under the standing latch.
  if (!experimentalCapacityLimited(status)) {
    if (!capacitySeen) return null;
    return status.capacityDroppedUpdates === undefined
      ? "Capacity interrupted coverage earlier this session; current rejection count unavailable."
      : `Capacity interrupted coverage earlier this session; the current server reports ${status.capacityDroppedUpdates} rejected updates.`;
  }
  // Interfering, so at least one of these two is a reported nonzero and
  // the result is never empty. Only reported counts appear.
  const parts: string[] = [];
  if (status.capacityDroppedUpdates !== undefined) parts.push(`${status.capacityDroppedUpdates} updates rejected at capacity`);
  if (status.evictions > 0) parts.push(`${status.evictions} histories evicted`);
  const recent = status.recentCapacityDroppedSymbols;
  if (recent?.length) parts.push(`recently excluded: ${recent.join(", ")}`);
  return parts.join(" · ");
}

/** Research updates dropped on the way to connected clients, aggregated
 * server-side across all of them -- so a nonzero value here does not
 * prove THIS phone missed those updates, only that the output channel
 * shed some. Stated that way rather than as a personal loss count. */
export function experimentalMissedUpdatesNote(status: ExperimentalStrategyStatus): string | null {
  const missed = status.outputLaggedEvents ?? 0;
  if (missed <= 0) return null;
  return `${missed} research updates missed across connected clients`;
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
