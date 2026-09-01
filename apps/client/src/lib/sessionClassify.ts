// Classifies a bar's real timestamp into pre-market / regular / after-
// hours, matching the Super Chart prototype's own Backtest Replay session
// boundaries (4:00-9:30 pre, 9:30-16:00 regular, 16:00-20:00 after -- see
// SESSION_LABEL/pre/regular/after in the prototype source). The prototype
// tagged each of its synthetic/embedded bars with a session at data-
// generation time; this app's bars come from a real Alpaca fetch with
// only a real UTC timestamp, so classification has to happen for real,
// off the actual wall-clock time in America/New_York -- not re-derived
// with a fixed UTC offset, which would silently misclassify half the
// year across the DST boundary.

export type Session = "pre" | "regular" | "after" | "closed";

const ET_FORMATTER = new Intl.DateTimeFormat("en-US", {
  timeZone: "America/New_York",
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
});

/** unix seconds -> minutes since ET midnight, DST-correct (reads the
 * real wall-clock hour/minute Intl computes for America/New_York, not a
 * fixed UTC-4/UTC-5 offset). */
function etMinutesSinceMidnight(unixSeconds: number): number {
  const parts = ET_FORMATTER.formatToParts(new Date(unixSeconds * 1000));
  const hour = Number(parts.find((p) => p.type === "hour")?.value ?? "0");
  const minute = Number(parts.find((p) => p.type === "minute")?.value ?? "0");
  return hour * 60 + minute;
}

const PRE_START = 4 * 60; // 4:00
const REGULAR_START = 9 * 60 + 30; // 9:30
const REGULAR_END = 16 * 60; // 16:00
const AFTER_END = 20 * 60; // 20:00

export function classifySession(unixSeconds: number): Session {
  const m = etMinutesSinceMidnight(unixSeconds);
  if (m >= PRE_START && m < REGULAR_START) return "pre";
  if (m >= REGULAR_START && m < REGULAR_END) return "regular";
  if (m >= REGULAR_END && m < AFTER_END) return "after";
  return "closed";
}

export const SESSION_LABEL: Record<Session, string> = {
  pre: "PRE",
  regular: "RTH",
  after: "AH",
  closed: "—",
};

const ET_DATETIME_FORMATTER = new Intl.DateTimeFormat("en-US", {
  timeZone: "America/New_York",
  month: "short",
  day: "numeric",
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
});

const ET_CLOCK_FORMATTER = new Intl.DateTimeFormat("en-US", {
  timeZone: "America/New_York",
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
  hourCycle: "h23",
});

/** "Aug 26, 20:14" -- the replay dialog's own scrub readout, real ET
 * wall-clock time (matches the prototype's own "Aug 26, 20:14 · AH"
 * label shape; the " · AH" part is SESSION_LABEL, appended by the
 * caller). */
export function formatBarDateTime(unixSeconds: number): string {
  return ET_DATETIME_FORMATTER.format(new Date(unixSeconds * 1000));
}

/** "09:30:00" -- real ET wall-clock time only, no date. */
export function formatBarClock(unixSeconds: number): string {
  return ET_CLOCK_FORMATTER.format(new Date(unixSeconds * 1000));
}
