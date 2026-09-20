# Native chart candidate, September 20, 2026

Branch `audit/chart-native-20260920`, based on deployed web source `9682944`.
No backend, ranking, experiment, cadence, aggregation, dependencies or configuration
changes. Not yet distributed to installed native clients.

## Implementation and validation

Mobile's separate WebView engine now uses incremental updates for unchanged
history plus the last/new bar, with full replacement on corrections, eviction,
reset or timeframe changes. All 12 series retain identical data/indicators.
Live updates preserve zoom. Explicit symbol/range/timeframe changes reframe.
Empty data crosses the RN bridge and clears old candles rather than leaving stale
content. The existing full-array JSON bridge remains; no new coalescing or cadence.

Read Expo 57 and its versioned react-native-webview docs before edits, as required
by `apps/mobile/AGENTS.md`. Mobile TypeScript passes with its locked compiler.
Offline Chromium (4x CPU slowdown, 390x844 DPR2) compared the actual generated HTML
before/after, using the locally installed pinned chart library and no network:

- Six datasets × 12 series match exactly: initial, last-bar update, append,
  historical correction, rolling eviction and timeframe-shaped replacement.
- Live range `[100,200]` stays intact; explicit 5m selection refits; empty clears.
- Four resize checks across folded/unfolded viewport sizes pass; no page errors.
- 100 updates after 20 warmups: p50 **11.0 → 3.4 ms**, p95 **14.5 → 4.6 ms**,
  p99 **25.7 → 5.2 ms**, max **25.8 → 5.2 ms**.

These are HTML submission timings, **not physical native/bridge/GPU latency**.
Actual Android/iOS pinch, background/resume, WebView readiness and foldable hardware
remain unverified. No claim that the native app has received an update.

## Distribution status

The Tauri desktop shares the now-validated web source but packages it in its
installer. The established workflow dispatch can target an isolated branch;
do not push master or change the frozen production branch merely to trigger it.
Latest observed published desktop version is 0.8.0; current candidate config is
0.9.0. Its existing required reusable validation workflow is blocked by the
separate dependency audit: nine pre-existing high advisories in the unchanged
lockfile. Full-workspace tests, lint and builds pass. No gate was disabled, signing
secret read, or new installer released.

Expo uses project `3d68c458-5291-4beb-b919-3ada3acbc2f7`, preview Android APK/internal
distribution and production EAS profile; runtime is app-version based. A safe
release needs authenticated EAS access, confirmed installed channel/runtime and
native smoke verification before promoting an OTA or distributing a new binary.
The installed EAS CLI returned **Not logged in**; no credentials were opened or
copied to bypass that. Next step is normal EAS sign-in and a preview/device check.
The browser deployment alone updates neither an installed Expo app nor Tauri.

## Reproduce the HTML check

Generate `data/native-chart-fixture/before.html` with `buildChartHtml()` from clean
base `9682944`, and `after.html` from this candidate. Run
`tools/chart-audit/test-native-rendering.cjs` with `PLAYWRIGHT_MODULE` and
`CHROME_PATH` pointing to an installed Playwright/Chromium runtime. Results go to
`data/native-chart-fixture/results.json`. The test blocks all external requests
and serves the pinned chart script from client node_modules.
