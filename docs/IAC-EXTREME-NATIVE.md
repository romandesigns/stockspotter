# IAC and extreme momentum: native transport addendum

Base: `cc03d36443b861d7fc87ad74c5c0ee831d4be59d` (branch
`feature/iac-extreme-native-20261013`), itself downstream of the contract's
own base `5807d6116f5c793d6fd62ac255f443620ce21fa8`.

`docs/IAC-EXTREME-MOMENTUM-V1.md` is a hashed historical measurement
contract and is **not edited by this work**; it is byte-identical to
`cc03d364`. This file is the addendum that records what changed after it.

## What this supersedes

Exactly one sentence of that document, in its "Isolation and delivery"
section:

> Native mobile delivery is deferred.

Native delivery is no longer deferred: it is implemented, and it is off by
default. Nothing else in that document is amended. In particular every
threshold, every hypothesis value, the historical evaluation plan, the
collector capacity figures, the budget figures and every measurement claim
stay exactly as hashed -- this addendum adds a delivery path and changes no
measurement.

Read the two together as: the research contract says what is measured and
on what terms; this file says which clients may receive it and under what
isolation.

## Native delivery scope

`EXPO_PUBLIC_EXPERIMENTAL_STRATEGIES=true` (exactly that value; any other
value, and an unset variable, is off) opts the native bundle into the same
second socket the web/desktop bundle already used via
`VITE_EXPERIMENTAL_STRATEGIES`. Both frontend flags remain off by default,
and both backend flags (`IAC_ENABLED`, `EXTREME_MOMENTUM_ENABLED`) remain
off by default and independent. These are source defaults; the running web/server
release was separately authorized and activated. This native integration has not
been distributed, and its new server capability is not deployed by this change.

The native lane uses a SEPARATE authenticated socket with
`experimentalOnly: true` and `experimentalStrategies: true`, never
multiplexed onto the app's ordinary feed socket. The ordinary socket never
receives experimental messages, and an ordinary `client: "mobile"` hello is
unchanged on the wire. Research backpressure therefore cannot lag or close
the socket this app's charts, alerts and halt warnings ride on.

Changed surface:

- `crates/ws-server/src/server.rs` -- `experimental_delivery_allowed` now
  admits `ClientKind::Mobile` alongside `Web`/`Desktop`. `AutoTrader` stays
  refused even when it opts in and the lane is enabled; that exclusion is
  the structural guarantee that experimental outputs never reach an order
  path. Admission remains reachable only from an `experimentalOnly` hello,
  which takes the separate `handle_strategy_connection` path and the
  separate research semaphore. The 96/64/32 admission figures, the capacity
  rejection and the backoff contract are unchanged.
- `apps/mobile/src/lib/experimentalStrategies.ts` -- a hand-mirrored copy
  of the web decoder and row reducer. Deliberately NOT promoted into
  `@stockspotter/shared-types`: that package is the frozen protocol every
  ordinary client imports, and putting a deliberately-isolated research
  contract in it is the coupling this lane exists to avoid. The copy's cost
  is drift, paid for by `apps/client/src/lib/experimentalParity.test.ts`,
  which imports BOTH decoders and asserts they agree frame by frame on the
  same wire vectors.
- `apps/mobile/src/lib/experimentalView.ts` -- pure presentation and
  transport arithmetic (backoff, staleness window, panel selection,
  disclosure strings), unit-tested through the same parity suite.
- `apps/mobile/src/useExperimentalStrategies.ts` -- the lane's own hook,
  its own socket, its own state, its own types.
- `apps/mobile/src/components/ExperimentalStrategies.tsx`,
  `apps/mobile/src/components/ui/section.tsx` (primitive extracted
  verbatim from `App.tsx`, plus an optional `count`), and the Radar-tab
  mount point in `apps/mobile/App.tsx`.
- `apps/mobile/.env.example`, `apps/mobile/README.md` -- the opt-in
  procedure and what the on-screen numbers do and do not mean.
- `tools/native-experimental/` -- the lifecycle regression described below,
  and one added step in `.github/workflows/validate.yml` that runs it. No
  existing step, gate, classifier or required check is changed, waived or
  reordered; no new dependency and no manifest or lockfile change is part
  of this, the harness reuses the chart harness's already-pinned
  `playwright-core`.

Nothing from this lane reaches a notification, an order, a recording path,
the auto-trader, the frozen evaluator, or the frozen data layer. Tapping a
row only selects a ticker into the chart the rest of the app already uses.

## Native-only transport properties

These are mobile's, not the web client's, and exist because a phone is not
a browser tab:

- The socket is torn down when `AppState` reports `background`, and only
  `background` -- never `inactive`. On iOS `inactive` fires for transient,
  still-visible states (notification shade, call banner, app-switcher
  gesture), so tearing down on it would reconnect-storm through normal use.
  Backgrounded, there is no socket, no heartbeat and no retry timer.
