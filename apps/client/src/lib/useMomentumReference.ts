// Slow-changing reference data for the Bullish Momentum panel: the fund
// symbols to leave out (GET /universe/funds) and the per-band track record
// (GET /momentum/track-record). Fetched on mount and every 30 minutes,
// same authenticatedFetch + base URL as useShortlist.ts. A failed or
// unrecognised response keeps the last good value; until there is one
// (or while the server's fund list is still empty) it retries every
// minute instead of waiting out the full interval.

import { authenticatedFetch } from "@stockspotter/shared-types";
import { useEffect, useMemo, useState } from "react";
import { resolveHttpUrl } from "./config";
import { decodeFunds, decodeMomentumTrackRecord, type MomentumTrackRecord } from "./momentumTrackRecord";

const REFRESH_MS = 30 * 60_000;
const RETRY_MS = 60_000;

function usePolled<T>(path: string, decode: (raw: unknown) => T | null, isComplete: (value: T) => boolean): T | null {
  const [value, setValue] = useState<T | null>(null);
  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let good = false;
    function fetchOnce() {
      authenticatedFetch(`${resolveHttpUrl()}${path}`)
        .then((r) => {
          if (!r.ok) throw new Error(`${path} request failed: ${r.status}`);
          return r.json() as Promise<unknown>;
        })
        .then((raw) => {
          const decoded = decode(raw);
          if (cancelled || decoded === null) return;
          const complete = isComplete(decoded);
          // An empty fund list after a good one is a server restart, not news.
          if (complete || !good) setValue(decoded);
          good = good || complete;
        })
        .catch(() => { /* keep the last good value */ })
        .finally(() => {
          if (!cancelled) timer = setTimeout(fetchOnce, good ? REFRESH_MS : RETRY_MS);
        });
    }
    fetchOnce();
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [path, decode, isComplete]);
  return value;
}

const fundsComplete = (funds: string[]) => funds.length > 0;
const recordComplete = () => true;
const EMPTY: ReadonlySet<string> = new Set();

export function useMomentumReference(): { funds: ReadonlySet<string>; trackRecord: MomentumTrackRecord | null } {
  const funds = usePolled("/universe/funds", decodeFunds, fundsComplete);
  const trackRecord = usePolled("/momentum/track-record", decodeMomentumTrackRecord, recordComplete);
  const fundSet = useMemo(() => (funds ? new Set(funds) : EMPTY), [funds]);
  return { funds: fundSet, trackRecord };
}
