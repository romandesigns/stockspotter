// The trader's own journal -- ws-server's GET/POST /journal, shared with the
// desktop app (one journal across devices). Loaded on demand (opening the
// Journal view, the refresh button), not polled: it only changes when the
// trader changes it. POST takes exactly one JournalEntry; the same id
// updates the same trade. A 422 carries a plain-text reason, surfaced as is.
import { authenticatedFetch } from "@stockspotter/shared-types";
import type { JournalEntry } from "@stockspotter/shared-types";
import { useCallback, useEffect, useRef, useState } from "react";
import { HTTP_URL } from "./config";
import { decodeJournal, decodeJournalEntry, upsertJournalEntry } from "./shortlist";

export type SaveResult = { ok: true; entry: JournalEntry } | { ok: false; error: string };

/** POST one entry. Exported so the plan form can log without loading the list. */
export async function postJournalEntry(entry: JournalEntry): Promise<SaveResult> {
  try {
    const response = await authenticatedFetch(`${HTTP_URL}/journal`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(entry),
    });
    if (!response.ok) {
      const text = (await response.text().catch(() => "")).trim();
      return { ok: false, error: text ? text.slice(0, 200) : `Save failed (HTTP ${response.status}).` };
    }
    // The server may echo the stored entry; fall back to what was sent.
    const echoed = await response.json().then(decodeJournalEntry, () => null);
    return { ok: true, entry: echoed && echoed.id === entry.id ? echoed : entry };
  } catch {
    return { ok: false, error: "Couldn't reach the server. Nothing was saved." };
  }
}

export interface TradeJournalState {
  entries: JournalEntry[] | null;
  loading: boolean;
  error: string | null;
  refresh: () => void;
  save: (entry: JournalEntry) => Promise<SaveResult>;
}

export function useTradeJournal(enabled: boolean): TradeJournalState {
  const [entries, setEntries] = useState<JournalEntry[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const requestId = useRef(0);

  const refresh = useCallback(() => {
    const id = ++requestId.current;
    setLoading(true);
    authenticatedFetch(`${HTTP_URL}/journal`)
      .then((r) => {
        if (!r.ok) throw new Error(`HTTP ${r.status}`);
        return r.json() as Promise<unknown>;
      })
      .then((raw) => {
        const decoded = decodeJournal(raw);
        if (!decoded) throw new Error("unrecognised response");
        if (id === requestId.current) {
          setEntries(decoded);
          setError(null);
        }
      })
      .catch((err: unknown) => {
        if (id === requestId.current) setError(err instanceof Error ? err.message : "request failed");
      })
      .finally(() => {
        if (id === requestId.current) setLoading(false);
      });
  }, []);

  // Load each time the Journal view opens -- a trade logged on the desktop
  // since the last look shows up without a manual refresh.
  useEffect(() => {
    if (enabled) refresh();
  }, [enabled, refresh]);

  const save = useCallback(async (entry: JournalEntry) => {
    const result = await postJournalEntry(entry);
    if (result.ok) setEntries((prev) => (prev ? upsertJournalEntry(prev, result.entry) : prev));
    return result;
  }, []);

  return { entries, loading, error, refresh, save };
}
