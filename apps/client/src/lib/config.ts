// Where to reach crates/ws-server -- one shared source of truth, used to
// be duplicated (byte-for-byte identical resolveWsUrl/resolveHttpUrl
// pairs) across useRealtimeFeed.ts, useMovers.ts, useReplayBars.ts,
// useMarketsToday.ts, and useHistoricalBackfill.ts.
//
// Real bug this fixes, not just a cleanup: the deployed site
// (stockspotter.wavystack) always fell back to ws://localhost:8787 --
// a visitor's own browser has no such server, so the connection failed
// immediately and useRealtimeFeed's reconnect loop retried forever
// ("connecting" -> "disconnected" on a 3s loop, never "open"). Simply
// pointing that at the Pi's tailnet address wouldn't have been enough
// either: the page loads over https:// (Caddy's own internal cert), and
// browsers block a plain insecure ws:// connection from an https://
// origin outright (mixed content) regardless of whether the target is
// reachable. The real fix is on both ends: crates/ws-server now also
// gets reverse-proxied through the SAME Caddy instance, on its own
// same-scheme subdomains (ws.stockspotter.wavystack /
// api.stockspotter.wavystack, both wss/https, same already-trusted
// cert) instead of a bare tailnet IP:port -- see docker-compose.yml's
// `ws` service labels. This file is the client-side half: detect
// "we're not on localhost" at runtime and target those subdomains,
// no build-time env var required at all (VITE_WS_URL/VITE_HTTP_URL
// still work as an explicit override, for local testing against a
// non-default backend).

const DEFAULT_WS_URL = "ws://localhost:8787";
const DEFAULT_HTTP_URL = "http://localhost:8788";

function envOverride(name: "VITE_WS_URL" | "VITE_HTTP_URL"): string | undefined {
  const fromEnv = (import.meta as { env?: Record<string, string | undefined> }).env?.[name];
  return fromEnv && fromEnv.length > 0 ? fromEnv : undefined;
}

function isLocalDev(): boolean {
  if (typeof window === "undefined") return true; // SSR/build-time -- no real origin to key off of
  return window.location.hostname === "localhost" || window.location.hostname === "127.0.0.1";
}

export function resolveWsUrl(): string {
  const override = envOverride("VITE_WS_URL");
  if (override) return override;
  if (!isLocalDev()) return "wss://ws.stockspotter.wavystack";
  return DEFAULT_WS_URL;
}

export function resolveHttpUrl(): string {
  const override = envOverride("VITE_HTTP_URL");
  if (override) return override;
  if (!isLocalDev()) return "https://api.stockspotter.wavystack";
  return DEFAULT_HTTP_URL;
}
