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
  type ConsolidationEvent,
  type FunnelSignal,
  type IgnitionEvent,
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

/** Everything panels read off the generic `events` feed except bar_update,
 * catalyst_update, and funnel_signal — all three routed to their own
 * dedicated state instead (barsBySymbol, catalystsBySymbol, funnelSignals;
 * see each one's own comment below for why). */
export type PanelEvent = Exclude<DetectionEvent, { type: "bar_update" } | { type: "catalyst_update" } | { type: "funnel_signal" }>;

const RECONNECT_DELAY_MS = 3000;
const MAX_EVENTS = 500;
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
const MAX_FUNNEL_SIGNALS = 200;
/** Same real bug, same fix, for Bullish Momentum's "just crossed the
 * qualify threshold" feed -- see momentumConfirmations below. */
const MAX_MOMENTUM_CONFIRMATIONS = 100;
/** ~8.3 hours of 1-minute bars per symbol — a full extended-hours session
 * plus room to spare. Bars get their own cap, separate from MAX_EVENTS
 * above and keyed per symbol rather than shared: halt_warning fires on
 * every trade (far more often than once/minute) and would otherwise flush
 * a symbol's whole bar history out of one shared ring buffer within
 * seconds of real trading activity — exactly the kind of chart-goes-blank
 * bug that'd only show up once real volume hit it, not in a quiet test. */
const MAX_BARS_PER_SYMBOL = 500;
/** Real signal volume confirmed live 2026-09-03 (the detection-efficiency
 * benchmark): a handful of micropullback EntryTriggered events per hour
 * across the whole tracked universe, nowhere near halt_warning's per-
 * trade flood -- a small cap here holds days of real history. Kept as
 * its own dedicated list (not derived from the shared `events` above)
 * for the same reason every other flood-prone type already has one. */
