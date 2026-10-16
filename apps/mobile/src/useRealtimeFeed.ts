import { authenticatedFetch, CHART_SUBSCRIPTION_LIMIT, getAccessKey, recordGap } from "@stockspotter/shared-types";
import { useCallback, useEffect, useRef, useState } from "react";
import type { BarUpdate, CatalystUpdate, ConsolidationEvent, FeedGap, FunnelSignal, IgnitionEvent, MomentumUpdate, RealtimeMessage, ServerFeedStatus } from "@stockspotter/shared-types";
import { WS_PROTOCOL_VERSION } from "@stockspotter/shared-types";
import { HTTP_URL, WS_URL } from "./config";
import type { DetectionEvent, FeedStatus } from "./types";
import { applyFeedBatch, coalesceFeedBatch, emptyFeedData, marketTimestampMs, type FeedData } from "./feedBatch";
const RECONNECT_MS = 3_000;
// The list caps (MAX_EVENTS, MAX_MICROPULLBACK_EVENTS,
// MAX_IGNITION_CONFIRMED_EVENTS, MAX_BARS_PER_SYMBOL) moved to
// feedBatch.ts with the reducer that applies them. Why each list exists
// at all, and why each is its own list instead of a view of `events`:
//
// - micropullbackEvents: real signal volume confirmed live 2026-09-03 (the
//   detection-efficiency benchmark): a handful of micropullback
//   EntryTriggered events per hour across the whole tracked universe,
//   nowhere near halt_warning's per-trade flood -- a small cap holds days
//   of real history. This is about keeping the alert feed's own state
//   dedicated (not derived from the shared, flood-prone `events` list,
//   same class of bug this project has hit repeatedly for bars/momentum/
//   funnel/catalysts), not about needing a large buffer.
// - ignitionConfirmedEvents: real gap found live (2026-09-04, Roman: "I
//   miss[ed] on[e] big move... due to not being alerted"): ONCO gapped 20%+
//   and ignition-detector confirmed it for real, but nothing ever surfaced
//   it. ignition_event's raw stream is FAR too frequent to alert on
//   directly (a single hot symbol fired follow_through_confirmed several
//   times within 90 seconds) -- useIgnitionAlerts.ts applies a per-symbol
//   cooldown on top of this dedicated feed.
// - MAX_BARS_PER_SYMBOL: same cap and reasoning as apps/client's own --
//   ~8.3 hours of 1-minute bars, a full extended-hours session plus room.

/** How long incoming data messages queue before being applied in one
 * state update (2026-10-16 chart diagnosis, root cause #1: ~230 msg/s in
 * regular hours, each one used to re-render the whole App).
 *
 * A timer, deliberately not requestAnimationFrame: on iOS the frame
 * callback stops while the app is backgrounded but still running, and
 * this state has to keep moving then -- useSpeedAlerts and the ignition/
 * micropullback hooks schedule LOCAL notifications precisely in that
 * window. ~12 updates a second is well under what a phone renders and
 * imperceptible on a candle that the server itself throttles to ~2/s. */
const FLUSH_MS = 80;
/** A queue this long is flushed at once rather than waiting for the
 * timer: bounds memory and the size of any single update if the JS
 * thread was stalled. */
const MAX_QUEUE = 4_000;

// Wire shape of ws-server's GET /catalysts/today rows -- same fields as
// CatalystUpdate minus the WS envelope's own `type` discriminant (a plain
// REST array, not a tagged union member). Same real endpoint the web app
// (apps/client) backfills from.
interface CatalystBackfillRow { symbol: string; timestamp: string; catalystTags: string[]; headlineCount: number; mostRecentHeadline: string | null; mostRecentPublishedAt?: string | null; }

/** The server's own upstream market-data stream, as last reported by
 * `feed_status`. null until a server that sends it says so -- an older
 * server never does, and then nothing about it is shown. */
export type UpstreamStatus = Pick<ServerFeedStatus, "state" | "since" | "reason">;

