// Owns price-alert state for the whole app (App.tsx mounts this once,
// same shape as useWatchlist.ts), NOT per-open-chart -- an alert has to
// keep monitoring a symbol you're no longer looking at, so this has to
// watch feed.barsBySymbol at the top level where every symbol's live
// bars already flow through, not just whatever ChartScreen happens to
// be open. ChartScreen.tsx only ever sees this symbol's own slice
// (App.tsx filters), same pattern liveBars/momentum already use.
import { useEffect, useRef, useState } from "react";
import { Alert } from "react-native";
import * as Notifications from "expo-notifications";
import type { BarUpdate } from "@stockspotter/shared-types";
import { directionFor, isTriggered, loadAlerts, saveAlerts, type PriceAlert } from "./priceAlerts";

// Foreground notifications are suppressed by default -- without this
// handler a triggered alert would silently do nothing while the app is
// open, the single most common case for glancing at stockspotter. Sets
// both the old (shouldShowAlert) and current (shouldShowBanner/List)
// field names since expo-notifications renamed these mid-SDK-57 life --
// harmless to set both.
Notifications.setNotificationHandler({
  handleNotification: async () => ({
    shouldShowAlert: true,
    shouldShowBanner: true,
    shouldShowList: true,
    shouldPlaySound: true,
    shouldSetBadge: false,
  }),
});

let androidChannelReady = false;
async function ensureAndroidChannel() {
  if (androidChannelReady) return;
  androidChannelReady = true;
  await Notifications.setNotificationChannelAsync("price-alerts", {
    name: "Price alerts",
    importance: Notifications.AndroidImportance.HIGH,
    sound: "default",
  }).catch(() => {});
}

export function usePriceAlerts(barsBySymbol: Map<string, BarUpdate[]>): {
  alerts: PriceAlert[];
  addAlert: (symbol: string, targetPrice: number, currentPrice: number) => void;
  removeAlert: (id: string) => void;
} {
  const [alerts, setAlerts] = useState<PriceAlert[]>([]);
  const loaded = useRef(false);
  const alertsRef = useRef<PriceAlert[]>([]);
  alertsRef.current = alerts;

  useEffect(() => {
    loadAlerts()
      .then(setAlerts)
      .finally(() => { loaded.current = true; });
  }, []);

  useEffect(() => {
    if (!loaded.current) return; // skip the first (empty) write, same guard as useWatchlist
    saveAlerts(alerts);
  }, [alerts]);

  function addAlert(symbol: string, targetPrice: number, currentPrice: number) {
    const alert: PriceAlert = {
      id: `${symbol}-${Date.now()}`,
      symbol,
      targetPrice,
      direction: directionFor(currentPrice, targetPrice),
      createdAt: new Date().toISOString(),
    };
    setAlerts((prev) => [...prev, alert]);

    Notifications.requestPermissionsAsync().then(({ status }) => {
      ensureAndroidChannel();
      if (status !== "granted") {
        Alert.alert(
          "Notifications are off",
          "This alert was saved, but stockspotter can't notify you when it fires until notifications are allowed in Settings.",
        );
      }
    });
  }

  function removeAlert(id: string) {
    setAlerts((prev) => prev.filter((a) => a.id !== id));
  }

  // Checked on every live bar update, for every symbol, not just an open
  // chart's -- feed.barsBySymbol gets a fresh Map reference on every
  // bar_update (useRealtimeFeed.ts), so this effect really does re-run
  // per tick. The list it scans is just the user's own saved alerts
  // (small), so a lookup per tick is cheap.
  useEffect(() => {
    if (alertsRef.current.length === 0) return;
    const fired: PriceAlert[] = [];
    for (const alert of alertsRef.current) {
      const bars = barsBySymbol.get(alert.symbol);
      const last = bars?.[bars.length - 1];
      if (last && isTriggered(alert, last.close)) fired.push(alert);
    }
    if (fired.length === 0) return;

    setAlerts((prev) => prev.filter((a) => !fired.some((f) => f.id === a.id)));
    for (const alert of fired) {
      Notifications.scheduleNotificationAsync({
        content: {
          title: `${alert.symbol} ${alert.direction === "above" ? "reached" : "dropped to"} $${alert.targetPrice.toFixed(alert.targetPrice < 1 ? 4 : 2)}`,
          body: "Tap to open the chart.",
          data: { symbol: alert.symbol },
          sound: "default",
        },
        trigger: null, // fire immediately -- this already IS the trigger condition
      }).catch(() => {});
    }
  }, [barsBySymbol]);

  return { alerts, addAlert, removeAlert };
}
