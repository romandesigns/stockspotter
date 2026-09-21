// Chart freshness: does the candle series we are drawing actually
// represent a complete, current picture of the market?
//
// This is deliberately NOT the same question as `ConnectionStatus` in
// useRealtimeFeed.ts. That one answers "is the socket up and did
// something arrive recently", which is a transport property. A socket can
// be wide open, delivering fresh bars every second, while the series on
// screen is missing ten minutes in the middle because the server dropped
// events for this client (`stream_lagged`) or because we reconnected and
// the retained snapshot only restored *latest* state, not the history.
//
// Before this module the two were conflated: `stream_lagged` set the
// status to "stale", and then the very next message carrying any fresh
// timestamp reset it to "open" -- see the per-message setStatus call in
// useRealtimeFeed. The chart went back to looking authoritative within
// milliseconds of losing data, which is the exact failure the chart
// fidelity audit (docs/chart-fidelity-latency-audit-2026-09-20.md, §6
// "Interpreting gaps") warns about: an empty minute may be a legitimate
// no-trade interval, an outage, or queue loss, and existing data cannot
// reliably tell them apart. So we never silently guess.
//
// A gap is therefore STICKY. It is not cleared by new data arriving; it
// is cleared only when the affected series no longer spans the moment
// continuity was lost -- either because authoritative history was
// re-fetched over it, or because the retained window has rolled forward
// far enough that every bar on screen arrived after the gap.

/** What the chart itself can honestly claim about its series. */
import type { SymbolFreshnessResult } from "./symbolFreshness";

export type ChartFreshness =
  /** Series believed complete and current. */
  | "live"
  /** First connection not yet established. */
  | "connecting"
  /** Transport dropped; a retry is scheduled or in flight. */
  | "reconnecting"
  /** Connected, but no market data for longer than the stale threshold. */
  | "stale"
  /** Known discontinuity inside the displayed window. Resync required. */
  | "gap";

/** A point at which this client is known to have stopped receiving a
 * continuous stream. `at` is the instant continuity was lost, which is
 * what makes it comparable against bar timestamps. */
export interface FeedGap {
  at: string;
  reason: "stream_lagged" | "reconnect";
  /** Server-reported count from `stream_lagged`; null for a reconnect,
   * where the number of missed events is genuinely unknown. */
  missedEvents: number | null;
}

/**
 * Fold a newly observed gap into whatever gap is already outstanding.
 *
 * Keeps the EARLIEST unresolved gap rather than the most recent one. A
 * series is suspect from the first moment continuity broke, so replacing
 * an older gap with a newer one would shrink the suspect window and let
 * `seriesSpansGap` clear a series that still straddles the original loss.
 * Two lags a minute apart leave the history suspect from the first.
 */
export function recordGap(existing: FeedGap | null, next: FeedGap): FeedGap {
  if (!existing) return next;
  const existingAt = Date.parse(existing.at);
  const nextAt = Date.parse(next.at);
  if (!Number.isFinite(nextAt)) return existing;
  if (!Number.isFinite(existingAt)) return next;
  return nextAt < existingAt ? next : existing;
}

/**
 * Does the displayed series straddle the gap instant?
 *
 * `earliestBarTimeSeconds` is the first bar the chart is actually
 * drawing, in seconds (CandleBar.time's unit), not milliseconds.
 *
 * An empty series returns false: there is no chart to mislead anyone
 * with, and flagging "gap" over an empty panel would just be noise.
 *
 * Once every retained bar starts after the gap, the window has rolled
 * past the discontinuity and nothing on screen spans it any more, so the
 * series is honest again without needing a re-fetch. That is what lets a
 * 30-second chart -- which has no authoritative backfill to repair it --
 * eventually recover on its own instead of being flagged forever.
 */
export function seriesSpansGap(earliestBarTimeSeconds: number | null, gap: FeedGap | null): boolean {
  if (!gap || earliestBarTimeSeconds === null) return false;
  const gapAt = Date.parse(gap.at);
  if (!Number.isFinite(gapAt)) return false;
  return earliestBarTimeSeconds * 1000 <= gapAt;
}

export interface FreshnessInput {
  /** useRealtimeFeed's transport-level status. Deliberately consumed as
   * given rather than recomputed from a timestamp here: the 90-second
   * market-data threshold lives in useRealtimeFeed, and duplicating it in
   * this module would be two thresholds that can silently drift apart. */
  transport: "connecting" | "open" | "closed" | "stale";
  gap: FeedGap | null;
  /** First bar of the series being drawn, in seconds. */
  earliestBarTimeSeconds: number | null;
}

