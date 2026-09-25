// Stand-in for apps/client/src/lib/config.ts.
//
// The real module resolves a deployed absolute origin
// (https://stockspotter.wavystyle.io/api) whenever import.meta.env.DEV
// is falsy, which a bundled harness page is. Returning a same-origin
// relative base keeps every URL the hooks build pointed at the
// loopback fixture server -- belt to the braces of the runner's own
// route blocking, and of the fixture's controlled `fetch` (which
// resolves these requests by hand and never lets one reach the wire).

export function resolveHttpUrl(): string {
  return "/api";
}

export function resolveWsUrl(): string {
  return "ws://127.0.0.1:1/ws";
}
