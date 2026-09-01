// Real device persistence for the Watchlist -- previously an in-memory
// Set that reset every time the app reopened, which is a real gap on a
// native app (unlike a web browser tab that might stay open for hours,
// a phone app is backgrounded/killed constantly; a watchlist that
// forgets itself every relaunch isn't a real watchlist).

import { useEffect, useRef, useState } from "react";
import AsyncStorage from "@react-native-async-storage/async-storage";

const STORAGE_KEY = "stockspotter.watchlist.v1";

export function useWatchlist(): { saved: Set<string>; toggleSaved: (symbol: string) => void } {
  const [saved, setSaved] = useState<Set<string>>(new Set());
  const loaded = useRef(false);

  useEffect(() => {
    AsyncStorage.getItem(STORAGE_KEY)
      .then((raw) => {
        if (!raw) return;
        const symbols = JSON.parse(raw) as string[];
        setSaved(new Set(symbols));
      })
      .catch(() => { /* best-effort -- starts empty if storage is unreadable */ })
      .finally(() => { loaded.current = true; });
  }, []);

  useEffect(() => {
    // Skip the very first write -- otherwise the initial empty Set
    // would overwrite real saved data for one render before the load
    // above resolves.
    if (!loaded.current) return;
    AsyncStorage.setItem(STORAGE_KEY, JSON.stringify([...saved])).catch(() => {});
  }, [saved]);

  function toggleSaved(symbol: string) {
    setSaved((current) => {
      const next = new Set(current);
      if (next.has(symbol)) next.delete(symbol); else next.add(symbol);
      return next;
    });
  }

  return { saved, toggleSaved };
}