/**
 * Collapse transport state and series continuity into the single claim
 * the UI is allowed to make.
 *
 * Precedence is deliberate:
 *
 *   reconnecting/connecting > gap > stale > live
 *
 * Transport comes first because while we are disconnected we cannot know
 * whether a gap is forming; "reconnecting" already tells the user not to
 * trust currency, and it is the more specific statement.
 *
 * `gap` outranks `stale` because they call for different responses. Stale
 * is transient and self-healing -- data resumes and the chart is correct
 * again. A gap never heals by waiting: it needs a resync, or it needs the
 * retained window to roll past it. Surfacing the recoverable condition
 * and hiding the one that needs action would be the wrong way round.
 */
export function resolveChartFreshness(input: FreshnessInput): ChartFreshness {
  if (input.transport === "connecting") return "connecting";
  if (input.transport === "closed") return "reconnecting";
  if (seriesSpansGap(input.earliestBarTimeSeconds, input.gap)) return "gap";
  if (input.transport === "stale") return "stale";
  return "live";
}


// ---------------------------------------------------------------------------
// Layer 2: the displayed symbol.
//
// Everything above describes the transport/capture channel. It stays exactly
// as it was -- the gap guarantees it provides are unchanged and still tested.
// What follows composes it with the per-symbol verdict from
// symbolFreshness.ts, because "the socket is healthy" and "this candle is
// current" turned out to be very different claims: on 2026-09-21, with the
// socket provably healthy, 52.4% of tracked symbols had no chart update for
// over 90 seconds.

/** The single claim the chart is allowed to make, transport and symbol
 *  combined. Superset of ChartFreshness plus the two symbol-only states. */
export type ChartStatus = ChartFreshness | "quiet" | "insufficient_history";

export interface ChartStatusInput extends FreshnessInput {
  /** Per-symbol verdict for the series being drawn, from
   *  resolveSymbolFreshness. Omit to get transport-only behaviour. */
  symbol?: SymbolFreshnessResult | null;
}

/**
 * Precedence: connecting > reconnecting > gap > symbol stale > symbol quiet
 *             > insufficient history > live
 *
 * Transport is evaluated first and the symbol layer can only ever *downgrade*
 * from live. Two consequences are load-bearing:
 *
 *   - Fresh activity on another symbol cannot clear this symbol's stale or
 *     quiet condition, because the symbol verdict is computed from that
 *     symbol's own history and nothing else reaches it.
 *   - A new tick cannot clear an unresolved feed gap, because the gap test
 *     runs before the symbol test and is itself sticky -- it clears only when
 *     the displayed series no longer spans the lost instant, which is the
 *     authoritative recovery contract, not the arrival of data.
 *
 * NOTE ON ORDERING. The 2026-09-21 brief suggested gap ahead of
 * disconnected/reconnecting. This keeps transport first, deliberately, and
 * the gap guarantee is unaffected either way because the gap is sticky and
 * resurfaces the moment the transport recovers. While disconnected the gap is
 * still *growing* and its extent is unknown, so "Reconnecting" is both the
 * more specific and the more actionable statement; showing "Gap - resync"
 * during a disconnect would imply a bounded, repairable discontinuity that we
 * cannot yet characterise. Both orderings are asserted in the tests so the
 * choice is visible rather than incidental.
 */
export function resolveChartStatus(input: ChartStatusInput): ChartStatus {
  const transport = resolveChartFreshness(input);
  if (transport !== "live") return transport;
  const sym = input.symbol;
  if (!sym) return "live";
  if (sym.freshness === "stale") return "stale";
  if (sym.freshness === "quiet") return "quiet";
  if (sym.freshness === "insufficient_history") return "insufficient_history";
  return "live";
}

/** Whole seconds, no false precision: these ages come from client receipt
 *  times and are meaningful to about a second, not better. */
export function formatAge(ageSecs: number | null): string | null {
  if (ageSecs === null || !Number.isFinite(ageSecs)) return null;
  const s = Math.max(0, Math.round(ageSecs));
  if (s < 90) return `${s}s`;
  const m = Math.round(s / 60);
  return `${m}m`;
}

export const STATUS_LABEL: Record<ChartStatus, string> = {
  live: "Live",
  connecting: "Connecting",
  reconnecting: "Reconnecting",
  stale: "Stale",
  gap: "Gap — resync",
  quiet: "Quiet",
  insufficient_history: "Waiting",
};

/** Minimal user-facing wording. Kept here rather than in the component so
 * web and the native WebView chart cannot drift apart on what a state is
 * called -- the same vocabulary rule the motion specs follow. */
export const FRESHNESS_LABEL: Record<ChartFreshness, string> = {
  live: "Live",
  connecting: "Connecting",
  reconnecting: "Reconnecting",
  stale: "Stale",
  gap: "Gap — resync",
};
