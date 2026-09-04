// Real cross-symbol "grab my attention" mechanism for a high-confidence
// micropullback formation (2026-09-03, Roman's own ask: "what is the
// mechanism in place to quickly alert me... when there's about to be the
// formation of a micropullback with a high level of confidence about the
// stock continuing moving upwards?"). Honest finding before building
// this: there was no such mechanism at all -- a micropullback
// EntryTriggered event only ever showed up as a passive "MPB" badge in
// the Alerts tab feed, nothing pushed it at the user or made a sound.
//
// "High confidence" is a real, defensible proxy, not a new backend
// score: consolidation_breakout's own EntryTriggered event is purely
// binary (fired or didn't, confirmed by reading the monitor's source --
// no strength/confidence field exists on the wire at all). Rather than
// invent one, this reuses the SAME real momentum-score gate the rest of
// this app already treats as "confirmed" (catalystConfirmation() in
// derive.ts, `overall >= FACTOR_GOOD_THRESHOLD && volumeConfirmation >=
// FACTOR_GOOD_THRESHOLD`) -- only alerts when that symbol's own latest
// known momentum reading clears the same bar, at the moment the
// micropullback fires.
//
// Notification delivery reuses expo-notifications' already-granted OS
// permission (confirmed global per-app, not per-category -- once
// granted for price alerts, or by this hook itself, it covers both) but
// gets its OWN, distinctly-named Android channel so Roman can mute one
// category without the other in system settings. Deliberately does NOT
// build a separate custom in-app toast on top of this: usePriceAlerts.ts's
// existing setNotificationHandler already sets shouldShowBanner:true /
// shouldPlaySound:true, meaning the same scheduled notification already
// shows a banner AND plays sound even while the app is foregrounded --
// one real mechanism already covers both "push when away" and "grab
// attention while in-app" here, building a redundant second UI for the
// identical event would be re-deriving something that already works.
import { useEffect, useRef } from "react";
import * as Notifications from "expo-notifications";
import type { ConsolidationEvent, MomentumUpdate } from "@stockspotter/shared-types";
import { catalystConfirmation } from "./derive";

let androidChannelReady = false;
async function ensureMicropullbackChannel() {
  if (androidChannelReady) return;
  androidChannelReady = true;
  await Notifications.setNotificationChannelAsync("micropullback-alerts", {
    name: "Micropullback alerts",
    importance: Notifications.AndroidImportance.HIGH,
    sound: "default",
  }).catch(() => {});
}

export function useMicropullbackAlerts(events: ConsolidationEvent[], momentumBySymbol: Map<string, MomentumUpdate>) {
  // Edge-detected via a Set of already-alerted event keys (symbol +
  // timestamp, unique per real detection) so a re-render never
  // double-fires for the same real event -- same real shape
  // usePriceAlerts.ts's own single-fire-then-disable logic protects
  // against, applied here since this list only ever grows, never mutates
  // an existing entry.
  const alertedRef = useRef<Set<string>>(new Set());

  useEffect(() => {
    for (const event of events) {
      const key = `${event.symbol}-${event.timestamp}`;
      if (alertedRef.current.has(key)) continue;
      alertedRef.current.add(key);

      const confirmation = catalystConfirmation(momentumBySymbol.get(event.symbol));
      if (confirmation !== "confirmed") continue; // real gate -- see this file's own header comment

      ensureMicropullbackChannel();
      Notifications.requestPermissionsAsync().then(({ status }) => {
        if (status !== "granted") return;
        Notifications.scheduleNotificationAsync({
          content: {
            title: `${event.symbol} micropullback forming`,
            body: `Real momentum confirms the move — $${event.price.toFixed(event.price < 1 ? 4 : 2)}. Tap to open the chart.`,
            data: { symbol: event.symbol },
            sound: "default",
          },
          trigger: null, // fire immediately -- this already IS the trigger condition
        }).catch(() => {});
      });
    }
    // Only the newest event needs checking each time in practice (events
    // arrives newest-first, capped small) -- iterating the whole list is
    // cheap at this size and correctly catches anything alertedRef
    // hasn't seen yet even across a fast-arriving batch.
  }, [events, momentumBySymbol]);
}
