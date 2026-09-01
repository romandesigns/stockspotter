// Left nav rail's launcher for the Super Chart prototype's Backtest
// Replay scenario (stockspotter-super-chart-prototype memory) -- a real
// shadcn Dialog, not an iframe embed: the published Artifact's own CSP
// sends `frame-ancestors 'self'` (confirmed via a direct curl against
// the real URL), so a third-party page genuinely cannot embed it inline.
// The dialog is an honest launcher instead -- explains what it is and
// opens the real prototype in a new tab, rather than pretending it's
// inline when it can't be.
//
// Backtest Replay itself is still a design prototype, not yet ported
// into this app (per that memory's own "still genuinely deferred" list)
// -- the dialog says so plainly rather than implying it's a live feature
// reachable from here.

import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle, DialogTrigger } from "@/components/ui/dialog";
import { ChartIcon } from "./ChartIcon";

const REPLAY_PROTOTYPE_URL = "https://claude.ai/code/artifact/49c333f5-76c2-4acc-8abc-68d465e773e5";

export function ReplayLauncher() {
  return (
    <Dialog>
      <DialogTrigger asChild>
        <Button variant="ghost" size="icon" className="app-rail-btn" aria-label="Backtest Replay" title="Backtest Replay (prototype)">
          <ChartIcon name="replay" />
        </Button>
      </DialogTrigger>
      <DialogContent className="replay-dialog">
        <DialogHeader>
          <DialogTitle>Backtest Replay</DialogTitle>
          <DialogDescription>
            The Super Chart engine's design prototype -- real historical data (5 tracked symbols, 10 real trading
            days), a session date-range picker, and tick-by-tick playback. This scenario hasn't been ported into the
            live app yet (Scanner Detail has); opens in a new tab.
          </DialogDescription>
        </DialogHeader>
        <Button asChild>
          <a href={REPLAY_PROTOTYPE_URL} target="_blank" rel="noopener noreferrer">
            Open Backtest Replay ↗
          </a>
        </Button>
      </DialogContent>
    </Dialog>
  );
}
