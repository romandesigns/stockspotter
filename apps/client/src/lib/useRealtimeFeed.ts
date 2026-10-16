import { authenticatedFetch, getAccessKey } from "@stockspotter/shared-types";
// Connects to crates/ws-server, speaks the handshake protocol in
// @stockspotter/shared-types, and hands back a bounded, ever-growing
// list of every detection event received — the one WebSocket connection
// every panel below reads from, so "every panel sees the same live feed"
// holds on the client side too, not just server side (see ws-server's
// own doc comment on that guarantee).
//
// Detection events are applied in batches, once per animation frame, not
// one setState per message: at the regular session's ~230 messages a
// second the per-message version re-rendered the whole app ~230 times a
// second and the chart fell behind the market (chart diagnosis
// 2026-10-16, root cause 1). The batch rules, and the guarantee that a
// batch gives exactly the per-message result, live in feedBatch.ts.
// Transport and control messages (welcome, ping, stream_lagged,
// feed_status) are still handled the moment they arrive.

import { useEffect, useState } from "react";
import {
  WS_PROTOCOL_VERSION,
  type ClientHello,
  type RealtimeMessage,
  type ServerFeedStatus,
} from "@stockspotter/shared-types";
import { recordGap, type FeedGap } from "./feedHealth";
import { attachOrderFlowSocket, dispatchOrderFlowFrame } from "./orderFlowWatch";
import { attachChartSocket } from "./chartSubscription";
import { resolveHttpUrl, resolveWsUrl } from "./config";
import { applyFeedBatch, emptyFeedState, type DetectionEvent, type FeedState } from "./feedBatch";

export type { DetectionEvent, PanelEvent } from "./feedBatch";

export type ConnectionStatus = "connecting" | "open" | "closed" | "stale";

const RECONNECT_DELAY_MS = 3000;
/** How long a queued batch may wait when no animation frame comes -- a
 * hidden tab gets no frames at all, and the browser clamps this timer to
 * about a second there anyway. While the tab is visible the frame always
 * wins. */
const FLUSH_FALLBACK_MS = 250;

/** Wire shape of ws-server's GET /catalysts/today rows -- same fields as
 * CatalystUpdate minus the WS envelope's `type` discriminant (this is a
 * plain REST array, not a tagged union member). */
interface CatalystBackfillRow {
  symbol: string;
  timestamp: string;
  catalystTags: string[];
  headlineCount: number;
  mostRecentHeadline: string | null;
  mostRecentPublishedAt?: string | null;
}

