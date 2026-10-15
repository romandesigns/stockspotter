// Compact tradability marker for a shortlist row, plus the full verdict
// badge DecisionCard uses -- one place mapping verdict -> label/color so
// the two can never disagree. The verdict itself is assessTradability's
// (shared-types), whose thresholds are heuristics, not backtested.

import type { TradabilityAssessment, TradabilityVerdict } from "@stockspotter/shared-types";
import { Badge } from "@/components/ui/badge";

const VERDICT_LABEL: Record<TradabilityVerdict, string> = {
  good: "Good",
  caution: "Caution",
  poor: "Poor",
  unknown: "Unknown",
};

function describe(assessment: TradabilityAssessment): string {
  const label = VERDICT_LABEL[assessment.verdict];
  return assessment.reasons.length > 0 ? `${label}: ${assessment.reasons.join(", ")}` : label;
}

export function TradabilityDot(props: { assessment: TradabilityAssessment }) {
  const description = describe(props.assessment);
  return (
    <span className={`tradability-dot tradability-${props.assessment.verdict}`} title={description} role="img" aria-label={description} />
  );
}

export function TradabilityBadge(props: { assessment: TradabilityAssessment }) {
  return (
    <Badge variant="outline" className={`tradability-badge tradability-${props.assessment.verdict}`} title={describe(props.assessment)}>
      <span className="tradability-dot" aria-hidden="true" />
      {VERDICT_LABEL[props.assessment.verdict]}
    </Badge>
  );
}
