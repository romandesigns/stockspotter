# stockspotter mobile

1. Copy `.env.example` to `.env`, **and** `.env.local.example` to `.env.local` -- two separate files, not one. Replace the sample IPs in both with this machine's real Tailscale address (`tailscale ip -4`): a physical phone is expected to join the tailnet directly, not just share a LAN with the dev machine.
2. Make sure `ws-server` is actually reachable at that address, not just on this machine: it must be running with `WS_SERVER_ADDR`/`HTTP_SERVER_ADDR` bound to `0.0.0.0` (the real default now), not `127.0.0.1` -- a loopback-only bind looks fine from `curl localhost:8788` on the same machine but is completely unreachable from a phone. Confirm with `curl http://<tailscale-ip>:8788/movers/today` from a second machine (or your phone's browser) before assuming the app itself is broken.
3. From the repository root, run `bun run dev:mobile`.
4. Open the QR code in Expo Go, or press `a`/`i` for a local simulator.

The phone and backend must be able to reach each other in **two separate places**, split across **two separate env files** on purpose:
- `.env` -- `EXPO_PUBLIC_WS_URL`/`EXPO_PUBLIC_HTTP_URL`, the app's own connection to `ws-server`.
- `.env.local` -- `REACT_NATIVE_PACKAGER_HOSTNAME`, the Metro/Expo CLI dev server's own address, used for Expo Go's initial connection and every bundle reload. This one **cannot go in `.env`**: Expo's own env loader hard-refuses to start at all with a non-`EXPO_PUBLIC_` variable there ("Refused to load personal environment variables from a non-.local env file") -- confirmed live, not a style preference.

Both matter and fail differently if missed:
- Wrong/missing `.env` -> the app itself loads fine but the connection status shows "disconnected -- retrying" forever.
- Wrong/missing `.env.local`, or putting `REACT_NATIVE_PACKAGER_HOSTNAME` in `.env` instead -> either Expo Go can't even reach the dev server in the first place ("Cannot connect to Expo CLI" -- `expo start` auto-detected the wrong interface on a machine with more than one, Tailscale/LAN/a Hyper-V or WSL virtual adapter all showing up as candidates), or the dev server refuses to start at all.

If you're troubleshooting this again: verify the real manifest with `curl -H "Expo-Platform: android" http://localhost:8081/` and check `launchAsset.url` directly rather than trusting a variable name from docs -- `EXPO_PACKAGER_PROXY_URL` looks like the documented fix for the Metro-address problem and does nothing in practice. `.expo/` also caches the detected host, so clear it (`rm -rf .expo`) after changing `REACT_NATIVE_PACKAGER_HOSTNAME` if a stale address keeps showing up.

`localhost` on a physical phone points to the phone itself, not the development machine -- never a valid value for any of these.
