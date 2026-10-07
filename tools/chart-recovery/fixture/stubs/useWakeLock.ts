// Stand-in for apps/client/src/lib/useWakeLock.ts. navigator.wakeLock
// is unavailable (and meaningless) in a headless harness; the real hook
// would silently no-op there, which would make "the chart asked to keep
// the screen awake" untestable. Recording the requested state instead
// keeps that observable without touching a platform API.

import { useEffect } from "react";

const states: boolean[] = [];

export function wakeLockStates(): boolean[] {
  return [...states];
}

export function useWakeLock(active: boolean): void {
  useEffect(() => {
    states.push(active);
  }, [active]);
}
