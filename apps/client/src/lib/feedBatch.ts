// The realtime feed's state and the one pure function that folds a batch
// of incoming messages into it.
//
// Why batches. In the regular session the socket delivers ~230 messages a
// second (measured 2026-10-15, chart diagnosis report), almost all of them
// ignition events and halt warnings. useRealtimeFeed used to apply each one
// with its own setState, so App re-rendered once per message. Now messages
// are queued and applied here once per animation frame: one state change,
// one render, however many messages arrived since the last frame.
//
// The rule this file keeps: **the result of one batch is exactly the result
// of applying its messages one at a time, in order**, as the per-message
// code did. Only the timing changes. Each "coalesce" below is a shortcut
// that is provably equivalent, never an approximation; the tests in
// feedBatch.test.ts compare against a one-message-at-a-time reference.
//
// Free of React so it is directly testable.

import type {
  BarUpdate,
  CatalystUpdate,
  ConsolidationEvent,
  FunnelHealth,
  FunnelSignal,
  HaltWarning,
  IgnitionEvent,
  MomentumUpdate,
  RealtimeMessage,
} from "@stockspotter/shared-types";
import { reconcileBars } from "./reconcileBars";

/** Detection events only — handshake, ping/pong and feed-control messages
 * are consumed by useRealtimeFeed and never reach this module or panels. */
export type DetectionEvent = Exclude<
  RealtimeMessage,
  | { type: "hello" }
  | { type: "welcome" }
  | { type: "hello_rejected" }
  | { type: "ping" }
  | { type: "pong" }
  | { type: "stream_lagged" }
  | { type: "feed_status" }
  | { type: "subscribe_chart" }
>;

/** Everything panels read off the generic `events` feed except bar_update,
 * catalyst_update, and funnel_signal — all three routed to their own
 * dedicated state instead (barsBySymbol, catalystsBySymbol, funnelSignals;
 * see each one's own comment below for why). */
export type PanelEvent = Exclude<DetectionEvent, { type: "bar_update" } | { type: "catalyst_update" } | { type: "funnel_signal" }>;

export const MAX_EVENTS = 500;
/** halt_warning keeps only the latest reading per symbol, at most this many
 * symbols, after every other event in `events`. */
export const MAX_HALT_SYMBOLS = 200;
/** Gap & Go's own real cadence (a handful of funnel evaluations per
 * tracked symbol per rescan cycle) is nowhere near halt_warning's
 * ~2000+/min, but it was still sharing the same MAX_EVENTS=500 ring
 * buffer with it -- confirmed live 2026-09-03: the buffer fully turns
 * over in well under a minute of real trading, so a funnel_signal
 * landing in it would get evicted within seconds, exactly matching what
 * Roman saw (Gap & Go empty, or a symbol visible only "for a few
 * seconds"). Same root cause, same fix shape as bar_update/
 * catalyst_update's own dedicated routing below -- funnel_signal just
 * hadn't gotten it yet. */
export const MAX_FUNNEL_SIGNALS = 200;
/** Same real bug, same fix, for Bullish Momentum's "just crossed the
 * qualify threshold" feed -- see momentumConfirmations below. */
export const MAX_MOMENTUM_CONFIRMATIONS = 100;
/** Bounded rolling chart history, per symbol AND per interval. Bars get
 * their own cap, separate from MAX_EVENTS above and keyed per symbol
 * rather than shared: halt_warning fires on every trade (far more often
 * than once/minute) and would otherwise flush a symbol's whole bar
 * history out of one shared ring buffer within seconds of real trading
 * activity — exactly the kind of chart-goes-blank bug that'd only show up
 * once real volume hit it, not in a quiet test.
 *
 * What 500 actually covers, corrected 2026-09-20: 8h20 of 1-minute bars
 * and 4h10 of 30-second bars. An earlier version of this comment claimed
 * "a full extended-hours session plus room to spare" — that is wrong.
 * Extended hours run 04:00–20:00 ET, sixteen hours, so a 1-minute chart
 * retains roughly half a session and a 30-second chart a quarter of one.
 *
 * Deliberately left at 500 rather than raised. This cap exists for memory
 * and rendering stability, not to define how much history is available:
 * 1-minute history is re-fetched authoritatively from ws-server's
 * /bars/:symbol (see useHistoricalBackfill), so the retained window is a
 * live buffer, not the source of truth. Raising it would multiply
 * per-symbol memory across up to four simultaneous chart slots to buy
 * history the backfill already provides. The 30-second stream has no
 * backfill at all (a real Alpaca constraint, see SuperChart.tsx), so its
 * 4h10 genuinely is all the history that exists client-side — that
 * limitation is surfaced, not hidden, via the gap/stale semantics in
 * feedHealth.ts rather than papered over with a bigger buffer. */
