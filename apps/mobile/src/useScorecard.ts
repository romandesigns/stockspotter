// Panel scorecard -- ws-server's GET /scorecard/{latest,days,day/:day}, same
// authenticatedFetch + HTTP_URL as useShortlist.ts. Decoding and wording
// live in shared-types' scorecard.ts (tested there) so this screen and the
// desktop Scorecard view say the same thing.
//
// Fetched when the Scorecard screen opens, then every 10 minutes while it is
// open and the app is in the foreground. The files change once a day, so
// returning to the app only re-fetches when the last load is older than
// that. A 404 means the server predates the scorecard routes -- a state,
// not an error. Day files (no per-detection rows) for the rolling window are
// fetched one at a time for the per-day trend and cached per day while
// latest.json's generatedAt is unchanged; the day picker reads that cache.
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
import { HTTP_URL } from "./config";
import { useAppActive } from "./useShortlist";

export type ScorecardStatus = "loading" | "ok" | "unavailable" | "error";

export interface ScorecardState {
  status: ScorecardStatus;
  latest: ScorecardLatest | null;
  error: string | null;
  /** Scored days, newest first. */
  days: string[];
  dayFiles: ReadonlyMap<string, ScorecardDay>;
  dayErrors: ReadonlyMap<string, string>;
  loadDay: (day: string) => void;
  reload: () => void;
}

const fetcher = (url: string) => authenticatedFetch(url);

export function useScorecard(enabled: boolean): ScorecardState {
  const appActive = useAppActive();
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
  const loading = useRef(false);

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const fetchDay = useCallback(async (day: string) => {
    if (cache.current.has(day) || inFlightDays.current.has(day)) return;
    inFlightDays.current.add(day);
    const r = await fetchScorecard(fetcher, scorecardUrls.day(HTTP_URL, day), decodeScorecardDay);
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
      setDayErrors((prev) => new Map(prev).set(day, r.kind === "unavailable" ? "no file for this day" : r.message));
    }
  }, []);

  const load = useCallback(async () => {
    if (loading.current) return; // a slow response never stacks requests
    loading.current = true;
    lastLoadAt.current = Date.now();
    try {
      const [l, d] = await Promise.all([
        fetchScorecard(fetcher, scorecardUrls.latest(HTTP_URL), decodeScorecardLatest),
        fetchScorecard(fetcher, scorecardUrls.days(HTTP_URL), decodeScorecardDays),
      ]);
      if (!alive.current) return;
      if (l.kind === "ok") {
        if (generatedAt.current !== l.body.generatedAt) {
          generatedAt.current = l.body.generatedAt;
          cache.current = new Map();
          setDayFiles(new Map());
        }
        setLatest(l.body);
        setStatus("ok");
        setError(null);
        setDays(d.kind === "ok" && d.body.length > 0 ? d.body : [...l.body.rolling.days].reverse());
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
    } finally {
      loading.current = false;
    }
  }, [fetchDay]);

  const polling = enabled && appActive;
  useEffect(() => {
    if (!polling) return;
    if (Date.now() - lastLoadAt.current >= SCORECARD_REFRESH_MS) void load();
    const timer = setInterval(() => void load(), SCORECARD_REFRESH_MS);
    return () => clearInterval(timer);
  }, [polling, load]);

  const loadDay = useCallback((day: string) => void fetchDay(day), [fetchDay]);
  const reload = useCallback(() => void load(), [load]);

  return { status, latest, error, days, dayFiles, dayErrors, loadDay, reload };
}
