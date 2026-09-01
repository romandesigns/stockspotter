// Top Gainers panel -- ranked by session change % across the *whole*
// tracked universe (not just symbols that cleared the funnel's own
// thresholds; see market_data::movers's doc comment). Defaults to
// today's live rankings; the date toggle in the header switches to a
// one-off historical lookup for whichever past trading day is picked --
// the toggle itself is SessionDatePicker, a real port of the Backtest
// Replay prototype's own date-range picker (see its own doc comment).

import { useState } from "react";
import { SessionDatePicker } from "../SessionDatePicker";
import { formatPct, formatPrice, formatVolume } from "../../lib/format";
import type { Mover, TodayMovers } from "../../lib/useMovers";
import { useGainersForDate } from "../../lib/useMovers";
import { EmptyState, PanelShell } from "../PanelShell";

function MoversList(props: { rows: Mover[]; emptyLabel: string }) {
  if (props.rows.length === 0) {
    return <EmptyState>{props.emptyLabel}</EmptyState>;
  }
  return (
    <ul className="feed">
      {props.rows.map((r, i) => (
        <li key={r.symbol} className="feed-row">
          <div className="feed-row-main">
            <span className="dim movers-rank">{i + 1}</span>
            <span className="ticker">{r.symbol}</span>
            <span className="price">{formatPrice(r.price)}</span>
            <span className={r.changePct >= 0 ? "pct-up" : "pct-down"}>{formatPct(r.changePct)}</span>
            <span className="dim">{formatVolume(r.volume)} vol</span>
          </div>
        </li>
      ))}
    </ul>
  );
}

export function TopGainersPanel(props: { today: TodayMovers; className?: string }) {
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
      headerExtra={<SessionDatePicker date={date} onChange={setDate} />}
      className={props.className}
    >
      <MoversList rows={rows} emptyLabel={emptyLabel} />
    </PanelShell>
  );
}
