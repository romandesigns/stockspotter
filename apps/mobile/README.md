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

## Experimental strategies (IAC / Extreme Momentum) -- off by default

The two research panels at the bottom of the Radar tab are behind `EXPO_PUBLIC_EXPERIMENTAL_STRATEGIES`, which defaults to **off**. Unset (or any value other than exactly `true`), the app opens no second socket, starts no timer, and renders nothing -- the build behaves identically to one without the feature. This is a research lane: it never fires a notification, never reaches the auto-trader or any order path, and never writes to a recording.

To enable it **after** validating it against a real backend:

1. Turn the lane on server-side first. `ws-server` must be running with `IAC_ENABLED=true` and/or `EXTREME_MOMENTUM_ENABLED=true` (both default off, independent). With **both** off, the opt-in hello is still accepted and the server sends its retained snapshot -- which is one `experimental_strategy_status` with `iacEnabled: false` and `extremeMomentumEnabled: false` -- and only then `hello_rejected: "Experimental strategies disabled"`. So the app does receive a status, and because neither strategy is enabled it renders **nothing at all** for this lane (not an "unavailable" card): two empty panels would read as "looked, found no candidates", a claim nothing has made. The Radar tab is then byte-for-byte what it is with the flag off.
2. Add `EXPO_PUBLIC_EXPERIMENTAL_STRATEGIES=true` to `apps/mobile/.env` (it is `EXPO_PUBLIC_`-prefixed, so it belongs in `.env`, not `.env.local` -- see the split above).
3. Fully reload the app -- shake -> Reload, or `r` in the CLI. `EXPO_PUBLIC_*` values are inlined into the bundle by Metro at transform time, exactly like `EXPO_PUBLIC_WS_URL`, so the value the running app holds is the one baked into the bundle it already loaded. Per the [SDK 57 environment-variable docs](https://docs.expo.dev/guides/environment-variables/) `expo start` does not need a restart or `--clear` for this; a reload is enough.

   `expo export` is different, and this was measured rather than assumed on 2026-10-13: exporting twice in a row with a changed `EXPO_PUBLIC_EXPERIMENTAL_STRATEGIES` and no `-c/--clear` produced a bundle with the **previous** value still inlined (`EXPERIMENTAL_STRATEGIES_ENABLED = true` in a flag-unset export). With `--clear` the same export correctly inlined `false`. Always pass `-c` to `expo export` when the flag has changed, and verify by grepping the exported JS -- a `--no-bytecode --no-minify` export makes `EXPERIMENTAL_STRATEGIES_ENABLED = true|false` directly greppable.
4. Confirm it actually connected: the "Experimental research" card shows the server's own status line (coverage, warmup, counters) and drops its `incomplete` badge once the server reports healthy with a fresh source timestamp. A `warming up` reason is normal right after the server starts -- the detector needs a baseline before it can observe anything.

Notes on what this flag does and does not change:
- It opens a **second, separate** authenticated WebSocket with `experimentalOnly: true`, which `ws-server` routes to its own handler and its own bounded research connection pool. The app's ordinary feed socket (`useRealtimeFeed.ts`) is untouched, and research backpressure cannot lag or close it.
- That socket is torn down whenever the app goes to **background** (`AppState`), not merely `inactive`, and reopened on return with a recorded gap -- it holds nothing open behind a locked screen. Reconnects use bounded exponential backoff with jitter (3s to 60s), unlike the normal feed's flat 3s retry, because a capacity rejection must not be hammered. A socket the hook has torn down or replaced cannot reopen, retry, or write into the live view, even when React Native's bridge delivers its `open`/`message`/`close` late; `tools/native-experimental/` is the executable regression for exactly that.
- Rows are immutable per-lifecycle observations, deduped so a replayed or out-of-order snapshot can never overwrite a newer one. Tapping a row only selects that ticker into the chart the rest of the app already uses.
- **Distribution is still blocked**, independently of this flag: the mobile dependency tree carries the known braces and node-forge advisories, so no EAS build/release of this app should be produced. See `docs/IAC-EXTREME-RELEASE-SCOPE.md`.

### Reading the two numbers on screen

Neither number is a coverage or readiness measure, and there is no per-symbol readiness telemetry anywhere in this lane -- don't infer one:

- The count beside each strategy's section title (`Ignition Anchor Candle`, `Extreme Momentum`) is **how many lifecycle observations this client is currently retaining** for that strategy: the latest observation per lifecycle, capped at 100 and shared across both strategies, from whatever arrived on this socket since it last connected (plus the server's own retained snapshot, itself capped at 100). It is not the number of symbols being watched, not the number of symbols that have finished warmup, and not a count of anything "ready". `0` means nothing has been retained yet, which is also what a healthy, quiet session looks like.
- `N observed` in the status card is the server's `activeSymbols`: how many symbols its bounded collector currently holds state for, which the server pins to `0` whenever it reports itself unhealthy (lag, budget breach, closed source). Coverage is `observed_subscribed_symbols` -- symbols this server happened to see minute bars for, never the whole market.
