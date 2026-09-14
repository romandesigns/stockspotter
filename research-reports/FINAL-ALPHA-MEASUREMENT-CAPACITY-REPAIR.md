# FINAL ALPHA MEASUREMENT CAPACITY REPAIR — REPORT

**Written:** 2026-09-12 · **Strategy baseline:** `ba722698af4fa2b387339eb017a2d58734f969c7` — **FROZEN**
**Deployed measurement commit:** `5b5da9b…` (unchanged — nothing deployed by this task)
**Exporter fix in lineage:** `6247dc03b06ce6619b9c6bf7af99de75a609f3e1` — preserved, is `HEAD`
**Capacity repair:** implemented and validated locally, **uncommitted, unpushed, undeployed**

---

## 1. Exact root cause of 48-A

`MAX_PENDING_OUTCOMES = 4096` was a flat constant with no relationship to how long an episode must be
retained or to how fast episodes arrive.

An episode must be held for `SETTLE_AFTER_SECS = 1920`s. The pending population is therefore the
number of episodes opened in the last 1920s. Measured directly from Session 002's exported artifact:

| Rolling-1920s pending population | Value |
|---|---:|
| median | 260 |
| p95 | 11,989 |
| p99 | 20,063 |
| **peak** | **24,217** |
| **minutes where population exceeded 4,096** | **415 of 1,384 (30%)** |

At 4,096 the set saturated after ≈789s and force-settled the oldest episode on every subsequent
close. Because `finalize(entry, None)` produces ordinary `InsufficientForwardData`, the truncation was
**indistinguishable from the market simply not printing**, and had to be inferred from span
distributions after the fact.

The damage was concentrated exactly where it mattered: regular-session 1800s observability **0.26%**
against premarket's **58.6%**, with identical code and settlement window. The only difference was
episode density.

## 2. Supported episode-rate envelope selected

**16.00 episodes/sec.**

Chosen against measured evidence, not rounded up from the observed mean:

| Session 002 regular session | Value |
|---|---:|
| mean rate | 5.19/s |
| median minute | 4.23/s |
| **peak sustained across a full 1920s window** | **12.61/s** |
| p99 minute | 23.40/s |
| peak instantaneous minute | 37.80/s |

The quantity that sizes a pending set is the **sustained** rate over one settlement window — that is
literally the population. 16.00/s carries **27% headroom over the observed sustained peak** and 3.1×
the mean.

Instantaneous bursts deliberately do **not** size this. A 37.8/s minute contributes ~2,268 episodes
to a population measured in tens of thousands; sizing for the instantaneous peak would inflate memory
by ~3× to absorb an effect worth ~6% of the window.

## 3. Safety factor selected

**1.25** (`PENDING_SAFETY_NUM/DEN = 5/4`). Absorbs a session materially busier than any yet observed
before any eviction occurs. Expressed as an integer ratio so the capacity below is exact const
arithmetic rather than float rounding.

## 4. Derived pending capacity

```rust
const MAX_PENDING_OUTCOMES: usize =
    ((SUPPORTED_EPISODE_RATE_CENTI * OUTCOME_WINDOW_SECS as u64 * PENDING_SAFETY_NUM)
      / (100 * PENDING_SAFETY_DEN)) as usize;
// 16.00/s x 1920s x 1.25 = 38,400
```

**38,400**, derived — not replaced with a round number. A compile-time assertion enforces the §2
invariant directly:

```rust
const _: () = {
    assert!((MAX_PENDING_OUTCOMES as u64) * 100
              >= SUPPORTED_EPISODE_RATE_CENTI * (OUTCOME_WINDOW_SECS as u64),
        "pending capacity cannot hold the supported episode rate for one settlement window; \
         episodes would be evicted before their horizons mature (see defect 48-A)");
};
```

Lowering the capacity, raising the supported rate, or extending the horizon grid now **fails the
build** rather than silently reintroducing capacity censoring.

## 5. Expected steady-state population at 5.19 eps/s

`5.19 × 1920 = ` **9,965** — **26% of capacity**. Session 002's observed peak of 24,217 is **63% of
capacity**.

## 6. Expected population at the supported maximum

`16.00 × 1920 = ` **30,720** — **80% of capacity**, leaving the 1.25 safety factor as genuine headroom
rather than as the operating point.

## 7. Memory-footprint analysis

Measured, not assumed (`the_pending_footprint_stays_within_its_stated_bound` prints and asserts these):

