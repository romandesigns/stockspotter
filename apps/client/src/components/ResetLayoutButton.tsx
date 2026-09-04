// Nav-rail control for undoing an awkward panel drag (2026-09-04, part of
// the resizable/persisted dashboard layout ask) -- same rail-icon
// mechanism ReplayLauncher/WatchlistPopover/AutoTraderPopover already
// established, but this one's a plain action button, not a
// popover/dialog launcher.
//
// Clears every "dashboardLayout" localStorage key react-resizable-panels'
// autoSaveId props wrote (App.tsx's three PanelGroups each get their own
// namespaced autoSaveId, e.g. "stockspotter.dashboardLayout.row1.v1") --
// matching on the substring rather than an exact key list is deliberate:
// it's robust regardless of the library's own exact internal key-
// derivation format, since every autoSaveId this app passes already
// contains "dashboardLayout" and nothing else in this app's localStorage
// does (confirmed: useWatchlist.ts is the only other localStorage user,
// key "stockspotter.watchlist.v1", no overlap). `onReset` then bumps a
// counter in App.tsx that's used as the PanelGroup tree's own `key`,
// forcing a full remount so it re-initializes from defaultSize with
// nothing left to restore.
import { Button } from "@/components/ui/button";
import { ChartIcon } from "./ChartIcon";

export function ResetLayoutButton(props: { onReset: () => void }) {
  function handleClick() {
    for (const key of Object.keys(localStorage)) {
      if (key.includes("dashboardLayout")) localStorage.removeItem(key);
    }
    props.onReset();
  }

  return (
    <Button variant="ghost" size="icon" className="app-rail-btn" aria-label="Reset dashboard layout" title="Reset dashboard layout" onClick={handleClick}>
      <ChartIcon name="reset" />
    </Button>
  );
}
