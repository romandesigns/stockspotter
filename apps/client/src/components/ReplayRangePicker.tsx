// Real port of the Super Chart prototype's Backtest Replay date-RANGE
// picker (stockspotter-super-chart-prototype memory) -- session-count
// presets (Last session/3/5/10 sessions) plus a real two-endpoint
// calendar (click once for the start day, click again for the end),
// using the exact date math/calendar-grid algorithm SessionDatePicker.tsx
// also uses, shared via lib/tradingDays.ts. Unlike SessionDatePicker
// (Top Gainers' single-date simplification), this is the genuine range
// picker -- range-start/range-end/in-range CSS classes are all actually
// exercised here, not collapsed to one day.
//
// Trigger/panel mechanism: this app's real shadcn Popover, same as
// SessionDatePicker -- see that component's own doc comment on why
// (wireDropdown was a stand-in for real shadcn the prototype didn't have
// yet).

import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { ChartIcon } from "./ChartIcon";
import {
  buildMonthGrid,
  dateKey,
  DOW_NAMES,
  fmtFull,
  fmtShort,
  lastNSessions,
  MONTH_NAMES,
  parseDateKey,
  TODAY,
} from "../lib/tradingDays";

const PRESETS = [1, 3, 5, 10] as const;
/** Mirrors ws-server's own MAX_REPLAY_SPAN_DAYS (http.rs) -- a custom
 * range wider than the backend will actually serve isn't offered as a
 * selectable end date, rather than letting the fetch fail after the
 * fact. */
const MAX_RANGE_DAYS = 45;

function fmtRangeLabel(start: Date, end: Date): string {
  return dateKey(start) === dateKey(end) ? fmtFull(start) : `${fmtShort(start)} – ${fmtFull(end)}`;
}

export function ReplayRangePicker(props: {
  start: string | null;
  end: string | null;
  onChange: (start: string, end: string) => void;
}) {
  const selStart = props.start ? parseDateKey(props.start) : null;
  const selEnd = props.end ? parseDateKey(props.end) : null;
  const [viewYear, setViewYear] = useState((selEnd ?? TODAY).getUTCFullYear());
  const [viewMonth, setViewMonth] = useState((selEnd ?? TODAY).getUTCMonth());
  const [open, setOpen] = useState(false);
  // Mid-selection state: set once the user has clicked a start day and is
  // now picking the end day -- mirrors the prototype's own
  // calSelStart/calSelEnd pairing (calSelEnd only set on the *second*
  // click).
  const [pendingStart, setPendingStart] = useState<Date | null>(null);
  const [activePreset, setActivePreset] = useState<number | null>(5);

  const isLatestMonth = viewYear === TODAY.getUTCFullYear() && viewMonth === TODAY.getUTCMonth();
  const cells = buildMonthGrid(viewYear, viewMonth, (d) => {
    if (d > TODAY) return true;
    if (pendingStart) {
      const spanDays = Math.abs((d.getTime() - pendingStart.getTime()) / 86400000);
      if (spanDays > MAX_RANGE_DAYS) return true;
    }
    return false;
  });

  function navigate(delta: number) {
    let m = viewMonth + delta;
    let y = viewYear;
    y += Math.floor(m / 12);
    m = ((m % 12) + 12) % 12;
    setViewYear(y);
    setViewMonth(m);
  }

  function pickPreset(n: number) {
    const days = lastNSessions(n, TODAY);
    setActivePreset(n);
    setPendingStart(null);
    setViewYear(days[days.length - 1].getUTCFullYear());
    setViewMonth(days[days.length - 1].getUTCMonth());
    props.onChange(dateKey(days[0]), dateKey(days[days.length - 1]));
    setOpen(false);
  }

  function pickDay(date: Date) {
    setActivePreset(null);
    if (!pendingStart) {
      setPendingStart(date);
      return;
    }
    let a = pendingStart;
    let b = date;
    if (b < a) [a, b] = [b, a];
    setPendingStart(null);
    props.onChange(dateKey(a), dateKey(b));
    setOpen(false);
  }

  const rangeLo = pendingStart ? dateKey(pendingStart) : selStart ? dateKey(selStart) : null;
  const rangeHi = pendingStart ? null : selEnd ? dateKey(selEnd) : null;

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button variant="outline" size="xs" aria-label="Choose replay date range" title="Choose replay date range">
          <ChartIcon name="calendar" />
          {selStart && selEnd ? fmtRangeLabel(selStart, selEnd) : "Choose range"}
        </Button>
      </PopoverTrigger>
      <PopoverContent align="start" className="range-popover">
        <div className="range-body">
          <div className="range-presets">
            {PRESETS.map((n) => (
              <button
                key={n}
                type="button"
                className={`range-preset${activePreset === n ? " active" : ""}`}
                onClick={() => pickPreset(n)}
              >
                Last {n} session{n === 1 ? "" : "s"}
              </button>
            ))}
            <div className="range-divider" />
            <div className="range-custom-label">Custom range</div>
          </div>
          <div className="range-calendar">
            <div className="cal-head">
              <button type="button" onClick={() => navigate(-1)} aria-label="Previous month">
                <ChartIcon name="chevron-l" />
              </button>
              <span className="cal-title">
                {MONTH_NAMES[viewMonth]} {viewYear}
              </span>
              <button type="button" onClick={() => navigate(1)} disabled={isLatestMonth} aria-label="Next month">
                <ChartIcon name="chevron-r" />
              </button>
            </div>
            <div className="cal-grid">
              {DOW_NAMES.map((n) => (
                <div key={n} className="cal-dow">
                  {n}
                </div>
              ))}
              {cells.map((cell, i) => {
                if (!cell) return <div key={`blank-${i}`} />;
                let cls = "cal-day";
                if (rangeLo && cell.key === rangeLo) cls += " range-start";
                if (rangeHi && cell.key === rangeHi) cls += " range-end";
                if (rangeLo && rangeHi && cell.key > rangeLo && cell.key < rangeHi) cls += " in-range";
                return (
                  <button key={cell.key} type="button" className={cls} disabled={cell.disabled} onClick={() => pickDay(cell.date)}>
                    {cell.date.getUTCDate()}
                  </button>
                );
              })}
            </div>
          </div>
        </div>
      </PopoverContent>
    </Popover>
  );
}
