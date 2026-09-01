// Ported from the Super Chart prototype's real Backtest Replay
// date-range picker (stockspotter-super-chart-prototype memory) -- the
// same trading-day date math and calendar-grid algorithm (weekend
// disabling, month-nav bounded at the edges, dateKey/parseDateKey/
// fmtFull, now shared with ReplayRangePicker.tsx via lib/tradingDays.ts),
// not re-derived from a screenshot. Roman asked for this component
// specifically, not a lookalike (see feedback-reuse-dont-rederive
// memory).
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
import { buildMonthGrid, dateKey, DOW_NAMES, fmtFull, MONTH_NAMES, parseDateKey, previousSession, TODAY } from "../lib/tradingDays";

export function SessionDatePicker(props: { date: string | null; onChange: (date: string | null) => void }) {
  const selected = props.date ? parseDateKey(props.date) : null;
  const [viewYear, setViewYear] = useState((selected ?? TODAY).getUTCFullYear());
  const [viewMonth, setViewMonth] = useState((selected ?? TODAY).getUTCMonth());
  const [open, setOpen] = useState(false);

  const isLatestMonth = viewYear === TODAY.getUTCFullYear() && viewMonth === TODAY.getUTCMonth();
  const cells = buildMonthGrid(viewYear, viewMonth, (d) => d > TODAY);
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