export function useRealtimeFeed() {
  const [status, setStatus] = useState<ConnectionStatus>("connecting");
  // Everything the detection events build, as one value: one batch, one
  // state change. Each collection inside keeps its identity unless the
  // batch changed it, so panels memoised on, say, catalystsBySymbol do not
  // re-render because a halt warning arrived. See feedBatch.ts.
  const [feed, setFeed] = useState<FeedState>(emptyFeedState);
  const [wsUrl] = useState(resolveWsUrl);
  // Whether this client has a known break in its event stream. Orthogonal
  // to `status` above on purpose: `status` is a transport property and
  // ConnectionStatus renders it as such, while this is a statement about
  // the candle series itself. See feedHealth.ts for the full reasoning on
  // why one cannot substitute for the other.
  const [feedGap, setFeedGap] = useState<FeedGap | null>(null);
  // Bumped whenever a gap is newly recorded, so consumers holding
  // authoritative history (useHistoricalBackfill) know to re-fetch over
  // it. A counter rather than a boolean: two lags in a row must trigger
  // two re-fetches, and a boolean would coalesce them.
  const [resyncNonce, setResyncNonce] = useState(0);
  // The server's own upstream market-data stream (`feed_status`). Null
  // until a server that sends it says something -- an older server never
  // does, and the chart then just relies on its periodic refetch.
  const [upstream, setUpstream] = useState<ServerFeedStatus | null>(null);

  useEffect(() => {
    let cancelled = false;
    let socket: WebSocket | null = null;
    let lastTransportAt = Date.now();
    let latestMarketAt = 0;
    let reconnectTimer: ReturnType<typeof setTimeout> | undefined;
    // A first connection is not a gap -- there is no prior continuity to
    // have lost. Every subsequent open is, because the server's retained
    // snapshot restores latest state per key, not the history that went
    // past while we were away (audit §6: "This restores some latest
    // state, not a complete time series").
    let hasConnectedBefore = false;
    const seenEvents = new Set<string>();
    // Last upstream state heard, across reconnects: a server that was down
    // when the old socket dropped and is up on the new one is a down->up.
    let lastUpstream: ServerFeedStatus | null = null;

    // The batch. `current` is the authoritative state; React gets a copy of
    // the reference once per flush. Applied here, never inside a setState
    // updater, so StrictMode's double-invoked updaters cannot double-apply
    // an edge (momentum crossings, alert lists).
    let current = emptyFeedState();
    let queue: DetectionEvent[] = [];
    let frame: number | null = null;
    let fallback: ReturnType<typeof setTimeout> | null = null;
    function flush() {
      if (frame !== null) cancelAnimationFrame(frame);
      if (fallback !== null) clearTimeout(fallback);
      frame = null;
      fallback = null;
      if (queue.length === 0) return;
      const batch = queue;
      queue = [];
      const next = applyFeedBatch(current, batch);
      if (next !== current) {
        current = next;
        setFeed(next);
      }
    }
    function enqueue(msg: DetectionEvent) {
      queue.push(msg);
      if (frame !== null || fallback !== null) return;
      if (typeof requestAnimationFrame === "function") frame = requestAnimationFrame(flush);
      fallback = setTimeout(flush, FLUSH_FALLBACK_MS);
    }
    /** For the one non-socket writer (the catalysts backfill): apply what
     * is queued first so it lands on current state, then the change. */
    function commit(change: (state: FeedState) => FeedState) {
      flush();
      const next = change(current);
      if (next !== current) {
        current = next;
        setFeed(next);
      }
    }

    function noteGap(next: FeedGap) {
      setFeedGap((prev) => recordGap(prev, next));
      setResyncNonce((n) => n + 1);
    }

    function marketStatus(): ConnectionStatus {
      return Date.now() - latestMarketAt < 90000 ? "open" : "stale";
    }

    function connect() {
      if (cancelled) return;
      setStatus("connecting");
      const ws = new WebSocket(wsUrl);
      socket = ws;

      ws.addEventListener("open", () => {
        if (hasConnectedBefore) {
          noteGap({ at: new Date().toISOString(), reason: "reconnect", missedEvents: null });
        }
        hasConnectedBefore = true;
        const hello: ClientHello = {
          type: "hello",
          protocolVersion: WS_PROTOCOL_VERSION,
          client: "web",
          token: getAccessKey(),
        };
        ws.send(JSON.stringify(hello));
      });

      ws.addEventListener("message", (raw) => {
        lastTransportAt = Date.now();
        let msg: RealtimeMessage;
        try {
          msg = JSON.parse(raw.data as string) as RealtimeMessage;
        } catch {
          return;
        }
        // Order-flow summaries for the charts this client has open. Not a
        // detection event (no eventId, no market timestamp, never
        // retained), so it is routed out BEFORE any of the handling below
        // can mistake it for one -- the `default` branch would otherwise
        // push it into the shared `events` list, where it is not a
        // PanelEvent at all. Returns false for anything that is not an
        // order-flow frame, so no other message's path changes. See
        // orderFlow.ts on why this type is web-only.
        if (dispatchOrderFlowFrame(msg)) return;
        const eventId = (msg as RealtimeMessage & { eventId?: string }).eventId;
        if (eventId) {
          if (seenEvents.has(eventId)) return;
          seenEvents.add(eventId);
          if (seenEvents.size > 20000) seenEvents.delete(seenEvents.values().next().value!);
        }
        if ("timestamp" in msg && msg.type !== "funnel_health" && msg.type !== "catalyst_update") {
          const at = Date.parse(msg.timestamp);
          if (Number.isFinite(at)) latestMarketAt = Math.max(latestMarketAt, at);
          // Immediate, not batched: an unchanged value costs React nothing,
          // and the transport indicator should not lag a frame.
          setStatus(marketStatus());
        }
        switch (msg.type) {
          case "hello":
          case "subscribe_chart":
            // Client -> server only; never received.
            return;
          case "welcome": {
            setStatus(marketStatus());
            // From here the socket can carry the order-flow and chart
            // subscriptions. Bound to THIS socket: a late call after a
            // reconnect must not write to the one that replaced it. With no
            // chart open this only stores the callback -- nothing is sent
            // until a chart registers a symbol.
            attachOrderFlowSocket((symbols) => {
              if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ type: "subscribe_order_flow", symbols }));
            });
            attachChartSocket((message) => {
              if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(message));
            });
            return;
          }
          case "hello_rejected":
            setStatus("closed");
            return;
          case "stream_lagged":
            // The server dropped events for this socket. It resends its
            // retained snapshot straight after, so current state recovers on
            // its own -- but anything that came and went inside the gap is
            // gone.
            //
            // This used to only `setStatus("stale")`, which the very next
            // message carrying a fresh timestamp reset to "open" a few
            // milliseconds later (see the per-message setStatus above).
            // The chart went straight back to looking authoritative while
            // still missing bars. The gap is now recorded separately and
            // is sticky -- see feedHealth.ts. `status` still goes stale so
            // the transport indicator reacts immediately too.
            console.warn(`stream lagged: missed ${msg.missedEvents} server events`);
            noteGap({ at: new Date().toISOString(), reason: "stream_lagged", missedEvents: msg.missedEvents });
            setStatus("stale");
            return;
          case "feed_status": {
            // The server's upstream stream went down or came back. On the
            // way back up every chart is missing the minutes in between:
            // the gap starts when the stream went DOWN (not now), and the
            // resync bump makes useHistoricalBackfill refetch over it.
            // `reconnect` is the closest existing FeedGap reason; see the
            // report on adding an "upstream" reason to shared-types.
            const previous = lastUpstream;
            lastUpstream = msg;
            setUpstream(msg);
            if (msg.state === "up" && previous?.state === "down") {
              noteGap({ at: previous.since, reason: "upstream", missedEvents: null });
            }
            return;
          }
          case "ping":
            ws.send(JSON.stringify({ type: "pong", at: msg.at }));
            return;
          case "pong":
            return;
          default:
            enqueue(msg);
        }
      });

      const scheduleReconnect = () => {
        // The socket that could carry the subscriptions is gone; the next
        // welcome re-attaches and re-sends the current symbol lists.
        attachOrderFlowSocket(null);
        attachChartSocket(null);
        if (cancelled) return;
        setStatus("closed");
        reconnectTimer = setTimeout(connect, RECONNECT_DELAY_MS);
      };
      ws.addEventListener("close", scheduleReconnect);
      ws.addEventListener("error", () => ws.close());
    }

    const heartbeat = setInterval(() => {
      if (socket?.readyState === WebSocket.OPEN) {
        if (Date.now() - lastTransportAt > 45000) { socket.close(); return; }
        socket.send(JSON.stringify({ type: "ping", at: new Date().toISOString() }));
        if (Date.now() - latestMarketAt > 90000) setStatus("stale");
      }
    }, 15000);
    connect();

    // Catalysts backfill -- catalyst_update fires once per symbol at
    // promotion time, not repeatedly like every other event type, so a
    // client that connects after that one-shot broadcast already happened
    // would otherwise show an honestly-empty Catalysts panel forever for
    // real, currently-tracked symbols (confirmed live 2026-09-01: 17 real
    // symbols had real catalyst tags server-side, a freshly-opened tab saw
    // none of them). ws-server's GET /catalysts/today reads the same cache
    // run_live_scan keeps in sync with the live watchlist. Best-effort and
    // additive only -- never overwrites a symbol the live socket already
    // populated (that's always at least as fresh as this one-time fetch).
    authenticatedFetch(`${resolveHttpUrl()}/catalysts/today`)
      .then((r) => {
        if (!r.ok) throw new Error(`catalysts backfill request failed: ${r.status}`);
        return r.json() as Promise<CatalystBackfillRow[]>;
      })
      .then((rows) => {
        if (cancelled || rows.length === 0) return;
        commit((state) => {
          const missing = rows.filter((row) => !state.catalystsBySymbol.has(row.symbol));
          if (missing.length === 0) return state;
          const copy = new Map(state.catalystsBySymbol);
          for (const row of missing) if (!copy.has(row.symbol)) copy.set(row.symbol, { type: "catalyst_update", ...row });
          return { ...state, catalystsBySymbol: copy };
        });
      })
      .catch(() => {
        // Best-effort -- the live socket still populates catalysts for
        // anything promoted from here on, just without this backfill.
      });

    return () => {
      clearInterval(heartbeat);
      cancelled = true;
      if (reconnectTimer) clearTimeout(reconnectTimer);
      if (frame !== null) cancelAnimationFrame(frame);
      if (fallback !== null) clearTimeout(fallback);
      attachOrderFlowSocket(null);
      attachChartSocket(null);
      socket?.close();
    };
  }, [wsUrl]);

  return {
    status,
    feedGap,
    resyncNonce,
    upstream,
    funnelHealth: feed.funnelHealth,
    events: feed.events,
    alerts: feed.alerts,
    halts: feed.halts,
    barsBySymbol: feed.barsBySymbol,
    chartSymbols: feed.chartSymbols,
    subMinuteBarsBySymbol: feed.subMinuteBarsBySymbol,
    momentumBySymbol: feed.momentumBySymbol,
    catalystsBySymbol: feed.catalystsBySymbol,
    funnelSignals: feed.funnelSignals,
    momentumConfirmations: feed.momentumConfirmations,
    momentumFadesBySymbol: feed.momentumFadesBySymbol,
    micropullbackEvents: feed.micropullbackEvents,
    ignitionConfirmedEvents: feed.ignitionConfirmedEvents,
    wsUrl,
  };
}
