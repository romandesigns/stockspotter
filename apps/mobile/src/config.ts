// Real bug this fixes, same class as apps/client's own (see
// apps/client/src/lib/config.ts's doc comment): these used to fall back
// to localhost:8787/8788, which resolves to the PHONE's own localhost,
// never the dev machine or the deployed backend -- harmless for the
// Expo Go dev workflow only because apps/mobile/.env (gitignored, dev-
// machine-only) always overrides both with this machine's own tailnet
// IP. A real installable build has no such .env (EAS Build doesn't see
// gitignored files unless separately configured), so it would have
// baked in a dead localhost address and never connected to anything.
//
// The fallback now points at the deployed Pi's own published ports
// directly (100.79.110.117:8787/8788) rather than the new
// ws.stockspotter.wavystack/api.stockspotter.wavystack Caddy routes
// apps/client uses -- deliberately different from the web fix, not an
// inconsistency: those two hostnames use a self-signed cert, fine for a
// browser (which shows an interactive "proceed anyway" prompt a user
// can click through, same one this session just walked through for
// api./ws.), but a native app has no such prompt at all for a
// WebSocket/fetch call -- it would just fail closed with no way for the
// user to grant trust, the exact failure mode this whole thread was
// spent debugging, now worse (silent, unrecoverable) on mobile. Plain
// ws://http:// straight to the Pi's own ports sidesteps needing any
// certificate trust at all. Requires the phone to be on the tailnet
// (Tailscale installed) -- already the documented plan, not new scope.
export const WS_URL = process.env.EXPO_PUBLIC_WS_URL || "ws://100.79.110.117:8787";
export const HTTP_URL = process.env.EXPO_PUBLIC_HTTP_URL || "http://100.79.110.117:8788";
