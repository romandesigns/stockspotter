// Real cross-symbol "grab my attention" mechanism for a confirmed
// ignition (2026-09-04, found live: Roman missed a real 20%+ ONCO move
// -- ignition-detector confirmed it multiple times, but nothing ever
// surfaced it. The only two alert mechanisms that existed before this
// were a manual per-symbol price target and useMicropullbackAlerts.ts's
// own Micropullback-only trigger -- neither covers "any symbol just had
// a real, evidence-backed ignition confirm", which is exactly what was
// missed.
//
// Unlike useMicropullbackAlerts.ts, this does NOT layer a momentum-score
// gate on top of the trigger event -- that gate was a proxy for
// confidence there because consolidation-breakout/micropullback's own
// EntryTriggered event had no independently-backtested hit rate at the
// time that file was written. ignition_event's own follow_through_
// confirmed ALREADY has strong, direct live evidence (32-35% hit rate
// across 10,000+ real signals, this project's single largest sample --
// see the auto-trader's own engine.rs for the same reasoning) -- adding
// a second, unrelated momentum-score filter on top would have silently
// dropped the exact real move that prompted this fix (ONCO's own
// momentum reading was 0.56, just under the 0.6 bar
// useMicropullbackAlerts.ts's gate uses).
//
// What this file adds instead of a confidence gate: a real per-symbol
// cooldown. ignition_event's raw stream is far too frequent to alert on
// every confirmation directly -- confirmed live, a single hot symbol
// fired follow_through_confirmed multiple times within 90 seconds, which
// would be a real notification-fatigue bug (and the fastest way for
// Roman to end up muting this channel entirely, defeating the whole
// point). One notification per symbol per cooldown window; a genuinely
// new ignition after the window re-alerts.
import { useEffect, useRef } from "react";
import * as Notifications from "expo-notifications";
import type { IgnitionEvent } from "@stockspotter/shared-types";

const IGNITION_ALERT_COOLDOWN_MS = 15 * 60 * 1000; // 15 minutes

let androidChannelReady = false;
async function ensureIgnitionChannel() {
  if (androidChannelReady) return;
  androidChannelReady = true;
  await Notifications.setNotificationChannelAsync("ignition-alerts", {
    name: "Ignition alerts",
    importance: Notifications.AndroidImportance.HIGH,
    sound: "default",
  }).catch(() => {});
}

export function useIgnitionAlerts(events: IgnitionEvent[]) {
  const seenKeysRef = useRef<Set<string>>(new Set());
  const lastAlertedAtRef = useRef<Map<string, number>>(new Map());

  useEffect(() => {
    for (const event of events) {
      const key = `${event.symbol}-${event.timestamp}`;
      if (seenKeysRef.current.has(key)) continue;
      seenKeysRef.current.add(key);

      const eventMs = Date.parse(event.timestamp);
      const lastAlertedMs = lastAlertedAtRef.current.get(event.symbol);
      if (lastAlertedMs !== undefined && eventMs - lastAlertedMs < IGNITION_ALERT_COOLDOWN_MS) continue; // still cooling down for this symbol
      lastAlertedAtRef.current.set(event.symbol, eventMs);

      ensureIgnitionChannel();
      Notifications.requestPermissionsAsync().then(({ status }) => {
        if (status !== "granted") return;
        Notifications.scheduleNotificationAsync({
          content: {
            title: `${event.symbol} ignition confirmed`,
            body: `Real follow-through at $${event.price.toFixed(event.price < 1 ? 4 : 2)}. Tap to open the chart.`,
            data: { symbol: event.symbol },
            sound: "default",
          },
          trigger: null, // fire immediately -- this already IS the trigger condition
        }).catch(() => {});
      });
    }
    // Only the newest event(s) need checking each time in practice
    // (events arrives newest-first, capped small) -- iterating the whole
    // list is cheap at this size and correctly catches anything
    // seenKeysRef hasn't seen yet even across a fast-arriving batch,
    // same reasoning as useMicropullbackAlerts.ts's own identical loop.
  }, [events]);
}
