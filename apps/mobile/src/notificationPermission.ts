export type PermissionResult = "granted" | "denied" | "unknown";
/** A timed-out caller does not cancel the native prompt or start another one. */
export function singleFlightPermission(request: () => Promise<boolean>, timeoutMs = 30_000) {
  let flight: Promise<PermissionResult> | null = null;
  return () => {
    if (flight) return flight;
    let timer: ReturnType<typeof setTimeout>;
    const underlying = Promise.resolve().then(request).then(value => value ? "granted" as const : "denied" as const).catch(() => "unknown" as const);
    const current = Promise.race([
      underlying,
      new Promise<PermissionResult>(resolve => { timer = setTimeout(() => resolve("unknown"), timeoutMs); }),
    ]);
    flight = current;
    void underlying.finally(() => { clearTimeout(timer); if (flight === current) flight = null; });
    return current;
  };
}