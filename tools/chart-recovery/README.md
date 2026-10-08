# Chart lifecycle regression harness (Batch1: F5 + F6 + F8)

Real React 19 commits, real `ChartPanel` / `ChartSlot` / `SuperChart` /
`useHistoricalBackfill` / `derive` / `chartIndicators`, real Radix
toolbar, driven in Chromium through Playwright. Exactly four application
modules are replaced, all in `fixture/stubs/`:

| Stub | Why |
|---|---|
| `superChartEngine` | Records mount / destroy / `chart.remove` / `setBars` / `setChartType` / series + price-scale `applyOptions` / `fitContent` / resize, plus the bar payload that crossed the boundary. Creates two DOM nodes, one removed by `chart.remove()` and one by `destroy()`, so a half-done cleanup leaves a visible orphan. |
| `useAssessment` | The real hook POSTs `/assess`, a billed Claude call. Inert here. |
| `useWakeLock` | `navigator.wakeLock` is unavailable headless; records the requested state instead. |
| `config` | Keeps every URL same-origin instead of resolving the deployed origin. |

## What a green run does and does not prove

**Does:** that a real React commit mounted/destroyed the engine the
expected number of times, at the expected moment, with the expected bar
payload, and that the committed DOM (header, container, waiting states,
timeframe pills, settings popover) agreed with it.

**Does not:** anything about rendering, canvas, pixels, series parity,
indicator values or latency. The engine here draws nothing. The real
twelve-series parity/latency evidence is a separate artefact and is not
replaced, supported or contradicted by this harness.

## Running

The repo's own locked toolchain (Bun, React 19) is used for the build,
so the root workspace install has to be there first. The runner's two
tools come from this directory's own `package.json` and `bun.lock`:

```sh
# once: playwright-core at the locked version, then the Chromium
# revision that exact version names (on Linux add --with-deps)
bun install --frozen-lockfile --cwd tools/chart-recovery
node tools/chart-recovery/node_modules/playwright-core/cli.js install chromium-headless-shell

# 1. build the fixture from this worktree's application source
bun tools/chart-recovery/build.ts

# 2. run the suite
node tools/chart-recovery/run-tests.cjs
```

Options: `--filter "<substring>"` to run a subset, `--headed` to watch
it (that needs the full `chromium`, not the headless shell). Results
land in `tools/chart-recovery/dist/results.json` (gitignored).
`PLAYWRIGHT_MODULE` and `CHROME_PATH` still override the module and the
browser binary for a machine that has them somewhere else; CI sets
neither.

## Tooling, and why it is not a workspace dependency

CI runs steps 1 and 2 in the `Tests, lint and build` job of
`validate.yml` and `validate-server.yml`, as a blocking step.

`playwright-core` is the only dependency: one package, no dependencies
of its own, pinned to an exact version with its registry sha512 in
`tools/chart-recovery/bun.lock`. The browser is not chosen separately --
each playwright-core release names one Chromium revision, so the
lockfile pins the browser too, as long as `CHROME_PATH` is unset.

It lives here rather than in `apps/client`'s devDependencies on purpose.
`tools/` is outside the root `workspaces` globs, so:

- the root `bun.lock` does not change, and no root install -- the
  server-filtered one, the web image's, or mobile's full one -- ever
  installs a browser driver;
- nothing the advisory gate certifies (`ops/ci/js_advisory_gate.py`,
  which reads the root lockfile and the installed server tree) contains
  it.

The cost of that is that the root `bun audit` cannot see it either. So
the `Dependency advisories` job installs this directory's locked tree
and runs `ops/ci/chart_harness_advisory_gate.py`, which puts this
lockfile and its `node_modules` through the same fail-closed
`js_advisory_gate.py` the release surfaces use (it supplies only the
surface map; a raw `bun audit` is not trusted for this). Dependabot has
its own entry for this directory. If this tooling ever grows past one
package, the honest home for it is a surface declared in the gate
itself, not a longer lockfile here.

The runner serves the built fixture on an ephemeral **loopback** port and
blocks every request that is not same-origin (Playwright route abort plus
`--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1`). A test fails
if any request tried to leave. History fetches never reach the network at
all: the fixture replaces the global `fetch` the real `authenticatedFetch`
calls and resolves each request by hand, so response timing/ordering is
controlled rather than raced.

## Baseline comparison