export const MAX_BARS_PER_SYMBOL = 500;
/** Real signal volume confirmed live 2026-09-03 (the detection-efficiency
 * benchmark): a handful of micropullback EntryTriggered events per hour
 * across the whole tracked universe, nowhere near halt_warning's per-
 * trade flood -- a small cap here holds days of real history. Kept as
 * its own dedicated list (not derived from the shared `events` above)
 * for the same reason every other flood-prone type already has one. */
export const MAX_MICROPULLBACK_EVENTS = 50;
/** Real gap found live (2026-09-04, Roman: "I miss[ed] on[e] big move...
 * due to not being alerted"): ONCO gapped 20%+ and ignition-detector
 * confirmed it for real (this project's strongest evidenced signal --
 * 32-35% hit rate over 10,000+ live signals), but nothing ever surfaced
 * it -- the only two alert mechanisms that existed were a manual price
 * target and micropullback-only. ignition_event's raw stream is FAR too
 * frequent to alert on directly (confirmed live: a single hot symbol
 * fired follow_through_confirmed multiple times within 90 seconds) --
 * useIgnitionAlerts.ts applies a real per-symbol cooldown on top of this
 * dedicated feed, same "own list, not the flood-prone shared `events`"
 * fix shape as micropullback above, just with different real-world
 * volume math (small cap is still enough -- confirmed genuinely
 * confirmed ignitions, even during a hot session, are nowhere near
 * halt_warning's per-trade flood). */
export const MAX_IGNITION_CONFIRMED_EVENTS = 100;

