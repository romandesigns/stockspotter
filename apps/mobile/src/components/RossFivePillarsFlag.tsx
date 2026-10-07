import type { CatalystUpdate, FunnelSignal, RossFivePillarAssessment } from "@stockspotter/shared-types";
import { assessRossFivePillars } from "@stockspotter/shared-types";
import { Badge } from "./ui/badge";

export function RossFivePillarsFlag({ funnel, catalyst, assessment: suppliedAssessment }: { funnel?: FunnelSignal; catalyst?: CatalystUpdate; assessment?: RossFivePillarAssessment }) {
  const assessment = suppliedAssessment ?? assessRossFivePillars(funnel, catalyst);
  if (!assessment) return null;
  const confirmed = assessment.state === "confirmed";
  const label = confirmed ? "5 PILLARS" : `${assessment.passed}/5`;
  const catalystMessage = assessment.catalystStatus === "verified"
    ? "A fresh catalyst was detected."
    : assessment.catalystStatus === "none-reported"
      ? "The latest catalyst lookup reported no news; this does not prove that no catalyst exists."
      : "Catalyst status is unverified because current catalyst evidence is unavailable or incomplete.";
  const accessibilityLabel = confirmed
    ? `All five Ross Cameron stock-selection pillars currently verified. ${catalystMessage}`
    : `Ross Cameron pillars, ${assessment.passed} of 5 verified. ${catalystMessage} Remaining or unmet: ${assessment.remaining.join(", ")}`;
  return <Badge variant={confirmed ? "good" : "warning"} accessibilityLabel={accessibilityLabel}>{label}</Badge>;
}
