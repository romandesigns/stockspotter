# Native experimental-lane socket lifecycle regression

Executable regression for `apps/mobile/src/useExperimentalStrategies.ts` --
the real hook, the real wire decoder, the real row reducer and the real
backoff, driven in a browser React runtime against a fake WebSocket whose
every event is delivered explicitly by the test.

Run from a checkout with locked workspace dependencies installed. Requires
Bun, Node, `playwright` or `playwright-core`, and a compatible Chromium.
Set `PLAYWRIGHT_MODULE` to the installed module; with `CHROME_PATH` unset
the runner launches that module's own pinned `chromium-headless-shell`, and
`CHROME_PATH` overrides it. No dependency downloads are performed by this
harness and no manifest is modified by it.

Candidate:

```
bun tools/native-experimental/build.ts
node tools/native-experimental/run.cjs tools/native-experimental/dist/candidate.json
```

The result-file argument is optional; without it results only go to stdout.
Validate runs this as a step, reusing the chart harness's own pinned
`playwright-core` and resolving React from the client app, which is the one
workspace it installs in full:

```
bun tools/native-experimental/build.ts --deps-root apps/client
node tools/native-experimental/run.cjs
```

Either deps root works -- the hook's React is whichever copy the deps root
provides (mobile 19.2.3, client 19.2.8) and both score 10/10.

Baseline (same harness, same installed React runtime, a different hook
source -- e.g. a checkout or copy without the handler guards):

```
bun tools/native-experimental/build.ts --source-root <baseline>/apps/mobile/src --deps-root <candidate>/apps/mobile
node tools/native-experimental/run.cjs tools/native-experimental/dist/baseline.json
```

Rebuild before every run when switching source. Default source and
dependencies are the containing checkout's mobile app; use the same
`--deps-root` for both sides of a comparison so fixture and hook share one
React/ReactDOM instance.

## What is real and what is substituted

Real: the hook itself, `src/lib/experimentalStrategies.ts` (decoder,
reducer), `src/lib/experimentalView.ts` (backoff, staleness),
`@stockspotter/shared-types`, and -- for the independence scenario --
`src/useRealtimeFeed.ts`, the ordinary feed hook, unmodified.

Substituted, and only where it is not JavaScript the hook owns:

- `react-native` -> an `AppState` stub (`currentState`,
  `addEventListener("change")` returning a removable subscription) the test
  drives. `AppState` is the only react-native surface in this hook's import
  graph.
- `./config` -> fixed `WS_URL`/`HTTP_URL`, so no `process.env` read reaches
  the browser bundle.
- `process.env.EXPO_PUBLIC_EXPERIMENTAL_STRATEGIES` -> a `globalThis` read
  set from the query string before the module graph loads, so one bundle
  can be loaded once with the flag on and once with it absent and
  `EXPERIMENTAL_STRATEGIES_ENABLED` -- a module-level const, exactly as
  Expo inlines it -- is what each page actually evaluates.
- `WebSocket`, `setTimeout`, `setInterval` -> recorders on `globalThis`.
  The fake socket delivers `open`/`message`/`close`/`error` on demand
  **regardless of `readyState`**, because React Native's bridge does too:
  that is the whole race under test. `send` is recorded rather than thrown,
  so "a retired socket wrote to the wire" is a direct assertion.

Every state transition is driven through React's own `act`, not a counted
number of macrotask ticks. Counted ticks were tried first and were
genuinely racy -- the same background-cleanup assertion passed or failed
depending on how many renders a scenario had already done.

## Coverage

Ten scenarios across two page loads (flag on, flag absent):

- The inlined flag is what the hook gates on; one socket, one AppState
  subscription, one 15s heartbeat.
- An `open` delivered after AppState-background cleanup, to a socket that
  was still CONNECTING: no hello on the wire, no replacement connection,
  no `open` status, no timer, and no phantom gap on the next real
  connection.
- A `message` delivered after cleanup: no row, no status change, no gap
  from an undecodable frame, no pong written back.
- A trailing `error` + `close` from a socket already replaced by a
  foreground reconnect: the live view stays open and no retry is scheduled.
- Positive control: a close on the *live* socket still closes the view,
  still schedules exactly one retry inside the 3s +-20% band, and the
  reconnect sends a hello and records the gap.
- Gap semantics: a background round trip after a real connection records a
  gap; a socket that never opened does not.
- iOS `inactive` is not `background` -- no teardown, no reconnect.
- Unmount releases the AppState subscription and the socket, and the
  unmounted hook cannot reconnect, retry, or write.
- Ordinary-feed independence: two separate sockets, the ordinary hello
  carries no experimental opt-in fields, backgrounding closes only the
  research socket, the ordinary feed keeps delivering bars while the
  research lane is down, and late research frames reach neither view.
- Flag absent: no socket, no interval, no timer, no AppState subscription,
  and AppState churn stays inert.

Recorded results: candidate 10/10 pass, zero page errors. A baseline copy
of the hook with the handler guards removed scores 5/10 -- the five
failures are the late-`open` write to the wire, the late-`message` row, the
retired socket closing the live view, the unmounted hook writing to a dead
socket, and the late research frame landing in the shared view.

## What this does not prove

Browser React coverage is not the React Native bridge, a real
`AppState` transition from the OS, a real WebSocket, a real network, a
WebView, touch input, authentication against a live server, or a
physical-device run. No claim is made about native reconnect timing under
real radio conditions, about Hermes, or about ws-server's own admission
behaviour (that is covered by the Rust tests in
`crates/ws-server/src/server.rs`). The two panels' rendering is not
exercised here at all -- this is the hook and its transport controller
only. Generated `dist/` and result JSON are gitignored and must not be
bundled in release source.