export interface FeedState {
  // Latest Stage-1 float-budget health, or null before the first
  // universe rescan lands. See FunnelHealth's own doc comment: without
  // this the Gap & Go panel can't tell "quiet market" from "the funnel
  // can't answer", since both render as an empty list.
  funnelHealth: FunnelHealth | null;
  /** Every non-halt event newest first (capped at MAX_EVENTS), then the
   * latest halt_warning per symbol, newest first: exactly
   * `[...alerts, ...halts]`. */
  events: PanelEvent[];
  /** The two halves of `events`, each keeping its identity while only the
   * other one changes. In the regular session most batches carry halt
   * warnings and nothing else; consumers of one half (the Ignition feed
   * reads alerts, the Halt panel reads halts) then do not recompute. */
  alerts: PanelEvent[];
  halts: HaltWarning[];
  barsBySymbol: Map<string, BarUpdate[]>;
  /** Every symbol with 1-minute bars, sorted: the chart's symbol picker.
   * Kept here rather than derived in ChartPanel, which re-sorted the whole
   * key list on every bar. Symbols are only ever added to barsBySymbol, so
   * this changes (and gets a new identity) only when a new one appears. */
  chartSymbols: string[];
  // Real sub-minute (30s) live-only bars (2026-09-03) -- a genuinely
  // separate stream from barsBySymbol above, not a filtered view of it.
  // BarUpdate.intervalSecs is what tells the two apart on the wire (see
  // that field's own doc comment in shared-types); routing a 30s bar
  // into the 1-minute map (or vice versa) would corrupt whichever
  // timeframe is currently displayed, since both share the same
  // symbol/timestamp/OHLCV shape otherwise.
  subMinuteBarsBySymbol: Map<string, BarUpdate[]>;
  // Latest-only, keyed by symbol -- same reasoning as barsBySymbol above:
  // momentum_update fires once per bar (confirmed live: ~14/min per
  // symbol vs. halt_warning's ~2000+/min across all tracked symbols), so
  // it's just as vulnerable to being flushed out of the shared MAX_EVENTS
  // ring buffer. Super Chart's momentum panel needs "the current reading
  // for this one symbol", which is exactly what this map gives it.
  momentumBySymbol: Map<string, MomentumUpdate>;
  // Latest-only, keyed by symbol -- catalyst_update fires just once per
  // symbol at promotion time, so it's exactly the kind of rare event that
  // funnel_signal's much higher frequency would eventually flush out of
  // the shared MAX_EVENTS ring buffer on a long-running session.
  catalystsBySymbol: Map<string, CatalystUpdate>;
  // Gap & Go's own dedicated, capped feed -- every Stage 1/2 verdict
  // (passed or not, the panel shows both). See MAX_FUNNEL_SIGNALS.
  funnelSignals: FunnelSignal[];
  // Bullish Momentum's own dedicated, capped feed of qualify-threshold
  // CROSSINGS specifically (not every momentum_update -- that would be
  // one new row per tracked symbol per bar, pure noise), computed
  // incrementally as messages arrive. Edge state lives in
  // momentumQualified below.
  momentumConfirmations: MomentumUpdate[];
  // The opposite edge: per symbol, the latest reading that dropped back
  // below the qualify threshold. Bullish Momentum shows a faded card for a
  // while after this reading, then drops it (lib/momentumChart.ts) --
  // momentumBySymbol alone can't say when it faded, its timestamp keeps
  // moving with every new non-qualifying bar.
  momentumFadesBySymbol: Map<string, MomentumUpdate>;
  // Real micropullback EntryTriggered events only (2026-09-03) -- not
  // every consolidation_event. What useMicropullbackAlerts.ts watches.
  micropullbackEvents: ConsolidationEvent[];
  // Real ignition follow_through_confirmed events only (2026-09-04) --
  // see MAX_IGNITION_CONFIRMED_EVENTS. What useIgnitionAlerts.ts watches.
  ignitionConfirmedEvents: IgnitionEvent[];
  /** Edge-detection bookkeeping for momentum crossings: per symbol, did
   * the last reading qualify. Not rendered by anyone. It used to be a ref
   * so a React updater could not double-apply it; this module is never
   * run inside an updater (see useRealtimeFeed), so it can live with the
   * rest of the state and stay pure. */
  momentumQualified: Map<string, boolean>;
}

export function emptyFeedState(): FeedState {
  return {
    funnelHealth: null,
    events: [],
    alerts: [],
    halts: [],
    barsBySymbol: new Map(),
    chartSymbols: [],
    subMinuteBarsBySymbol: new Map(),
    momentumBySymbol: new Map(),
    catalystsBySymbol: new Map(),
    funnelSignals: [],
    momentumConfirmations: [],
    momentumFadesBySymbol: new Map(),
    micropullbackEvents: [],
    ignitionConfirmedEvents: [],
    momentumQualified: new Map(),
  };
}

/** `[...newest-first additions, ...prev]` capped, or `prev` itself when
 * nothing was added, so an untouched list keeps its identity. */
function prependCapped<T>(prev: T[], addedOldestFirst: T[], cap: number): T[] {
  if (addedOldestFirst.length === 0) return prev;
  const next = addedOldestFirst.slice().reverse();
  for (const item of prev) {
    if (next.length >= cap) break;
    next.push(item);
  }
  return next.length > cap ? next.slice(0, cap) : next;
}

/**
 * Folds every bar for one symbol+interval in a batch down to at most one
 * per bucket timestamp, keeping what sequential reconcileBars would have
 * kept: a later bar replaces an earlier one, except that a forming bar
 * never replaces a final one. Applying the folded bars through
 * reconcileBars then gives the same series as applying them all in order:
 * reconcileBars keys by timestamp, so order between DIFFERENT timestamps
 * does not matter, and its trim only ever drops the oldest bars, which a
 * newer insertion can never bring back.
 *
 * This is where forming-bar floods collapse: ws-server re-sends the
 * current bucket on every throttled tick, and only the newest one of
 * each batch is ever visible.
 */
