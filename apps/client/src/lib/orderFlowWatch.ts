// Keeps one symbol's order-flow summary current for a chart, from two
// sources at once:
//
// - **Pushed over the realtime socket.** The client names the symbols it
//   has charts open for and the server sends each one's lite summary
//   every couple of seconds. This is what makes the strip feel live: the
//   HTTP poll it replaces runs every 5 seconds, because it draws on a
//   request budget shared with the whole app (ws-server allows 120 a
//   minute process-wide).
// - **Fetched over HTTP** for the full shape -- the chart arrays (delta
//   buckets, large prints, profile bins) are large and change slowly, so
//   they stay an occasional request -- and for everything, when there is
//   no push.
//
// The push is strictly an upgrade. A server that predates it ignores the
// subscribe message, no frames ever arrive, and this behaves exactly as
// the polling-only version does. Nothing here assumes the socket is up.
//
// Not a `ScanEvent` and not part of the broadcast every client receives:
// see crates/ws-server/src/server.rs on why order flow is per-connection.
//
// Free of React so the rules are testable directly, and free of
// `@stockspotter/shared-types` so it can be: the authenticated fetch is
// injected by the caller (`useOrderFlow.ts` passes the shared one).
import { decodeOrderFlowBody, decodeOrderFlowFrame, isFullSummary, withChartArrays, type OrderFlowCoverage, type OrderFlowPushFrame, type OrderFlowResponse, type OrderFlowSummary, type OrderFlowTape } from "./orderFlow";

export type OrderFlowState =
  | { status: "loading" }
  // The endpoint is missing (a backend that predates it) or unreachable.
  | { status: "unavailable" }
  // The server has it but ORDER_FLOW_ENABLED is off.
  | { status: "disabled" }
  // Enabled, but this symbol has not traded while tracked.
  | { status: "waiting" }
  // `coverage` says how far back the summary reaches: "session" once the
  // server has replayed the day's earlier trades into this engine,
  // "session_pending" while it is fetching them, "partial" when it only
  // has data since it began tracking the symbol (backfill off, failed, an
  // older server, or an engine that was evicted and recreated).
  | { status: "ready"; summary: OrderFlowSummary; coverage: OrderFlowCoverage; tape: OrderFlowTape | null };

type FrameListener = (frame: OrderFlowPushFrame) => void;

// One registry per app: every open chart's watcher and the one realtime
// socket have to meet somewhere, and neither owns the other.
const listeners = new Map<string, Set<FrameListener>>();
let sendSubscription: ((symbols: string[]) => void) | null = null;

function syncSubscription() {
  sendSubscription?.([...listeners.keys()]);
}

/**
 * Called by the realtime feed when its socket has been welcomed (with a
 * function that sends the subscription) and when it closes (with null).
 * Re-sends the current symbol list on every attach, because a new socket
 * starts with no subscription.
 *
 * With no chart open this is the whole cost of the feature on the socket:
 * one stored callback, and not even a subscribe message, since
 * `syncSubscription` is only called when a symbol list exists.
 */
export function attachOrderFlowSocket(send: ((symbols: string[]) => void) | null): void {
  sendSubscription = send;
  if (send && listeners.size > 0) syncSubscription();
}

/** Called by the realtime feed for every message that might be an
 * `order_flow` frame. Returns true if it was one and was handled, so the
 * caller knows not to treat it as a detection event. */
export function dispatchOrderFlowFrame(message: unknown): boolean {
  const frame = decodeOrderFlowFrame(message);
  if (!frame) return false;
  for (const listener of listeners.get(frame.symbol) ?? []) listener(frame);
  return true;
}

function listen(symbol: string, listener: FrameListener): () => void {
  let set = listeners.get(symbol);
  const isNewSymbol = !set;
  if (!set) listeners.set(symbol, (set = new Set()));
  set.add(listener);
  if (isNewSymbol) syncSubscription();
  return () => {
    const current = listeners.get(symbol);
    if (!current) return;
    current.delete(listener);
    if (current.size === 0) {
      listeners.delete(symbol);
      syncSubscription();
    }
  };
}