- Returning to foreground opens a fresh socket and records a sticky gap if
  this client had ever connected before. The server's retained snapshot
  restores current state but cannot restore a lifecycle that came and went
  inside the gap -- the same reasoning as the ordinary feed's own
  `FeedGap`. A socket that never opened records no gap.
- Reconnects use bounded exponential backoff with +-20% jitter, 3s to 60s,
  rather than the ordinary feed's flat 3s retry, because this socket
  competes for the server's bounded research connection pool and a
  capacity rejection must not be hammered.
- Retained rows are the latest immutable observation per lifecycle, capped
  at 100 across both strategies, isolated from the ordinary feed's event
  ring.
- A socket the hook has torn down or replaced is *retired*: every
  `open`/`message`/`close`/`error` handler checks both the effect's stopped
  flag and the socket's identity before touching state, the liveness clock,
  the backoff, or the wire, and effect cleanup invalidates the current
  identity before closing the captured socket. React Native delivers socket
  events over the bridge asynchronously, so a socket that was still
  CONNECTING when the app was backgrounded can hand JavaScript an `open`,
  a `message` or a `close` after cleanup ran and after the foreground
  re-run opened its replacement. Without those checks a late event would
  write a hello to an unowned socket, fabricate a gap on the next
  connection, push rows and status into the live view, flip the connection
  status under the current socket, or schedule a retry timer that no
  cleanup is left to clear.

## Executable verification

- `cargo test --workspace` covers the server-side admission change,
  including that mobile is refused unless it opts in, that no client gets
  anything while the strategies are off, and that `AutoTrader` stays
  refused when it asks and the lane is enabled.
- `bun --cwd=apps/client run test` runs the web/native decoder parity
  vectors and the native presentation helpers.
- `tools/native-experimental/` drives the REAL hook (and the real ordinary
  feed hook alongside it) in a browser React runtime against a fake
  WebSocket whose every event is delivered explicitly, with `react-native`
  mocked only for `AppState`. Ten scenarios across two page loads cover
  delayed `open`/`message` after background cleanup, trailing
  `error`/`close` from a retired socket, the positive control that a live
  close still retries inside the jitter band, gap semantics,
  `inactive` != `background`, unmount teardown, ordinary-feed independence,
  and the flag-absent build opening no socket, no timer and no AppState
  subscription at all. Candidate: 10/10, zero page errors. A baseline copy
  of the hook without the handler guards: 5/10. See that directory's README
  for the substitution list and the honest limits -- browser React is not
  the native bridge, a real OS AppState transition, a real socket, a real
  network, or a physical device, and the panels' rendering is not exercised
  there.
  It runs in Validate as a step (`.github/workflows/validate.yml`), reusing
  the chart harness's already-pinned `playwright-core` and resolving React
  from `apps/client`, the one workspace that job installs in full. Verified
  locally against both deps roots: 10/10 either way.
- `tsc --noEmit` (apps/mobile) and `tsc -b` (apps/client) both clean;
  `oxlint` clean (ten pre-existing warnings, none in the new files).

Recorded run, 2026-10-13, this checkout: 1458 Rust tests pass / 0 fail (1
pre-existing ignored); 188 client tests pass / 0 fail; native lifecycle
harness 10/10 with zero page errors.

### Activated bundle check

`npx expo export` with `EXPO_PUBLIC_EXPERIMENTAL_STRATEGIES=true` was run
locally into gitignored output (`apps/mobile/dist/experimental-20261013`,
android + ios, Hermes bytecode, 3.5MB per platform) and succeeded. A
`--no-bytecode --no-minify` export of the same source and flag was then
grepped to confirm the gate is genuinely inlined: `EXPERIMENTAL_STRATEGIES_ENABLED = true`.
A control export with the variable unset
(`apps/mobile/dist/control-flagoff-20261013`) inlines
`EXPERIMENTAL_STRATEGIES_ENABLED = false`.

One measured caveat worth recording, because it contradicts the natural
reading of the SDK 57 environment-variable guide: that guide's "no cache
clear needed" applies to `expo start`, not to `expo export`. An export run
immediately after a flag change **without** `-c/--clear` reused the
previous transform result and inlined the stale value -- a flag-unset
export produced `EXPERIMENTAL_STRATEGIES_ENABLED = true`. With `--clear`
the same command inlines `false`. Every export reported above was run with
`--clear`, and `apps/mobile/README.md` now carries this warning.

No EAS build, no native publish, no deploy, and no push was performed.

## Distribution

Unchanged and still blocked, independently of every flag above: the mobile
dependency tree carries the known braces and node-forge advisories, so no
EAS build and no native release of this app is produced. See
`docs/IAC-EXTREME-RELEASE-SCOPE.md`. A local `npx expo export` is a bundle
check only -- it is not a build, not a submission, and not evidence that
distribution is unblocked.