export function foldBarsByBucket(bars: BarUpdate[]): BarUpdate[] {
  // Keyed by the parsed instant, as reconcileBars keys, not the string: two
  // spellings of one instant are one bucket.
  const byBucket = new Map<number, BarUpdate>();
  for (const bar of bars) {
    const time = Date.parse(bar.timestamp);
    // reconcileBars ignores an unparseable timestamp; so does the fold.
    if (!Number.isFinite(time)) continue;
    const current = byBucket.get(time);
    if (current?.isFinal && !bar.isFinal) continue;
    // Delete then set moves it to the end, so the folded list keeps the
    // order the latest bars arrived in (cosmetic: reconcileBars sorts).
    byBucket.delete(time);
    byBucket.set(time, bar);
  }
  return [...byBucket.values()];
}

function applyBars(prev: Map<string, BarUpdate[]>, incoming: Map<string, BarUpdate[]>): Map<string, BarUpdate[]> {
  if (incoming.size === 0) return prev;
  let next: Map<string, BarUpdate[]> | null = null;
  for (const [symbol, bars] of incoming) {
    const existing = prev.get(symbol) ?? [];
    let series = existing;
    for (const bar of foldBarsByBucket(bars)) series = reconcileBars(series, bar, MAX_BARS_PER_SYMBOL);
    // reconcileBars returns `existing` itself when nothing changed (a
    // forming bar arriving after its final), so an untouched symbol keeps
    // its array identity and the chart's memos do not re-run for it.
    if (series === existing && prev.has(symbol)) continue;
    next ??= new Map(prev);
    next.set(symbol, series);
  }
  return next ?? prev;
}

/**
 * Applies a batch of detection events, in arrival order, to `state`.
 * Returns `state` itself if nothing changed; otherwise a new object in
 * which every collection the batch did not touch keeps its identity.
 */