// 5 seconds, not faster: see the header on the shared request budget. A
// 2-second poll spends a quarter of it per open chart and pushes the rest
// of the app into 429s.
const POLL_MS = 5_000;
/** Every sixth poll (30 seconds) fetches the full shape. */
const FULL_EVERY = 6;
// A 429 means that shared budget is spent; back off rather than keep
// taking from the other panels' share. The full shape has its own,
// tighter budget on the server (6/min across the process), so this is the
// one that trips first.
const THROTTLED_RETRY_MS = 30_000;
// A missing endpoint will not appear mid-session short of a deploy, so
// asking again every few seconds would just fill the server log with 404s.
const UNAVAILABLE_RETRY_MS = 60_000;
/** The server pushes every 2 seconds; three missed frames and the push is
 * treated as gone, and lite polling resumes. */
const PUSH_LIVE_MS = 6_500;

export interface OrderFlowWatchOptions {
  symbol: string;
  baseUrl: () => string;
  onState: (state: OrderFlowState) => void;
  fetch: (input: string) => Promise<Response>;
  now?: () => number;
}

function coverageOf(body: OrderFlowResponse): OrderFlowCoverage {
  if (body.coverage) return body.coverage;
  // A server that predates `coverage` only reports its backfill state.
  // Anything short of "done" is partial, which is the safe reading.
  return body.backfill === "done" ? "session" : body.backfill === "running" ? "session_pending" : "partial";
}

/** When a summary's chart arrays were built, which for a full body is its
 * own `asOfSecs` and for a carried-forward one is the full body they came
 * from. */
function arraysAsOf(summary: OrderFlowSummary): number {
  return summary.chartArraysAsOfSecs ?? summary.asOfSecs;
}

function stateOf(body: OrderFlowResponse, summary: OrderFlowSummary | null): OrderFlowState {
  if (!body.enabled) return { status: "disabled" };
  if (!summary) return { status: "waiting" };
  return { status: "ready", summary, coverage: coverageOf(body), tape: body.tape ?? null };
}

