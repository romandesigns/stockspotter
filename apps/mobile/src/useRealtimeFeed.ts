import { useEffect, useRef, useState } from "react";
import type { CatalystUpdate, RealtimeMessage } from "@stockspotter/shared-types";
import { WS_PROTOCOL_VERSION } from "@stockspotter/shared-types";
import { HTTP_URL, WS_URL } from "./config";
import type { DetectionEvent, FeedStatus } from "./types";
const MAX_EVENTS = 500; const RECONNECT_MS = 3_000;

// Wire shape of ws-server's GET /catalysts/today rows -- same fields as
// CatalystUpdate minus the WS envelope's own `type` discriminant (a plain
// REST array, not a tagged union member). Same real endpoint the web app
// (apps/client) backfills from.
interface CatalystBackfillRow { symbol: string; timestamp: string; catalystTags: string[]; headlineCount: number; mostRecentHeadline: string | null; }

export function useRealtimeFeed(): { status: FeedStatus; events: DetectionEvent[] } {
  const [status, setStatus] = useState<FeedStatus>("connecting"); const [events, setEvents] = useState<DetectionEvent[]>([]); const retryRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => { let disposed = false; let socket: WebSocket | null = null;
    const connect = () => { if (disposed) return; setStatus("connecting"); socket = new WebSocket(WS_URL);
      socket.addEventListener("open", () => socket?.send(JSON.stringify({ type: "hello", protocolVersion: WS_PROTOCOL_VERSION, client: "mobile" })));
      socket.addEventListener("message", (raw) => { let message: RealtimeMessage; try { message = JSON.parse(String(raw.data)) as RealtimeMessage; } catch { return; }
        if (message.type === "welcome") { setStatus("open"); return; } if (message.type === "hello_rejected") { setStatus("closed"); socket?.close(); return; } if (message.type === "ping") { socket?.send(JSON.stringify({ type: "pong", at: message.at })); return; } if (message.type === "hello" || message.type === "pong") return;
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
        setEvents((current) => { const known = new Set(current.filter((e): e is CatalystUpdate => e.type === "catalyst_update").map((e) => e.symbol));
          const fresh: CatalystUpdate[] = rows.filter((row) => !known.has(row.symbol)).map((row) => ({ type: "catalyst_update", ...row }));
          return fresh.length === 0 ? current : [...current, ...fresh].slice(0, MAX_EVENTS); }); })
      .catch(() => { /* best-effort -- the live socket still populates catalysts for anything promoted from here on */ });
    return () => { disposed = true; }; }, []);

  return { status, events };
}