`--source-root` points the same harness, the same stubs and the same
tests at a different `apps/client/src`:

```sh
bun tools/chart-recovery/build.ts --source-root ../stockspotter-chart-web-release/apps/client/src
PLAYWRIGHT_MODULE=... CHROME_PATH=... node tools/chart-recovery/run-tests.cjs
```

Nothing else about the build changes, so a pass/fail difference between
two source roots is a difference in the application code. The source root
actually built is printed at startup and recorded in `results.json`.

`react`, `react-dom` and `@stockspotter/shared-types` always resolve
from **this** worktree's `apps/client`, whatever `--source-root` says:
Bun installs them into `apps/client/node_modules`, so nothing under
`tools/` resolves them on its own, and a second worktree's copy would
put two React runtimes in one bundle.

Tests expected to **fail on the pre-Batch1 baseline** (they are the
regression evidence):

| Test | Finding |
|---|---|
| initially empty 1m: first bar mounts exactly once | F6 — mount effect never re-ran when the container appeared |
| initially empty 5m: first bar mounts a resampled chart exactly once | F6 — same gate, 5m path |
| first HISTORY arrival on a completely empty chart mounts it | F6 — same gate reached through the backfill hook alone, with no live bar involved |
| 30s readiness in both directions | F6 — minute-empty DOM gate vs. sub-minute readiness (and the engine being handed an empty series) |
| A->B->A: no previous symbol's history ever reaches the engine | F5 — the first commit after a symbol change carried the old symbol's history |
| unrelated symbols produce zero engine calls | F8 — conversion was keyed on the whole `barsBySymbol` Map |
| four slots: an unrelated symbol touches no engine | F8 — same defect, multiplied by four live slots |

The remaining tests (same-symbol ticks, explicit 1m↔5m refit, settings
across remount, unmount cleanup, StrictMode, resize) exist to prove the
fix did not trade one defect for another. They pass on the fixed code.
They do NOT all pass on the pre-fix root: there the first mount fails
(F6), and several of these tests start from a mounted chart, so the
failure cascades into them. Measured 2026-09-25: 13/13 on the
integration base (6166e3a), 1/13 on the pre-fix 9682944. Read a pre-fix
run through the F-numbered table above, not as a count.

Known limits: the fixture is not type-checked, so a new required
ChartPanel prop can go missing without a compile error. The harness
waits on `settle()` (a timer, an animation frame, a timer) rather than
on an event from the component, so it assumes React's asynchronous work
for one step finishes within a frame; that held locally under a 20x CPU
throttle, but it is an assumption, not a guarantee.

## Fixture data

Each symbol owns a disjoint block of the time axis (`fixture/data.ts`),
so any bar array that reached the engine can be attributed back to the
symbol it came from without the engine ever being told a symbol — that is
what makes the F5 assertion an assertion about real engine input.
`mergeBars` keys by time, so a leaked symbol's history survives the merge
and stays visible instead of being absorbed.

Every block base is derived onto the 5-minute grid (`ANCHOR`), because
the runner asserts bucket alignment and half-minute boundaries on the
payload that reached the engine. Within a block: history and live
minute bars are minute-aligned but deliberately **off** the 5m grid, so
a resampled 5m payload is distinguishable from a raw 1m one; 30s bars
start on a half-minute boundary and step by 30, so `time % 60`
alternates 30/0 exactly as a real sub-minute series does.

State reaches the components exactly the way `useRealtimeFeed` delivers
it: the real `reconcileBars`, then the array set into a **copy** of the
Map. Untouched symbols keep their array identity; the selected symbol's
appends and in-place corrections change it. If that contract were wrong,
these tests would fail rather than quietly agree with the component.

## Known gaps

- Zero-height initialisation is covered as a **resize** test (mount at
  height 0, then observe the engine's `ResizeObserver` on a real box). It
  is deliberately not asserted as a permanent defect.
- Multi-slot coverage is the 4-panel routing case only (four engines,
  an unslotted fifth symbol, a single-slot update). Per-slot timeframes,
  the extra slots' own symbol pickers and 2/3-panel layouts are not
  covered.
- `useRealtimeFeed` itself, reconnect and the Batch2 loading/error/retry
  surface are not covered here.
- The fixture is outside `apps/client/tsconfig.app.json`'s `include`, so
  it is bundled (types stripped) but not type-checked by `tsc -b`.
