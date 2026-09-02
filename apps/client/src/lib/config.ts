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
// "we're not running via the dev server" at build time and target those
// subdomains, no runtime env var required at all (VITE_WS_URL/
// VITE_HTTP_URL still work as an explicit override, for local testing
// against a non-default backend).

const DEFAULT_WS_URL = "ws://localhost:8787";
const DEFAULT_HTTP_URL = "http://localhost:8788";

function envOverride(name: "VITE_WS_URL" | "VITE_HTTP_URL"): string | undefined {
  const fromEnv = (import.meta as { env?: Record<string, string | undefined> }).env?.[name];
  return fromEnv && fromEnv.length > 0 ? fromEnv : undefined;
}

// Real bug caught in a self-audit, not shipped then fixed: an earlier
// version of this keyed off window.location.hostname === "localhost".
// That's correct for the plain web deploy, but this same bundle also
// ships inside the Tauri DESKTOP app (src-tauri/tauri.conf.json's
// frontendDist), which loads it through Tauri's own custom asset
// protocol rather than a real HTTP origin -- and on macOS/Linux that
// protocol is literally `tauri://localhost` (a fixed origin identifier
// for the scheme, not an actual loopback address), so
// window.location.hostname reads back as "localhost" there too. A
// hostname check would have silently pointed every real packaged
// desktop release at a nonexistent local server on the end user's own
// machine. import.meta.env.DEV is Vite's own build-time flag instead --
// true only for the actual dev server (`vite dev`/`tauri dev`, this
// project's own `bun run dev:client`), false in every built bundle
// regardless of how it's served afterward, which is exactly what both
// the web deploy and the packaged desktop app are and should both
// resolve the same way.
function isLocalDev(): boolean {
  return Boolean((import.meta as { env?: Record<string, unknown> }).env?.DEV);
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
