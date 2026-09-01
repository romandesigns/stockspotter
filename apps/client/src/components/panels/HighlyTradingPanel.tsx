// Highly Trading panel -- stocks most active during the current session,
// ranked by raw session share volume across the whole tracked universe
// (see market_data::movers's doc comment on why raw volume, not relative
// volume: relative volume already has its own home in the funnel/halt
// panels). Always the live session -- no date toggle, unlike Top Gainers.

import type { CatalystUpdate } from "@stockspotter/shared-types";
import { MoversList } from "../MoversList";
import { UpdatedAgo } from "../UpdatedAgo";
import type { Mover } from "../../lib/useMovers";
import { PanelShell } from "../PanelShell";

export function HighlyTradingPanel(props: {
  rows: Mover[];
  lastUpdated: Date | null;
  catalystsBySymbol: Map<string, CatalystUpdate>;
  onSelectSymbol: (symbol: string) => void;
  className?: string;
}) {
  return (
    <PanelShell
      title="Highly Trading"
      subtitle="most active, current session"
      count={props.rows.length}
      headerExtra={<UpdatedAgo lastUpdated={props.lastUpdated} />}
      className={props.className}
    >
      <MoversList
        rows={props.rows}
        emptyLabel="Waiting for the universe scan's first pass…"
        catalystsBySymbol={props.catalystsBySymbol}
        onSelectSymbol={props.onSelectSymbol}
      />
    </PanelShell>
  );
}