```
PricePoint      =    24 B
PendingOutcome  = 1,176 B   (struct only; heap strings/vecs additional)
capacity        = 38,400
expected_total  = 167.9 MiB
theoretical_max =   1.80 GiB
```

| Basis | Per entry | Total |
|---|---:|---:|
| **Expected** — Session 002 mean of 141.4 retained points | 1,176 + 142×24 ≈ 4.6 KB | **~168 MiB** |
| Plus heap (symbol strings, `momentum_track`, `confirmations`, context) | ≈ +0.7 KB | **~195–210 MiB** |
| Conservative — p90 path (292 points) for *every* slot | ≈ 8.2 KB | ~300 MiB |
| Theoretical — every slot at `MAX_PATH_POINTS` | 1,176 + 2048×24 ≈ 50 KB | **1.80 GiB** |

The theoretical maximum requires all 38,400 simultaneously-pending episodes to be top-0.5%-liquidity
names printing >1/sec for the full window. Session 002 had **0.52%** of episodes at the path cap, so
the aggregate is driven by the mean, not the ceiling. `MAX_PATH_POINTS` was deliberately **not**
reduced — that would change excursion precision, i.e. measurement semantics, to solve a memory
problem that the evidence says does not exist.

**Stated uncertainty:** I could not verify VPS RAM — this task forbids SSH. Docker Compose sets no
memory limit. The expected ~200 MiB is unremarkable for a host already running nine containers, but
**confirm available RAM before deployment**, and treat `pending_peak` (§10) as the operational check.

### The cost that was *not* memory — and nearly shipped

Raising capacity 9.4× also multiplies per-event CPU 9.4×, because both hot paths were linear in the
pending set: `extend_paths` compared every entry's symbol on every price, and `settle_due` rescanned
the whole vector on every event.

This was not hypothetical. The load tests took **612.56 seconds** against the naive `Vec` store.

A collector that cannot keep up lags its broadcast receiver and silently misses observations — it
would have traded one measurement defect for another. §6 says to redesign rather than hide the cost,
so the pending store was restructured (§8). **Same tests now run in 3.11 seconds — a ~197× reduction
on identical work.**

## 8. Settlement / eviction architecture after repair

```
pending: BTreeMap<(opened_at, id), PendingOutcome>   // key order == settlement order
pending_by_symbol: HashMap<String, Vec<PendingKey>>  // price fan-out index
```

| Operation | Before | After |
|---|---|---|
| Normal settlement | full rescan per event, O(n) | range query `..=(now − 1920s, u64::MAX)`, touches only due entries |
| Price fan-out | compare every pending symbol, O(n) | symbol index → only affected episodes |
| Capacity eviction | `pending.remove(0)`, O(n) | first key, O(log n) |

**Age governs normal settlement**, as §3 requires: settlement is `opened_at + SETTLE_AFTER_SECS`, a
constant offset, so key order *is* due order. Count pressure is now a separate, explicitly-marked
path that should never fire within the supported envelope. `take_pending` removes from both indexes
so they cannot disagree.

## 9. New capacity censor reason

`CensorReason::PendingCapacityReached` (`crates/backtest-metrics/src/horizon.rs`), serialized
`pending_capacity_reached`, applied via `HorizonOutcome::mark_capacity_censored()`.

Deliberately narrow in two directions:

- **Matured observations are kept.** A 30s horizon that completed before eviction is a real
  measurement. §4's "preserve whatever valid short-horizon observations already matured" is
  implemented and directly tested.
- **Only `InsufficientForwardData` converts.** `DataGap` is a genuine property of the prices we did
  see; `SessionEnded`/`CaptureEnded` already name their own cause. Relabelling those would trade one
  misattribution for another. Directly tested
  (`capacity_censoring_leaves_a_real_data_gap_alone`).

Downstream can now separate *the market produced no further prices* from *we stopped looking*.
**§4's required property is met: no analyst needs to infer saturation from span distributions again.**

## 10. New operational counters / health information

On `MeasurementCollector` (which owns the pending set — deliberately not duplicated onto
`MeasurementHealth`, which would create two sources of truth):

| Accessor | Meaning |
|---|---|
| `pending_outcomes()` | current pending count |
| `pending_peak()` | high-water mark |
| `capacity_evictions()` | episodes force-settled for capacity |
| `pending_capacity()` | the configured bound, so peak can be judged against it |

Surfaced two ways, no new infrastructure and no client-protocol change:

- **During the session** — a `warn!` on each power-of-two eviction, naming capacity and stating that
  long-horizon outcomes are capacity-censored.
