export function sourceCommit(value: unknown): string | null {
  return typeof value === "string" && /^[0-9a-f]{40}$/.test(value) ? value : null;
}
export function nativeSourceCommit(version: string | null): string | null {
  return version?.match(/^\d+\.\d+\.\d+\+git\.([0-9a-f]{40})$/)?.[1] ?? null;
}
export function backendSourceCommit(value: unknown): string | null {
  if (!value || typeof value !== "object" || !("report" in value)) return null;
  const report = value.report;
  return report && typeof report === "object" && "commit" in report ? sourceCommit(report.commit) : null;
}