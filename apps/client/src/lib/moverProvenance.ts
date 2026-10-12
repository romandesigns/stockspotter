import type { Mover, TodayMovers } from "./useMovers";

const tradeTime = new Intl.DateTimeFormat("en-US", {
  timeZone: "America/New_York", month: "short", day: "numeric", year: "numeric",
  hour: "2-digit", minute: "2-digit", hourCycle: "h23",
});

export function moverProvenanceLabel(row: Mover, now = Date.now()): { label: string; title: string } {
  const p = row.provenance;
  if (!p) return { label: "Trade time unavailable", title: "This server did not provide price provenance." };
  const receipt = p.snapshotReceivedAt ? Date.parse(p.snapshotReceivedAt) : NaN;
  const receiptText = Number.isFinite(receipt) ? `Snapshot received: ${tradeTime.format(receipt)} ET.` : "Snapshot receipt time unavailable.";
  if (p.priceSource === "daily_bar") return {
    label: `Daily bar${p.dailyBarDate ? ` · ${p.dailyBarDate}` : " · date unavailable"}`,
    title: `Price came from the daily bar, not a latest trade. ${receiptText}`,
  };
  const at = p.latestTradeAt ? Date.parse(p.latestTradeAt) : NaN;
  if (!Number.isFinite(at)) return { label: "Trade time unavailable", title: `Latest-trade price has no usable timestamp. ${receiptText}` };
  if (at > now) return { label: "Trade time ahead", title: `Provider trade timestamp is ahead of this device's clock: ${tradeTime.format(at)} ET. ${receiptText}` };
  return { label: `Trade ${tradeTime.format(at)} ET`, title: `Last reported trade time; fetching a snapshot does not imply a new trade. ${receiptText}` };
}

export function moverRefreshLabel(today: TodayMovers): { label: string; title: string } | null {
  const p = today.scanProvenance;
  const detail = p ? `${p.requestedSymbolCount} requested entries; ${p.returnedEntryCount} provider response entries; ${p.convertedSnapshotCount} unique converted snapshots. Classification day: ${p.classificationMarketDay}. Counts are not scanner eligibility or outcome completeness.` : "No successful scan provenance available.";
  const retained = today.lastUpdated ? "previous snapshot" : "no snapshot yet";
  if (today.clientFetchFailed) return { label: `Connection failed · ${retained}`, title: detail };
  if (today.refreshStatus === "failed") return { label: `Refresh failed · ${retained}`, title: detail };
  if (p && p.providerOmittedCount > 0) return { label: `${p.providerOmittedCount} requested readings missing`, title: `Provider omissions in successful responses. ${detail}` };
  return null;
}

export function moverVolumeKnown(row: Mover, historical = false): boolean {
  return historical || row.volumeSource === "snapshot_daily_bar_current" || row.volumeSource === "minute_bars_since_open";
}
