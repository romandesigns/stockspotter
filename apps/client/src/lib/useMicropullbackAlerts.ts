// Real cross-symbol "grab my attention" mechanism for a high-confidence
// micropullback formation (2026-09-03, Roman's own ask) -- web/desktop
// counterpart to apps/mobile/src/useMicropullbackAlerts.ts (same real
// confidence gate, same reasoning for why it's a proxy off the existing
// momentum score rather than a new backend field -- see that file's own
// header comment for the full story, not re-derived here).
//
// Fresh confirmed detections are coalesced across replay frames. Visual
// delivery is capped independently of backend/panel events; sound and OS
// notifications are suppressed between bursts. Permission is still requested
// lazily, with one in-flight prompt and stale-delivery protection.
import { useCallback, useEffect, useRef, useState } from "react";
import type { ConsolidationEvent, MomentumUpdate } from "@stockspotter/shared-types";
import { isFreshMicropullback, MAX_VISIBLE_MICROPULLBACK_TOASTS, micropullbackKey, reduceMicropullbackDelivery } from "./micropullbackDelivery";

export interface MicropullbackToastEntry {
  id: string;
  symbol: string;
  price: number;
}

const TOAST_DURATION_MS = 8000;

// Exported (2026-09-04) so useIgnitionAlerts.ts can reuse the exact same
// chime rather than a second copy of the same few lines of Web Audio API
// code -- no reason this needs to be one micropullback-specific sound
// vs. another "something happened" alert's own identical need.
export function playChime() {
  try {
    const Ctx = window.AudioContext || (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
    if (!Ctx) return;
    const ctx = new Ctx();
    const now = ctx.currentTime;
    // Two quick ascending tones -- deliberately short and simple, not
    // meant to be a full "sound design" pass, just a real, distinct
    // "something happened" cue.
    [660, 880].forEach((freq, i) => {
      const osc = ctx.createOscillator();
      const gain = ctx.createGain();
      osc.type = "sine";
      osc.frequency.value = freq;
      gain.gain.setValueAtTime(0.0001, now + i * 0.12);
      gain.gain.exponentialRampToValueAtTime(0.15, now + i * 0.12 + 0.01);
      gain.gain.exponentialRampToValueAtTime(0.0001, now + i * 0.12 + 0.15);
      osc.connect(gain);
      gain.connect(ctx.destination);
      osc.start(now + i * 0.12);
      osc.stop(now + i * 0.12 + 0.16);
    });
    setTimeout(() => ctx.close().catch(() => {}), 500);
  } catch {
    // Real, expected failure modes: no AudioContext support, autoplay
    // policy blocking sound before any user gesture on this page yet --
    // neither is worth surfacing, the toast/notification still fire.
  }
}

export function useMicropullbackAlerts(events: ConsolidationEvent[], momentumBySymbol: Map<string, MomentumUpdate>) {
  const seen = useRef(new Map<string, number>());
  const pending = useRef(new Map<string, ConsolidationEvent>());
  const visible = useRef<MicropullbackToastEntry[]>([]);
  const flushTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const expiryTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const permissionTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const expiresAt = useRef(0);
  const lastNotice = useRef(-Infinity);
  const mounted = useRef(false);
  const permissionPending = useRef(false);
  const permissionToken = useRef(0);
  const [toasts, setToasts] = useState<MicropullbackToastEntry[]>([]);
  const [overflow, setOverflow] = useState(0);

  const clearVisible = useCallback(() => {
    if (expiryTimer.current !== null) clearTimeout(expiryTimer.current);
    visible.current = [];
    expiresAt.current = 0;
    expiryTimer.current = null;
    setToasts([]);
    setOverflow(0);
  }, []);

  const armExpiry = useCallback(() => {
    if (expiryTimer.current !== null) clearTimeout(expiryTimer.current);
    expiryTimer.current = null;
    if (expiresAt.current === 0) return;
    expiryTimer.current = setTimeout(clearVisible, Math.max(0, expiresAt.current - performance.now()));
  }, [clearVisible]);

  const emitNotice = useCallback((batch: ConsolidationEvent[]) => {
    const now = performance.now();
    // Suppress rather than defer repeated sound/OS bursts, so a late chime
    // cannot arrive after the associated toast has expired.
    if (now - lastNotice.current < TOAST_DURATION_MS) return;
    lastNotice.current = now;
    playChime();
    if (!("Notification" in window)) return;
    try {
      if (Notification.permission === "granted") fireNotification(batch);
      else if (Notification.permission === "default" && !permissionPending.current) {
        permissionPending.current = true;
        const token = ++permissionToken.current;
        // Cache the in-flight request only. A dismissed prompt may never settle.
        permissionTimer.current = setTimeout(() => {
          permissionPending.current = false;
          permissionToken.current++;
          permissionTimer.current = null;
        }, 30_000);
        const finish = () => {
          if (token !== permissionToken.current) return;
          permissionPending.current = false;
          if (permissionTimer.current !== null) clearTimeout(permissionTimer.current);
          permissionTimer.current = null;
        };
        Notification.requestPermission().then((permission) => {
          if (!mounted.current || token !== permissionToken.current) return;
          const fresh = batch.filter((event) => isFreshMicropullback(event, Date.now()));
          if (permission === "granted" && Notification.permission === "granted" && fresh.length) fireNotification(fresh);
        }).catch(() => {}).finally(finish);
      }
    } catch {
      permissionPending.current = false;
      if (permissionTimer.current !== null) clearTimeout(permissionTimer.current);
      permissionTimer.current = null;
    }
  }, []);

  const flush = useCallback(() => {
    flushTimer.current = null;
    if (!mounted.current) return;
    const now = Date.now();
    const batch = [...pending.current.values()].filter((event) => isFreshMicropullback(event, now))
      .sort((a, b) => Date.parse(b.timestamp) - Date.parse(a.timestamp));
    pending.current.clear();
    if (!batch.length) return;
    const entries = batch.map((event) => ({ id: micropullbackKey(event), symbol: event.symbol, price: event.price }));
    visible.current = [...entries, ...visible.current].slice(0, MAX_VISIBLE_MICROPULLBACK_TOASTS);
    setToasts(visible.current);
    // Count only the last coalesced batch, not a claim about durable history.
    setOverflow(Math.max(0, entries.length - MAX_VISIBLE_MICROPULLBACK_TOASTS));
    expiresAt.current = performance.now() + TOAST_DURATION_MS;
    armExpiry();
    emitNotice(batch);
  }, [armExpiry, emitNotice]);

  const scheduleFlush = useCallback(() => {
    if (pending.current.size && flushTimer.current === null) flushTimer.current = setTimeout(flush, 1000);
  }, [flush]);

  const invalidatePermission = useCallback(() => {
    permissionToken.current++;
    permissionPending.current = false;
  }, []);

  useEffect(() => {
    mounted.current = true;
    scheduleFlush();
    armExpiry();
    const onVisibility = () => { if (!document.hidden) armExpiry(); };
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      mounted.current = false;
      for (const timer of [flushTimer, expiryTimer, permissionTimer]) {
        if (timer.current !== null) clearTimeout(timer.current);
        timer.current = null;
      }
      invalidatePermission();
      document.removeEventListener("visibilitychange", onVisibility);
      // StrictMode cancels timers, not queued delivery. Setup re-arms it;
      // emitted events stay seen and are never repeated.
    };
  }, [armExpiry, scheduleFlush, invalidatePermission]);

  useEffect(() => {
    const now = Date.now();
    const next = reduceMicropullbackDelivery(seen.current, events, momentumBySymbol, now);
    seen.current = next.seen;
    for (const event of next.fresh) pending.current.set(micropullbackKey(event), event);
    for (const [key, event] of pending.current) if (!isFreshMicropullback(event, now)) pending.current.delete(key);
    scheduleFlush();
  }, [events, momentumBySymbol, scheduleFlush]);

  function dismissToast(id: string) {
    visible.current = visible.current.filter((t) => t.id !== id);
    if (visible.current.length === 0) {
      clearVisible();
      return;
    }
    setToasts(visible.current);
  }
  return { toasts, overflow, dismissToast };
}

function fireNotification(events: ConsolidationEvent[]) {
  const event = events[0];
  try {
    // renotify is supported by Chromium but omitted from the cross-browser
    // TypeScript DOM dictionary. Other browsers can ignore this option.
    const options: NotificationOptions & { renotify: boolean } = {
      body: events.length === 1 ? `Real momentum confirms the move — $${event.price.toFixed(event.price < 1 ? 4 : 2)}` : `${events.slice(0, 3).map((e) => e.symbol).join(", ")} — recent confirmed momentum`,
      tag: "micropullback-summary",
      renotify: true,
    };
    new Notification(events.length === 1 ? `${event.symbol} micropullback forming` : `${events.length} micropullback detections`, options);
  } catch {
    // Embedded WebViews may expose Notification but reject construction.
    // Visual delivery and sound are independent of OS notification support.
  }
}