/** Starts watching; returns the function that stops it. */
export function watchOrderFlow(options: OrderFlowWatchOptions): () => void {
  const { symbol, onState } = options;
  const request = options.fetch;
  const now = options.now ?? (() => Date.now());
  let cancelled = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  // Two response shapes, because the chart arrays are most of the bytes
  // (crates/ws-server/src/order_flow_wire.rs): the push and most polls
  // carry the "lite" summary, and every FULL_EVERY-th poll asks for the
  // full one to refresh the delta pane and the profile. Between full
  // fetches the arrays from the last one are carried forward WITH their
  // own age -- see `withChartArrays`.
  let polls = 0;
  let lastFull: OrderFlowSummary | null = null;
  let lastPushAt = -Infinity;
  // The summary currently on screen, with the body it came from. Needed
  // because answers do not arrive in the order they were asked for: the
  // push is live while a full HTTP fetch takes as long as it takes, and a
  // response that has been in flight across two pushes is older than what
  // the user is looking at.
  let shown: { body: OrderFlowResponse; summary: OrderFlowSummary } | null = null;

  /**
   * Applies one answer, from either source.
   *
   * A body whose summary is older than the one on screen does not replace
   * it -- the header would visibly step backwards and then forwards again
   * on the next push. But a *full* body is still worth having even when it
   * is late, because the chart arrays are the only place they come from:
   * its arrays are attached to what is already shown, with their own age,
   * and the header is left exactly where it is.
   *
   * Returns the state it emitted, or null if it emitted nothing, which is
   * what the poll's own retry delay is chosen from.
   */
  function apply(body: OrderFlowResponse): OrderFlowState | null {
    const emit = (state: OrderFlowState) => {
      onState(state);
      return state;
    };
    const summary = body.summary;
    if (!summary) {
      // "Enabled but nothing traded" is a real transition (an engine that
      // was evicted), so what was shown is no longer current.
      shown = null;
      return emit(stateOf(body, null));
    }
    const full = isFullSummary(summary);
    // The arrays are only adopted if they are newer than the ones already
    // held, for the same reason the header is not rolled back: two full
    // fetches can land in the wrong order, and the older one must not
    // replace the newer one's arrays.
    const adopted = full && (lastFull === null || arraysAsOf(summary) > arraysAsOf(lastFull));
    if (adopted) lastFull = summary;
    if (shown && summary.asOfSecs < shown.summary.asOfSecs) {
      // A late body leaves the header where it is. If it brought newer
      // arrays than the ones on screen, those are attached to what is
      // shown -- with their own age, so the panes still say how old they
      // are. If it brought nothing new, it changes nothing.
      if (!adopted) return null;
      shown = { body: shown.body, summary: withChartArrays(shown.summary, lastFull) };
      return emit(stateOf(shown.body, shown.summary));
    }
    const merged = full ? summary : withChartArrays(summary, lastFull);
    shown = { body, summary: merged };
    return emit(stateOf(body, merged));
  }

  const stopListening = listen(symbol, (frame) => {
    if (cancelled) return;
    lastPushAt = now();
    apply(frame);
  });

  async function poll() {
    const wantFull = polls % FULL_EVERY === 0;
    // While frames are arriving, the lite poll would only fetch what the
    // push has already delivered more recently. The full fetch still runs
    // on its own cadence, and is also what starts the server's session
    // backfill for a newly opened symbol.
    if (!wantFull && now() - lastPushAt < PUSH_LIVE_MS) {
      polls++;
      timer = setTimeout(poll, POLL_MS);
      return;
    }
    // A state this poll has to emit itself (nothing usable arrived), and
    // separately whatever `apply` put on screen. `apply` is the only
    // emitter for a body, so these are never both set.
    let failure: OrderFlowState | null = null;
    let applied: OrderFlowState | null = null;
    try {
      const path = `/orderflow/${encodeURIComponent(symbol)}${wantFull ? "/full" : ""}`;
      const res = await request(`${options.baseUrl()}${path}`);
      if (res.status === 429) {
        // Keep showing the last good summary; just ask again later.
        if (!cancelled) timer = setTimeout(poll, THROTTLED_RETRY_MS);
        return;
      }
      if (!res.ok) {
        failure = { status: "unavailable" };
      } else {
        // Decoded, not cast: see the decoder's own comment. A body that
        // cannot be read, or that is about another symbol, is "this
        // server is not giving me order flow" and not a half-built object
        // handed to a chart.
        const body = decodeOrderFlowBody(await res.json(), symbol);
        if (cancelled) return;
        if (body === null) failure = { status: "unavailable" };
        else applied = apply(body);
      }
    } catch {
      failure = { status: "unavailable" };
    }
    if (cancelled) return;
    // Advanced only on an answered poll: a throttled one returns above
    // and retries the same shape.
    polls++;
    if (failure) onState(failure);
    // A late body emits nothing and changes nothing, so it keeps the
    // ordinary cadence.
    const resolved = failure ?? applied;
    timer = setTimeout(poll, resolved?.status === "unavailable" || resolved?.status === "disabled" ? UNAVAILABLE_RETRY_MS : POLL_MS);
  }

  void poll();
  return () => {
    cancelled = true;
    if (timer) clearTimeout(timer);
    stopListening();
  };
}

/** Test seam: the module-level registry is process-wide by design, so a
 * test that attaches a socket has to be able to put it back. */
export function __resetOrderFlowRegistry(): void {
  listeners.clear();
  sendSubscription = null;
}
