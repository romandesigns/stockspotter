function isoDate(d: Date): string {
  return d.toISOString().slice(0, 10);
}

/** Most recent weekday before `from` -- same weekend-skipping logic the
 * web app's tradingDays.ts uses, ported to the one case mobile needs. */
export function previousSession(from: Date): string {
  const parts = new Intl.DateTimeFormat("en-US", { timeZone: "America/New_York", year: "numeric", month: "2-digit", day: "2-digit" }).formatToParts(from);
  const part = (name: string) => Number(parts.find(value => value.type === name)?.value);
  const cur = new Date(Date.UTC(part("year"), part("month") - 1, part("day")));
  cur.setUTCDate(cur.getUTCDate() - 1);
  while (cur.getUTCDay() === 0 || cur.getUTCDay() === 6) cur.setUTCDate(cur.getUTCDate() - 1);
  return isoDate(cur);
}

