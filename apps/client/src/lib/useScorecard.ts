// Panel scorecard -- ws-server's GET /scorecard/{latest,days,day/:day}, with
// the same base-URL resolution and authenticatedFetch as useJournal. All
// decoding and wording live in shared-types' scorecard.ts (tested there).
//
// Fetched when the Scorecard view opens, then every 10 minutes while the
// view is showing and the tab is visible. The files change once a day, so
// returning to the tab only re-fetches when the last load is older than
// that. A 404 means the server predates the scorecard routes ("not
// available on this server yet"), which is a state, not an error.
//
// Day files (without per-detection rows) are fetched for the rolling window
// to draw each panel's per-day trend, one at a time, and cached per day for
// as long as latest.json's generatedAt is unchanged. The day picker reads
// the same cache.

import {
  authenticatedFetch,
  decodeScorecardDay,
  decodeScorecardDays,
  decodeScorecardLatest,
  fetchScorecard,
  SCORECARD_REFRESH_MS,
  scorecardUrls,
  type ScorecardDay,
  type ScorecardLatest,
} from "@stockspotter/shared-types";
import { useCallback, useEffect, useRef, useState } from "react";
import { resolveHttpUrl } from "./config";

export type ScorecardStatus = "loading" | "ok" | "unavailable" | "error";

export interface ScorecardState {
  status: ScorecardStatus;
  latest: ScorecardLatest | null;
  /** Why the most recent load failed; kept alongside the last good file. */
  error: string | null;
  /** Scored days, newest first (from /scorecard/days, else the rolling window). */
  days: string[];
  /** Day files fetched so far, by day. */
  dayFiles: ReadonlyMap<string, ScorecardDay>;
  /** Days whose file could not be loaded. */
  dayErrors: ReadonlyMap<string, string>;
  /** Fetch one day's file (no-op when cached). */
  loadDay: (day: string) => void;
  reload: () => void;
}

const fetcher = (url: string) => authenticatedFetch(url);

export function useScorecard(enabled: boolean): ScorecardState {
  const [status, setStatus] = useState<ScorecardStatus>("loading");
  const [latest, setLatest] = useState<ScorecardLatest | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [days, setDays] = useState<string[]>([]);
  const [dayFiles, setDayFiles] = useState<ReadonlyMap<string, ScorecardDay>>(new Map());
  const [dayErrors, setDayErrors] = useState<ReadonlyMap<string, string>>(new Map());
  const lastLoadAt = useRef(0);
  const generatedAt = useRef<string | null>(null);
  const cache = useRef(new Map<string, ScorecardDay>());
  const inFlightDays = useRef(new Set<string>());
  const alive = useRef(true);

  const fetchDay = useCallback(async (day: string) => {
    if (cache.current.has(day) || inFlightDays.current.has(day)) return;
    inFlightDays.current.add(day);
    const r = await fetchScorecard(fetcher, scorecardUrls.day(resolveHttpUrl(), day), decodeScorecardDay);
    inFlightDays.current.delete(day);
    if (!alive.current) return;
    if (r.kind === "ok") {
      cache.current.set(day, r.body);
      setDayFiles(new Map(cache.current));
      setDayErrors((prev) => {
        if (!prev.has(day)) return prev;
        const next = new Map(prev);
        next.delete(day);
        return next;
      });
    } else {
      const message = r.kind === "unavailable" ? "no file for this day" : r.message;
      setDayErrors((prev) => new Map(prev).set(day, message));
    }
  }, []);

  const load = useCallback(async () => {
    lastLoadAt.current = Date.now();
    const base = resolveHttpUrl();
    const [l, d] = await Promise.all([
      fetchScorecard(fetcher, scorecardUrls.latest(base), decodeScorecardLatest),
      fetchScorecard(fetcher, scorecardUrls.days(base), decodeScorecardDays),
    ]);
    if (!alive.current) return;
    if (l.kind === "ok") {
      if (generatedAt.current !== l.body.generatedAt) {
        // A new run may have rewritten day files; start the cache over.
        generatedAt.current = l.body.generatedAt;
        cache.current = new Map();
        setDayFiles(new Map());
      }
      setLatest(l.body);
      setStatus("ok");
      setError(null);
      const list = d.kind === "ok" && d.body.length > 0 ? d.body : [...l.body.rolling.days].reverse();
      setDays(list);
      // Trend: the rolling window's days, newest first, one request at a time.
      for (const day of [...l.body.rolling.days].reverse()) {
        if (!alive.current) return;
        await fetchDay(day);
      }
    } else if (l.kind === "unavailable") {
      setStatus((prev) => (prev === "ok" ? prev : "unavailable"));
    } else {
      setError(l.message);
      setStatus((prev) => (prev === "ok" ? prev : "error"));
    }
  }, [fetchDay]);

  useEffect(() => {
    alive.current = true;
    if (!enabled) return () => { alive.current = false; };
    let timer: ReturnType<typeof setInterval> | null = null;
    function start() {
      if (timer !== null) return;
      if (Date.now() - lastLoadAt.current >= SCORECARD_REFRESH_MS) void load();
      timer = setInterval(() => void load(), SCORECARD_REFRESH_MS);
    }
    function stop() {
      if (timer === null) return;
      clearInterval(timer);
      timer = null;
    }
    function onVisibility() {
      if (document.visibilityState === "visible") start();
      else stop();
    }
    onVisibility();
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      alive.current = false;
      stop();
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [enabled, load]);

  const loadDay = useCallback((day: string) => void fetchDay(day), [fetchDay]);
  const reload = useCallback(() => void load(), [load]);

  return { status, latest, error, days, dayFiles, dayErrors, loadDay, reload };
}
