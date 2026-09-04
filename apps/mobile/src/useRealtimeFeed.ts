import { useEffect, useRef, useState } from "react";
import type { BarUpdate, CatalystUpdate, ConsolidationEvent, FunnelSignal, MomentumUpdate, RealtimeMessage } from "@stockspotter/shared-types";
import { WS_PROTOCOL_VERSION } from "@stockspotter/shared-types";
import { HTTP_URL, WS_URL } from "./config";
import type { DetectionEvent, FeedStatus } from "./types";
const MAX_EVENTS = 500; const RECONNECT_MS = 3_000;
/** Real signal volume confirmed live 2026-09-03 (the detection-efficiency
 * benchmark): a handful of micropullback EntryTriggered events per hour
 * across the whole tracked universe, nowhere near halt_warning's per-
 * trade flood -- a small cap here holds days of real history, this is
 * about keeping the alert feed's own state dedicated (not derived from
 * the shared, flood-prone `events` list, same class of bug this project
 * has hit repeatedly for bars/momentum/funnel/catalysts), not about
 * needing a large buffer. */
const MAX_MICROPULLBACK_EVENTS = 50;
/** Same cap and same reasoning as apps/client's own MAX_BARS_PER_SYMBOL:
 * ~8.3 hours of 1-minute bars, a full extended-hours session plus room
 * to spare. */
const MAX_BARS_PER_SYMBOL = 500;

// Wire shape of ws-server's GET /catalysts/today rows -- same fields as
// CatalystUpdate minus the WS envelope's own `type` discriminant (a plain
// REST array, not a tagged union member). Same real endpoint the web app
// (apps/client) backfills from.
interface CatalystBackfillRow { symbol: string; timestamp: string; catalystTags: string[]; headlineCount: number; mostRecentHeadline: string | null; }

