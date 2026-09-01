// Top Gainers panel -- ranked by session change % across the *whole*
// tracked universe (not just symbols that cleared the funnel's own
// thresholds; see market_data::movers's doc comment). Defaults to
// today's live rankings; the date toggle in the header switches to a
// one-off historical lookup for whichever past trading day is picked.

import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { ChartIcon } from "../ChartIcon";
import { formatPct, formatPrice, formatVolume } from "../../lib/format";
import type { Mover, TodayMovers } from "../../lib/useMovers";
import { useGainersForDate } from "../../lib/useMovers";
import { EmptyState, PanelShell } from "../PanelShell";

function todayIso(): string {
  return new Date().toISOString().slice(0, 10);
}

function formatDateLabel(iso: string): string {
  // Parsed as local midnight, not UTC -- otherwise a date typed in the
  // picker can display as the day before depending on timezone offset.
  const d = new Date(`${iso}T00:00:00`);
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

function DateToggle(props: { date: string | null; onChange: (date: string | null) => void }) {
  const max = todayIso();
  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button variant="outline" size="xs" aria-label="Choose session date" title="Choose session date">
          <ChartIcon name="calendar" />
          {props.date ? formatDateLabel(props.date) : "Today"}
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="movers-date-popover">
        <input
          type="date"
          className="movers-date-input"
          max={max}
          value={props.date ?? ""}
          onChange={(e) => props.onChange(e.target.value || null)}
        />
        {props.date && (
          <Button variant="ghost" size="xs" onClick={() => props.onChange(null)}>
            Reset to today
          </Button>
        )}
      </PopoverContent>
    </Popover>
  );
}

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
      headerExtra={<DateToggle date={date} onChange={setDate} />}
      className={props.className}
    >
      <MoversList rows={rows} emptyLabel={emptyLabel} />
    </PanelShell>
  );
}
