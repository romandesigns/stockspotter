# ALPHA MILESTONE B — COMPLETION (live wiring)

**Date:** 2026-09-09
**Branch:** `gpt/audit-remediation-20260909`
**Commit created:** `ba722698af4fa2b387339eb017a2d58734f969c7`
**Status:** committed locally — **not pushed, not deployed**

---

## 1. Live integration architecture

```
market_data::run_live_scan
        │
        └─► broadcast::Sender<ScanEvent>   (unchanged)
              ├─► server::run          → WS clients      (unchanged)
              ├─► LiveSignalTracker    → pending signals (unchanged)
              ├─► push notifier        → ignition alerts (unchanged)
              └─► MeasurementCollector → episodes        ★ NEW, 4th subscriber
                        │
                        └─► MeasurementRecorder
                              bounded queue (64) → dedicated writer thread
                                    → data/research/episodes-<UTC-date>.ndjson
```

A **fourth independent `broadcast::Receiver`**, in exactly the shape the
detection-efficiency collector and push notifier already use. `subscribe()` gives each consumer its
own receiver, so this cannot consume, delay, reorder or filter what any other consumer sees.

**`MeasurementCollector`** owns the `EpisodeTracker` and `FeatureCache` and decides *when* to rank
and *when* an outcome is settled. It is deliberately separate from the recorder so the decision logic
is testable without a filesystem.

**Ranking is paced on a 30s timer**, not run per event — a burst of events must not become a burst of
sorts. An **empty cohort does not consume the window**; without that, the very first event (which
arrives before any momentum score exists) would burn the interval and nothing would ever be ranked.
My own test caught this.

---

## 2. Files persisted at runtime

| Path | Contents | Written by |
|---|---|---|
| `data/research/episodes-<UTC-date>.ndjson` | one settled `OpportunityEpisode` per line — including its `openingContext` (full signal-time snapshot), `momentumTrack`, `researchRank`, `trader` linkage slot and `outcome` | measurement writer thread |
| `data/live_pending_signals.jsonl` | unchanged | existing collector |
| `data/auto_trader_journal.jsonl` | unchanged | auto-trader process |
| `$DISCOVERY_AUDIT_DIR` | unchanged | discovery recorder |

Signal contexts are persisted **inside** their episode rather than as a parallel file: the context
only has meaning relative to the episode that caused it, and a single file avoids a join at analysis
time that could silently drop unmatched rows.

Append-only NDJSON, one file per UTC day — the same rotation every other capture in this project
uses. No database.

---

## 3. How write failures are isolated

Three layers, in priority order:

1. **Cannot block dispatch.** Records go to a bounded `sync_channel(64)` via `try_send`. A full queue
   **drops and counts** — it never awaits. Same shape and same reasoning as
   `market_data::discovery_audit`, whose header already states *"Never block market dispatch on disk
   I/O"*.
2. **Cannot fail the realtime path.** Every disk error is caught, counted in `write_errors`, and
   logged on powers of two so a persistent failure stays visible without flooding. Nothing
   propagates. If the directory cannot even be created, `MeasurementRecorder::start` returns `None`
   and **measurement is simply off** — the server carries on. Asserted by
   `an_unwritable_directory_disables_capture_without_failing`.
3. **Cannot alter production.** It reads events already broadcast; it emits nothing and gates
   nothing.

Counters are separate — `dropped`, `write_errors`, `episodes_written` — because a silent gap would
invalidate exactly the completeness claims this data exists to support. `is_degraded()` is reported
at shutdown.

**Deliberately not done:** wiring a degradation flag into `FunnelHealth` or a client-facing surface.
The brief allowed it only if small and natural; it would mean changing an event schema clients
consume, which is not worth it for a counter already in the logs.

---

## 4. Auto-Trader linkage

