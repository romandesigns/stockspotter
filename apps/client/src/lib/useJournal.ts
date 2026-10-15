// Trade journal -- ws-server's GET/POST /journal, with the same base-URL
// resolution and authenticatedFetch as useShortlist/useMovers. Loaded on
// mount and again after every save, so the list always shows what the
// server stored (last version per trade id, newest trade first), not an
// optimistic local guess. Decoding and entry building live in journal.ts.

import { authenticatedFetch } from "@stockspotter/shared-types";
import type { JournalEntry } from "@stockspotter/shared-types";
import { useCallback, useEffect, useRef, useState } from "react";
import { resolveHttpUrl } from "./config";
import { decodeJournal, journalPayload } from "./journal";

export type JournalSaveResult = { ok: true } | { ok: false; error: string };

export interface JournalState {
  entries: JournalEntry[];
  /** Why the most recent load failed; null once one succeeds. */
  error: string | null;
  loading: boolean;
  save: (entry: JournalEntry) => Promise<JournalSaveResult>;
  reload: () => void;
}

/** POSTs one entry (a new trade, or a new version of one by id). The
 * server answers 422 with a plain-text reason, which is returned as-is. */
export async function postJournalEntry(entry: JournalEntry): Promise<JournalSaveResult> {
  try {
    const r = await authenticatedFetch(`${resolveHttpUrl()}/journal`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(journalPayload(entry)),
    });
    if (r.ok) return { ok: true };
    const text = (await r.text().catch(() => "")).trim();
    return { ok: false, error: text && text.length < 300 ? text : `journal save failed: ${r.status}` };
  } catch (err) {
    return { ok: false, error: err instanceof Error ? err.message : "journal save failed" };
  }
}

export function useJournal(): JournalState {
  const [entries, setEntries] = useState<JournalEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const mounted = useRef(true);
  // Only the newest load may write state, so a slow earlier response can
  // never overwrite the list a later save fetched.
  const latestLoad = useRef(0);

  // Only async callbacks write state here, so the mount effect can call
  // it without a synchronous setState inside the effect.
  const load = useCallback(() => {
    const ticket = ++latestLoad.current;
    authenticatedFetch(`${resolveHttpUrl()}/journal`)
      .then((r) => {
        if (!r.ok) throw new Error(`journal request failed: ${r.status}`);
        return r.json() as Promise<unknown>;
      })
      .then((raw) => {
        const decoded = decodeJournal(raw);
        if (!decoded) throw new Error("journal response was not recognised");
        if (mounted.current && ticket === latestLoad.current) {
          setEntries(decoded);
          setError(null);
        }
      })
      .catch((err: unknown) => {
        if (mounted.current && ticket === latestLoad.current) setError(err instanceof Error ? err.message : "journal request failed");
      })
      .finally(() => {
        if (mounted.current && ticket === latestLoad.current) setLoading(false);
      });
  }, []);

  const reload = useCallback(() => {
    setLoading(true);
    load();
  }, [load]);

  useEffect(() => {
    mounted.current = true;
    load();
    return () => {
      mounted.current = false;
    };
  }, [load]);

  const save = useCallback(
    async (entry: JournalEntry) => {
      const result = await postJournalEntry(entry);
      if (result.ok) reload();
      return result;
    },
    [reload],
  );

  return { entries, error, loading, save, reload };
}
