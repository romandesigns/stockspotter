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
  type ClientHello,
  type RealtimeMessage,
} from "@stockspotter/shared-types";

export type ConnectionStatus = "connecting" | "open" | "closed";

/** Detection events only — handshake/ping-pong messages are consumed
 * internally and never surfaced to panels. */
export type DetectionEvent = Exclude<
  RealtimeMessage,
  { type: "hello" } | { type: "welcome" } | { type: "hello_rejected" } | { type: "ping" } | { type: "pong" }
>;

/** Everything panels read off the generic `events` feed except bar_update
 * — that's routed to its own `barsBySymbol` map instead (see
 * MAX_BARS_PER_SYMBOL's doc comment on why it needs separate retention). */
export type PanelEvent = Exclude<DetectionEvent, { type: "bar_update" }>;

const DEFAULT_WS_URL = "ws://localhost:8787";
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

function resolveWsUrl(): string {
  const fromEnv = (import.meta as { env?: Record<string, string | undefined> }).env?.VITE_WS_URL;
  return fromEnv && fromEnv.length > 0 ? fromEnv : DEFAULT_WS_URL;
}

export function useRealtimeFeed() {
  const [status, setStatus] = useState<ConnectionStatus>("connecting");
  const [events, setEvents] = useState<PanelEvent[]>([]);
  const [barsBySymbol, setBarsBySymbol] = useState<Map<string, BarUpdate[]>>(new Map());
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
              const next = [...existing, msg];
              const trimmed = next.length > MAX_BARS_PER_SYMBOL ? next.slice(next.length - MAX_BARS_PER_SYMBOL) : next;
              const copy = new Map(prev);
              copy.set(msg.symbol, trimmed);
              return copy;
            });
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

  return { status, events, barsBySymbol, wsUrl: urlRef.current };
}
