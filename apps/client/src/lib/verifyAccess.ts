export type AccessVerdict = "accepted" | "rejected" | "unknown";

// A transport failure must never discard a previously accepted saved key.
// Admission on "unknown" belongs to the caller: restore and submit differ.
export async function verifyAccessKey(
  url: string,
  key: string,
  options: { signal?: AbortSignal; timeoutMs?: number } = {},
): Promise<AccessVerdict> {
  const controller = new AbortController();
  const abort = () => controller.abort();
  const timer = setTimeout(abort, options.timeoutMs ?? 8000);
  options.signal?.addEventListener("abort", abort, { once: true });
  if (options.signal?.aborted) abort();
  try {
    const response = await fetch(url, {
      headers: { Authorization: `Bearer ${key}` },
      signal: controller.signal,
    });
    if (controller.signal.aborted) return "unknown";
    return response.ok ? "accepted" : response.status === 401 ? "rejected" : "unknown";
  } catch {
    return "unknown";
  } finally {
    clearTimeout(timer);
    options.signal?.removeEventListener("abort", abort);
  }
}