export function useRealtimeFeed(): {
  status: FeedStatus;
  events: DetectionEvent[];
  barsBySymbol: Map<string, BarUpdate[]>;
  subMinuteBarsBySymbol: Map<string, BarUpdate[]>;
  momentumBySymbol: Map<string, MomentumUpdate>;
  catalystsBySymbol: Map<string, CatalystUpdate>;
  funnelBySymbol: Map<string, FunnelSignal>;
  /** Real micropullback EntryTriggered events only (2026-09-03) -- not
   * every consolidation_event, and not derived from the shared `events`
   * list (see MAX_MICROPULLBACK_EVENTS' own comment). This is what
   * useMicropullbackAlerts.ts watches to fire a real OS notification. */
  micropullbackEvents: ConsolidationEvent[];
} {
  const [status, setStatus] = useState<FeedStatus>("connecting"); const [events, setEvents] = useState<DetectionEvent[]>([]); const retryRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Dedicated latest-bars-per-symbol map, kept separate from the shared
  // capped `events` list -- same real bug already found and fixed on the
  // web app (apps/client/src/lib/useRealtimeFeed.ts's own
  // MAX_BARS_PER_SYMBOL comment): bar_update fires far less often than
  // halt_warning's per-trade frequency, so a symbol's bar history would
  // get flushed out of one shared ring buffer within seconds of real
  // trading activity if bars just lived in `events` like everything else.
  const [barsBySymbol, setBarsBySymbol] = useState<Map<string, BarUpdate[]>>(new Map());
  // Real sub-minute (30s) live-only bars (2026-09-03) -- a genuinely
  // separate stream from barsBySymbol above, not a filtered view of it.
  // BarUpdate.intervalSecs is what tells the two apart on the wire; a
  // 1-minute and a 30-second bar for the same symbol are otherwise
  // structurally identical, so routing on intervalSecs before merging
  // is required, not optional (see that field's own doc comment in
  // shared-types).
  const [subMinuteBarsBySymbol, setSubMinuteBarsBySymbol] = useState<Map<string, BarUpdate[]>>(new Map());
  // Dedicated latest-per-symbol map for catalysts too, matching the web
  // app's own catalystsBySymbol state (apps/client/src/lib/
  // useRealtimeFeed.ts) rather than deriving it from the shared `events`
  // list the way this file used to. catalyst_update fires once per symbol
  // at promotion time -- rare, but the shared list is dominated by
  // halt_warning's per-trade volume, so on a long enough session a real
  // catalyst would eventually get evicted the same way a Funnel/momentum
  // signal can (see buildFocusRows's own doc comment in derive.ts for the
  // demonstrated version of this exact class of bug). catalyst_update is
  // excluded from `events` below in favor of this map, same as web.
  const [catalystsBySymbol, setCatalystsBySymbol] = useState<Map<string, CatalystUpdate>>(new Map());
  // Dedicated latest-per-symbol map for momentum too, matching the web
  // app's own momentumBySymbol state (apps/client/src/lib/
  // useRealtimeFeed.ts): momentum_update fires once per bar per symbol,
  // the same flooding risk already found and fixed for bars/catalysts
  // above -- the shared `events` ring buffer is dominated by
  // halt_warning's per-trade volume, so a symbol's momentum reading would
  // get flushed out within seconds of real trading activity otherwise.
  // Feeds the new mobile MomentumScoreRow port (see momentumLabel.ts/
  // momentumNarrative.ts) the same way barsBySymbol feeds the chart.
  const [momentumBySymbol, setMomentumBySymbol] = useState<Map<string, MomentumUpdate>>(new Map());
  // Dedicated latest-per-symbol map for the funnel too (2026-09-03) --
  // buildFocusRows used to derive this from the shared `events` list
  // exactly the way it derived momentum before the fix above, and hit
  // the identical real bug: confirmed live (Roman: "It's not displaying
  // any stocks... when it does, they just last for a few seconds") --
  // funnel_signal is rare enough relative to halt_warning's per-trade
  // volume that it was getting evicted from the shared ring buffer
  // within seconds of real trading activity. This was the "demonstrated
  // version" the momentumBySymbol comment above already referenced but
  // hadn't actually been fixed for funnel_signal itself yet.
  const [funnelBySymbol, setFunnelBySymbol] = useState<Map<string, FunnelSignal>>(new Map());
  const [micropullbackEvents, setMicropullbackEvents] = useState<ConsolidationEvent[]>([]);
  useEffect(() => { let disposed = false; let socket: WebSocket | null = null;
    const connect = () => { if (disposed) return; setStatus("connecting"); socket = new WebSocket(WS_URL);
      socket.addEventListener("open", () => socket?.send(JSON.stringify({ type: "hello", protocolVersion: WS_PROTOCOL_VERSION, client: "mobile" })));
      socket.addEventListener("message", (raw) => { let message: RealtimeMessage; try { message = JSON.parse(String(raw.data)) as RealtimeMessage; } catch { return; }
        if (message.type === "welcome") { setStatus("open"); return; } if (message.type === "hello_rejected") { setStatus("closed"); socket?.close(); return; } if (message.type === "ping") { socket?.send(JSON.stringify({ type: "pong", at: message.at })); return; } if (message.type === "hello" || message.type === "pong") return;
        if (message.type === "bar_update") {
          // ws-server now live-updates the CURRENT, still-forming bucket
          // from raw trade ticks (throttled ~2/sec) instead of only
          // sending a bar once the bucket closes, so the chart's last
          // candle actually grows in real time instead of snapping into
          // existence once a minute. Multiple messages can share the same
          // `timestamp` (the bucket's own start) as that candle grows --
          // replace the last entry in place when that happens rather than
          // appending every one, same fix as the web app's own
          // useRealtimeFeed.ts (see its comment): appending would blow
          // through MAX_BARS_PER_SYMBOL's cap much faster than it's
          // actually sized for. Routed by intervalSecs (2026-09-03) into
          // either the 1-minute map or the new sub-minute one -- see
          // subMinuteBarsBySymbol's own doc comment for why this can't be
          // skipped.
          const setter = message.intervalSecs === 30 ? setSubMinuteBarsBySymbol : setBarsBySymbol;
          setter((prev) => { const existing = prev.get(message.symbol) ?? []; const last = existing[existing.length - 1];
            const next = last && last.timestamp === message.timestamp ? [...existing.slice(0, -1), message] : [...existing, message];
            const trimmed = next.length > MAX_BARS_PER_SYMBOL ? next.slice(next.length - MAX_BARS_PER_SYMBOL) : next;
            const copy = new Map(prev); copy.set(message.symbol, trimmed); return copy; });
          return;
        }
        if (message.type === "momentum_update") {
          setMomentumBySymbol((prev) => { const copy = new Map(prev); copy.set(message.symbol, message); return copy; });
          // Used to also fall through into the shared `events` list here
          // (no early return) for buildFocusRows' own sake -- no longer
          // needed now that buildFocusRows reads momentumBySymbol
          // directly, and keeping the fall-through was exactly the real
          // bug (see funnelBySymbol's own comment above).
          return;
        }
        if (message.type === "funnel_signal") {
          setFunnelBySymbol((prev) => { const copy = new Map(prev); copy.set(message.symbol, message); return copy; });
          return;
        }
        if (message.type === "catalyst_update") {
          setCatalystsBySymbol((prev) => { const copy = new Map(prev); copy.set(message.symbol, message); return copy; });
          return;
        }
        // Captured into its own dedicated list (not a `return` -- still
        // falls through into the shared `events` list below too, since
        // buildAlerts()/derive.ts already reads consolidation_event from
        // there for the Alerts tab's own feed row; this is an ADDITIONAL
        // consumer, not a replacement).
        if (message.type === "consolidation_event" && message.kind === "entry_triggered" && message.strategy === "micropullback") {
          setMicropullbackEvents((prev) => [message, ...prev].slice(0, MAX_MICROPULLBACK_EVENTS));
        }
        setEvents((current) => [message, ...current].slice(0, MAX_EVENTS)); } );
      const reconnect = () => { if (disposed) return; setStatus("closed"); if (retryRef.current) clearTimeout(retryRef.current); retryRef.current = setTimeout(connect, RECONNECT_MS); };
      socket.addEventListener("close", reconnect); socket.addEventListener("error", () => socket?.close()); };
    connect(); return () => { disposed = true; if (retryRef.current) clearTimeout(retryRef.current); socket?.close(); }; }, []);

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
    fetch(`${HTTP_URL}/catalysts/today`).then((r) => { if (!r.ok) throw new Error(`catalysts backfill failed: ${r.status}`); return r.json() as Promise<CatalystBackfillRow[]>; })
      .then((rows) => { if (disposed || rows.length === 0) return;
        setCatalystsBySymbol((prev) => { const copy = new Map(prev); for (const row of rows) { if (copy.has(row.symbol)) continue; copy.set(row.symbol, { type: "catalyst_update", ...row }); } return copy; }); })
      .catch(() => { /* best-effort -- the live socket still populates catalysts for anything promoted from here on */ });
    return () => { disposed = true; }; }, []);

  return { status, events, barsBySymbol, subMinuteBarsBySymbol, momentumBySymbol, catalystsBySymbol, funnelBySymbol, micropullbackEvents };
}