**Architectural constraint that determined the design:** the auto-trader is a **separate process**
(`ops/vps/docker-compose.yml`'s `auto-trader` service) that reaches ws-server over a WebSocket. There
is no in-process handle to link through at runtime, and creating one would mean changing the wire
protocol or the trader's behaviour — both forbidden.

So linkage is an **analysis-time join**, which is the smallest mechanism consistent with the
architecture:

```rust
link_trader_decisions(&mut [OpportunityEpisode], &[TraderDecision]) -> LinkReport
```

- `TraderDecision` is a minimal local type, **not** `auto_trader::JournalEntry` — `backtest-metrics`
  does not depend on `auto-trader`, and inverting that would couple measurement to the thing being
  measured.
- Matches on `(symbol, time)`, choosing the **latest episode opened at or before** the decision. A
  decision predating an episode is never attributed to it — asserted.
- **Exits get a one-hour attribution grace**; entries and skips get none. A position legitimately
  outlives the signal that opened it, but a fresh entry long after an episode went quiet is more
  likely a new opportunity than a late reaction to an old one.
- Unmatched decisions are **counted, never forced** onto the nearest episode. A non-zero `unmatched`
  is a real finding about coverage.

The trader's journal already records everything needed, and the export tool already groups it.
**Nothing about trader behaviour changed.**

---

## 5. Research ranking integration

`assign_research_rank` runs on the 30s timer over currently-open episodes, ordered by each one's most
recent `momentum.overall`. Persisted per episode as `{ rank, cohortSize, score, rankedAt, windowId }`.

**Verified non-production:** it runs *after* `tracker.observe` returns, writes only to a field nothing
in the production path reads, and the auto-trader is a different process entirely so it is
structurally unreachable. It cannot reorder client messages, suppress events, or alter a gate.

Episodes with **no** momentum score are left unranked rather than ranked last — ranking them last
would assert an ordering the data does not support.

---

## 6. Shutdown and censoring

On `RecvError::Closed` (broadcast shutdown):

1. every still-open episode closes as **`CaptureEnded`** — censored, never concluded;
2. every pending outcome settles with **whatever was observed**, unobserved horizons marked
   `Censored`, never synthesized;
3. the writer drains with a **5-second bound** — research bookkeeping must not hang shutdown;
4. if `is_degraded()`, a warning states plainly that completeness claims are invalid.

`finalize` passes a `session_end` only when capture itself stopped, so an unobserved horizon is
attributed to **us** (`CaptureEnded`) rather than to the session — the two mean different things for
whether more collection would help.

---

## 7. Expected disk use, one full session

| System | Raw | Notes |
|---|---|---|
| Discovery audit | **≈ 5.1 GB** | 258 B/symbol-snapshot × 12,628 × 1,560 cadences (measured shape) |
| Episode measurement | **≈ 10–40 MB** | ~2–4 KB per episode incl. full context; thousands of episodes/session |
| **Total transient** | **≈ 5.1 GB** | dominated entirely by discovery |
| After export (`gzip`) | **≈ 0.17 GB** | 31.5× measured on realistic capture shape |
| Five sessions raw | ≈ 25.5 GB | |
| Five sessions exported | **≈ 0.85 GB** | |

**Coexistence checks, all pass:**

- **Paths do not collide** — `data/research/` vs `$DISCOVERY_AUDIT_DIR` (`/app/data/discovery-audit`
  on the VPS). Verified.
- **Both write paths are off critical dispatch** — each has its own bounded queue and dedicated
  writer thread.
- **Dropped-record accounting is independent** — discovery reports `lost_records`; measurement
  reports `dropped` / `write_errors` separately. Neither masks the other.

**Operational note:** `DISCOVERY_AUDIT_DIR` is already set in the VPS `.env`, so discovery capture is
enabled there whenever the updated binary runs. Plan ~5 GB per session accordingly.

---

## 8. Files changed across the complete Milestone B (12)

```
A crates/backtest-metrics/src/context.rs     signal-time snapshots + FeatureCache
A crates/backtest-metrics/src/episode.rs     episodes, boundaries, ranking, trader join
A crates/backtest-metrics/src/horizon.rs     multi-horizon outcomes + censoring
M crates/backtest-metrics/src/lib.rs         re-exports only
M crates/backtest-metrics/src/live_signals.rs  test fixture field (+1/-1)
M crates/market-data/src/events.rs           1 optional field + 3 compat tests
M crates/market-data/src/live.rs             catalyst plumbing, ZERO deletions
M crates/ws-server/src/main.rs               4th subscriber + shutdown
A crates/ws-server/src/measurement.rs        live wiring, recorder, outcome progression
M packages/shared-types/src/index.ts         1 optional field + docs
A python/export_session.py                   portable compressed export
A python/test_export_session.py              6 tests
```

---

## 9. Tests added

**79 new tests across the full milestone.** The integration-level ones added in this pass:

| Requirement | Test |
|---|---|
| Live observation → episode | `a_live_event_sequence_opens_and_accumulates_one_episode` |
| Outcome accumulation | `forward_prices_accumulate_and_produce_real_horizon_outcomes` |
| Censoring on short observation | `an_unobserved_horizon_is_censored_not_reported_as_zero` |
| Ranking from contemporaneous cohort | `contemporaneous_candidates_receive_stable_research_ranks` |
| Ranking paced, not per-event | `ranking_runs_on_a_timer_not_on_every_event` |
| Trader linkage | `trader_decisions_link_to_the_intended_episode_after_the_fact` |
| Linkage causality | `a_decision_predating_an_episode_is_never_attributed_to_it` |
| Writer failure | `an_unwritable_directory_disables_capture_without_failing`, `a_full_queue_drops_and_counts_rather_than_blocking` |
| Persistence readable | `records_are_appended_as_readable_ndjson` |
| Shutdown censoring | `shutdown_closes_active_episodes_as_censored` |
| Bounded memory | `the_pending_set_stays_bounded` |
| **No behavior change** | **`measurement_does_not_alter_the_events_production_sees`** |

**The no-behavior-change test is the important one.** It feeds an identical 60-event stream through a
consumer twice — once alone, once alongside a running collector — and asserts the serialized events
are byte-identical in content *and* order, then asserts the collector was genuinely active. If
measurement ever consumed, reordered, filtered or mutated an event, it fails.

**Five real bugs my own tests caught across this milestone:**

1. **NaN poisoned serialization** — `openingPrice` became JSON `null` and could not be read back.
2. **Inactivity pre-empted session boundary** — every cross-session close was mislabelled.
3. **Empty ranking cohort consumed the window** — nothing would ever have been ranked in production.
4. **Forward paths were lost** — prices were only collected for episodes *already* pending, so an
   episode that stayed open or closed at shutdown carried no path and censored every horizon.
5. **A time-trimmed rolling path punched an artificial gap** between the opening price and the
   retained tail, which `evaluate_horizons` correctly but uselessly reported as `DataGap`. Fixed by
   keying paths to open episodes and bounding by count only.

Three of those would have silently produced useless research data.

---

## 10. Validation results

| Command | Result |
|---|---|
| `cargo test -p backtest-metrics` | **103 passed, 0 failed** |
| `cargo test -p ws-server` | **64 passed, 0 failed** |
| `cargo test -p auto-trader` | **45 passed, 0 failed** |
| `cargo test -p market-data` | **76 passed, 0 failed** |
| `cargo test --workspace --no-fail-fast` | **399 passed, 0 failed**, 24 binaries, **0 SIGKILLs, 0 errors** |
| `bun --cwd=apps/client run test` | **60 passed, 0 failed** |
| `tsc --noEmit -p apps/client/tsconfig.app.json` | **exit 0** |
| `tsc --noEmit -p apps/mobile/tsconfig.json` | **exit 0** |
| `python -m unittest discover -s python -p 'test_discovery*.py'` | **10 passed** |
| `python -m unittest python.test_export_session` | **6 passed** |
| `python -m pytest -q python/tests` | **could not run — `No module named pytest`** |

Workspace 347 → **399**. No repository-wide rustfmt churn.

**Environment limitation reported, not worked around:** pytest is not installed locally. CI installs
`python/requirements.txt` and does run that suite.

---

## 11. Commit

```
ba722698af4fa2b387339eb017a2d58734f969c7
Build causal alpha measurement pipeline
```

12 files staged individually by pathname. No `git add .`, no `git add -A`.
`git diff --check` and `git diff --cached --check` both exit 0.

## 12. `git status --short`

```
?? .claude/
?? AUDIT-2026-09-09.md
```

Both remain untracked and were never staged. Working tree otherwise clean.

---

## 13. No strategy decision logic changed — confirmed

**No detector, scorer or auto-trader source file was modified.** Verified against
`crates/{fast-funnel,ignition-detector,momentum-scorer,consolidation-breakout,halt-detector,auto-trader}`.

The two `market-data` files that changed contain **pure additions with zero deletions**, all catalyst
timestamp plumbing (every changed line was quoted in the previous report). `ws-server/main.rs` gains
a subscriber and a shutdown line; nothing existing was altered.

Unchanged and unread by any new code: funnel thresholds, ignition thresholds, momentum 0.60 gate,
auto-trader momentum gate, 0.40 deterioration exit, consolidation thresholds, halt thresholds,
catalyst qualification, strategy enablement, sizing, stops, targets, timeouts, max concurrent
positions, production event ordering and filtering.

## 14. Nothing pushed, deployed or accessed

- **Not pushed** — `origin/gpt/audit-remediation-20260909` still points at `0544790`; the new commit
  is local only.
- **Not deployed.** **No SSH.** **No production access.** **No credentials touched.**
- No new dependency in any language.

---

## 15. Would the next real market session produce scientifically useful Alpha data?

**Yes** — for the six capabilities in scope.

| Capability | Status |
|---|---|
| Signal quality | ✅ full signal-time context persisted per episode |
| Earliness | ✅ pre-detection anchors + `moveBeforeDetectionPct` |
| Candidate ranking | ✅ contemporaneous cohorts ranked and persisted |
| Richer outcomes | ✅ 7 horizons, MFE, MAE, drawdown-before-MFE, time-to-target |
| Censoring | ✅ explicit, typed, never coerced to failure |
| Auto-Trader separation | ✅ journal joins to episodes with causality guards |
| Missed opportunities | ✅ discovery recorder unchanged and coexisting |

**Three limitations, all of which the brief explicitly instructed to defer** (§8: *"Do NOT expand
detector event schemas in this completion pass"*), and none of which prevents the analysis:

- ignition internals (compression, trade-frequency acceleration, spread tightening, ask absorption)
- consolidation internals (surge magnitude, volume ratio, consolidation length)
- live bid/ask/spread and numeric relative volume

These are recorded as **unavailable**, not fabricated. Detector-*interaction* analysis will therefore
be partial — momentum, funnel, halt and catalyst combinations are measurable; ignition-internal and
consolidation-internal ones are not. Whether that matters is precisely what the first session should
tell us, which is the sequencing the brief chose.

**One operational prerequisite that is not a code blocker:** this commit is local and undeployed. The
session capture requires deploying it — and per the H1 guard added in Checkpoint 2,
`ops/vps/deploy.sh` will now **refuse** to deploy a release commit absent from `origin`. So the
release branch must be pushed before the deploy, which is H2's still-open work.

---

### Constraints observed

No strategy tuning · no detector logic changed · no auto-trader behaviour changed · no thresholds,
gates, sizing, stops, targets, timeouts or limits touched · production event ordering and filtering
preserved · raw event stream preserved · nothing pushed, deployed, or accessed on production · no new
dependencies.
