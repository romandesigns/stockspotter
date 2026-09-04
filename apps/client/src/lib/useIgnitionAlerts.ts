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

export interface IgnitionAlertToastEntry {
  id: string;
  symbol: string;
  price: number;
}

const TOAST_DURATION_MS = 8000;
const IGNITION_ALERT_COOLDOWN_MS = 15 * 60 * 1000; // 15 minutes -- see this file's own header comment

export function useIgnitionAlerts(events: IgnitionEvent[]) {
  const seenKeysRef = useRef<Set<string>>(new Set());
  const lastAlertedAtRef = useRef<Map<string, number>>(new Map());
  const [toasts, setToasts] = useState<IgnitionAlertToastEntry[]>([]);

  useEffect(() => {
    for (const event of events) {
      const key = `${event.symbol}-${event.timestamp}`;
      if (seenKeysRef.current.has(key)) continue;
      seenKeysRef.current.add(key);

      const eventMs = Date.parse(event.timestamp);
      const lastAlertedMs = lastAlertedAtRef.current.get(event.symbol);
      if (lastAlertedMs !== undefined && eventMs - lastAlertedMs < IGNITION_ALERT_COOLDOWN_MS) continue; // still cooling down for this symbol
      lastAlertedAtRef.current.set(event.symbol, eventMs);

      playChime();

      const id = `${key}-${Math.random().toString(36).slice(2)}`;
      setToasts((prev) => [...prev, { id, symbol: event.symbol, price: event.price }]);
      setTimeout(() => setToasts((prev) => prev.filter((t) => t.id !== id)), TOAST_DURATION_MS);

      if ("Notification" in window) {
        if (Notification.permission === "default") {
          Notification.requestPermission().then((permission) => {
            if (permission === "granted") fireNotification(event);
          });
        } else if (Notification.permission === "granted") {
          fireNotification(event);
        }
      }
    }
  }, [events]);

  function dismissToast(id: string) {
    setToasts((prev) => prev.filter((t) => t.id !== id));
  }

  return { toasts, dismissToast };
}

function fireNotification(event: IgnitionEvent) {
  try {
    new Notification(`${event.symbol} ignition confirmed`, {
      body: `Real follow-through at $${event.price.toFixed(event.price < 1 ? 4 : 2)}`,
      tag: `ignition-${event.symbol}`, // real dedup at the OS level too, not just this hook's own cooldown
    });
  } catch {
    // Real, expected failure mode: some embedded WebViews (Tauri on
    // certain platforms) don't implement Notification even when the
    // constructor exists -- the toast + chime above still fired either way.
  }
}
