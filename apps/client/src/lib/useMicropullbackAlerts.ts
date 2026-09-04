// Real cross-symbol "grab my attention" mechanism for a high-confidence
// micropullback formation (2026-09-03, Roman's own ask) -- web/desktop
// counterpart to apps/mobile/src/useMicropullbackAlerts.ts (same real
// confidence gate, same reasoning for why it's a proxy off the existing
// momentum score rather than a new backend field -- see that file's own
// header comment for the full story, not re-derived here).
//
// Fully new capability on web: confirmed zero existing Notification/
// audio-playback code anywhere in apps/client before this. Three real
// pieces, all firing together per qualifying event: (1) a browser
// Notification (lazily permission-requested on the FIRST real
// detection, not at page load, since an unprompted permission request
// on load is poor practice and often just gets auto-denied by the
// browser anyway); (2) an in-app toast (MicropullbackToast.tsx) --
// unlike mobile, a focused browser tab isn't guaranteed to show its own
// Notification as prominently, so this doesn't rely on that alone; (3) a
// short synthesized two-tone chime via the Web Audio API -- no bundled
// audio asset needed for one short beep, avoids a licensing/asset-
// management concern for something this small.
import { useEffect, useRef, useState } from "react";
import type { ConsolidationEvent, MomentumUpdate } from "@stockspotter/shared-types";
import { catalystConfirmation } from "./derive";

export interface MicropullbackToastEntry {
  id: string;
  symbol: string;
  price: number;
}

const TOAST_DURATION_MS = 8000;

function playChime() {
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
  const alertedRef = useRef<Set<string>>(new Set());
  const [toasts, setToasts] = useState<MicropullbackToastEntry[]>([]);

  useEffect(() => {
    for (const event of events) {
      const key = `${event.symbol}-${event.timestamp}`;
      if (alertedRef.current.has(key)) continue;
      alertedRef.current.add(key);

      const confirmation = catalystConfirmation(momentumBySymbol.get(event.symbol));
      if (confirmation !== "confirmed") continue; // real gate -- see this file's own header comment

      playChime();

      const id = `${key}-${Math.random().toString(36).slice(2)}`;
      setToasts((prev) => [...prev, { id, symbol: event.symbol, price: event.price }]);
      setTimeout(() => setToasts((prev) => prev.filter((t) => t.id !== id)), TOAST_DURATION_MS);

      if ("Notification" in window) {
        // Lazily requested on the first real qualifying detection, not
        // at page load -- an unprompted permission ask on load is poor
        // practice and the browser often just auto-denies it anyway.
        if (Notification.permission === "default") {
          Notification.requestPermission().then((permission) => {
            if (permission === "granted") fireNotification(event);
          });
        } else if (Notification.permission === "granted") {
          fireNotification(event);
        }
      }
    }
  }, [events, momentumBySymbol]);

  function dismissToast(id: string) {
    setToasts((prev) => prev.filter((t) => t.id !== id));
  }

  return { toasts, dismissToast };
}

function fireNotification(event: ConsolidationEvent) {
  try {
    new Notification(`${event.symbol} micropullback forming`, {
      body: `Real momentum confirms the move — $${event.price.toFixed(event.price < 1 ? 4 : 2)}`,
      tag: event.symbol, // real dedup at the OS level too, not just this hook's own Set
    });
  } catch {
    // Real, expected failure mode: some embedded WebViews (Tauri on
    // certain platforms) don't implement Notification even when the
    // constructor exists -- the toast + chime above still fired either way.
  }
}
