// Persistent watchlist for the web app -- ported from the mobile app's
// own useWatchlist.ts (same storage key, same toggle shape), backed by
// localStorage instead of AsyncStorage since that's the real equivalent
// on this platform. Simpler than the mobile version: localStorage reads
// are synchronous, so there's no load-then-write race to guard against
// (mobile's `loaded` ref exists only because AsyncStorage.getItem is
// async and an early write could otherwise clobber real saved data with
// an empty initial Set before the load resolves -- not a risk here).

import { useEffect, useState } from "react";

const STORAGE_KEY = "stockspotter.watchlist.v1";

function loadInitial(): Set<string> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    return raw ? new Set(JSON.parse(raw) as string[]) : new Set();
  } catch {
    return new Set(); // best-effort -- starts empty if storage is unreadable (private mode, etc.)
  }
}

export function useWatchlist(): { saved: Set<string>; toggleSaved: (symbol: string) => void } {
  const [saved, setSaved] = useState<Set<string>>(loadInitial);

  useEffect(() => {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify([...saved]));
    } catch {
      // best-effort -- e.g. storage quota/private-mode restrictions
    }
  }, [saved]);

  function toggleSaved(symbol: string) {
    setSaved((current) => {
      const next = new Set(current);
      if (next.has(symbol)) next.delete(symbol);
      else next.add(symbol);
      return next;
    });
  }

  return { saved, toggleSaved };
}
