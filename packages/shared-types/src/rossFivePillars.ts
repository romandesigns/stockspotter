import type { CatalystUpdate, FunnelSignal } from "./index";

export type RossPillarName = "Price" | "Relative volume" | "Daily gain" | "News catalyst" | "Float";
export type RossFivePillarState = "forming" | "confirmed";
export type RossCatalystStatus = "verified" | "none-reported" | "unverified";
export interface RossFivePillarAssessment {
  state: RossFivePillarState;
  passed: number;
  total: 5;
  /** News is one of the five pillars; keep its evidence state explicit. */
  catalystStatus: RossCatalystStatus;
  /** Names of pillars whose current values are missing or fail the rule. */
  remaining: RossPillarName[];
}

/**
 * Conservative, display-only implementation of the five criteria in Ross
 * Cameron's Stock Selection guide: $1-$20 price, >=5x relative volume,
 * >=10% up on the day, a fresh news catalyst, and <10M float.
 *
 * The two-minute signal and 24-hour news freshness cutoffs are app safety
 * rules, not thresholds attributed to Ross. Unknown values never pass. "Forming" is shown only
 * once at least two of the five measured pillars pass; it is not a score,
 * probability, recommendation, or order trigger.
 */
export function assessRossFivePillars(
  funnel: FunnelSignal | undefined,
  catalyst: CatalystUpdate | undefined,
  nowMs = Date.now(),
): RossFivePillarAssessment | null {
  if (!funnel) return null;
  const signalAtMs = Date.parse(funnel.timestamp);
  const signalAgeMs = nowMs - signalAtMs;
  // Allow 30 seconds of server/client clock skew around the bar-close
  // timestamp, but never display materially future-dated or stale data.
  if (!Number.isFinite(signalAtMs) || signalAgeMs < -30_000 || signalAgeMs > 2 * 60 * 1000) return null;

  const catalystObservedAtMs = catalyst ? Date.parse(catalyst.timestamp) : NaN;
  const catalystObservationUsable = Boolean(
    catalyst && Number.isFinite(catalystObservedAtMs) &&
    catalystObservedAtMs <= nowMs + 30_000 && nowMs - catalystObservedAtMs <= 24 * 60 * 60 * 1000,
  );
  const publishedAtMs = catalyst?.mostRecentPublishedAt ? Date.parse(catalyst.mostRecentPublishedAt) : NaN;
  const freshNews = Boolean(
    catalystObservationUsable && catalyst && catalyst.catalystTags.length > 0 && catalyst.headlineCount > 0 &&
    catalyst.mostRecentHeadline?.trim() && Number.isFinite(publishedAtMs) &&
    publishedAtMs <= nowMs && nowMs - publishedAtMs <= 24 * 60 * 60 * 1000,
  );
  // A successful, recent lookup with no returned headlines is different from
  // missing/stale lookup data. Do not claim that no catalyst exists globally.
  const noCatalystReported = Boolean(
    catalystObservationUsable && catalyst && catalyst.headlineCount === 0 &&
    catalyst.catalystTags.length === 0 && catalyst.mostRecentHeadline == null,
  );
  const catalystStatus: RossCatalystStatus = freshNews
    ? "verified"
    : noCatalystReported
      ? "none-reported"
      : "unverified";
  const checks: [RossPillarName, boolean][] = [
    ["Price", Number.isFinite(funnel.price) && funnel.price >= 1 && funnel.price <= 20],
    ["Relative volume", funnel.relativeVolume != null && Number.isFinite(funnel.relativeVolume) && funnel.relativeVolume >= 5],
    ["Daily gain", Number.isFinite(funnel.gapPct) && funnel.gapPct >= 10],
    ["News catalyst", freshNews],
    ["Float", funnel.floatShares != null && Number.isFinite(funnel.floatShares) && funnel.floatShares > 0 && funnel.floatShares < 10_000_000],
  ];
  const passed = checks.filter(([, ok]) => ok).length;
  if (passed < 2) return null;
  return {
    state: passed === 5 ? "confirmed" : "forming",
    passed,
    total: 5,
    catalystStatus,
    remaining: checks.filter(([, ok]) => !ok).map(([name]) => name),
  };
}
