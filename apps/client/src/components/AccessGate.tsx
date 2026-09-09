import { useEffect, useState, type ReactNode } from "react";
import { configureAccessKeyStorage, restoreAccessKey, setAccessKey } from "@stockspotter/shared-types";
import { browserAccessKeyStorage } from "../lib/accessStorage";
import { resolveHttpUrl } from "../lib/config";

// Registered at module scope so a key persisted by a previous visit is
// reachable before the first render -- see accessStorage.ts.
configureAccessKeyStorage(browserAccessKeyStorage);

type Verdict = "accepted" | "rejected" | "unknown";

// "unknown" is deliberately distinct from "rejected". A restored key must only
// be discarded when the server actively says it's wrong (401); a 502 or a
// dropped connection means the backend is unreachable, and treating that as a
// bad key would sign the user out every time the service hiccups -- exactly
// the wrong response to an outage.
async function verifyKey(key: string): Promise<Verdict> {
  try {
    const response = await fetch(`${resolveHttpUrl()}/health`, { headers: { Authorization: `Bearer ${key}` } });
    if (response.ok) return "accepted";
    return response.status === 401 ? "rejected" : "unknown";
  } catch {
    return "unknown";
  }
}

export function AccessGate({ children }: { children: ReactNode }) {
  const [allowed, setAllowed] = useState(import.meta.env.DEV);
  const [restoring, setRestoring] = useState(!import.meta.env.DEV);
  const [key, setKey] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (import.meta.env.DEV) return;
    let cancelled = false;
    void (async () => {
      const stored = await restoreAccessKey();
      if (cancelled) return;
      if (stored) {
        const verdict = await verifyKey(stored);
        if (cancelled) return;
        // Rotating STOCKSPOTTER_API_TOKEN invalidates every stored copy at
        // once; clearing here turns that into a normal sign-in prompt rather
        // than an app that silently 401s on every request.
        if (verdict === "rejected") setAccessKey("");
        else setAllowed(true);
      }
      setRestoring(false);
    })();
    return () => { cancelled = true; };
  }, []);

  // Nothing is rendered while the stored key is being read and checked --
  // showing the sign-in form first would make an already-signed-in device
  // flash a password prompt on every load.
  if (restoring) return null;
  if (allowed) return children;
  return <main style={{ minHeight: "100vh", display: "grid", placeItems: "center", background: "#101210", color: "#eee" }}>
    <form style={{ width: "min(360px, 90vw)", display: "grid", gap: 16 }} onSubmit={async (event) => {
      event.preventDefault(); setBusy(true); setError("");
      try {
        const response = await fetch(`${resolveHttpUrl()}/health`, { headers: { Authorization: `Bearer ${key.trim()}` } });
        if (!response.ok) throw new Error(response.status === 401 ? "Access key not recognized." : "Service unavailable. Try again shortly.");
        setAccessKey(key.trim()); setKey(""); setAllowed(true);
      } catch (e) { setError(e instanceof Error ? e.message : "Unable to connect."); }
      finally { setBusy(false); }
    }}>
      <h1 style={{ fontSize: 24 }}>Stockspotter</h1>
      <label htmlFor="access-key">Enter your private access key</label>
      <input id="access-key" type="password" autoComplete="off" required value={key} onChange={(e) => setKey(e.target.value)}
        style={{ padding: 12, border: "1px solid #555", borderRadius: 6, background: "#202420", color: "white" }} />
      <button type="submit" disabled={busy} style={{ padding: 12, borderRadius: 6, background: "#86ce8d", color: "#101210" }}>{busy ? "Connecting…" : "Continue"}</button>
      {error && <p role="alert">{error}</p>}
    </form>
  </main>;
}
