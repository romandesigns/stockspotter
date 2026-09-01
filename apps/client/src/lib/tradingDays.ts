// Real trading-day date math ported from the Super Chart prototype's
// Backtest Replay date-range picker (stockspotter-super-chart-prototype
// memory) -- extracted into its own module so SessionDatePicker.tsx
// (Top Gainers' single-date picker) and ReplayRangePicker.tsx (Backtest
// Replay's real two-endpoint range picker) share one real source instead
// of each carrying its own copy of the same math. Same "reuse, don't
// re-derive" principle applied to this app's own code, not just the
// prototype's.

export const MONTH_NAMES = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
export const DOW_NAMES = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];

export function dateOnly(d: Date): Date {
  return new Date(Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate()));
}
export function isWeekend(d: Date): boolean {
  const w = d.getUTCDay();
  return w === 0 || w === 6;
}
export function addDays(d: Date, n: number): Date {
  return new Date(d.getTime() + n * 86400000);
}
function pad2(n: number): string {
  return n < 10 ? `0${n}` : `${n}`;
}
export function dateKey(d: Date): string {
  return `${d.getUTCFullYear()}-${pad2(d.getUTCMonth() + 1)}-${pad2(d.getUTCDate())}`;
}
export function parseDateKey(k: string): Date {
  const [y, m, d] = k.split("-").map(Number);
  return new Date(Date.UTC(y, m - 1, d));
}
export function fmtShort(d: Date): string {
  return `${MONTH_NAMES[d.getUTCMonth()]} ${d.getUTCDate()}`;
}
export function fmtFull(d: Date): string {
  return `${MONTH_NAMES[d.getUTCMonth()]} ${d.getUTCDate()}, ${d.getUTCFullYear()}`;
}

export const TODAY = dateOnly(new Date());

/** Same weekday-only backward walk as the prototype's own lastNSessions,
 * specialized to "the one trading session immediately before `from`". */
export function previousSession(from: Date): Date {
  let cur = addDays(dateOnly(from), -1);
  while (isWeekend(cur)) cur = addDays(cur, -1);
  return cur;
}

/** Real port of the prototype's own lastNSessions() -- walks backward
 * from `endDate` (inclusive), collecting the `n` most recent weekdays. */
export function lastNSessions(n: number, endDate: Date): Date[] {
  const out: Date[] = [];
  let cur = new Date(endDate);
  while (out.length < n) {
    if (!isWeekend(cur)) out.unshift(new Date(cur));
    cur = addDays(cur, -1);
  }
  return out;
}

/** Real port of the prototype's own enumerateWeekdays(). */
export function enumerateWeekdays(start: Date, end: Date): Date[] {
  const out: Date[] = [];
  let cur = new Date(start);
  while (cur <= end) {
    if (!isWeekend(cur)) out.push(new Date(cur));
    cur = addDays(cur, 1);
  }
  return out;
}

export interface CalendarCell {
  date: Date;
  key: string;
  disabled: boolean;
}

/** Real port of the prototype's renderCalendar() grid-building logic --
 * same offset/blank-cell/weekend-disabling shape, just returning data
 * instead of an innerHTML string. `isDisabled` lets each caller apply its
 * own bounds (SessionDatePicker only disables the future; ReplayRangePicker
 * additionally disables anything the picked start endpoint couldn't
 * legally range to) on top of the always-true weekend rule. */
export function buildMonthGrid(viewYear: number, viewMonth: number, isDisabled: (d: Date) => boolean): (CalendarCell | null)[] {
  const first = new Date(Date.UTC(viewYear, viewMonth, 1));
  const startOffset = first.getUTCDay();
  const daysInMonth = new Date(Date.UTC(viewYear, viewMonth + 1, 0)).getUTCDate();
  const cells: (CalendarCell | null)[] = [];
  for (let i = 0; i < startOffset; i++) cells.push(null);
  for (let d = 1; d <= daysInMonth; d++) {
    const date = new Date(Date.UTC(viewYear, viewMonth, d));
    cells.push({ date, key: dateKey(date), disabled: isWeekend(date) || isDisabled(date) });
  }
  return cells;
}
