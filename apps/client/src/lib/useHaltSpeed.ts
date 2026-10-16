// Halt Early-Warning as a speed meter (2026-10-16) -- the web/desktop
// half. One SpeedTracker per page session (shared-types/haltSpeed.ts, the
// same module the phone uses, so all three surfaces show identical
// numbers) is fed the latest halt reading per symbol on every events
// change. It keeps the sampled 10-minute history behind each card's
// sparkline, "moving N min" and "+N pts / 1m", and decides speed alerts.
//
// Delivery copies useIgnitionAlerts.ts rather than inventing a fourth
// alert mechanism: an in-app toast (SpeedAlertToast.tsx), a browser
// Notification when permission is already granted ("Unproven signal, not
// advice.", same as ignition), and playChime -- here only when the user's
// Sound setting is on. Bursts coalesce over one second and notifications
// are rate-limited to one per toast lifetime, exactly as ignition does.
//
// Only alerts the tracker raises live reach the user: its first reading
// per symbol is a baseline, never an alert, so the opening snapshot (and a
// reconnect's replay) cannot fire a burst. The age guard below is a second
// fence for a reading that arrives late.
//
// Mute is per symbol for 30 minutes and is delivery-only: a muted symbol's
// alert still lands in "Recent alerts" (the outcomes there are the data
// that will tune these unmeasured thresholds), it just does not pop up,
// notify or chime.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { SpeedTracker, isBullishReading, speedRows, type HaltWarning, type SpeedAlert, type SpeedDirection } from "@stockspotter/shared-types";
import { playChime } from "./useMicropullbackAlerts";
import {
  alertDetail,
  isMuted,
  isNewSymbol,
  muteSymbol as addMute,
  readSpeedDirection,
  readSpeedSettings,
  saveSpeedDirection,
  saveSpeedSettings,
  trackerSettings,
  type ClientSpeedSettings,
  type SpeedMutes,
} from "./haltSpeedView";

export const SPEED_TOAST_DURATION_MS = 12_000;
export const MAX_VISIBLE_SPEED_TOASTS = 3;
/** A reading older than this when its alert would be shown is history,
 * not a live alert. */
export const SPEED_ALERT_MAX_AGE_MS = 120_000;

export interface SpeedToastEntry extends SpeedAlert {
  bullish: boolean;
}

/** Mutable per-session state. Held in useState (lazy, never replaced)
 * rather than refs so render can read the tracker without tripping the
 * refs-during-render rule; nothing here is React state, it is the
 * external store the memo below feeds. */
interface SpeedSession {
  tracker: SpeedTracker;
  mutes: SpeedMutes;
  pending: SpeedAlert[];
}

function createSession(): SpeedSession {
  return { tracker: new SpeedTracker(20), mutes: new Map(), pending: [] };
}

function storage(): Storage | null {
  try { return localStorage; } catch { return null; }
}