export function useRealtimeFeed(): {
  status: FeedStatus;
  /** Known break in this client's event stream. Orthogonal to `status`:
   * that is transport, this is whether the candle series is complete.
   * Same model and same implementation as the web app -- both import it
   * from @stockspotter/shared-types so the two cannot drift. */
  feedGap: FeedGap | null;
  /** Bumped on every newly recorded gap so history consumers re-fetch. */
  resyncNonce: number;
  /** Upstream market-data state from `feed_status`; see UpstreamStatus. */
  upstream: UpstreamStatus | null;
  /** Ask the server to build live candles for exactly these symbols on
   * this connection (`subscribe_chart`). Replaces the previous set; an
   * empty list clears it. Remembered and re-sent after every reconnect.
   * Harmless on a server that predates the message. */
  subscribeChart: (symbols: string[]) => void;
  events: DetectionEvent[];
  barsBySymbol: Map<string, BarUpdate[]>;
  subMinuteBarsBySymbol: Map<string, BarUpdate[]>;
  momentumBySymbol: Map<string, MomentumUpdate>;
  catalystsBySymbol: Map<string, CatalystUpdate>;
  funnelBySymbol: Map<string, FunnelSignal>;
  /** Real micropullback EntryTriggered events only (2026-09-03) -- not
   * every consolidation_event, and not derived from the shared `events`
   * list. This is what useMicropullbackAlerts.ts watches to fire a real
   * OS notification. */
  micropullbackEvents: ConsolidationEvent[];
  /** Real ignition follow_through_confirmed events only (2026-09-04).
   * What useIgnitionAlerts.ts watches to fire a real OS notification. */
  ignitionConfirmedEvents: IgnitionEvent[];
} {
  const [status, setStatus] = useState<FeedStatus>("connecting"); const retryRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [feedGap, setFeedGap] = useState<FeedGap | null>(null);
  const [resyncNonce, setResyncNonce] = useState(0);
  const [upstream, setUpstream] = useState<UpstreamStatus | null>(null);
  const noteGap = useRef((next: FeedGap) => { setFeedGap((prev) => recordGap(prev, next)); setResyncNonce((n) => n + 1); }).current;
  // Everything the data messages feed, in ONE state object updated once
  // per flush (see feedBatch.ts). Each field is still its own dedicated
  // structure, never a view of the shared `events` ring buffer -- the
  // reasons are long-standing and still hold:
  // - barsBySymbol: bar_update fires far less often than halt_warning's
  //   per-trade frequency, so a symbol's bar history would get flushed
  //   out of one shared ring buffer within seconds of real trading.
  // - subMinuteBarsBySymbol: real sub-minute (30s) live-only bars
  //   (2026-09-03), a genuinely separate stream. BarUpdate.intervalSecs
  //   is what tells the two apart on the wire, so routing on it before
  //   merging is required, not optional.
  // - catalystsBySymbol / momentumBySymbol / funnelBySymbol: latest per
  //   symbol, matching the web app. funnel_signal was the demonstrated
  //   version of the eviction bug (Roman: "It's not displaying any
  //   stocks... when it does, they just last for a few seconds").
  // reducer fields keep their identity when a batch does not touch them,
  // so App's memos keyed on (say) momentumBySymbol do not recompute on a
  // bars-only batch.
  const [data, setData] = useState<FeedData>(emptyFeedData);
  const seenEvents = useRef(new Set<string>());
  const latestMarketAt = useRef(0);
  const socketRef = useRef<WebSocket | null>(null);
  const welcomedRef = useRef(false);
  const chartSymbolsRef = useRef<string[]>([]);
  // What this connection last told the server, so an unchanged set is not
  // re-sent on every render of the caller. Cleared on every new socket.
  const sentChartKeyRef = useRef<string | null>(null);

  const sendChartSubscription = useRef(() => {
    const socket = socketRef.current;
    if (!socket || socket.readyState !== WebSocket.OPEN || !welcomedRef.current) return;
    const symbols = chartSymbolsRef.current;
    const key = symbols.join(",");
    // A fresh connection has no subscription server-side, so an empty set
    // needs no message there.
    if (key === sentChartKeyRef.current || (sentChartKeyRef.current === null && symbols.length === 0)) return;
    socket.send(JSON.stringify({ type: "subscribe_chart", symbols }));
    sentChartKeyRef.current = key;
  }).current;

  const subscribeChart = useCallback((symbols: string[]) => {
    const unique: string[] = [];
    for (const raw of symbols) {
      const symbol = raw.trim().toUpperCase();
      if (symbol && !unique.includes(symbol)) unique.push(symbol);
      if (unique.length >= CHART_SUBSCRIPTION_LIMIT) break;
    }
    chartSymbolsRef.current = unique;
    sendChartSubscription();
  }, [sendChartSubscription]);

  useEffect(() => { let disposed = false; let socket: WebSocket | null = null;
    // A first connection is not a gap. Every later one is: the server's
    // retained snapshot restores latest state per key, not the history
    // that went past while this client was away. Stamped at reopen rather
    // than at close, because the series is suspect right up to the moment
    // the stream resumed, and seriesSpansGap clears only once every
    // retained bar starts after that instant.
    let hasConnectedBefore = false;
    let lastTransportAt = Date.now();
    // Last upstream state seen on ANY connection of this session, so a
    // `down` before a reconnect and an `up` after it still count as one
    // outage to repair.
    let lastUpstream: UpstreamStatus | null = null;

    let queue: RealtimeMessage[] = [];
    let flushTimer: ReturnType<typeof setTimeout> | null = null;
    const flush = () => {
      if (flushTimer) { clearTimeout(flushTimer); flushTimer = null; }
      if (disposed || queue.length === 0) return;
      const batch = coalesceFeedBatch(queue);
      queue = [];
      setData((prev) => applyFeedBatch(prev, batch));
      // One status decision per flush instead of per message; React bails
      // out when it is unchanged.
      setStatus(Date.now() - latestMarketAt.current < 90000 ? "open" : "stale");
    };
    const enqueue = (message: RealtimeMessage) => {
      queue.push(message);
      if (queue.length >= MAX_QUEUE) { flush(); return; }
      if (!flushTimer) flushTimer = setTimeout(flush, FLUSH_MS);
    };

    const onFeedStatus = (message: ServerFeedStatus) => {
      const next: UpstreamStatus = { state: message.state, since: message.since, reason: message.reason };
      const previous = lastUpstream;
      lastUpstream = next;
      setUpstream(next);
      // down -> up: every chart is missing whatever traded while the
      // server had no data. Mark the gap from the moment it went down
      // (that is when continuity was lost) and bump resyncNonce so open
      // charts refetch authoritative history over it. The FeedGap reason
      // vocabulary is shared with web.
      if (previous?.state === "down" && next.state === "up") {
        noteGap({ at: previous.since, reason: "upstream", missedEvents: null });
      }
    };

    const connect = () => { if (disposed) return; setStatus("connecting"); socket = new WebSocket(WS_URL); socketRef.current = socket;
      welcomedRef.current = false; sentChartKeyRef.current = null;
      socket.addEventListener("open", () => {
        if (hasConnectedBefore) noteGap({ at: new Date().toISOString(), reason: "reconnect", missedEvents: null });
        hasConnectedBefore = true;
        socket?.send(JSON.stringify({ type: "hello", protocolVersion: WS_PROTOCOL_VERSION, client: "mobile", token: getAccessKey() }));
      });
      socket.addEventListener("message", (raw) => { lastTransportAt = Date.now(); let message: RealtimeMessage; try { message = JSON.parse(String(raw.data)) as RealtimeMessage; } catch { return; }
        const eventId = (message as RealtimeMessage & { eventId?: string }).eventId;
        if (eventId) {
          if (seenEvents.current.has(eventId)) return;
          seenEvents.current.add(eventId);
          if (seenEvents.current.size > 20000) seenEvents.current.delete(seenEvents.current.values().next().value!);
        }
        const at = marketTimestampMs(message);
        if (at !== null) latestMarketAt.current = Math.max(latestMarketAt.current, at);
        // Control messages are handled the moment they arrive; only data
        // messages wait for the next flush.
        if (message.type === "welcome") {
          setStatus(Date.now() - latestMarketAt.current < 90000 ? "open" : "stale");
          // Subscriptions are per connection: re-send the chart set now
          // that the server has accepted this one.
          welcomedRef.current = true; sendChartSubscription();
          return;
        }
        if (message.type === "hello_rejected") { setStatus("closed"); socket?.close(); return; }
        if (message.type === "feed_status") { onFeedStatus(message); return; }
        // Server dropped events for this socket; it resends its retained
        // snapshot next, so state recovers, but the gap itself is lost.
        // The old behaviour only set "stale", which the next status update
        // cleared on the very next event carrying a fresh timestamp -- the
        // chart was back to looking authoritative within milliseconds
        // while still missing bars. The gap is now recorded separately and
        // is sticky; see feedHealth.ts in shared-types.
        if (message.type === "stream_lagged") { noteGap({ at: new Date().toISOString(), reason: "stream_lagged", missedEvents: message.missedEvents }); setStatus("stale"); return; }
        if (message.type === "ping") { socket?.send(JSON.stringify({ type: "pong", at: message.at })); return; }
        if (message.type === "hello" || message.type === "pong" || message.type === "subscribe_chart") return;
        enqueue(message);
      });
      const reconnect = () => { if (disposed) return; welcomedRef.current = false; setStatus("closed"); if (retryRef.current) clearTimeout(retryRef.current); retryRef.current = setTimeout(connect, RECONNECT_MS); };
      socket.addEventListener("close", reconnect); socket.addEventListener("error", () => socket?.close()); };
    const heartbeat = setInterval(() => {
      if (socket?.readyState === WebSocket.OPEN) {
        if (Date.now() - lastTransportAt > 45000) { socket.close(); return; }
        socket.send(JSON.stringify({ type: "ping", at: new Date().toISOString() }));
        if (Date.now() - latestMarketAt.current > 90000) setStatus("stale");
      }
    }, 15000);
    connect(); return () => { disposed = true; clearInterval(heartbeat); if (flushTimer) clearTimeout(flushTimer); if (retryRef.current) clearTimeout(retryRef.current); socketRef.current = null; socket?.close(); }; }, [noteGap, sendChartSubscription]);

  // Catalyst backfill -- catalyst_update fires once per symbol at
  // promotion time, not repeatedly like every other event type, so a
  // client that only just connected would otherwise show zero catalyst
  // alerts for real, currently-tracked symbols whose one-shot lookup
  // already happened before this session opened (the exact same gap
  // found and fixed on the web app -- see apps/client/src/lib/
  // useRealtimeFeed.ts's own backfill effect). Best-effort, additive
  // only: never overwrites a symbol the live socket already delivered
  // (that's always at least as fresh as this one-time fetch).
  useEffect(() => { let disposed = false;
    authenticatedFetch(`${HTTP_URL}/catalysts/today`).then((r) => { if (!r.ok) throw new Error(`catalysts backfill failed: ${r.status}`); return r.json() as Promise<CatalystBackfillRow[]>; })
      .then((rows) => { if (disposed || rows.length === 0) return;
        setData((prev) => { const copy = new Map(prev.catalystsBySymbol); for (const row of rows) { if (copy.has(row.symbol)) continue; copy.set(row.symbol, { type: "catalyst_update", ...row }); } return { ...prev, catalystsBySymbol: copy }; }); })
      .catch(() => { /* best-effort -- the live socket still populates catalysts for anything promoted from here on */ });
    return () => { disposed = true; }; }, []);

  return { status, feedGap, resyncNonce, upstream, subscribeChart, ...data };
}