const MAX_MICROPULLBACK_EVENTS = 50;
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
const MAX_IGNITION_CONFIRMED_EVENTS = 100;

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
  // Real sub-minute (30s) live-only bars (2026-09-03) -- a genuinely
  // separate stream from barsBySymbol above, not a filtered view of it.
  // BarUpdate.intervalSecs is what tells the two apart on the wire (see
  // that field's own doc comment in shared-types); routing a 30s bar
  // into the 1-minute map (or vice versa) would corrupt whichever
  // timeframe is currently displayed, since both share the same
  // symbol/timestamp/OHLCV shape otherwise.
  const [subMinuteBarsBySymbol, setSubMinuteBarsBySymbol] = useState<Map<string, BarUpdate[]>>(new Map());
  // Latest-only, keyed by symbol -- same reasoning as barsBySymbol above:
  // momentum_update fires once per bar (confirmed live: ~14/min per
  // symbol vs. halt_warning's ~2000+/min across all tracked symbols), so
  // it's just as vulnerable to being flushed out of the shared MAX_EVENTS
  // ring buffer. Super Chart's momentum panel needs "the current reading
  // for this one symbol", which is exactly what this map gives it.
  const [momentumBySymbol, setMomentumBySymbol] = useState<Map<string, MomentumUpdate>>(new Map());
  // Latest-only, keyed by symbol -- same flooding risk as momentumBySymbol
  // above: catalyst_update fires just once per symbol at promotion time,
  // so it's exactly the kind of rare event that funnel_signal's much
  // higher per-bar-per-tracked-symbol frequency would eventually flush out
  // of the shared MAX_EVENTS ring buffer on a long-running session, even
  // though there's no acute per-trade flood the way halt_warning has.
  const [catalystsBySymbol, setCatalystsBySymbol] = useState<Map<string, CatalystUpdate>>(new Map());
  // Gap & Go's own dedicated, capped feed -- every Stage 1/2 verdict
  // (passed or not, the panel shows both), immune to halt_warning/
  // ignition_event/momentum_update's own combined volume in the shared
  // `events` list. See MAX_FUNNEL_SIGNALS' own comment for the real bug
  // this fixes.
  const [funnelSignals, setFunnelSignals] = useState<FunnelSignal[]>([]);
  // Bullish Momentum's own dedicated, capped feed of qualify-threshold
  // CROSSINGS specifically (not every momentum_update -- that would be
  // one new row per tracked symbol per bar, pure noise), computed
  // incrementally as messages arrive rather than re-derived from the
  // flood-prone `events` list the way this used to work. Edge state
  // lives in momentumQualifiedRef below, not in this array itself.
  const [momentumConfirmations, setMomentumConfirmations] = useState<MomentumUpdate[]>([]);
  // Real micropullback EntryTriggered events only (2026-09-03) -- not
  // every consolidation_event. What useMicropullbackAlerts.ts watches to
  // fire a real browser Notification + in-app toast.
  const [micropullbackEvents, setMicropullbackEvents] = useState<ConsolidationEvent[]>([]);
  // Real ignition follow_through_confirmed events only (2026-09-04) --
  // see MAX_IGNITION_CONFIRMED_EVENTS' own comment. What
  // useIgnitionAlerts.ts watches to fire a real cross-symbol alert.
  const [ignitionConfirmedEvents, setIgnitionConfirmedEvents] = useState<IgnitionEvent[]>([]);
  // Plain bookkeeping for the edge-detection above -- a ref, not state,
  // since nothing needs to re-render off it directly, and updating it
  // inside a setState updater (the natural place otherwise) risks a
  // duplicate push if React ever re-invokes that updater (StrictMode
  // double-invoke, concurrent rendering).
  const momentumQualifiedRef = useRef<Map<string, boolean>>(new Map());
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
          case "bar_update": {
            // Real correctness requirement (2026-09-03): a 1-minute and a
            // 30-second bar for the same symbol are otherwise structurally
            // indistinguishable (same symbol/timestamp/OHLCV shape) --
            // intervalSecs routes each into its own dedicated map so
            // neither stream can corrupt the other (see BarUpdate's own
            // doc comment in shared-types).
            const setter = msg.intervalSecs === 30 ? setSubMinuteBarsBySymbol : setBarsBySymbol;
            setter((prev) => {
              const existing = prev.get(msg.symbol) ?? [];
              // ws-server now live-updates the CURRENT, still-forming
              // bucket from raw trade ticks (throttled ~2/sec) instead of
              // only sending a bar once the bucket closes -- multiple
              // messages can share the same `timestamp` (the bucket's own
              // start) as that candle grows. Replace the last entry in
              // place when that happens rather than appending every one:
              // appending would (a) make the chart's last candle flicker
              // between stale/current values depending on render timing
              // (mergeBars/toChartBars key by time, so array ORDER doesn't
              // matter for correctness, but MAX_BARS_PER_SYMBOL's own trim
              // does -- at ~2 updates/sec instead of 1/bucket, an append-only
              // array would fill its whole cap much faster than the buffer
              // is sized for) and (b) defeat the point of a bounded
              // per-symbol history entirely.
              const last = existing[existing.length - 1];
              const next = last && last.timestamp === msg.timestamp ? [...existing.slice(0, -1), msg] : [...existing, msg];
              const trimmed = next.length > MAX_BARS_PER_SYMBOL ? next.slice(next.length - MAX_BARS_PER_SYMBOL) : next;
              const copy = new Map(prev);
              copy.set(msg.symbol, trimmed);
              return copy;
            });
            return;
          }
          case "momentum_update": {
            // Edge-detect BEFORE updating momentumBySymbol, off the ref
            // (not off momentumBySymbol's own prior state inside its
            // updater — see momentumQualifiedRef's own comment on why).
            const wasQualified = momentumQualifiedRef.current.get(msg.symbol) ?? false;
            momentumQualifiedRef.current.set(msg.symbol, msg.qualifies);
            if (msg.qualifies && !wasQualified) {
              setMomentumConfirmations((prev) => {
                const next = [msg, ...prev];
                return next.length > MAX_MOMENTUM_CONFIRMATIONS ? next.slice(0, MAX_MOMENTUM_CONFIRMATIONS) : next;
              });
            }
            setMomentumBySymbol((prev) => {
              const copy = new Map(prev);
              copy.set(msg.symbol, msg);
              return copy;
            });
            return;
          }
          case "funnel_signal":
            setFunnelSignals((prev) => {
              const next = [msg, ...prev];
              return next.length > MAX_FUNNEL_SIGNALS ? next.slice(0, MAX_FUNNEL_SIGNALS) : next;
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
          case "ignition_event":
            // Same "own dedicated list, still falls through too" shape as
            // consolidation_event below -- deriveIgnitionFeed already
            // reads ignition_event from the shared `events` list for the
            // Ignition panel's own feed, this is an ADDITIONAL consumer
            // (useIgnitionAlerts.ts).
            if (msg.kind === "follow_through_confirmed") {
              setIgnitionConfirmedEvents((prev) => {
                const next = [msg, ...prev];
                return next.length > MAX_IGNITION_CONFIRMED_EVENTS ? next.slice(0, MAX_IGNITION_CONFIRMED_EVENTS) : next;
              });
            }
            setEvents((prev) => {
              const next = [msg, ...prev];
              return next.length > MAX_EVENTS ? next.slice(0, MAX_EVENTS) : next;
            });
            return;
          case "consolidation_event":
            if (msg.kind === "entry_triggered" && msg.strategy === "micropullback") {
              setMicropullbackEvents((prev) => {
                const next = [msg, ...prev];
                return next.length > MAX_MICROPULLBACK_EVENTS ? next.slice(0, MAX_MICROPULLBACK_EVENTS) : next;
              });
            }
            // Falls through to the generic `events` list below too (no
            // `return` here) -- deriveIgnitionFeed() already reads
            // consolidation_event from there for the Ignition panel's own
            // "CB"/"MPB" chip row; this is an ADDITIONAL consumer, not a
            // replacement.
            setEvents((prev) => {
              const next = [msg, ...prev];
              return next.length > MAX_EVENTS ? next.slice(0, MAX_EVENTS) : next;
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

  return {
    status,
    events,
    barsBySymbol,
    subMinuteBarsBySymbol,
    momentumBySymbol,
    catalystsBySymbol,
    funnelSignals,
    momentumConfirmations,
    micropullbackEvents,
    ignitionConfirmedEvents,
    wsUrl: urlRef.current,
  };
}
