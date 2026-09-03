// Where to reach crates/ws-server -- one shared source of truth, used to
// be duplicated (byte-for-byte identical resolveWsUrl/resolveHttpUrl
// pairs) across useRealtimeFeed.ts, useMovers.ts, useReplayBars.ts,
// useMarketsToday.ts, and useHistoricalBackfill.ts.
//
// Real bug this originally fixed, not just a cleanup: a deployed site
// falling back to ws://localhost:8787 -- a visitor's own browser has no
// such server, so the connection fails immediately and
// useRealtimeFeed's reconnect loop retries forever ("connecting" ->
// "disconnected" on a 3s loop, never "open"). Simply pointing that at a
// bare IP:port isn't enough either: the page loads over https://, and
// browsers block a plain insecure ws:// connection from an https://
// origin outright (mixed content) regardless of whether the target is
// reachable.
//
// Now pointed at the VPS deploy (srv1170872, ops/vps/) --
// stockspotter.wavystyle.io, a real public domain with genuine
// Let's Encrypt HTTPS via the VPS's own native Caddy (see
// ops/vps/Caddyfile.snippet), not the Pi's old self-signed
// tls=internal setup. Single domain, path-based (/ws, /api/*) rather
// than the Pi's three-subdomain split -- ops/vps/Caddyfile.snippet's
// own comment has the real reasoning. No runtime env var required at
// all (VITE_WS_URL/VITE_HTTP_URL still work as an explicit override,
// for local testing against a non-default backend).

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
  if (!isLocalDev()) return "wss://stockspotter.wavystyle.io/ws";
  return DEFAULT_WS_URL;
}

export function resolveHttpUrl(): string {
  const override = envOverride("VITE_HTTP_URL");
  if (override) return override;
  if (!isLocalDev()) return "https://stockspotter.wavystyle.io/api";
  return DEFAULT_HTTP_URL;
}
