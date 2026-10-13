// Opt-in experimental research lane for mobile (IAC / Extreme Momentum).
//
// A SEPARATE authenticated socket from useRealtimeFeed's, on purpose and
// not as an accident of convenience:
//   - ws-server routes an `experimentalOnly` hello to its own handler
//     (handle_strategy_connection) backed by its own bounded connection
//     semaphore, so research backpressure cannot close or lag the normal
//     EventFrame socket this app's charts, alerts, and halt warnings all
//     ride on. Multiplexing the two onto one socket would hand that
//     property away.
//   - Nothing from this lane is allowed to reach a notification, an
//     order, a recording path, or the auto-trader. Keeping its state in
//     its own hook with its own types makes that structural rather than
//     a rule someone has to remember.
//
// Default OFF: EXPO_PUBLIC_EXPERIMENTAL_STRATEGIES must be exactly
// "true" (see apps/mobile/README.md for how to turn it on after
// validation). With the flag unset this hook opens no socket at all and
// the panels render nothing -- a normal build is byte-identical in
// behaviour to one without this feature.
import { useEffect, useRef, useState } from "react";
import { AppState } from "react-native";
import { WS_PROTOCOL_VERSION, getAccessKey } from "@stockspotter/shared-types";
import { WS_URL } from "./config";
import {
  decodeExperimentalMessage,
  reduceExperimentalRows,
  type ExperimentalStrategyStatus,
  type ExperimentalStrategyUpdate,
} from "./lib/experimentalStrategies";
import {
  EXPERIMENTAL_HEARTBEAT_MS,
  EXPERIMENTAL_RECONNECT_BASE_MS,
  EXPERIMENTAL_TRANSPORT_DEAD_MS,
  experimentalBackoff,
} from "./lib/experimentalView";
import type { FeedStatus } from "./types";

/** Build-time flag. `process.env.EXPO_PUBLIC_*` is inlined by Expo's own
 * env loader at bundle time, the same mechanism config.ts's WS_URL/
 * HTTP_URL already rely on -- so an unset flag is a literal `undefined`
 * in the shipped bundle, not a runtime lookup. */
export const EXPERIMENTAL_STRATEGIES_ENABLED = process.env.EXPO_PUBLIC_EXPERIMENTAL_STRATEGIES === "true";

export interface ExperimentalStrategiesFeed {
  enabled: boolean;
  rows: ExperimentalStrategyUpdate[];
  status: ExperimentalStrategyStatus | null;
  /** Sticky: once this socket is known to have missed observations, it
   * stays known. The server's retained snapshot restores current state
   * but cannot restore a lifecycle that came and went inside the gap,
   * same reasoning as the normal feed's own FeedGap. */
  gap: boolean;
  connection: FeedStatus;
}

