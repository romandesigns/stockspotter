// Ported from the Super Chart prototype's real Backtest Replay
// date-range picker (stockspotter-super-chart-prototype memory) -- the
// same trading-day date math and calendar-grid algorithm (weekend
// disabling, month-nav bounded at the edges, dateKey/parseDateKey/
// fmtFull), not re-derived from a screenshot. Roman asked for this
// component specifically, not a lookalike (see feedback-reuse-dont-
// rederive memory).
//
// Two real, deliberate departures from the original:
// 1. Single-date selection, not a range -- Top Gainers picks one past
//    session to rank, there's nothing to range over. The range-
//    start/range-end/in-range CSS classes are still reused verbatim
//    (a single day just gets both range-start AND range-end, which the
//    prototype's own CSS already renders as one solid rounded capsule --
//    a real, correct reuse of that rule, not a new one).
// 2. The session-count presets (Last session/3/5/10) don't make sense
//    for a single date -- replaced with Today/Yesterday, using the same
//    weekend-skipping walk the original's lastNSessions() did.
// 3. No EARLIEST_DATE bound -- the prototype was bounded to its 10-day
//    embedded demo dataset; this app's backend can look up any real past
//    trading day, so only the future is disabled (TODAY), not the past.
//
// The trigger/panel mechanism itself uses this app's real shadcn Popover
// instead of the prototype's own hand-built wireDropdown() -- wireDropdown
// was only ever a stand-in for real shadcn in an artifact that didn't
// have it yet (see stockspotter-client-architecture memory); now that
// real shadcn exists here, it's the actual thing wireDropdown imitated.

import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { ChartIcon } from "./ChartIcon";

const MONTH_NAMES = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const DOW_NAMES = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];

function dateOnly(d: Date): Date {
  return new Date(Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate()));
}
function isWeekend(d: Date): boolean {
  const w = d.getUTCDay();
  return w === 0 || w === 6;
}
function addDays(d: Date, n: number): Date {
  return new Date(d.getTime() + n * 86400000);
}
function pad2(n: number): string {
  return n < 10 ? `0${n}` : `${n}`;
}
function dateKey(d: Date): string {
  return `${d.getUTCFullYear()}-${pad2(d.getUTCMonth() + 1)}-${pad2(d.getUTCDate())}`;
}
function parseDateKey(k: string): Date {
  const [y, m, d] = k.split("-").map(Number);
  return new Date(Date.UTC(y, m - 1, d));
}
function fmtFull(d: Date): string {
  return `${MONTH_NAMES[d.getUTCMonth()]} ${d.getUTCDate()}, ${d.getUTCFullYear()}`;
}
/** Same weekday-only backward walk as the prototype's own lastNSessions,
 * specialized to "the one trading session immediately before `from`". */
function previousSession(from: Date): Date {
  let cur = addDays(dateOnly(from), -1);
  while (isWeekend(cur)) cur = addDays(cur, -1);
  return cur;
}

const TODAY = dateOnly(new Date());

interface CalendarCell {
  date: Date;
  key: string;
  disabled: boolean;
}

/** Real port of the prototype's renderCalendar() grid-building logic --
 * same offset/blank-cell/weekend-disabling shape, just returning data
 * instead of an innerHTML string. */
function buildMonthGrid(viewYear: number, viewMonth: number): (CalendarCell | null)[] {
  const first = new Date(Date.UTC(viewYear, viewMonth, 1));
  const startOffset = first.getUTCDay();
  const daysInMonth = new Date(Date.UTC(viewYear, viewMonth + 1, 0)).getUTCDate();
  const cells: (CalendarCell | null)[] = [];
  for (let i = 0; i < startOffset; i++) cells.push(null);
  for (let d = 1; d <= daysInMonth; d++) {
    const date = new Date(Date.UTC(viewYear, viewMonth, d));
    cells.push({ date, key: dateKey(date), disabled: isWeekend(date) || date > TODAY });
  }
  return cells;
}

export function SessionDatePicker(props: { date: string | null; onChange: (date: string | null) => void }) {
  const selected = props.date ? parseDateKey(props.date) : null;
  const [viewYear, setViewYear] = useState((selected ?? TODAY).getUTCFullYear());
  const [viewMonth, setViewMonth] = useState((selected ?? TODAY).getUTCMonth());
  const [open, setOpen] = useState(false);

  const isLatestMonth = viewYear === TODAY.getUTCFullYear() && viewMonth === TODAY.getUTCMonth();
  const cells = buildMonthGrid(viewYear, viewMonth);
  const selectedKey = selected ? dateKey(selected) : null;

  function navigate(delta: number) {
    let m = viewMonth + delta;
    let y = viewYear;
    y += Math.floor(m / 12);
    m = ((m % 12) + 12) % 12;
    setViewYear(y);
    setViewMonth(m);
  }

  function jumpTo(date: Date | null) {
    props.onChange(date ? dateKey(date) : null);
    setViewYear((date ?? TODAY).getUTCFullYear());
    setViewMonth((date ?? TODAY).getUTCMonth());
    setOpen(false);
  }

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button variant="outline" size="xs" aria-label="Choose session date" title="Choose session date">
          <ChartIcon name="calendar" />
          {selected ? fmtFull(selected) : "Today"}
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="range-popover">
        <div className="range-body">
          <div className="range-presets">
            <button type="button" className={`range-preset${!props.date ? " active" : ""}`} onClick={() => jumpTo(null)}>
              Today
            </button>
            <button type="button" className="range-preset" onClick={() => jumpTo(previousSession(TODAY))}>
              Yesterday
            </button>
            <div className="range-divider" />
            <div className="range-custom-label">Custom date</div>
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
              {cells.map((cell, i) =>
                cell ? (
                  <button
                    key={cell.key}
                    type="button"
                    className={`cal-day${cell.key === selectedKey ? " range-start range-end" : ""}`}
                    disabled={cell.disabled}
                    onClick={() => jumpTo(cell.date)}
                  >
                    {cell.date.getUTCDate()}
                  </button>
                ) : (
                  <div key={`blank-${i}`} />
                ),
              )}
            </div>
          </div>
        </div>
      </PopoverContent>
    </Popover>
  );
}
