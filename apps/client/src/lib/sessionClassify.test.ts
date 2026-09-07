// Session-boundary tests.
//
// The reason this is worth testing at all is stated in the module's own
// header: classification must come from real America/New_York wall-clock
// time, not a fixed UTC offset, or it silently misclassifies half the
// year. So the cases below deliberately straddle the DST boundary and
// assert the SAME ET clock time lands in the same session on both sides
// of it — a fixed-offset implementation passes the summer cases and
// fails the winter ones.

import { describe, expect, test } from "bun:test";
import { classifySession, formatBarClock } from "./sessionClassify";

/** unix seconds for a given ET wall-clock time on a given date.
 *
 * Built by adding the offset in milliseconds rather than by formatting
 * an hour string, so ET times whose UTC hour rolls past midnight (20:00
 * EST is 01:00 UTC the next day) produce a real instant instead of an
 * invalid "T25:00:00Z". */
function etTime(date: string, hhmm: string, offsetHours: number): number {
  const [h, m] = hhmm.split(":").map(Number);
  const midnightUtc = Date.parse(`${date}T00:00:00Z`);
  return (midnightUtc + ((h + offsetHours) * 60 + m) * 60_000) / 1000;
}

// EDT (UTC-4) in August; EST (UTC-5) in January.
const summer = (hhmm: string) => etTime("2026-08-28", hhmm, 4);
const winter = (hhmm: string) => etTime("2026-01-15", hhmm, 5);

describe("classifySession", () => {
  test("classifies each session in summer (EDT)", () => {
    expect(classifySession(summer("03:59"))).toBe("closed");
    expect(classifySession(summer("04:00"))).toBe("pre");
    expect(classifySession(summer("09:29"))).toBe("pre");
    expect(classifySession(summer("09:30"))).toBe("regular");
    expect(classifySession(summer("15:59"))).toBe("regular");
    expect(classifySession(summer("16:00"))).toBe("after");
    expect(classifySession(summer("19:59"))).toBe("after");
    expect(classifySession(summer("20:00"))).toBe("closed");
  });

  test("classifies identically in winter (EST) — the DST case", () => {
    // Same ET clock times, one hour further from UTC. A fixed-offset
    // implementation gets all of these wrong by an hour.
    expect(classifySession(winter("03:59"))).toBe("closed");
    expect(classifySession(winter("04:00"))).toBe("pre");
    expect(classifySession(winter("09:29"))).toBe("pre");
    expect(classifySession(winter("09:30"))).toBe("regular");
    expect(classifySession(winter("15:59"))).toBe("regular");
    expect(classifySession(winter("16:00"))).toBe("after");
    expect(classifySession(winter("20:00"))).toBe("closed");
  });

  test("the 9:30 open boundary is inclusive of regular, not of pre", () => {
    // The single most consequential boundary here: it's the same one
    // halt_detector::bands::luld_in_effect uses to decide whether a LULD
    // band exists at all, so the two must not disagree.
    expect(classifySession(summer("09:29"))).toBe("pre");
    expect(classifySession(summer("09:30"))).toBe("regular");
  });

  test("overnight is closed", () => {
    expect(classifySession(summer("00:30"))).toBe("closed");
    expect(classifySession(summer("23:00"))).toBe("closed");
  });
});

describe("formatBarClock", () => {
  test("renders ET wall-clock time, not the host timezone", () => {
    // 13:30 UTC in August is 09:30 ET. If this returned the machine's
    // local time the whole replay session-shading would be wrong for
    // anyone not sitting in New York.
    expect(formatBarClock(Date.parse("2026-08-28T13:30:00Z") / 1000)).toContain("09:30");
  });
});