export function applyFeedBatch(state: FeedState, batch: readonly DetectionEvent[]): FeedState {
  if (batch.length === 0) return state;

  const minuteBars = new Map<string, BarUpdate[]>();
  const subMinuteBars = new Map<string, BarUpdate[]>();
  const newAlerts: PanelEvent[] = [];
  const newHalts: HaltWarning[] = [];
  const newFunnel: FunnelSignal[] = [];
  const newConfirmations: MomentumUpdate[] = [];
  const newMicropullbacks: ConsolidationEvent[] = [];
  const newIgnitionConfirmed: IgnitionEvent[] = [];
  let funnelHealth = state.funnelHealth;
  let momentumBySymbol: Map<string, MomentumUpdate> | null = null;
  let fades: Map<string, MomentumUpdate> | null = null;
  let qualified: Map<string, boolean> | null = null;
  let catalysts: Map<string, CatalystUpdate> | null = null;

  for (const msg of batch) {
    switch (msg.type) {
      case "bar_update": {
        // Real correctness requirement (2026-09-03): a 1-minute and a
        // 30-second bar for the same symbol are otherwise structurally
        // indistinguishable -- intervalSecs routes each into its own map so
        // neither stream can corrupt the other.
        const target = msg.intervalSecs === 30 ? subMinuteBars : minuteBars;
        const list = target.get(msg.symbol);
        if (list) list.push(msg);
        else target.set(msg.symbol, [msg]);
        break;
      }
      case "momentum_update": {
        // Edge-detect in arrival order: a symbol that crosses up and back
        // down inside one batch records both edges, exactly as it did when
        // each message was applied on its own.
        qualified ??= new Map(state.momentumQualified);
        const wasQualified = qualified.get(msg.symbol) ?? false;
        qualified.set(msg.symbol, msg.qualifies);
        if (msg.qualifies && !wasQualified) newConfirmations.push(msg);
        if (!msg.qualifies && wasQualified) {
          fades ??= new Map(state.momentumFadesBySymbol);
          fades.set(msg.symbol, msg);
        }
        momentumBySymbol ??= new Map(state.momentumBySymbol);
        momentumBySymbol.set(msg.symbol, msg);
        break;
      }
      case "funnel_signal":
        newFunnel.push(msg);
        break;
      case "catalyst_update":
        // Catalysts panel only ever needs "the latest tags for this
        // symbol", so this one doesn't duplicate into `events`.
        catalysts ??= new Map(state.catalystsBySymbol);
        catalysts.set(msg.symbol, msg);
        break;
      case "funnel_health":
        // Scanner telemetry, not a market event -- a single latest-value
        // slot rather than pushed into `events`.
        funnelHealth = msg;
        break;
      case "ignition_event":
        // An ADDITIONAL consumer (useIgnitionAlerts.ts); the event still
        // goes to `events` for the Ignition panel's own feed.
        if (msg.kind === "follow_through_confirmed") newIgnitionConfirmed.push(msg);
        newAlerts.push(msg);
        break;
      case "consolidation_event":
        // Same shape: the micropullback list is an additional consumer,
        // deriveIgnitionFeed still reads it from `events`.
        if (msg.kind === "entry_triggered" && msg.strategy === "micropullback") newMicropullbacks.push(msg);
        newAlerts.push(msg);
        break;
      case "halt_warning":
        newHalts.push(msg);
        break;
      default:
        newAlerts.push(msg);
    }
  }

  // `events` is alerts (non-halt, newest first, capped) followed by the
  // latest halt_warning per symbol (newest first, capped). The halt half
  // applied one message at a time is "move this symbol's reading to the
  // front, keep 200"; for a whole batch that is exactly: the batch's latest
  // reading per symbol, newest first, then the older readings of every
  // other symbol, keep 200. The intermediate trims cannot differ, because
  // the list of distinct symbols never shrinks and a trim only drops the
  // least recently updated one.
  const alerts = prependCapped(state.alerts, newAlerts, MAX_EVENTS);
  let halts = state.halts;
  if (newHalts.length > 0) {
    halts = [];
    const seen = new Set<string>();
    for (let i = newHalts.length - 1; i >= 0 && halts.length < MAX_HALT_SYMBOLS; i--) {
      const h = newHalts[i];
      if (seen.has(h.symbol)) continue;
      seen.add(h.symbol);
      halts.push(h);
    }
    for (const h of state.halts) {
      if (halts.length >= MAX_HALT_SYMBOLS) break;
      if (!seen.has(h.symbol)) halts.push(h);
    }
  }
  const events = alerts === state.alerts && halts === state.halts ? state.events : [...alerts, ...halts];

  const barsBySymbol = applyBars(state.barsBySymbol, minuteBars);
  const next: FeedState = {
    funnelHealth,
    events,
    alerts,
    halts,
    barsBySymbol,
    chartSymbols: barsBySymbol.size === state.barsBySymbol.size ? state.chartSymbols : [...barsBySymbol.keys()].sort(),
    subMinuteBarsBySymbol: applyBars(state.subMinuteBarsBySymbol, subMinuteBars),
    momentumBySymbol: momentumBySymbol ?? state.momentumBySymbol,
    catalystsBySymbol: catalysts ?? state.catalystsBySymbol,
    funnelSignals: prependCapped(state.funnelSignals, newFunnel, MAX_FUNNEL_SIGNALS),
    momentumConfirmations: prependCapped(state.momentumConfirmations, newConfirmations, MAX_MOMENTUM_CONFIRMATIONS),
    momentumFadesBySymbol: fades ?? state.momentumFadesBySymbol,
    micropullbackEvents: prependCapped(state.micropullbackEvents, newMicropullbacks, MAX_MICROPULLBACK_EVENTS),
    ignitionConfirmedEvents: prependCapped(state.ignitionConfirmedEvents, newIgnitionConfirmed, MAX_IGNITION_CONFIRMED_EVENTS),
    momentumQualified: qualified ?? state.momentumQualified,
  };
  const changed = (Object.keys(next) as (keyof FeedState)[]).some((key) => next[key] !== state[key]);
  return changed ? next : state;
}
