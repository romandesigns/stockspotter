// Connects to crates/ws-server, speaks the handshake protocol in
// @stockspotter/shared-types, and hands back a bounded, ever-growing
// list of every detection event received — the one WebSocket connection
// every panel below reads from, so "every panel sees the same live feed"
// holds on the client side too, not just server side (see ws-server's
// own doc comment on that guarantee).

import { useEffect, useRef, useState } from "react";
import {
  WS_PROTOCOL_VERSION,
  type BarUpdate,
  type CatalystUpdate,
  type ClientHello,
  type MomentumUpdate,
  type RealtimeMessage,
} from "@stockspotter/shared-types";
import { resolveHttpUrl, resolveWsUrl } from "./config";

export type ConnectionStatus = "connecting" | "open" | "closed";

/** Detection events only — handshake/ping-pong messages are consumed
 * internally and never surfaced to panels. */
export type DetectionEvent = Exclude<
  RealtimeMessage,
  { type: "hello" } | { type: "welcome" } | { type: "hello_rejected" } | { type: "ping" } | { type: "pong" }
>;

/** Everything panels read off the generic `events` feed except bar_update
 * and catalyst_update — both routed to their own latest-per-symbol maps
 * instead (barsBySymbol, catalystsBySymbol; see MAX_BARS_PER_SYMBOL's doc
 * comment and catalystsBySymbol's own comment below for why). */
export type PanelEvent = Exclude<DetectionEvent, { type: "bar_update" } | { type: "catalyst_update" }>;

const RECONNECT_DELAY_MS = 3000;
const MAX_EVENTS = 500;
/** ~8.3 hours of 1-minute bars per symbol — a full extended-hours session
 * plus room to spare. Bars get their own cap, separate from MAX_EVENTS
 * above and keyed per symbol rather than shared: halt_warning fires on
 * every trade (far more often than once/minute) and would otherwise flush
 * a symbol's whole bar history out of one shared ring buffer within
 * seconds of real trading activity — exactly the kind of chart-goes-blank
 * bug that'd only show up once real volume hit it, not in a quiet test. */
const MAX_BARS_PER_SYMBOL = 500;

/** Wire shape of ws-server's GET /catalysts/today rows -- same fields as
 * CatalystUpdate minus the WS envelope's `type` discriminant (this is a
 * plain REST array, not a tagged union member). */
interface CatalystBackfillRow {
  symbol: string;
  timestamp: string;
  catalystTags: string[];
  headlineCount: number;
  mostRecentHeadline: string | null;
}

