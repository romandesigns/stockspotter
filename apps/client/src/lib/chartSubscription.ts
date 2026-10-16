// Which symbols this client has charts open for, told to the server so it
// builds live candles for them (`subscribe_chart` in shared-types).
//
// Without this a chart only forms live for the handful of symbols the
// server already streams to everyone (funnel/movers); any other symbol
// opened in the regular session stayed static until its history was
// fetched again (chart diagnosis 2026-10-16, root cause 3).
//
// Same shape as the order-flow registry in orderFlowWatch.ts: one registry
// per app, so every open chart and the one realtime socket meet here and
// neither owns the other. Charts register symbols (refcounted, so two
// slots on one symbol are one entry); the socket attaches a sender on
// `welcome` and detaches on close. The message REPLACES the server's set,
// so every change re-sends the whole list, and a new socket gets it again.
//
// Strictly an upgrade: a server that predates `subscribe_chart` ignores it
// and the chart falls back to the periodic history refetch in
// useHistoricalBackfill.ts. Free of React so the rules are testable.
import { CHART_SUBSCRIPTION_LIMIT, type ClientSubscribeChart } from "@stockspotter/shared-types";

const counts = new Map<string, number>();
let sender: ((message: ClientSubscribeChart) => void) | null = null;
/** What this socket was last told, as a key, so an unchanged set is not
 * re-sent. Null means "nothing sent on this socket yet". */
let lastSent: string | null = null;
let syncQueued = false;

/** The set to send: registration order, at most the server's limit.
 * Registration order rather than sorted so a fifth chart opened past the
 * limit is the one left out, not whichever sorts last. */
export function currentChartSymbols(): string[] {
  return [...counts.keys()].slice(0, CHART_SUBSCRIPTION_LIMIT);
}

function sync() {
  syncQueued = false;
  if (!sender) return;
  const symbols = currentChartSymbols();
  const key = symbols.join(",");
  // A fresh socket has no subscription, so an empty set needs no message;
  // after a non-empty one it does (empty clears).
  if (key === (lastSent ?? "")) return;
  sender({ type: "subscribe_chart", symbols });
  lastSent = key;
}

/** Coalesced to one message per task: a symbol switch is an unregister and
 * a register in the same React commit, which would otherwise be two sends
 * (the first briefly dropping the old chart's live bars on the server). */
function scheduleSync() {
  if (syncQueued) return;
  syncQueued = true;
  queueMicrotask(sync);
}

/**
 * Called by the realtime feed on `welcome` with a sender bound to that
 * socket, and with null when it closes. Subscriptions are per connection,
 * so attaching always re-sends the current set.
 */
export function attachChartSocket(send: ((message: ClientSubscribeChart) => void) | null): void {
  sender = send;
  lastSent = null;
  if (send) sync();
}

/** Registers one open chart's symbol; returns the function that releases it. */
export function registerChartSymbol(symbol: string): () => void {
  const normalized = symbol.trim().toUpperCase();
  if (!normalized) return () => {};
  counts.set(normalized, (counts.get(normalized) ?? 0) + 1);
  scheduleSync();
  let released = false;
  return () => {
    if (released) return;
    released = true;
    const n = (counts.get(normalized) ?? 1) - 1;
    if (n <= 0) counts.delete(normalized);
    else counts.set(normalized, n);
    scheduleSync();
  };
}

/** Test seam: the registry is process-wide by design. */
export function __resetChartSubscriptions(): void {
  counts.clear();
  sender = null;
  lastSent = null;
  syncQueued = false;
}
