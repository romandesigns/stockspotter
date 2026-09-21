// Candle coverage: completeness, authority precedence, and time-finality.
//
// Three distinct properties get conflated easily, so they are named and
// ordered here once rather than re-derived per call site:
//
//   AUTHORITY     who says so -- a provider official bar outranks anything
//                 we aggregated locally.
//   COMPLETENESS  did the producer observe the whole interval.
//   TIME-FINALITY has the interval elapsed. Derivable from the clock.
//
// The 2026-09-21 audit showed why the first two must stay separate: a bar
// can be time-final and still coverage-partial (30s bucket 15:37:00-30 with
// observation beginning 15:37:12), and a locally aggregated bar is never
// authoritative no matter how complete its coverage.

import type { BarUpdate, Coverage } from "./index";

/** Absent coverage is `unknown`, never `complete`.
 *
 * This default is the whole point. Every frame from a server predating the
 * field arrives without it, and assuming completeness there would silently
 * reintroduce the exact lie the field exists to prevent -- on the majority
 * of traffic, until every server is upgraded. */
export function coverageOf(bar: Pick<BarUpdate, "coverage">): Coverage {
  return bar.coverage ?? { state: "unknown" };
}

/**
 * Precedence for deciding whether an incoming version of a bucket may
 * replace one already held. Higher wins.
 *
 *   3  provider official/corrected  (isFinal)
 *   2  locally complete coverage
 *   1  coverage unknown
 *   0  locally partial coverage
 *
 * The ordering exists to enforce one invariant from the 2026-09-21 brief:
 * a partial lower-authority candle must never degrade a complete
 * higher-authority one. The concrete failure it prevents was measured -- an
 * authoritative REST bar with volume 1,000 being overwritten by a partially
 * observed live bar with volume 3.
 *
 * `unknown` sits ABOVE partial and below complete deliberately. Ranking it
 * lowest would let a known-partial bar overwrite an old-server bar that may
 * well have been complete; ranking it highest would let an old-server frame
 * overwrite a bar we know to be complete.
 */
export function barAuthority(bar: Pick<BarUpdate, "coverage" | "isFinal">): number {
  if (bar.isFinal) return 3;
  const c = coverageOf(bar);
  if (c.state === "complete") return 2;
  if (c.state === "unknown") return 1;
  return 0;
}

/**
 * May `incoming` replace `existing` for the same bucket?
 *
 * Equal authority replaces, because that is ordinary live updating: a
 * forming bucket's newer provisional state supersedes its older one. Only a
 * genuine DOWNGRADE is refused.
 */
export function mayReplace(
  existing: Pick<BarUpdate, "coverage" | "isFinal"> | undefined,
  incoming: Pick<BarUpdate, "coverage" | "isFinal">,
): boolean {
  if (!existing) return true;
  return barAuthority(incoming) >= barAuthority(existing);
}

/** Has the bucket's interval elapsed? Derived, never taken from the wire. */
export function isTimeFinal(bucketStartMs: number, intervalSecs: number, nowMs: number): boolean {
  return nowMs >= bucketStartMs + intervalSecs * 1000;
}

/** True only when completeness is positively established. Unknown is not
 *  complete -- that is the conservative reading the contract requires. */
export function isCoverageComplete(bar: Pick<BarUpdate, "coverage" | "isFinal">): boolean {
  return bar.isFinal === true || coverageOf(bar).state === "complete";
}

/** True only when incompleteness is positively established. Unknown is not
 *  partial either, so neither predicate is the negation of the other -- the
 *  third state is real and both callers must handle it. */
export function isCoveragePartial(bar: Pick<BarUpdate, "coverage" | "isFinal">): boolean {
  if (bar.isFinal === true) return false;
  return coverageOf(bar).state === "partial";
}
