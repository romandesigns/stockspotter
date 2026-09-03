// Prevent the screen from sleeping/locking while the chart is open --
// ported to web/desktop (2026-09-03) from the same real ask mobile got
// ("prevent phone from going sleep mode... prevent the phone from
// locking the screen"). Real value here too: leaving stockspotter's
// chart open on a desktop monitor while watching a live move shouldn't
// let the display sleep either, same underlying problem, different
// platform mechanism.
//
// Screen Wake Lock API (navigator.wakeLock) -- feature-detected, no
// dependency needed (unlike mobile's expo-keep-awake, this is a
// standard browser API). Silently no-ops where unsupported (older
// Safari, some embedded WebViews including, possibly, Tauri's own on
// some platforms) rather than throwing -- a missing wake lock is a
// degraded experience, not a broken one, so it fails open quietly.
import { useEffect } from "react";

export function useWakeLock(active: boolean) {
  useEffect(() => {
    if (!active) return;
    if (!("wakeLock" in navigator)) return;

    let sentinel: WakeLockSentinel | null = null;
    let cancelled = false;

    async function acquire() {
      try {
        sentinel = await navigator.wakeLock.request("screen");
      } catch {
        // Real, expected failure modes: permission denied, battery
        // saver mode, or the tab isn't visible yet when this fires --
        // none of these are worth surfacing to the user, the chart
        // still works fine, the screen just might sleep.
      }
    }

    // Re-acquire on becoming visible again -- the OS/browser releases a
    // wake lock automatically when the tab is backgrounded, and it
    // doesn't come back on its own once the tab is foregrounded again.
    function onVisibilityChange() {
      if (document.visibilityState === "visible" && !sentinel && !cancelled) acquire();
    }

    acquire();
    document.addEventListener("visibilitychange", onVisibilityChange);

    return () => {
      cancelled = true;
      document.removeEventListener("visibilitychange", onVisibilityChange);
      sentinel?.release().catch(() => {});
      sentinel = null;
    };
  }, [active]);
}
