import { useState, type ReactNode } from "react";
import { getAccessKey, setAccessKey } from "@stockspotter/shared-types";
import { resolveHttpUrl } from "../lib/config";

export function AccessGate({ children }: { children: ReactNode }) {
  const [allowed, setAllowed] = useState(import.meta.env.DEV || Boolean(getAccessKey()));
  const [key, setKey] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
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
