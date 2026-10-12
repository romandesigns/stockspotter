import type { TodayMovers } from "./useMovers";

/** Empty rows alone cannot establish scan success or market conditions. */
export function moverEmptyState(today: TodayMovers, peak: boolean): string {
  if (today.clientFetchFailed) return "Couldn't retrieve the mover snapshot. Retrying…";
  if (today.refreshStatus === "failed") return "The mover refresh failed. Retrying…";
  if (today.refreshStatus === "refreshing") return "Refreshing the mover snapshot…";
  if (today.refreshStatus === "success") {
    return peak ? "No retained peak readings in this snapshot." : "No readings available for this list in the latest snapshot.";
  }
  return "Mover snapshot status is unavailable.";
}

