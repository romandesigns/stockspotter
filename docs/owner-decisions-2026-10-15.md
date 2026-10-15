# Owner decisions, 2026-10-15

Roman delegated these decisions to Claude after the 2026-10-15 audit. The
audit is in `H:\wavystack\outputs\stockspotter-audit-20261015\AUDIT.md`, and
the research summary is in
[consolidation-index-2026-10-08.md](consolidation-index-2026-10-08.md). Each
decision takes the reversible option and states what would change it.

## 1. Product direction: a discretionary scanner, not an automated edge

No detector has shown an edge after costs:
- ignition alerts score 0.90–1.00× against random minutes (R1);
- every executable test was negative (R2, R6, R11).

The one robust result is that simple pre-open rankers, |gap| × relative
volume and premarket range, concentrate the day's big movers (R3).
Stockspotter is therefore a watchlist and context tool for a human trader.

What follows from that:
- **Alerts say what they are.**
  - The push, browser-notification and toast text now says "detector
    follow-through … unproven signal, not advice" instead of "real
    follow-through".
  - The web app shows "Research tool · signals unproven · not financial
    advice" under the wordmark.
  - Both changes are in the same PR as this document.
  - The same wording for the mobile app is in a separate PR. It is held with
    the other mobile changes until the mobile advisory gate is unblocked.
- **Detector alpha stops being the goal.** The next product feature worth
  building is a pre-open watchlist ranked by the R3 formulas, labelled as a
  ranking of likely movers and not as a direction or entry signal. It is not
  started.
- **What would change this:** a preregistered, cost-aware forward test that
  passes.

## 2. Step 4: suspended, not run

The campaign has never captured a session. It is suspended rather than
closed, for these reasons:
- Running it needs a 3–4 week freeze on production deploys, because the
  freeze binds the whole production SHA. That conflicts with ordinary
  product fixes.
- The gen-2 binding (`de5b55e`) is already stale.
- A pass would still say nothing about costs or fills.

What is left alone: the freeze artifacts, `campaign/step4-ledger` and the
evidence archive stay untouched, as the standing holds require. Nothing is
recorded in the ledger.

To restart, all three of these are needed:
1. An observer build isolated from the product build, so that product deploys
   do not invalidate the freeze.
2. A gen-3 re-freeze on that build.
3. A storage plan for 4.25–9.4 GB per session.

GPT, as the Step orchestrator, should be told of this suspension.

## 3. Auto-trader: keep running, paper only

It trades only the Alpaca paper account, so no money is at risk, and it is
the only continuous forward record. Its results are **not** evidence of an
edge, because it has no control arm (the ledger stood at 150 wins and 315
losses on 2026-10-01). Live-money trading stays out of scope.

## 4. New detectors: gated

IAC, Extreme Momentum, order-flow and any future detector stay opt-in
"Experimental" research until they pass a matched-random-minute,
cost-aware evaluation of the kind R1 and R2 applied. Until then:
- no default panel;
- no push notifications;
- no use by the auto-trader.

Production complies today: IAC and Extreme Momentum run only on the
authenticated research channel, behind a per-user toggle that is off by
default. Order flow is off.

## 5. Android advisory deadlock: exception chosen, not yet applied

The braces (GHSA-vfj7-8cjw-p6xm) and node-forge (GHSA-86w9-cpqp-85rv)
advisories have no upstream fix. Both are confined to build tooling:
- braces comes in through chokidar and micromatch, used by Tailwind and Metro;
- node-forge comes in through `@expo/cli` and expo-updates' code-signing CLI.

The exported production Android bundle (1,267 modules) contains neither
package. The app does not enable update code signing, and on-device signature
checks are native.

The chosen fix is a narrow, expiring exception in `ops/ci/js_advisory_gate.py`.
It is drafted but not merged, because it deliberately relaxes a gate built to
take no exceptions, and needs an explicit sign-off. The alternative is an Expo
SDK upgrade, if that removes both paths.

## 6. Production operations (done the same day)

See [production-state-2026-10-15.md](production-state-2026-10-15.md) (PR #72):
- the 135 GB of build cache pruned;
- leftover containers stopped;
- rollback tags added;
- the hand-swapped production recipe recorded.
