// Real cross-symbol "grab my attention" mechanism for a confirmed
// ignition (2026-09-04, found live: Roman missed a real 20%+ ONCO move
// -- ignition-detector confirmed it multiple times, but nothing ever
// surfaced it. The only two alert mechanisms that existed before this
// were a manual per-symbol price target (usePriceAlerts.ts) and
// useMicropullbackAlerts.ts's own Micropullback-only trigger -- neither
// covers "any symbol just had a real, evidence-backed ignition confirm",
// which is exactly what was missed. Web/desktop counterpart to
// apps/mobile/src/useIgnitionAlerts.ts (same real trigger, same real
// per-symbol cooldown reasoning -- see that file's own header comment
// for the full story, not re-derived here).
//
// Unlike useMicropullbackAlerts.ts, this does NOT layer a momentum-score
// gate on top of the trigger event -- that gate was a proxy for
// confidence there because consolidation-breakout/micropullback's own
// EntryTriggered event had no independently-backtested hit rate at the
// time that file was written. ignition_event's own follow_through_
// confirmed ALREADY has strong, direct live evidence (32-35% hit rate
// across 10,000+ real signals, this project's single largest sample) --
// adding a second, unrelated momentum-score filter on top would have
// silently dropped the exact real move that prompted this fix (ONCO's
// own momentum reading was 0.56, just under the 0.6 bar
// useMicropullbackAlerts.ts's gate uses).
//
// Same three real pieces as useMicropullbackAlerts.ts, same reasoning
// for all three: (1) a browser Notification, (2) an in-app toast
// (IgnitionAlertToast.tsx), (3) a short synthesized chime -- reuses that
// file's own playChime rather than a second copy of the same few lines
// of Web Audio API code.
import { useEffect, useRef, useState } from "react";
import type { IgnitionEvent } from "@stockspotter/shared-types";
import { playChime } from "./useMicropullbackAlerts";
import { ALERT_MAX_AGE_MS, collectFreshIgnitions, createIgnitionDeliveryState, MAX_VISIBLE_IGNITION_TOASTS } from "./ignitionDelivery";

export interface IgnitionAlertToastEntry {
  id: string;
  symbol: string;
  price: number;
}

const TOAST_DURATION_MS = 8000;
export function useIgnitionAlerts(events: IgnitionEvent[]) {
  const delivery = useRef(createIgnitionDeliveryState());
  const pending = useRef<IgnitionEvent[]>([]);
  const flushTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const expiryTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const noticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const notices = useRef<IgnitionEvent[]>([]);
  const lastNotice = useRef(0);
  const [toasts, setToasts] = useState<IgnitionAlertToastEntry[]>([]);
  const [overflow, setOverflow] = useState(0);
  const visibleCount = useRef(0);

  useEffect(() => () => {
    if (flushTimer.current) clearTimeout(flushTimer.current);
    if (expiryTimer.current) clearTimeout(expiryTimer.current);
    if (noticeTimer.current) clearTimeout(noticeTimer.current);
    // StrictMode may cancel the first scheduled delivery and replay effects.
    // Undelivered entries must remain eligible on that replay.
    for (const event of pending.current) {
      delivery.current.seen.delete(`${event.symbol}-${event.timestamp}`);
      if (delivery.current.lastAlerted.get(event.symbol) === Date.parse(event.timestamp)) delivery.current.lastAlerted.delete(event.symbol);
    }
    pending.current = [];
    flushTimer.current = null;
    expiryTimer.current = null;
    noticeTimer.current = null;
    notices.current = [];
  }, []);

  useEffect(() => {
    pending.current.push(...collectFreshIgnitions(delivery.current, events, Date.now()));
    if (!pending.current.length || flushTimer.current) return;
    // Frames arrive singly during snapshot replay: coalesce across effect runs.
    flushTimer.current = setTimeout(() => {
      flushTimer.current = null;
      const batch = pending.current.splice(0).filter((event) => Date.now() - Date.parse(event.timestamp) <= ALERT_MAX_AGE_MS);
      if (!batch.length) return;
      const entries = batch.map((event) => ({ id: `${event.symbol}-${event.timestamp}`, symbol: event.symbol, price: event.price }));
      const displaced = Math.max(0, visibleCount.current + entries.length - MAX_VISIBLE_IGNITION_TOASTS);
      setOverflow((n) => n + displaced);
      visibleCount.current = Math.min(MAX_VISIBLE_IGNITION_TOASTS, visibleCount.current + entries.length);
      setToasts((prev) => [...entries.reverse(), ...prev].slice(0, MAX_VISIBLE_IGNITION_TOASTS));
      if (expiryTimer.current) clearTimeout(expiryTimer.current);
      expiryTimer.current = setTimeout(() => {
        setToasts([]);
        setOverflow(0);
        visibleCount.current = 0;
        expiryTimer.current = null;
      }, TOAST_DURATION_MS);
      notices.current.push(...batch);
      const emitNotice = () => {
        noticeTimer.current = null;
        const grouped = notices.current.splice(0).filter((event) => Date.now() - Date.parse(event.timestamp) <= ALERT_MAX_AGE_MS);
        if (!grouped.length) return;
        lastNotice.current = Date.now();
        playChime();
        if ("Notification" in window && Notification.permission === "granted") fireNotification(grouped);
      };
      const delay = Math.max(0, TOAST_DURATION_MS - (Date.now() - lastNotice.current));
      if (!noticeTimer.current) {
        if (delay === 0) emitNotice();
        else noticeTimer.current = setTimeout(emitNotice, delay);
      }
    }, 1000);
  }, [events]);

  function dismissToast(id: string) {
    visibleCount.current = Math.max(0, visibleCount.current - 1);
    setToasts((prev) => prev.filter((t) => t.id !== id));
  }

  return { toasts, overflow, dismissToast };
}

function fireNotification(events: IgnitionEvent[]) {
  const event = events[events.length - 1];
  try {
    new Notification(events.length === 1 ? `${event.symbol} ignition confirmed` : `${events.length} ignition confirmations`, {
      body: events.length === 1 ? `Real follow-through at $${event.price.toFixed(event.price < 1 ? 4 : 2)}` : `${events.slice(-3).map((e) => e.symbol).join(", ")} — see Ignition for all signals`,
      tag: "ignition-summary",
    });
  } catch {
    // Real, expected failure mode: some embedded WebViews (Tauri on
    // certain platforms) don't implement Notification even when the
    // constructor exists -- the toast + chime above still fired either way.
  }
}
