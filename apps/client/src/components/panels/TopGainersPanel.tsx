// Top Gainers panel -- ranked by session change % across the *whole*
// tracked universe (not just symbols that cleared the funnel's own
// thresholds; see market_data::movers's doc comment). Defaults to
// today's live rankings; the date toggle in the header switches to a
// one-off historical lookup for whichever past trading day is picked --
// the toggle itself is SessionDatePicker, a real port of the Backtest
// Replay prototype's own date-range picker (see its own doc comment).

import { useState } from "react";
import type { CatalystUpdate } from "@stockspotter/shared-types";
import { MoversList } from "../MoversList";
import { SessionDatePicker } from "../SessionDatePicker";
import { UpdatedAgo } from "../UpdatedAgo";
import type { TodayMovers } from "../../lib/useMovers";
import { useGainersForDate } from "../../lib/useMovers";
import { PanelShell } from "../PanelShell";

export function TopGainersPanel(props: {
  today: TodayMovers;
  catalystsBySymbol: Map<string, CatalystUpdate>;
  onSelectSymbol: (symbol: string) => void;
  className?: string;
}) {
  const [date, setDate] = useState<string | null>(null);
  const historical = useGainersForDate(date);

  const rows = date ? historical.rows : props.today.gainers;
  const emptyLabel = date
    ? historical.loading
      ? "Scanning that session…"
      : historical.error
        ? "Couldn't load that session — try another date."
        : "No gainers recorded for that session."
    : "Waiting for the universe scan's first pass…";

  return (
    <PanelShell
      title="Top Gainers"
      subtitle={date ? `session: ${date}` : "today's session, live"}
      count={rows.length}
      headerExtra={
        <>
          {/* Only meaningful for the live default (no date picked) --
              a historical session is a one-off snapshot, not something
              that "updates". */}
          {!date && <UpdatedAgo lastUpdated={props.today.lastUpdated} />}
          <SessionDatePicker date={date} onChange={setDate} />
        </>
      }
      className={props.className}
    >
      <MoversList rows={rows} emptyLabel={emptyLabel} catalystsBySymbol={props.catalystsBySymbol} onSelectSymbol={props.onSelectSymbol} />
    </PanelShell>
  );
}
