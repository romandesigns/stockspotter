// Highly Trading panel -- stocks most active during the current session,
// ranked by raw session share volume across the whole tracked universe
// (see market_data::movers's doc comment on why raw volume, not relative
// volume: relative volume already has its own home in the funnel/halt
// panels). Always the live session -- no date toggle, unlike Top Gainers.

import type { CatalystUpdate, FunnelSignal } from "@stockspotter/shared-types";
import { useState } from "react";
import { MoversList } from "../MoversList";
import { UpdatedAgo } from "../UpdatedAgo";
import type { Mover, TodayMovers } from "../../lib/useMovers";
import { PanelShell } from "../PanelShell";
import { MoverRefreshStatus } from "../MoverRefreshStatus";
import { moverEmptyState } from "../../lib/moverEmptyState";

export function HighlyTradingPanel(props: {
  today: TodayMovers;
  rows: Mover[];
  peakRows: Mover[];
  currentAvailable: boolean;
  lastUpdated: Date | null;
  catalystsBySymbol: Map<string, CatalystUpdate>;
  funnelBySymbol?: Map<string, FunnelSignal>;
  saved: Set<string>;
  onToggleSaved: (symbol: string) => void;
  onSelectSymbol: (symbol: string) => void;
  className?: string;
}) {
  const [peaks, setPeaks] = useState(false);
  const showPeaks = peaks || !props.currentAvailable;
  return (
    <PanelShell
      title="Highly Trading"
      subtitle={showPeaks ? "peak snapshots" : "most active, current snapshot"}
      count={(showPeaks ? props.peakRows : props.rows).length}
      headerExtra={<><MoverRefreshStatus today={props.today} /><select aria-label="Highly Trading view" value={showPeaks ? "peak" : "current"} onChange={(e) => setPeaks(e.target.value === "peak")}><option value="current" disabled={!props.currentAvailable}>Current</option><option value="peak">Peak</option></select><UpdatedAgo lastUpdated={props.lastUpdated} /></>}
      className={props.className}
    >
      <MoversList
        rows={showPeaks ? props.peakRows : props.rows}
        peak={showPeaks}
        emptyLabel={moverEmptyState(props.today, showPeaks)}
        catalystsBySymbol={props.catalystsBySymbol}
        funnelBySymbol={showPeaks ? undefined : props.funnelBySymbol}
        saved={props.saved}
        onToggleSaved={props.onToggleSaved}
        onSelectSymbol={props.onSelectSymbol}
      />
    </PanelShell>
  );
}
