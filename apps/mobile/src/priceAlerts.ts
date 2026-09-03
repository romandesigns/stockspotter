// Real device persistence for price alerts -- modeled directly on
// Robinhood's own real flow per Roman's explicit ask (confirmed against
// Robinhood's own support docs: tap the bell on a stock's chart, toggle
// "Price moves above" / "Price moves below" independently, Edit to set
// each one's price). That's a two-slot model, not an open-ended list --
// at most one "above" alert and one "below" alert per symbol, each
// togglable without losing its configured price. Same load-once/
// write-on-change pattern as useWatchlist.ts.

import AsyncStorage from "@react-native-async-storage/async-storage";

export type AlertDirection = "above" | "below";

export interface PriceAlert {
  /** `${symbol}:${direction}` -- deterministic, so there's structurally
   *  at most one record per symbol+direction, matching the two-slot
   *  model rather than needing separate id-uniqueness bookkeeping. */
  id: string;
  symbol: string;
  direction: AlertDirection;
  targetPrice: number;
  /** Off keeps the configured price (a one-tap re-arm) rather than
   *  deleting it -- same real behavior as flipping Robinhood's own
   *  toggle back off, and what a fired alert switches itself to. */
  enabled: boolean;
  updatedAt: string;
}

// v2: earlier build's shape (arbitrary list, id = `${symbol}-${Date.now()}`,
// direction inferred from creation-time price) is abandoned rather than
// migrated -- pre-release, no real alerts exist yet to carry over.
const STORAGE_KEY = "stockspotter.priceAlerts.v2";

export function alertId(symbol: string, direction: AlertDirection): string {
  return `${symbol}:${direction}`;
}

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

/** Pure trigger check -- an armed alert's price crossed by a bar's
 *  close. Callers filter to `enabled` alerts first (see
 *  usePriceAlerts.ts) so this stays a one-line, easily tested function. */
export function isTriggered(alert: PriceAlert, latestClose: number): boolean {
  return alert.direction === "above" ? latestClose >= alert.targetPrice : latestClose <= alert.targetPrice;
}