- **At shutdown** (`main.rs`) — always reported, pass or fail: a `warn!` with evictions/peak/capacity
  if capacity bound, otherwise an `info!` confirming *"measurement pending capacity never bound"*.
  The negative statement matters as much as the positive one.

## 11. Session-002 regression result — **PASS**

`session_002_pressure_profile_no_longer_truncates_long_horizons` replays the pressure profile
(5.19 eps/s, 1920s settlement, >9,000 episodes) on synthetic time:

- **capacity evictions: 0** — the rate that broke Session 002 no longer force-settles anything.
- `pending_peak` **> 4,096** — the fixture genuinely reproduces the Session 002 condition (it would
  have saturated the old bound) rather than passing vacuously.
- `pending_peak` ≤ 38,400 — still bounded.

## 12. Load test A — 5.19 eps/s for > 1920s — **PASS**

`a_at_the_observed_session_002_rate_nothing_is_capacity_evicted`: zero capacity evictions; peak within
capacity; 1800s reachable.

## 13. Load test B — supported maximum (16.00/s) for > 1920s — **PASS**

`b_at_the_declared_supported_rate_nothing_is_capacity_evicted`: **zero capacity evictions at the full
declared envelope** — the §2 invariant demonstrated, not just asserted. Pending set bounded.

## 14. Overload / burst result — **PASS**

`c_a_burst_above_the_envelope_evicts_explicitly_and_stays_bounded` drives 40/s for a full settlement
window (76,800 episodes vs 38,400 capacity):

- evictions **> 0** — the path is genuinely exercised;
- pending set **remains bounded**;
- every censored horizon on an evicted episode is `PendingCapacityReached`, and the test **fails
  loudly if any is `InsufficientForwardData`**. No silent substitution is possible.

## 15. Recovery result — **PASS**

`d_the_collector_recovers_and_returns_to_age_based_settlement`: overload (48,000 episodes), then a
quiet period, then a return to 5.19/s. The backlog **drains to zero via age-based settlement**, and
the eviction counter **does not advance** once the rate is back inside the envelope.

*This test initially failed — my own error, not the code's.* I drove 40/s for 600s = 24,000 episodes,
which is *below* the 38,400 capacity, so `assert!(during > 0)` could never hold. The burst was
widened to 1,200s so it actually exceeds capacity.

## 16. 48-C queue-loss implementation

The honest constraint first: when `try_send` fails the queue is full **by definition**, so a marker
cannot be inserted at that moment — the attempt would fail for the same reason. Designing around that
rather than pretending otherwise:

- Loss accumulates in `queue_lost_unreported`, with `queue_loss_onset_micros` stamped on the 0→1
  transition and `queue_loss_classes` accumulating a **bitmask** of affected record classes.
- The next record that *is* admitted carries a `queue_loss` object: `{lost, onset, onsetMicros,
  classes[], reason:"queue_full"}`.
- On success the counter is decremented by **exactly what that record reported**, so a loss arriving
  in between stays pending for the next record — never double-reported, never dropped.
- `try_send` and bounded behaviour preserved; market dispatch is never blocked.
- Class tracking is a fixed 8-bit mask (7 known kinds + `other`), so cardinality cannot grow.

**§9's required invariant is met:** once writing resumes, the persisted stream carries the span's
onset, count and affected classes. Session 002's 1,339 lost records (0.025%) would now be locatable
in the data instead of only in container logs.

## 17. Exporter-fix lineage confirmation

`6247dc03b06ce6619b9c6bf7af99de75a609f3e1` is **`HEAD`** and is on `origin`. Not rewritten, not
squashed, not amended. The capacity repair sits **uncommitted on top of it**, so when it is committed
the exporter fix necessarily precedes it in the lineage, as §10 requires.

```
6247dc0 (HEAD -> release/operating-run-20260907, origin/release/…) Classify discovery segments by their capture directory
5b5da9b Repair causal alpha measurement and discovery capture
ba72269 Build causal alpha measurement pipeline
```

## 18. Files changed

```
 crates/backtest-metrics/src/horizon.rs    | 108 ++++++++
 crates/market-data/src/discovery_audit.rs | 133 ++++++++-
 crates/ws-server/src/main.rs              |  20 ++
 crates/ws-server/src/measurement.rs       | 431 ++++++++++++++++++++++++++++--
 python/test_export_session.py             |  15 +-
 5 files changed, 677 insertions(+), 30 deletions(-)
```

