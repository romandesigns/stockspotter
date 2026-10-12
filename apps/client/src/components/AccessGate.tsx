import { useEffect, useRef, useState, type ReactNode } from "react";
import { configureAccessKeyStorage, restoreAccessKey, setAccessKey } from "@stockspotter/shared-types";
import { browserAccessKeyStorage } from "../lib/accessStorage";
import { resolveHttpUrl } from "../lib/config";
import { verifyAccessKey } from "../lib/verifyAccess";

// Registered at module scope so a key persisted by a previous visit is
// reachable before the first render -- see accessStorage.ts.
configureAccessKeyStorage(browserAccessKeyStorage);

// "unknown" is deliberately distinct from "rejected". A restored key must only
// be discarded when the server actively says it's wrong (401); a 502 or a
// dropped connection means the backend is unreachable, and treating that as a
// bad key would sign the user out every time the service hiccups -- exactly
// the wrong response to an outage.
export function AccessGate({ children }: { children: ReactNode }) {
  const [allowed, setAllowed] = useState(import.meta.env.DEV);
  const [restoring, setRestoring] = useState(!import.meta.env.DEV);
  const [key, setKey] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const submitRequest = useRef<AbortController | null>(null);

  useEffect(() => () => {
    submitRequest.current?.abort();
    submitRequest.current = null;
  }, []);

  useEffect(() => {
    if (import.meta.env.DEV) return;
    let cancelled = false;
    const request = new AbortController();
    void (async () => {
      try {
        // The browser adapter wraps synchronous localStorage. Do not race
        // this shared restore: a late completion could overwrite a new key.
        const stored = await restoreAccessKey();
        if (cancelled) return;
        if (stored) {
          const verdict = await verifyAccessKey(`${resolveHttpUrl()}/health`, stored, { signal: request.signal });
          if (cancelled) return;
          // Only an explicit 401 invalidates a saved key. During an outage,
          // preserve the existing key and let the dashboard show connectivity.
          if (verdict === "rejected") setAccessKey("");
          else setAllowed(true);
        }
      } catch {
        if (!cancelled) setError("Unable to restore saved access. Enter your key to retry.");
      } finally {
        if (!cancelled) setRestoring(false);
      }
    })();
    return () => { cancelled = true; request.abort(); };
  }, []);

  // Show status while the stored key is being read and checked --
  // showing the sign-in form first would make an already-signed-in device
  // flash a password prompt on every load.
  if (restoring) return <main style={{ minHeight: "100vh", display: "grid", placeItems: "center", background: "#101210", color: "#eee" }}>
    <div role="status" aria-live="polite"><h1 style={{ fontSize: 24 }}>Stockspotter</h1><p>Checking saved access…</p></div>
  </main>;
  if (allowed) return children;
  return <main style={{ minHeight: "100vh", display: "grid", placeItems: "center", background: "#101210", color: "#eee" }}>
    <form style={{ width: "min(360px, 90vw)", display: "grid", gap: 16 }} onSubmit={async (event) => {
      event.preventDefault();
      if (submitRequest.current) return;
      const request = new AbortController();
      submitRequest.current = request;
      setBusy(true); setError("");
      try {
        const verdict = await verifyAccessKey(`${resolveHttpUrl()}/health`, key.trim(), { signal: request.signal });
        if (submitRequest.current !== request || request.signal.aborted) return;
        if (verdict !== "accepted") throw new Error(verdict === "rejected" ? "Access key not recognized." : "Unable to verify access. Check your connection and try again.");
        setAccessKey(key.trim()); setKey(""); setAllowed(true);
      } catch (e) { if (submitRequest.current === request && !request.signal.aborted) setError(e instanceof Error ? e.message : "Unable to connect."); }
      finally { if (submitRequest.current === request) { submitRequest.current = null; setBusy(false); } }
    }}>
      <h1 style={{ fontSize: 24 }}>Stockspotter</h1>
      <label htmlFor="access-key">Enter your private access key</label>
      <input id="access-key" type="password" autoComplete="off" required disabled={busy} value={key} onChange={(e) => setKey(e.target.value)}
        style={{ padding: 12, border: "1px solid #555", borderRadius: 6, background: "#202420", color: "white" }} />
      <button type="submit" disabled={busy} style={{ padding: 12, borderRadius: 6, background: "#86ce8d", color: "#101210" }}>{busy ? "Connecting…" : "Continue"}</button>
      {error && <p role="alert">{error}</p>}
    </form>
  </main>;
}
