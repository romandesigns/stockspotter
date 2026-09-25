// Inert stand-in for apps/client/src/lib/useAssessment.ts.
//
// The real hook POSTs /assess (a real, billed Claude call behind
// ws-server). The fixture drives momentum-free state, so the real hook
// would no-op anyway -- but it is stubbed rather than trusted to no-op
// so that the harness's controlled fetch queue only ever contains /bars
// requests, and a future change that starts assessing on mount cannot
// quietly turn these lifecycle tests into network tests.

export interface AssessmentResult {
  summary: string[];
  generatedAt: string;
}

export function useAssessment(_symbol: string | null, _momentum: unknown) {
  return {
    assessment: null as AssessmentResult | null,
    loading: false,
    error: false,
    regenerate: () => {},
  };
}
