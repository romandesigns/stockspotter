import type { CatalystUpdate, FunnelSignal } from "@stockspotter/shared-types";
import { assessRossFivePillars } from "@stockspotter/shared-types";

export function RossFivePillarsBadge(props: { funnel?: FunnelSignal; catalyst?: CatalystUpdate }) {
  const assessment = assessRossFivePillars(props.funnel, props.catalyst);
  if (!assessment) return null;
  const catalystMessage = assessment.catalystStatus === "verified"
    ? "A fresh catalyst was detected."
    : assessment.catalystStatus === "none-reported"
      ? "The latest catalyst lookup reported no news; this does not prove that no catalyst exists."
      : "Catalyst status is unverified because current catalyst evidence is unavailable or incomplete.";
  const title = assessment.state === "confirmed"
    ? `All five Ross Cameron stock-selection pillars are currently verified. ${catalystMessage} This is descriptive, not a trade recommendation.`
    : `Forming candidate: ${assessment.passed}/5 verified. ${catalystMessage} Still missing or unmet: ${assessment.remaining.join(", ")}.`;
  const catalystSuffix = assessment.catalystStatus === "none-reported"
    ? " · NO NEWS"
    : assessment.catalystStatus === "unverified"
      ? " · NEWS ?"
      : " · NEWS";
  const label = assessment.state === "confirmed"
    ? `5 PILLARS${catalystSuffix}`
    : `FORMING ${assessment.passed}/5${catalystSuffix}`;
  return (
    <span className={`ross-pillars-badge ross-pillars-${assessment.state}`} title={title} aria-label={title}>
      {label}
    </span>
  );
}
