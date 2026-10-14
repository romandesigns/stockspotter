export const RESEARCH_PREFERENCE_KEY = "stockspotter.researchPanels.v1";

export const RESEARCH_RECONNECT_BASE_MS = 3000;

export function researchBuildDefault(): boolean {
  return (import.meta as { env?: Record<string, string> }).env?.VITE_EXPERIMENTAL_STRATEGIES === "true";
}

type Reader = Pick<Storage, "getItem">;
type Writer = Pick<Storage, "setItem">;

export function readResearchPreference(storage: Reader | null, fallback: boolean): boolean {
  try {
    const value = storage?.getItem(RESEARCH_PREFERENCE_KEY);
    if (value === "true") return true;
    if (value === "false") return false;
  } catch { /* A storage failure must not stop the ordinary app. */ }
  return fallback;
}

export function saveResearchPreference(storage: Writer | null, enabled: boolean): boolean {
  if (!storage) return false;
  try { storage.setItem(RESEARCH_PREFERENCE_KEY, String(enabled)); return true; }
  catch { return false; }
}

/** Capacity is temporary; disabled, invalid credentials and protocol rejection
 * need a user retry after configuration changes, not an endless reconnect. */
export function retryResearchRejection(reason: unknown): boolean {
  return reason === "Experimental connection capacity reached" || reason === "Connection capacity reached";
}
