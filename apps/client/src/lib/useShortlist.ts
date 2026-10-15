// Today's Shortlist -- ws-server's GET /shortlist, polled. Same base-URL
// resolution and authenticatedFetch as useMovers.ts, and the same
// best-effort-on-failure rule: a failed poll keeps showing the last good
// body (with `error` set) rather than blanking the list, and never bumps
// `lastSuccessAt`, so the age shown keeps counting from the last real
// refresh. Decoding lives in shortlist.ts so it is unit tested.
//
// Each successful poll is also diffed against the previous one into a
// bounded "what changed" history (shortlistChanges.ts), reset whenever the
// market day changes. Display only -- no sound, push or OS notification.

import { authenticatedFetch } from "@stockspotter/shared-types";
import type { ShortlistResponse } from "@stockspotter/shared-types";
import { useEffect, useRef, useState } from "react";
import { resolveHttpUrl } from "./config";
import { decodeShortlist } from "./shortlist";
import { appendChanges, EMPTY_HISTORY, type ChangeHistory, type ShortlistChange } from "./shortlistChanges";

/** The server rebuilds the shortlist from live state on every request,
 * so a short cadence is cheap -- but only while someone can see it. */
const POLL_MS = 5_000;

export interface ShortlistState {
  body: ShortlistResponse | null;
  /** Why the most recent poll failed; null once one succeeds again. */
  error: string | null;
  /** Client time of the last successful poll. */
  lastSuccessAt: Date | null;
  /** Recent list changes for today's market day, newest first. */
  changes: ShortlistChange[];
}

/** Polls only while `enabled` (the Shortlist view is showing) and the tab
 * is visible; becoming visible again polls immediately instead of waiting
 * out the interval. */
export function useShortlist(enabled = true): ShortlistState {
  const [state, setState] = useState<ShortlistState>({ body: null, error: null, lastSuccessAt: null, changes: [] });
  // The diff baseline lives in refs, not state: polls resolve outside
  // render and must always compare against the body actually shown last.
  const lastBody = useRef<ShortlistResponse | null>(null);
  const history = useRef<ChangeHistory>(EMPTY_HISTORY);

  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    let id: ReturnType<typeof setInterval> | null = null;

    function poll() {
      authenticatedFetch(`${resolveHttpUrl()}/shortlist`)
        .then((r) => {
          if (!r.ok) throw new Error(`shortlist request failed: ${r.status}`);
          return r.json() as Promise<unknown>;
        })
        .then((raw) => {
          const body = decodeShortlist(raw);
          if (!body) throw new Error("shortlist response was not recognised");
          if (cancelled) return;
          // A slow poll answering after a newer one would diff backwards.
          if (lastBody.current && Date.parse(body.generatedAt) < Date.parse(lastBody.current.generatedAt)) return;
          history.current = appendChanges(history.current, lastBody.current, body);
          lastBody.current = body;
          setState({ body, error: null, lastSuccessAt: new Date(), changes: history.current.changes });
        })
        .catch((err: unknown) => {
          // Best-effort -- keep whatever was last fetched, just flag it.
          const message = err instanceof Error ? err.message : "shortlist request failed";
          if (!cancelled) setState((previous) => ({ ...previous, error: message }));
        });
    }

    function start() {
      if (id !== null) return;
      poll();
      id = setInterval(poll, POLL_MS);
    }
    function stop() {
      if (id === null) return;
      clearInterval(id);
      id = null;
    }
    function onVisibility() {
      if (document.visibilityState === "visible") start();
      else stop();
    }

    onVisibility();
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      cancelled = true;
      stop();
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [enabled]);

  return state;
}
