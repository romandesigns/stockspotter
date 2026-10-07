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
    : `${assessment.passed} of 5 Ross Cameron stock-selection pillars are currently verified. ${catalystMessage} Still missing or unmet: ${assessment.remaining.join(", ")}.`;
  const label = assessment.state === "confirmed"
    ? "5 PILLARS"
    : `${assessment.passed}/5`;
  return (
    <span className={`ross-pillars-badge ross-pillars-${assessment.state}`} title={title} aria-label={title}>
      {label}
    </span>
  );
}
