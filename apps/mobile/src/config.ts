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
// Updated to the VPS deploy (ops/vps/, stockspotter.wavystyle.io)
// instead of the Pi's raw tailnet IP:port. The original reasoning for
// avoiding wss:// on mobile doesn't apply anymore: that was specifically
// about the Pi's self-signed tls=internal cert -- fine for a browser
// (an interactive "proceed anyway" prompt a user can click through, the
// one this whole project walked through for the Pi's api./ws.
// subdomains), but a native app has no such prompt at all for a
// WebSocket/fetch call, so it would fail closed with no way to grant
// trust. The VPS has a genuine, publicly-trusted Let's Encrypt
// certificate (confirmed live -- a real browser connected with zero
// cert workarounds needed, the first time that's been true all
// session), so wss://https:// just work here the same as any other
// HTTPS client, no special handling needed, no tailnet requirement
// either (real public DNS + IP, not tailnet-only like the Pi).
export const WS_URL = process.env.EXPO_PUBLIC_WS_URL || "wss://stockspotter.wavystyle.io/ws";
export const HTTP_URL = process.env.EXPO_PUBLIC_HTTP_URL || "https://stockspotter.wavystyle.io/api";