export function useRealtimeFeed() {
  const [status, setStatus] = useState<ConnectionStatus>("connecting");
  const [events, setEvents] = useState<PanelEvent[]>([]);
  const [barsBySymbol, setBarsBySymbol] = useState<Map<string, BarUpdate[]>>(new Map());
  // Latest-only, keyed by symbol -- same reasoning as barsBySymbol above:
  // momentum_update fires once per bar (confirmed live: ~14/min per
  // symbol vs. halt_warning's ~2000+/min across all tracked symbols), so
  // it's just as vulnerable to being flushed out of the shared MAX_EVENTS
  // ring buffer. Super Chart's momentum panel needs "the current reading
  // for this one symbol", which is exactly what this map gives it,
  // without needing history the way deriveConfirmedMomentum's edge-
  // triggering does off the generic `events` list below (so
  // momentum_update still gets pushed there too, not routed away from
  // it — this map is additive, not a replacement).
  const [momentumBySymbol, setMomentumBySymbol] = useState<Map<string, MomentumUpdate>>(new Map());
  // Latest-only, keyed by symbol -- same flooding risk as momentumBySymbol
  // above: catalyst_update fires just once per symbol at promotion time,
  // so it's exactly the kind of rare event that funnel_signal's much
  // higher per-bar-per-tracked-symbol frequency would eventually flush out
  // of the shared MAX_EVENTS ring buffer on a long-running session, even
  // though there's no acute per-trade flood the way halt_warning has.
  const [catalystsBySymbol, setCatalystsBySymbol] = useState<Map<string, CatalystUpdate>>(new Map());
  const urlRef = useRef(resolveWsUrl());

  useEffect(() => {
    let cancelled = false;
    let socket: WebSocket | null = null;
    let reconnectTimer: ReturnType<typeof setTimeout> | undefined;

    function connect() {
      if (cancelled) return;
      setStatus("connecting");
      socket = new WebSocket(urlRef.current);

      socket.addEventListener("open", () => {
        const hello: ClientHello = {
          type: "hello",
          protocolVersion: WS_PROTOCOL_VERSION,
          client: "web",
        };
        socket?.send(JSON.stringify(hello));
      });

      socket.addEventListener("message", (raw) => {
        let msg: RealtimeMessage;
        try {
          msg = JSON.parse(raw.data as string) as RealtimeMessage;
        } catch {
          return;
        }
        switch (msg.type) {
          case "hello":
            // Never sent server->client — only here so TS can narrow
            // `default` below to exactly `DetectionEvent`.
            return;
          case "welcome":
            setStatus("open");
            return;
          case "hello_rejected":
            setStatus("closed");
            return;
          case "ping":
            socket?.send(JSON.stringify({ type: "pong", at: msg.at }));
            return;
          case "pong":
            return;
          case "bar_update":
            setBarsBySymbol((prev) => {
              const existing = prev.get(msg.symbol) ?? [];
              // ws-server now live-updates the CURRENT, still-forming
              // minute from raw trade ticks (throttled ~2/sec) instead of
              // only sending a bar once a full minute closes -- multiple
              // messages can share the same `timestamp` (the minute's own
              // start) as that candle grows. Replace the last entry in
              // place when that happens rather than appending every one:
              // appending would (a) make the chart's last candle flicker
              // between stale/current values depending on render timing
              // (mergeBars/toChartBars key by time, so array ORDER doesn't
              // matter for correctness, but MAX_BARS_PER_SYMBOL's own trim
              // does -- at ~2 updates/sec instead of 1/min, an append-only
              // array would fill its whole cap in a few minutes instead of
              // the ~8.3 hours the cap is sized for) and (b) defeat the
              // point of a bounded per-symbol history entirely.
              const last = existing[existing.length - 1];
              const next = last && last.timestamp === msg.timestamp ? [...existing.slice(0, -1), msg] : [...existing, msg];
              const trimmed = next.length > MAX_BARS_PER_SYMBOL ? next.slice(next.length - MAX_BARS_PER_SYMBOL) : next;
              const copy = new Map(prev);
              copy.set(msg.symbol, trimmed);
              return copy;
            });
            return;
          case "momentum_update":
            setMomentumBySymbol((prev) => {
              const copy = new Map(prev);
              copy.set(msg.symbol, msg);
              return copy;
            });
            // No `return` here — momentum updates still need to land in
            // the generic `events` list too, for deriveConfirmedMomentum's
            // edge-triggered feed. TS's noFallthroughCasesInSwitch blocks
            // implicit fallthrough into `default` even with a comment, so
            // this duplicates default's own two lines rather than fight it.
            setEvents((prev) => {
              const next = [msg, ...prev];
              return next.length > MAX_EVENTS ? next.slice(0, MAX_EVENTS) : next;
            });
            return;
          case "catalyst_update":
            setCatalystsBySymbol((prev) => {
              const copy = new Map(prev);
              copy.set(msg.symbol, msg);
              return copy;
            });
            // Catalysts panel only ever needs "the latest tags for this
            // symbol", not a scrolling history the way FunnelPanel's edge-
            // triggered feed does off the generic list — unlike
            // momentum_update above, there's no second consumer that needs
            // it there too, so this one doesn't duplicate into `events`.
            return;
          default:
            setEvents((prev) => {
              const next = [msg, ...prev];
              return next.length > MAX_EVENTS ? next.slice(0, MAX_EVENTS) : next;
            });
        }
      });

      const scheduleReconnect = () => {
        if (cancelled) return;
        setStatus("closed");
        reconnectTimer = setTimeout(connect, RECONNECT_DELAY_MS);
      };
      socket.addEventListener("close", scheduleReconnect);
      socket.addEventListener("error", () => socket?.close());
    }

    connect();
    return () => {
      cancelled = true;
      if (reconnectTimer) clearTimeout(reconnectTimer);
      socket?.close();
    };
  }, []);

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
  useEffect(() => {
    let cancelled = false;
    fetch(`${resolveHttpUrl()}/catalysts/today`)
      .then((r) => {
        if (!r.ok) throw new Error(`catalysts backfill request failed: ${r.status}`);
        return r.json() as Promise<CatalystBackfillRow[]>;
      })
      .then((rows) => {
        if (cancelled || rows.length === 0) return;
        setCatalystsBySymbol((prev) => {
          const copy = new Map(prev);
          for (const row of rows) {
            if (copy.has(row.symbol)) continue;
            copy.set(row.symbol, { type: "catalyst_update", ...row });
          }
          return copy;
        });
      })
      .catch(() => {
        // Best-effort -- the live socket still populates catalysts for
        // anything promoted from here on, just without this backfill.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return { status, events, barsBySymbol, momentumBySymbol, catalystsBySymbol, wsUrl: urlRef.current };
}