export function useExperimentalStrategies(enabled = EXPERIMENTAL_STRATEGIES_ENABLED): ExperimentalStrategiesFeed {
  const [rows, setRows] = useState<ExperimentalStrategyUpdate[]>([]);
  const [status, setStatus] = useState<ExperimentalStrategyStatus | null>(null);
  const [gap, setGap] = useState(false);
  const [connection, setConnection] = useState<FeedStatus>("closed");
  // "background" only, never "inactive": on iOS, "inactive" fires for
  // transient, still-visible states (pulling down the notification
  // shade, an incoming call banner, the app switcher mid-gesture), so
  // tearing the socket down on it would reconnect-storm through normal
  // use. "background" is the state that actually means the OS has
  // stopped giving this app foreground time.
  const [foreground, setForeground] = useState(() => AppState.currentState !== "background");
  // Survives effect re-runs (a background round trip is one), unlike the
  // web hook's effect-local flag -- otherwise resuming from background
  // would look like a first connection and silently under-report the
  // observations missed while the phone was asleep.
  const connectedBefore = useRef(false);

  useEffect(() => {
    if (!enabled) return;
    const subscription = AppState.addEventListener("change", (next) => setForeground(next !== "background"));
    return () => subscription.remove();
  }, [enabled]);

  useEffect(() => {
    if (!enabled) return;
    // Backgrounded: the previous run's cleanup already closed the socket
    // and cleared both timers. Nothing is reopened until the app is
    // actually foregrounded again -- no heartbeat, no retry timer, and
    // no socket held open behind a locked screen.
    if (!foreground) { setConnection("closed"); return; }

    let stopped = false;
    let socket: WebSocket | null = null;
    let retry: ReturnType<typeof setTimeout> | undefined;
    let lastTransportAt = Date.now();
    let reconnectDelay = EXPERIMENTAL_RECONNECT_BASE_MS;

    const connect = () => {
      if (stopped) return;
      setConnection("connecting");
      const active = new WebSocket(WS_URL);
      socket = active;
      /** True once this particular socket is no longer the one this effect
       * run owns -- either the effect was cleaned up (`stopped`) or a later
       * `connect()` already replaced it.
       *
       * Every handler below checks this FIRST, before touching state, the
       * liveness clock, the backoff, or the wire. React Native delivers
       * socket events over the bridge asynchronously, so a socket that was
       * still CONNECTING when the app was backgrounded can hand JS an
       * `open`, a `message`, or a `close` that was already in flight --
       * after cleanup ran, and after a foreground effect re-run has opened
       * its replacement. Without this check those late events would send a
       * hello on a socket nobody owns, mark `connectedBefore` (so the next
       * real connection reports a gap that never happened), push rows and
       * status into the live view, flip `connection` to "open"/"closed"
       * under the current socket, or -- worst -- schedule a retry timer
       * that no cleanup is left to clear. */
      const retired = () => stopped || socket !== active;
      active.addEventListener("open", () => {
        if (retired()) return;
        if (connectedBefore.current) setGap(true);
        connectedBefore.current = true;
        lastTransportAt = Date.now();
        active.send(JSON.stringify({
          type: "hello",
          protocolVersion: WS_PROTOCOL_VERSION,
          client: "mobile",
          token: getAccessKey(),
          experimentalStrategies: true,
          experimentalOnly: true,
        }));
      });
      active.addEventListener("message", (raw) => {
        if (retired()) return;
        lastTransportAt = Date.now();
        let parsed: unknown;
        try { parsed = JSON.parse(String(raw.data)); } catch { setGap(true); return; }
        if (typeof parsed !== "object" || parsed === null || !("type" in parsed)) { setGap(true); return; }
        if (parsed.type === "welcome") { setConnection("open"); return; }
        if (parsed.type === "ping" && "at" in parsed) { active.send(JSON.stringify({ type: "pong", at: parsed.at })); return; }
        if (parsed.type === "pong") return;
        // Flags off server-side, an unsupported client, or the research
        // connection pool being full all arrive here. Close and let the
        // backoff decide when to ask again -- it must not hammer a
        // server that just said no.
        if (parsed.type === "hello_rejected") { setConnection("closed"); active.close(); return; }
        const message = decodeExperimentalMessage(parsed);
        // Anything this lane's own strict decoder rejects is a gap, not
        // a row to render loosely: an observation that cannot be fully
        // validated is an observation this panel does not have.
        if (!message) { setGap(true); return; }
        if (message.type === "experimental_strategy_status") {
          setStatus(message);
          if (message.healthy && (message.iacEnabled || message.extremeMomentumEnabled)) reconnectDelay = EXPERIMENTAL_RECONNECT_BASE_MS;
          return;
        }
        if (message.type === "experimental_strategy_gap") { setGap(true); return; }
        // Immutable per-lifecycle dedup: a replayed or out-of-order
        // snapshot can never overwrite a later one (reduceExperimentalRows).
        setRows((previous) => reduceExperimentalRows(previous, message));
      });
      active.addEventListener("close", () => {
        if (retired()) return;
        setConnection("closed");
        // Retire this socket before scheduling the retry, so a trailing
        // `error`/`open` from the same dead connection cannot re-enter any
        // of these handlers, and so the heartbeat stops looking at it.
        socket = null;
        const step = experimentalBackoff(reconnectDelay, Math.random());
        reconnectDelay = step.nextDelayMs;
        retry = setTimeout(connect, step.waitMs);
      });
      active.addEventListener("error", () => { if (retired()) return; active.close(); });
    };

    const heartbeat = setInterval(() => {
      if (socket?.readyState !== WebSocket.OPEN) return;
      if (Date.now() - lastTransportAt > EXPERIMENTAL_TRANSPORT_DEAD_MS) { socket.close(); return; }
      socket.send(JSON.stringify({ type: "ping", at: new Date().toISOString() }));
    }, EXPERIMENTAL_HEARTBEAT_MS);

    connect();
    return () => {
      stopped = true;
      clearInterval(heartbeat);
      if (retry) clearTimeout(retry);
      // Invalidate the current identity BEFORE closing, so the close this
      // very call provokes (and anything the bridge still delivers for
      // that socket afterwards) is already retired by the time any
      // handler sees it. `stopped` alone would cover it today; both are
      // kept because `retired()` is the single rule every handler reads,
      // and a future caller that tears down one socket without ending the
      // effect would otherwise have no way to express it.
      const captured = socket;
      socket = null;
      captured?.close();
    };
  }, [enabled, foreground]);

  return { enabled, rows, status, gap, connection };
}
