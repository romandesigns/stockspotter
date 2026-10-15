import { decodeAlertMemory } from "./attentionMemory";
import { useEffect, useRef, useState } from "react";
import * as Notifications from "expo-notifications";
import AsyncStorage from "@react-native-async-storage/async-storage";
import { isFreshMicropullback, micropullbackKey, reduceMicropullbackDelivery, MICROPULLBACK_SEEN_TTL_MS } from "@stockspotter/shared-types";
import type { ConsolidationEvent, IgnitionEvent, MomentumUpdate } from "@stockspotter/shared-types";
import { ensureNotificationChannel, requestNotificationPermission } from "./nativeNotificationPermission";

const COOLDOWN = 15 * 60_000;
type Event = ConsolidationEvent | IgnitionEvent;
type Kind = "micropullback" | "ignition";
const writes = new Map<string, { latest: string | null; busy: boolean }>();
function persist(key: string, value: string) {
  const state = writes.get(key) ?? { latest: null, busy: false };
  writes.set(key, state); state.latest = value;
  if (state.busy) return;
  state.busy = true;
  void (async () => {
    while (state.latest !== null) {
      const next = state.latest; state.latest = null;
      await AsyncStorage.setItem(key, next).catch(() => {});
    }
    state.busy = false;
    writes.delete(key);
  })();
}
/** Local delivery only. Persisted replay suppression is best-effort, not a
 * crash-durable delivery ledger. Server push registration stays independent. */
export function useNativeAttentionAlerts(kind: Kind, events: Event[], momentum?: Map<string, MomentumUpdate>) {
  const key = `stockspotter:attention:${kind}:v1`;
  const memory = useRef({ seen: new Map<string, number>(), cooldown: new Map<string, number>() });
  const pending = useRef(new Map<string, Event>());
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const mounted = useRef(false);
  const busy = useRef(false);
  const lastNotice = useRef(-Infinity);
  const generation = useRef(0);
  const [ready, setReady] = useState(false);
  const savedMemory = useRef<string | null>(null);
  useEffect(() => {
    mounted.current = true;
    const token = ++generation.current;
    AsyncStorage.getItem(key).then(raw => {
      if (mounted.current && token === generation.current) { memory.current = decodeAlertMemory(raw, Date.now()); setReady(true); }
    }).catch(() => { if (mounted.current && token === generation.current) setReady(true); });
    return () => { mounted.current = false; generation.current++; if (timer.current) clearTimeout(timer.current); timer.current = null; };
  }, [key]);
  useEffect(() => {
    if (!ready) return;
    const now = Date.now();
    let fresh: Event[];
    if (kind === "micropullback") {
      const next = reduceMicropullbackDelivery(memory.current.seen, events as ConsolidationEvent[], momentum ?? new Map(), now);
      memory.current.seen = next.seen; fresh = next.fresh;
    } else {
      fresh = [];
      for (const event of events) {
        const id = micropullbackKey(event);
        if (memory.current.seen.has(id)) continue;
        memory.current.seen.set(id, now);
        if (!isFreshMicropullback(event, now) || now - (memory.current.cooldown.get(event.symbol) ?? -Infinity) < COOLDOWN) continue;
        memory.current.cooldown.set(event.symbol, now); fresh.push(event);
      }
    }
    for (const [id, at] of memory.current.seen) if (now - at > MICROPULLBACK_SEEN_TTL_MS) memory.current.seen.delete(id);
    for (const [symbol, at] of memory.current.cooldown) if (now - at > COOLDOWN) memory.current.cooldown.delete(symbol);
    const serialized = JSON.stringify({ version: 1, seen: [...memory.current.seen], cooldown: [...memory.current.cooldown] });
    if (serialized !== savedMemory.current) { savedMemory.current = serialized; persist(key, serialized); }
    for (const event of fresh) pending.current.set(micropullbackKey(event), event);
    if (!pending.current.size || timer.current !== null) return;
    timer.current = setTimeout(() => {
      timer.current = null;
      const batch = [...pending.current.values()].filter(event => isFreshMicropullback(event, Date.now())).sort((a, b) => Date.parse(b.timestamp) - Date.parse(a.timestamp));
      pending.current.clear();
      if (!mounted.current || !batch.length || busy.current || performance.now() - lastNotice.current < 8000) return;
      lastNotice.current = performance.now(); busy.current = true;
      const token = generation.current;
      const channelId = `${kind}-alerts`;
      (async () => {
        if (!await ensureNotificationChannel(channelId, kind === "ignition" ? "Ignition alerts" : "Micropullback alerts")) return;
        if (await requestNotificationPermission() !== "granted" || !mounted.current || token !== generation.current) return;
        const current = batch.filter(event => isFreshMicropullback(event, Date.now()));
        if (!current.length) return;
        const event = current[0];
        const identifier = `stockspotter-${kind}-attention`;
        await Notifications.dismissNotificationAsync(identifier).catch(() => {});
        if (!mounted.current || token !== generation.current || !isFreshMicropullback(event, Date.now())) return;
        await Notifications.scheduleNotificationAsync({ identifier,
          content: { title: `${event.symbol} ${kind === "ignition" ? "ignition confirmed" : "micropullback forming"}`,
            body: `${kind === "ignition" ? "Real follow-through" : "Momentum confirmation"} at $${event.price.toFixed(event.price < 1 ? 4 : 2)}.${current.length > 1 ? ` ${current.length - 1} more in this batch.` : ""} Tap to open the chart.`,
            data: { symbol: event.symbol }, sound: "default" }, trigger: { channelId },
        });
      })().catch(() => {}).finally(() => { busy.current = false; });
    }, 1000);
  }, [events, momentum, ready, key, kind]);
}