export function useHaltSpeed(readings: HaltWarning[]) {
  const [session] = useState(createSession);
  const [settings, setSettingsState] = useState<ClientSpeedSettings>(() => readSpeedSettings(storage()));
  const [direction, setDirectionState] = useState<SpeedDirection>(() => readSpeedDirection(storage()));
  const decide = useMemo(() => trackerSettings(settings), [settings]);

  // Observing here rather than in an effect means the rows below already
  // include this reading's point, without a second render per frame.
  // Re-observing the same reading is a no-op in the tracker, so a
  // StrictMode double render or a settings change cannot double-count.
  const observed = useMemo(() => {
    let raised = 0;
    for (const reading of readings) {
      const alert = session.tracker.observe(reading, decide);
      if (alert) { session.pending.push(alert); raised += 1; }
    }
    return { raised };
  }, [readings, decide, session]);

  const rows = useMemo(() => {
    void observed;
    return speedRows(readings, session.tracker, direction);
  }, [readings, direction, session, observed]);
  const counts = useMemo(() => {
    let up = 0;
    for (const reading of readings) if (isBullishReading(reading)) up += 1;
    return { up, down: readings.length - up, all: readings.length };
  }, [readings]);
  // recentAlerts() returns a new array only when an alert is raised.
  const recentAlerts = useMemo(() => {
    void observed;
    return session.tracker.recentAlerts();
  }, [session, observed]);

  const setSettings = useCallback((patch: Partial<ClientSpeedSettings>) => {
    setSettingsState((prev) => {
      const next = { ...prev, ...patch };
      saveSpeedSettings(storage(), next);
      return next;
    });
  }, []);
  const setDirection = useCallback((next: SpeedDirection) => {
    setDirectionState(next);
    saveSpeedDirection(storage(), next);
  }, []);
  // Wall clock of the first commit that had readings: the opening
  // snapshot. Set in an effect (not during render) and read only when a
  // card asks, so the first render simply shows no NEW tags.
  const sessionStart = useRef<number | null>(null);
  useEffect(() => {
    if (sessionStart.current === null && readings.length) sessionStart.current = Date.now();
  }, [readings]);
  const isNew = useCallback((symbol: string, now: number) => isNewSymbol(session.tracker.firstSeenAt(symbol), sessionStart.current, now), [session]);

  // ---- delivery (toast, notification, chime) -------------------------
  const settingsRef = useRef(settings);
  useEffect(() => { settingsRef.current = settings; }, [settings]);
  const queue = useRef<SpeedAlert[]>([]);
  const notices = useRef<SpeedAlert[]>([]);
  const flushTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const expiryTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const noticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const lastNotice = useRef(0);
  const visibleCount = useRef(0);
  const [toasts, setToasts] = useState<SpeedToastEntry[]>([]);
  const [overflow, setOverflow] = useState(0);

  useEffect(() => () => {
    // StrictMode replays effects: clear the timers but keep the queues,
    // so the replayed effect below reschedules what was not yet shown.
    for (const timer of [flushTimer, expiryTimer, noticeTimer]) {
      if (timer.current) clearTimeout(timer.current);
      timer.current = null;
    }
  }, []);

  useEffect(() => {
    void observed;
    queue.current.push(...session.pending.splice(0));
    if (!queue.current.length || flushTimer.current) return;
    flushTimer.current = setTimeout(() => {
      flushTimer.current = null;
      const now = Date.now();
      const batch = queue.current.splice(0).filter((alert) => now - alert.at <= SPEED_ALERT_MAX_AGE_MS && !isMuted(session.mutes, alert.symbol, now));
      if (!batch.length) return;
      // Newest first, matching the tracker's own order.
      const entries = batch.map((alert) => ({ ...alert, bullish: alert.movePct === null || alert.movePct >= 0 })).reverse();
      const displaced = Math.max(0, visibleCount.current + entries.length - MAX_VISIBLE_SPEED_TOASTS);
      setOverflow((n) => n + displaced);
      visibleCount.current = Math.min(MAX_VISIBLE_SPEED_TOASTS, visibleCount.current + entries.length);
      setToasts((prev) => [...entries, ...prev].slice(0, MAX_VISIBLE_SPEED_TOASTS));
      if (expiryTimer.current) clearTimeout(expiryTimer.current);
      expiryTimer.current = setTimeout(() => {
        setToasts([]);
        setOverflow(0);
        visibleCount.current = 0;
        expiryTimer.current = null;
      }, SPEED_TOAST_DURATION_MS);
      notices.current.push(...batch);
      const emitNotice = () => {
        noticeTimer.current = null;
        const at = Date.now();
        const grouped = notices.current.splice(0).filter((alert) => at - alert.at <= SPEED_ALERT_MAX_AGE_MS && !isMuted(session.mutes, alert.symbol, at));
        if (!grouped.length) return;
        lastNotice.current = at;
        if (settingsRef.current.sound) playChime();
        if ("Notification" in window && Notification.permission === "granted") fireNotification(grouped);
      };
      const delay = Math.max(0, SPEED_TOAST_DURATION_MS - (now - lastNotice.current));
      if (!noticeTimer.current) {
        if (delay === 0) emitNotice();
        else noticeTimer.current = setTimeout(emitNotice, delay);
      }
    }, 1000);
  }, [observed, session]);

  const dismissToast = useCallback((id: string) => {
    setToasts((prev) => {
      const next = prev.filter((t) => t.id !== id);
      visibleCount.current = next.length;
      return next;
    });
  }, []);

  const muteSymbol = useCallback((symbol: string) => {
    addMute(session.mutes, symbol, Date.now());
    notices.current = notices.current.filter((alert) => alert.symbol !== symbol);
    setToasts((prev) => {
      const next = prev.filter((t) => t.symbol !== symbol);
      visibleCount.current = next.length;
      return next;
    });
  }, [session]);

  return { settings, setSettings, direction, setDirection, rows, counts, recentAlerts, isNew, toasts, overflow, dismissToast, muteSymbol };
}

export type HaltSpeedState = ReturnType<typeof useHaltSpeed>;

function fireNotification(alerts: SpeedAlert[]) {
  const alert = alerts[alerts.length - 1];
  try {
    new Notification(alerts.length === 1 ? `${alert.symbol} moving fast: gauge ${alert.pct}%` : `${alerts.length} fast movers`, {
      body: alerts.length === 1
        ? `$${alert.price.toFixed(alert.price < 1 ? 4 : 2)} · ${alertDetail(alert)}. Unproven signal, not advice.`
        : `${alerts.slice(-3).map((a) => a.symbol).join(", ")} — see Halt Early-Warning. Unproven signal, not advice.`,
      tag: "halt-speed-summary",
    });
  } catch {
    // Same expected failure as ignition: some embedded WebViews (Tauri on
    // some platforms) expose the constructor but do not implement it. The
    // toast and chime have already fired.
  }
}