Five files, all measurement/capture/telemetry. **No strategy crate appears.**

## 19. Tests added / modified

**12 new tests; 1 test-infrastructure fix.**

`horizon.rs` (2): capacity censoring converts only missing forward data (and preserves matured
observations) · capacity censoring leaves a real `DataGap` alone.

`measurement.rs` (7): pending footprint stays within its stated bound (prints measured sizes) ·
capacity is derived from supported rate and settlement window · **A** observed-rate no eviction ·
**B** supported-rate no eviction · **C** burst evicts explicitly and stays bounded · **D** recovery
returns to age-based settlement · **Session-002 pressure profile no longer truncates long horizons**.

`discovery_audit.rs` (3): every emitted class maps to exactly one bit · a loss span decodes to every
class it swallowed · an empty mask names nothing.

`test_export_session.py` (fix): tests imported `export_session` by name, which only worked from
`python/`. Replaced with a path-based `load_exporter()` so the documented command
`python -m unittest python.test_export_session` works from the repo root. **This was a real defect in
my own §12 validation instructions** — the suite could not be run as specified.

## 20. Full validation results

| Suite | Result |
|---|---|
| `cargo test -p backtest-metrics` | **114 passed**, 0 failed |
| `cargo test -p market-data` | passed, 0 failed (16 discovery-audit tests) |
| `cargo test -p ws-server` | **77 passed**, 0 failed |
| `cargo test -p auto-trader` | passed, 0 failed |
| **`cargo test --workspace --no-fail-fast`** | **439 passed, 0 failed, 0 errors** |
| `python -m unittest discover -s python -p 'test_discovery*.py'` | 10 passed, OK |
| `python -m unittest python.test_export_session` | **20 passed**, OK |
| `bun --cwd=apps/client run test` | **60 pass, 0 fail** (500 assertions) |
| `tsc --noEmit -p apps/client/tsconfig.app.json` | exit **0** |
| `tsc --noEmit -p apps/mobile/tsconfig.json` | exit **0** |

`pytest` not run locally — not installed on this machine, reported as before; CI runs it, and the same
Python files are covered above by `unittest`.

No repository-wide `rustfmt` churn: `cargo fmt` was not run (it fails repo-wide on 63 pre-existing
files unrelated to this work).

**One transient failure worth recording rather than hiding.** An intermediate
`cargo test --workspace` run reported `fast-funnel` and `ignition-detector` targets failing while
simultaneously reporting 437 passed and zero assertion failures. Both crates passed cleanly in
isolation, local disk was at 95–97% during the 3.3 GB Session 002 transfer, and the failure did not
reproduce once disk recovered to 22 GiB. It was build-time disk pressure, **not** a code fault — and
notably those are strategy crates this work does not touch.

## 21. Adversarial invariant results

| Invariant | Result | Evidence |
|---|---|---|
| Every configured horizon remains reachable | ✅ | compile-time assert + A/B pass with 1800s observed |
| Normal settlement is age-driven | ✅ | `settle_due` is a range query on `opened_at`; eviction is a separate marked path |
| Pending capacity sufficient at the declared rate | ✅ | **B: zero evictions at 16.00/s sustained** |
| Memory remains bounded | ✅ | measured 168 MiB expected / 1.80 GiB theoretical; asserted in test |
| Capacity eviction is explicit and distinguishable | ✅ | **C fails loudly if any evicted horizon reports `InsufficientForwardData`** |
| Pending-cap pressure is observable operationally | ✅ | peak/evictions/capacity logged during and at shutdown, both directions |
| Collector recovers after overload | ✅ | **D: drains to zero, counter stops advancing** |
| Discovery queue loss becomes visible in-band | ✅ | `queue_loss{lost,onset,classes}` on the next admitted record |
| No measurement failure can block market dispatch | ✅ | `try_send` + bounded channel preserved; no `await`, no blocking send added |
| No strategy decision changes | ✅ | diff touches no strategy crate; `live.rs` untouched |
| Session 001 and 002 byte-identical | ✅ | 29 files re-checksummed, zero differences |

## 22. Session 001 checksum preservation — **unchanged**

## 23. Session 002 checksum preservation — **unchanged**

Both re-verified after all work: **29 files across the two sessions, all checksums identical** to the
baseline recorded before any edit. No corrected copies, no filtering, no regenerated manifests, no
pooling. `INSTRUMENT VALIDATION SESSION 001` and `002` remain immutable evidence of instrument
evolution.

## 24. No strategy logic changed — confirmed

