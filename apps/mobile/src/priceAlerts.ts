// Real device persistence for price alerts -- same load-once/write-on-
// change pattern as useWatchlist.ts, applied to a richer per-alert
// record instead of a bare symbol set.

import AsyncStorage from "@react-native-async-storage/async-storage";

export interface PriceAlert {
  id: string;
  symbol: string;
  targetPrice: number;
  /** Computed once at creation time from where price sat relative to
   *  targetPrice ("fire once price reaches this level from below" vs.
   *  "...from above") -- re-deriving this at check time from a stale
   *  reference would be wrong the moment price has already crossed once. */
  direction: "above" | "below";
  createdAt: string;
}

const STORAGE_KEY = "stockspotter.priceAlerts.v1";

export async function loadAlerts(): Promise<PriceAlert[]> {
  try {
    const raw = await AsyncStorage.getItem(STORAGE_KEY);
    return raw ? (JSON.parse(raw) as PriceAlert[]) : [];
  } catch {
    return []; // best-effort, same as useWatchlist -- starts empty if storage is unreadable
  }
}

export async function saveAlerts(alerts: PriceAlert[]): Promise<void> {
  try {
    await AsyncStorage.setItem(STORAGE_KEY, JSON.stringify(alerts));
  } catch {
    /* best-effort */
  }
}

export function directionFor(currentPrice: number, targetPrice: number): "above" | "below" {
  return targetPrice >= currentPrice ? "above" : "below";
}

/** Pure trigger check -- a bar's close crossing the alert's armed level.
 *  Exported standalone so it's testable without AsyncStorage/
 *  notifications/the hook around it. */
export function isTriggered(alert: PriceAlert, latestClose: number): boolean {
  return alert.direction === "above" ? latestClose >= alert.targetPrice : latestClose <= alert.targetPrice;
}