Untouched: universe construction · relative volume · Fast Funnel · float logic · Ignition · ignition
cooldown · flat-base · Momentum Scorer · momentum weights · momentum thresholds · Consolidation ·
Micropullback · Halt Detector · catalyst behaviour · Auto-Trader gates · sizing · entries · exits ·
targets · stops · strategy enablement.

`crates/market-data/src/live.rs` is not in the diff, so `ScanEvent`, the wire format and event
ordering are unchanged. Every change is in the measurement consumer, the capture writer, or telemetry.

## 25. `git diff --stat`

```
 crates/backtest-metrics/src/horizon.rs    | 108 ++++++++
 crates/market-data/src/discovery_audit.rs | 133 ++++++++-
 crates/ws-server/src/main.rs              |  20 ++
 crates/ws-server/src/measurement.rs       | 431 ++++++++++++++++++++++++++++--
 python/test_export_session.py             |  15 +-
 5 files changed, 677 insertions(+), 30 deletions(-)
```

## 26. `git status --short`

```
 M crates/backtest-metrics/src/horizon.rs
 M crates/market-data/src/discovery_audit.rs
 M crates/ws-server/src/main.rs
 M crates/ws-server/src/measurement.rs
 M python/test_export_session.py
?? .claude/
?? AUDIT-2026-09-09.md
```

**Uncommitted, as instructed.** Nothing staged, nothing pushed, nothing deployed.

## 27. Deployment recommendation

**Yes — recommended, with two pre-deployment checks.**

Every §13 invariant is proven, the full suite is green, the derivation is evidence-based and
compile-enforced, and the CPU regression that would have silently broken the collector was found and
removed before it shipped.

**Check 1 — confirm VPS RAM.** Expected footprint is ~200 MiB and the theoretical worst is 1.80 GiB.
I could not verify available RAM (SSH forbidden this task) and Compose sets no limit. Confirm headroom
before deploying; consider a Compose memory limit so the worst case is bounded by policy rather than
by argument.

**Check 2 — watch the shutdown line on day one.** *"measurement pending capacity never bound"* with
`pending_peak` well under 38,400 is the confirmation that 48-A is closed. If `pending_peak` exceeds
~30,000, the supported envelope needs revisiting before trusting long horizons.

Deploy alone, capture one session, validate, and only then consider strategy work.

## 28. Is one more complete session sufficient to qualify ANALYSIS BASELINE 001?

**Yes for the measurement instrument — with one honest caveat about scope.**

Every defect that blocked Session 002 is now addressed, and each is verified by a test that fails if
it regresses: F1 (settlement collision), F4 (causal timestamps), 48-A (capacity), 48-B (exporter
classification), 48-C (queue-loss visibility), R6 (temporal consistency). Session 002 already passed
§8B–§8E; only §8A failed, and §8A is what this batch fixes.

So if the next session shows **zero capacity evictions, 1800s observability in the regular session
comparable to premarket's ~58%, zero inverted episodes, and continuous discovery coverage**, the
instrument is validated and the session qualifies.

**The caveat is not about measurement.** Two limits found in Session 002 are *not* measurement defects
and will persist:

- **`SkipReason` evidence still does not exist.** Production runs the paper-runtime path;
  `auto_trader_journal.jsonl` has been stale since 2026-09-04, and skips go only to container stdout.
  Episode→trade attribution will remain incomplete until that is addressed separately — it is an
  Auto-Trader instrumentation question, and this batch changes no Auto-Trader code by instruction.
- **Structural properties are unchanged by design.** Ignition will still dominate ~98% of episodes
  under `IGNITION_UNIVERSE_MODE=1`; catalyst coverage will still track funnel membership at ~1.5%;
  episodes will still cluster heavily within symbols.

A correct instrument measuring a narrow population still yields a narrow result. The next session
should qualify as `ANALYSIS BASELINE 001` **for outcome and recall analysis**, and should be expected
to carry those two stated limitations into whatever Alpha work follows.

---

## Standing confirmations

- **Not committed, not pushed, not deployed. No SSH, no production access** in this task.
- **Strategy frozen**; no strategy file touched.
- **Both validation sessions byte-identical**, re-verified after all work.
- **No Alpha analysis performed** — every figure here is a capacity, memory or data-quality
  measurement.
- **Nothing hidden:** the CPU regression, my failed test D, the transient workspace failure, the
  broken Python test-import in the §12 command, and the unverifiable VPS RAM are all reported.

**Stopping here. Awaiting instruction.**
