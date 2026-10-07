# ALPHA OPPORTUNITY INTELLIGENCE V1 — Pre-Checkpoint Correction Report

**Date:** 2026-09-15, 9:19 AM EDT
**Project:** Stockspotter
**Workstream:** Alpha Opportunity Intelligence V1
**Milestone:** Pre-Checkpoint Correction Review
**Branch:** `research/alpha-reports`
**HEAD at start and end:** `c8f78dc2677591c6b9de37d2dfc034b506810ade` (unchanged)
**State:** implemented locally, validated, **uncommitted and unstaged**

---

## 1. Executive verdict

All three corrections are implemented, plus the requested audit, the evaluation
artifact and the full research-chain test. **26 new tests**, all passing;
workspace at **532 passed / 0 failed**. No pre-existing test was modified.

The architecture was not redesigned. Everything listed as "keep intact" is
intact: opportunity collapse, detector isolation, regimes, EarlyQuality /
Continuation separation, cross-sectional ranking, confluence-as-feature,
shadow-only isolation, deterministic replay, discovery attribution, the bounded
writer, production isolation.

| Correction | Outcome |
|---|---|
| 1 — opportunity ↔ outcome join | Implemented offline. `OpportunityEvaluationRecord` + `oi_evaluate`. No outcome field added to the live record. |
| 2 — no id-identity assumption | Replaced with explicit temporal membership. The identity assumption was **unsafe and now proven so by test**: episode sequences 2 and 3 belong to opportunity sequence 1. |
| 3 — missingness must not determine rank | Investigated, then core-gated coverage normalization. Quantified: the old policy understated a 0.65-coverage candidate by **35%**. |
| Audit — priorMovePct contradiction | Prose was imprecise; the **model is correct as built**. Clarified, model unchanged. |

**Recommendation: READY FOR CHECKPOINT**, with one item for your decision (§18).

## 2. Opportunity ↔ episode mapping architecture

New module `crates/backtest-metrics/src/membership.rs` (342 lines).

```
opportunities[] ──┐
                  ├──> map_memberships() ──> MembershipReport
episodes[]      ──┘                            ├── mappings[]            (opportunityId -> episodeIds[])
                                               ├── ambiguous_episodes[]  (reported, never assigned)
                                               └── unassigned_episodes[] (reported, never dropped)
```

Inputs are grouped by `(session_date, symbol)` and sorted before use, so neither
arrival order can change the result. Only causal fields participate: symbol,
session date, and the two open/close intervals. The outcome being joined is
never consulted — proved by `n08`.

`MembershipReport` exposes `members_of(opportunityId)` and
`opportunity_of(episodeId)`, so the relationship is navigable in both
directions.

### Why the identity join had to go

`OpportunityId` and `EpisodeId` share the shape `symbol:sessionDate:sequence`,
and the V1 report treated that as licence to join on the key. The lifecycles
differ on purpose, in exactly one rule:

| Event | `EpisodeTracker` | `OpportunityIntelligence` |
|---|---|---|
| `FollowThroughRejected` | **closes** the episode | **absorbs** it; the opportunity continues |

So the counters advance at different rates the moment any invalidation occurs.
`n02` demonstrates the consequence directly: three fragmented episodes
(sequences 1, 2, 3) belong to **one** opportunity (sequence 1), and
`AAA:2026-09-14:2` as an episode has no opportunity counterpart at all. A key
join would have attached the wrong outcome to the wrong score — and would have
done so *more* often precisely for the invalidation-heavy fragmented moves this
layer exists to study.

## 3. Exact membership rules

For each episode, evaluated in this order:

| # | Rule | Result |
|---|---|---|
| 1 | Episode's `(symbol, sessionDate)` has no opportunity | `Unassigned{NoOpportunityForSymbolSession}` |
| 2 | Candidate set = opportunities where `ep.openedAt >= op.openedAt` **and** `ep.openedAt <= windowEnd` | — |
| 3 | Candidate set empty | `Unassigned{OutsideEveryWindow}` |
| 4 | Candidate set > 1 | `Ambiguous{MultipleCandidates}` |
| 5 | Exactly one candidate, and `ep.closedAt > windowEnd` | `Ambiguous{EpisodeOutlivesOpportunity}` |
| 6 | Exactly one candidate, otherwise | **Member** |

`windowEnd = op.closedAt` where the opportunity closed; otherwise
`op.lastSeenAt`, with `opportunityOpen: true` recorded. An open opportunity's
tail is genuinely unknown, so nothing after `lastSeenAt` is claimed — the
mapping **under-assigns rather than guesses** (`n09`).

Membership is decided by where the episode **opened**. An episode that opened
before its candidate opportunity existed is not a member, whatever else
overlaps (`n10`). Symbol and session mismatches are not near-misses; they are
not applicable (`n05`, `n06`).

Per-opportunity status: `Resolved` / `PartiallyAmbiguous` / `NoEpisodes`. The
opportunity's own `episodeFragments` count travels alongside the resolved
member count, so the two can be compared without re-deriving either.

## 4. Ambiguity semantics

Two ambiguity reasons, both reported and both leaving the episode unassigned:

**`EpisodeOutlivesOpportunity{opportunityId, episodeClosedAt, windowEnd}`** —
the episode was still open after its candidate window ended, so part of its
life, and therefore part of any outcome measured from it, belongs to something
else.

**`MultipleCandidates{candidates[]}`** — the opening instant falls inside more
than one window. The engine keeps at most one opportunity open per symbol, so
in practice this means two windows touch at a boundary instant, or the inputs
came from different runs. Reachable and tested (`n04`).

The design choice worth flagging: window bounds are **inclusive at both ends**.
An episode opening exactly at op1's close and op2's open therefore matches both
and is reported ambiguous, rather than silently awarded to one. That is the
conservative reading of "do not assign when evidence is ambiguous."

Nothing is inferred and nothing is dropped: every episode lands in exactly one
of member / ambiguous / unassigned, and `q10` asserts the counts reconcile.

## 5. Outcome-join architecture

New module `crates/backtest-metrics/src/evaluation.rs` (552 lines) plus
`bin/oi_evaluate.rs` (135 lines).

```
shadow log (snapshots) ──> reconstruct_opportunities_from_snapshots()
                                        │
                                        ▼
                              map_memberships()  ◄── measurement episodes
                                        │
                                        ▼
                               build_evaluation()
                                        │
                                        ▼
                      OpportunityEvaluationRecord per scoring window
```

### Offline by construction, not by discipline

`OpportunityScoreSnapshot` gains **no** outcome field. The join produces a
separate type that can only be built after the forward observations exist.
There is nowhere for an outcome to leak backwards into the live record, so
causal purity is a property of the schema rather than of this tool's
restraint. `q05` proves it: the same snapshots joined against two opposite
futures produce byte-identical scoring halves.

### The anchor problem, stated rather than hidden

Measurement outcomes are anchored at an **episode's** signal instant; a score
snapshot is taken at a **ranking window** instant. These are different moments,
so "the forward return from this score" is not directly available. What is
available is the forward return from the episode this opportunity contained
around that time.

Rather than absorb that gap, every joined outcome carries:

- `anchorRuleVersion` — `anchor-v1-next-at-or-after-then-latest-before`
- `anchorSelection` — `next_at_or_after` (preferred: the score precedes the
  measurement) or `latest_before` (backward-looking; treat separately)
- `anchorOffsetSecs` — **signed** `episode signalAt − score timestamp`

Selection prefers the first member episode anchored at or after the score;
failing that, the most recent earlier anchor, flagged. Ties break on episode id,
so the choice is deterministic. On the synthetic run this split 11 forward / 20
backward — the offset is not a formality.

### Opportunity windows are derived, not persisted

A gap found while building the CLI: **`Opportunity` records are persisted
nowhere.** The shadow log holds scoring decisions; the measurement capture holds
episodes. So the join had no windows to work from.

`reconstruct_opportunities_from_snapshots` derives them from data already on
disk. `openedAt = timestamp − opportunityAgeSecs` recovers the open instant
**exactly**. The close is **not** recoverable — scoring stops at the last
ranking window, up to the inactivity boundary before the real close — so the
reconstructed window is marked open and the tail is not claimed. Adding a
second live write path for something recoverable was the alternative, and was
rejected. `q10` asserts the reconstruction never over-assigns and that any
shortfall is reported.

## 6. `OpportunityEvaluationRecord` schema

30 top-level fields, camelCase on the wire.

**Provenance** — `schemaVersion`, `versions` (all nine: opportunity schema,
feature schema, regime classifier, price regime, both models, ranking, **score
policy**, config fingerprint), `membershipRulesVersion`.

**Identity / membership** — `opportunityId`, `symbol`, `sessionDate`,
`memberEpisodeIds[]`, `membershipStatus`, `ambiguousEpisodeIds[]`.

**The scoring decision, as it stood** — `scoreTimestamp`, `windowId`, `regime`,
`priceRegime{band,lower,upper}`, `earlyQuality` (full `ShadowScore` incl.
components, missingness, coverage, rawWeighted, presentWeight, totalWeight,
unrankableReason), `earlyQualityRank`, `continuation` (same), `continuationRank`,
`earlyCohortSize`, `continuationCohortSize`, `shadowState`,
`featureCoverage{earlyQuality, continuation, earlyQualityComparable,
continuationComparable}`, `confluenceCount`, `detectorsSeen[]`,
`confirmationSpanSecs`, `opportunityAgeSecs`, `currentPrice`,
`moveBeforeDetectionPct`, `moveFromStartPct`, `episodeFragments`,
`invalidationsAbsorbed`, `rawEventCount`.

**What happened next** — `outcome` or `joinGap`, never both and never neither
(asserted in `q06`):

`outcome` = `episodeId`, `anchorRuleVersion`, `anchorSelection`,
`anchorOffsetSecs`, `signalAt`, `signalPrice`, `returns[]` (all seven horizons
30/60/180/300/600/900/1800, censoring intact), `excursion` (MFE, MAE, seconds to
each, **drawdown before MFE**), `timeToTarget[]` (+2% / +5% / +10% first touch),
`observedSpanSecs`, `observationCount`, `censoredHorizons[]`,
`censorReasons[]`, `fullyObserved`.

`joinGap` = `opportunity_not_supplied` / `no_member_episode` /
`no_settled_outcome` / `membership_ambiguous`.

Every field the correction batch asked to be evaluable is present, and every
outcome measure it asked to evaluate against is carried whole rather than
summarized.

## 7. Missing-feature scoring investigation

Three findings, all mechanical properties of the code rather than judgements.

### Finding 1 — missingness is block-structured, not arbitrary

All four momentum features read the same `Option<&MomentumFeatures>`, so they
are **co-missing as a unit**. `confluence.detectorCount` is `Some(..)`
unconditionally — it is derived, never observed. The missingness space is
therefore small and enumerable.

### Finding 2 — the reachable coverage classes

Both models' weights sum to exactly 1.00.

*EarlyQuality:*

| Evidence | Coverage | Present | v1 status |
|---|---|---|---|
| momentum + earliness | 1.00 | 5 | rankable |
| momentum, no earliness | 0.85 | 4 | rankable |
| earliness only | 0.15 | 1 | already unranked |

Two rankable classes, and the 0.85 class carried a hard **15% score cap**
purely from a missing pre-detection measurement.

*Continuation:*

| Evidence | Coverage | Present | v1 status |
|---|---|---|---|
| full | 1.00 | 6 | rankable |
| no halt | 0.90 | 5 | rankable |
| no momentum | 0.75 | 4 | rankable |
| no priorMove | 0.65 | 5 | rankable |
| moveFromStart + confluence | 0.30 | 2 | **rankable** |
| confluence + halt only | 0.20 | 2 | **rankable** |
| confluence alone | 0.10 | 1 | already unranked |

A *continuation confidence* with **no measured move at all** (0.20) was being
ranked against a fully-observed candidate.

### Finding 3 — `contribution = 0` is not neutral, it is maximally pessimistic

Minimum attainable transformed value per feature:

| Feature | Floor |
|---|---|
| `momentum.maSlope.presence` | 0.00 |
| `momentum.structure` | 0.00 |
| `momentum.volumeConfirmation` | 0.00 |
| `momentum.wickRejection` | 0.00 |
| `earliness.priorMovePct` | 0.00 |
| `continuation.priorMovePct` | **0.10** |
| `continuation.moveFromStartPct` | 0.00 |
| `confluence.detectorCount` | 0.00 |
| `halt.proximityRatio` | 0.00 |

Zero is at or below the floor of every transform, and **strictly below the
observable floor** of `continuation.priorMovePct`. The v1 policy was therefore
already imputing — it imputed the worst observable value, and for one feature
something worse than observable. This is the decisive argument: normalization
does not add an assumption, it replaces a severe one with a weaker one.

## 8. Final scoring-comparability policy and rationale

**`SCORE_POLICY_VERSION = "score-policy-v2-core-gated-coverage-normalized"`**

Versioned separately from the models, deliberately: the feature sets and
weights did not move, only the comparability transform. Bumping a model version
would have implied the model changed, which an analyst needs to distinguish.
`OiVersions` now carries `scorePolicy`; `ShadowScore` carries `policyVersion`.

### The policy

Three gates, fixed order so the reported reason is stable:

1. every **core** feature present, else `CoreFeatureMissing`;
2. at least `MIN_PRESENT_INPUTS` (2) present, else `TooFewInputs`;
3. `coverage >= MIN_COMPARABLE_COVERAGE` (0.50), else `InsufficientCoverage`.

Passing all three: `value = rawWeighted / presentWeight` — the present-weighted
mean of transformed inputs, in `[0, 1]`.

**Core sets.** EarlyQuality: the momentum block (0.85 of the model; co-missing
as a unit). Without any momentum evidence there is no quality to assess, and
earliness alone would rank a candidate on one weak input. `earliness.priorMovePct`
stays **optional** — a symbol with no pre-detection history is still assessable
on its momentum structure. Continuation: `continuation.moveFromStartPct` only —
the opportunity's own measured move. The model's question presupposes a move;
`confluence.detectorCount` cannot carry that requirement because it is always
present, and `priorMovePct` stays optional because pre-detection history is
genuinely unavailable for some symbols.

**Why 0.50.** Not tuned. Set to exclude the coverage classes that carry no
evidence of the quantity the model names — continuation's 0.20 and 0.30 classes
(`m81` proves the 0.30 class is now explicitly unranked and was rankable
before).

### Why this class of solution, and what it assumes

- It **preserves UNKNOWN ≠ ZERO**: an absent input still records `raw: None`,
  `transformed: None`, appears in `missing`, and is excluded from
  `presentWeight`. `m79` asserts all of it.
- It is **causal** — no new input, no future observation.
- It is **deterministic** — `m83`.
- The assumption it makes, stated plainly: an absent optional feature is
  treated as scoring at the present-weighted mean. Per Finding 3 this is
  *weaker* than what v1 did, not additional.
- **Bounded blast radius**: since both models' weights sum to 1.0, at full
  coverage normalization divides by 1.0, so a fully-observed candidate's score
  is **numerically identical** to v1. `m84` pins it, and §10 confirms it on real
  output (30 full-coverage rows, 0 moved).

### Policies A, C and D remain available offline

Every record persists `totalWeight`, `presentWeight`, `coverage`,
`rawWeighted`, the final comparable `value`, and each component's own weight and
transformed value. So the v1 sum policy (A′), cohort ranking by
availability class (C), and any re-weighting (D) are all reconstructible from
the artifact **without a code change and without re-running the engine** —
`m82` asserts each reconstruction explicitly. Investigation did not find a
worse statistical assumption, so no STOP was required; the alternatives stay
testable rather than foreclosed.

### Behaviour change, quantified

Some candidates that were ranked are now explicitly unranked (continuation at
coverage < 0.50). This is intended. §10 reports what actually happened on real
output.

## 9. EarlyQuality `priorMovePct` clarification

**The implementation is correct. The V1 report's prose was imprecise.** No
model change was made.

The precise statement: *EarlyQualityScore excludes positive continuation
magnitude as a reward, and includes prior movement only as an earliness
penalty.*

The transform is the proof:

```
Piecewise { knots: [(-5.0, 0.2), (0.0, 1.0), (2.0, 0.8),
                    (5.0, 0.4), (10.0, 0.1), (20.0, 0.0)] }
```

It **peaks at 0.0% prior move** and decreases monotonically in both directions:
to 0.0 by +20%, and to 0.2 at −5%. There is no input value for which *more*
prior movement yields a *higher* contribution. Larger moves are penalised, not
rewarded — the opposite of what a magnitude reward would do, and the reason the
score can be an early signal rather than a confirmation one.

Rewarding magnitude is `ContinuationConfidence`'s job, where
`continuation.priorMovePct` uses a rise-then-flatten transform at weight 0.35.
The two models use the same *input* with opposite *shapes*, which is the whole
point of keeping them separate. `f29` and `g40` assert the resulting quantities
are independent.

## 10. Full synthetic end-to-end evaluation result

### The research chain (`q01`–`q10`)

Built from the **real** `EpisodeTracker`, the real `OpportunityIntelligence` and
the real `evaluate_horizons` — no stand-ins:

```
market events -> 3 detector episodes -> 1 collapsed Opportunity
  -> multiple ranking windows -> forward price path -> episode outcomes
  -> offline join
```

Proved: scoring is causal (`q05`); fragmentation does not duplicate the
opportunity or its rank slot (`q02`); each snapshot joins to a stated forward
anchor (`q03`); censored observations stay censored with reasons (`q04`); future
information never influences the historical score (`q05`); the record is
self-describing (`q06`); the join is deterministic and not id-based (`q07`);
gaps are explicit (`q08`, `q09`); windows reconstruct from snapshots alone
(`q10`).

### The CLI, on real files

`oi_replay` → 31 snapshots; a consistent 10-episode artifact → `oi_evaluate`:

```json
{ "snapshotsIn": 31, "recordsOut": 31, "joined": 31,
  "joinedForward": 11, "joinedBackward": 20,
  "fullyObservedOutcomes": 0,
  "gapOpportunityNotSupplied": 0, "gapNoMemberEpisode": 0,
  "gapNoSettledOutcome": 0, "gapMembershipAmbiguous": 0,
  "earlyQualityComparable": 30, "continuationComparable": 31 }
membership: 5 mappings, 0 ambiguous, 0 unassigned (rules membership-v1-temporal-containment)
```

Every record resolved 2 member episodes onto 1 opportunity; all 31 statuses
`resolved`.

### Correction 3, measured before/after on identical input

| | Value changed | Unchanged | Became unranked | Became rankable |
|---|---|---|---|---|
| earlyQuality | 0 | 30 | 0 | 0 |
| continuation | **31** | 0 | 0 | 0 |

Full-coverage earlyQuality rows: 30 — **none moved**, confirming the invariance
`m84` asserts.

Worked example, `S0:2026-09-14:1 / oiw-1`, continuation:

| | |
|---|---|
| coverage | 0.65 |
| v1 policy value | 0.075000 (absent features imputed at 0) |
| v2 policy value | 0.115385 (= rawWeighted 0.075000 / presentWeight 0.65) |
| **v1 understatement** | **35.0% of the v2 figure** |

On this fixture no candidate lost rankability — the 0.20/0.30 continuation
classes are reachable in principle (`m81`) but did not occur here. What changed
is score *values* for partial-coverage candidates, by up to 35%.

## 11. Tests added / modified

**26 added. Zero pre-existing tests modified.**

Group M — score comparability (6), `opportunity_tests.rs`:
`m79_an_absent_feature_is_never_treated_as_observed_zero` ·
`m80_a_non_informative_absent_optional_feature_does_not_change_rank` ·
`m81_insufficient_evidence_remains_unranked_with_an_explicit_reason` ·
`m82_score_comparability_is_reconstructible_from_the_record` ·
`m83_the_policy_is_deterministic_and_round_trips` ·
`m84_full_coverage_scores_are_identical_to_the_previous_policy`

`m80` is worth reading: "non-informative" is *made precise* rather than
asserted — the optional feature's transformed value is set to exactly the
present-weighted mean of the others by inverting the piecewise transform in the
test, so it carries no information the rest do not. Under v1 the candidate
lacking it scored strictly lower; under v2 they tie, and the test asserts both.

Group N — membership (10), `membership_tests.rs`. The eight required
invariants, in order: `n01` one↔one · `n02` three fragments → one opportunity ·
`n03` two opportunities stay separate · `n04` boundary straddle not silently
assigned (both shapes) · `n05` different symbols never join · `n06` different
sessions never join · `n07` deterministic under input reversal · `n08` future
outcomes never influence membership. Plus `n09` open-opportunity
under-assignment and `n10` episode predating its opportunity.

Group Q — evaluation join (10), `evaluation_tests.rs`: `q01`–`q10` as described
in §10.

`MIN_PRESENT_INPUTS` note: with the chosen core sets, `TooFewInputs` is
unreachable through the public scorers (EarlyQuality's core implies 4 present
inputs, Continuation's implies 2). Rather than leave an untested branch, `m81`
exercises `finalize_score` directly — the same discipline applied to the
attribution module's `StoppedAtVisibility` in the previous batch.

## 12. Complete validation results

Every command from the batch, as run:

| Command | Result |
|---|---|
| `cargo test -p backtest-metrics` | **192 passed, 0 failed** |
| `cargo test -p ws-server` | **92 passed, 0 failed** |
| `cargo test -p auto-trader` | **45 passed, 0 failed** |
| `cargo test --workspace --no-fail-fast` | **532 passed, 0 failed** |
| `bun --cwd=apps/client run test` | **60 pass, 0 fail**, 500 expect() calls |
| `tsc --noEmit -p apps/client/tsconfig.app.json` | **clean**, exit 0 |
| `tsc --noEmit -p apps/mobile/tsconfig.json` | **clean**, exit 0 |
| `python -m unittest discover -s python -p 'test_discovery*.py'` | **10 tests, OK** |
| `python -m unittest python.test_export_session` | **20 tests, OK** |
| `git diff --check` | **clean** |

Per-group: opportunity 50 · attribution 8 · membership 10 · evaluation 10.
Build: `cargo build --workspace --all-targets` — 0 warnings.

**One note on the JS toolchain.** `node_modules` was completely empty in this
checkout, so the client tests and both typechecks initially could not run at
all (the first client run failed with `Cannot find module
'@stockspotter/shared-types'`). Resolved with **`bun install --frozen-lockfile`**,
chosen specifically because it cannot modify the tracked lockfile — `bun.lock`
and every `package.json` are verified unchanged in §15. `node_modules` is
gitignored. `tsc` is not a declared dependency of this project, so it ran via
`bunx` at version 7.0.2; both projects typecheck clean under it, but that is not
a version this repo pins.

## 13. Bugs found during this correction

**(a) `serde_json` f64 round-trip is not bit-exact — dependency-level, real,
not fixed.** Reproduced in an isolated crate: `10.909090909090903_f64`
serializes correctly to `10.909090909090903` and **parses back as
`10.909090909090905`** — one ULP off. The workspace declares plain
`serde_json = "1"`, so the `float_roundtrip` feature that guarantees exactness
is **off**.

Consequence: any exact-equality assertion on a float-bearing research record
passes or fails by luck, and a write/read cycle of the evaluation or shadow
artifact can shift a measured value by 1 ULP. Immaterial for statistics;
material for anyone doing exact comparison. It does **not** affect the group J
replay-parity claim, which compares serialization of in-memory values.

**Not fixed deliberately.** Enabling `float_roundtrip` is the correct remedy but
changes workspace-wide float *parsing*, including `auto-trader`'s `ScanEvent`
deserialization. However microscopic, that is a production behaviour change and
this batch is explicitly forbidden from making one. Flagged for your decision
(§18). Meanwhile `m83` and `q06` were written to assert the property that
actually holds — exact on the scoring half, tolerance on measured floats — with
the reason recorded in-test rather than as a silent loosening.

**(b) Opportunity records are persisted nowhere.** Found while building
`oi_evaluate`: the shadow log holds snapshots, the measurement capture holds
episodes, and the join needed windows neither provides. Resolved by deriving
them (§5) rather than adding a live write path. The derivation's cost —
unrecoverable close, under-assigned tail — is asserted by `q10`.

**(c) My own test fixture bug.** `EpisodeTracker::finish` returns only episodes
still *open*; those closed earlier by invalidation are returned by `observe` at
the moment they close. Collecting only `finish` made the chain fixture see **one**
episode where the tracker had produced **three** — which would have made `q01`
and `q02` vacuous. Fixed, with the reason recorded in the fixture.

**(d) My own acausal assertion.** `q02` first asserted
`records[0].invalidations_absorbed >= 2`. `records[0]` is the *earliest* ranking
window, taken before the invalidations happened, so the correct value there is
0 — the test was wrong about causality, not the engine. The corrected version
asserts absorption on the **last** window, asserts 0 on the first, and adds a
strictly stronger property: the counts never decrease across windows.

**(e) Continuation was rankable on 0.20 coverage** — the Correction 3 defect
itself, quantified in §7 Finding 2 and now explicitly unranked.

## 14. Files changed

**New (5):**

| File | Lines |
|---|---|
| `crates/backtest-metrics/src/membership.rs` | 342 |
| `crates/backtest-metrics/src/membership_tests.rs` | 399 |
| `crates/backtest-metrics/src/evaluation.rs` | 552 |
| `crates/backtest-metrics/src/evaluation_tests.rs` | 545 |
| `crates/backtest-metrics/src/bin/oi_evaluate.rs` | 135 |

**Modified (3):**

`crates/backtest-metrics/src/opportunity.rs` — Correction 3 only. Added
`SCORE_POLICY_VERSION`, `UnrankableReason`, `MIN_COMPARABLE_COVERAGE`,
`EARLY_QUALITY_CORE`, `CONTINUATION_CORE`, `finalize_score`; extended
`ShadowScore` with `policyVersion`, `rawWeighted`, `presentWeight`,
`totalWeight`, `coverage`, `coreMissing`, `unrankableReason`; `push` now
accumulates present/total weight; `OiVersions` carries `scorePolicy`. **No
feature, weight or transform was altered.**

`crates/backtest-metrics/src/opportunity_tests.rs` — group M appended only.

`crates/backtest-metrics/src/lib.rs` — two `pub mod` lines and two `pub use`
blocks. Additive.

Also present from the prior batch and unchanged by this one: `opportunity.rs`
(aside from the above), `attribution.rs`, `attribution_tests.rs`,
`bin/oi_replay.rs`, `bin/oi_attribute.rs`, `ws-server/src/opportunity_shadow.rs`,
`ws-server/src/opportunity_shadow_tests.rs`, `python/discovery_evidence.py`,
`ws-server/src/main.rs`.

All files normalized to CRLF to match the checkout.

## 15. Production-isolation re-verification

```
fast-funnel              modified=0 untracked=0
ignition-detector        modified=0 untracked=0
momentum-scorer          modified=0 untracked=0
consolidation-breakout   modified=0 untracked=0
halt-detector            modified=0 untracked=0
market-data              modified=0 untracked=0
auto-trader              modified=0 untracked=0
replay-engine            modified=0 untracked=0
python/app               modified=0
ops                      modified=0
docker-compose.yml       modified=0
deploy.sh                modified=0
bun.lock                 modified=0
package.json             modified=0
```

No detector, strategy, Auto-Trader, market-data, Python service, ops or
configuration file was modified or added. No production threshold changed. No
shadow or live session was started — `OPPORTUNITY_INTELLIGENCE_SHADOW` remains
default-off and was not set.

`i46` (Auto-Trader decisions byte-identical with and without the shadow layer,
driven through the real `Engine`) and `i45` (client event stream byte-identical)
both still pass, as part of ws-server's 92.

The one dependency-level temptation was declined: `float_roundtrip` would have
altered production float parsing (§13a).

## 16. `git diff --stat`

```
 crates/backtest-metrics/src/lib.rs | 27 +++++++++++++++++++
 crates/ws-server/src/main.rs       | 54 ++++++++++++++++++++++++++++++++++++++
 2 files changed, 81 insertions(+)
```

Untracked files do not appear here by design; they are listed in §17 and sized
in §14. `git rev-parse HEAD` → `c8f78dc2677591c6b9de37d2dfc034b506810ade`,
unchanged. `git diff --cached --name-only` → empty.

## 17. `git status --short`

```
 M crates/backtest-metrics/src/lib.rs
 M crates/ws-server/src/main.rs
?? crates/backtest-metrics/src/attribution.rs
?? crates/backtest-metrics/src/attribution_tests.rs
?? crates/backtest-metrics/src/bin/oi_attribute.rs
?? crates/backtest-metrics/src/bin/oi_evaluate.rs
?? crates/backtest-metrics/src/bin/oi_replay.rs
?? crates/backtest-metrics/src/evaluation.rs
?? crates/backtest-metrics/src/evaluation_tests.rs
?? crates/backtest-metrics/src/membership.rs
?? crates/backtest-metrics/src/membership_tests.rs
?? crates/backtest-metrics/src/opportunity.rs
?? crates/backtest-metrics/src/opportunity_tests.rs
?? crates/ws-server/src/opportunity_shadow.rs
?? crates/ws-server/src/opportunity_shadow_tests.rs
?? python/discovery_evidence.py
?? research-reports/ALPHA-OPPORTUNITY-INTELLIGENCE-V1-2026-09-15.md
```

Plus this report. Nothing staged, nothing committed, nothing pushed, nothing
deployed.

## 18. Final recommendation

**READY FOR CHECKPOINT.**

All three corrections are closed, the audit item resolved without a model
change, the evaluation artifact exists and has been exercised on real files, and
the full research chain is proved end to end from the real components. 532 tests
pass with zero failures and zero build warnings. Production isolation
re-verified mechanically.

The blocker the previous report identified — "no opportunity↔outcome join is
implemented" — is closed, and closed without the unsafe identity assumption
that would have made it wrong for exactly the fragmented moves it matters most
for.

**One item for your decision, not a blocker:**

`serde_json`'s `float_roundtrip` feature (§13a). Leaving it off means a
write/read cycle of the evaluation artifact can move a measured float by 1 ULP.
That is irrelevant to any statistic GPT will compute and I would not hold the
checkpoint for it. Enabling it is a one-line `Cargo.toml` change that makes
every round-trip exact — but it also changes `auto-trader`'s float parsing,
which is production behaviour this batch may not touch. If you want it, it
should be its own authorized change with the Auto-Trader test suite re-run, not
something folded in here.

Two known limitations carry forward unchanged and are stated again so they are
not rediscovered as surprises: there is still no production raw-`ScanEvent`
capture, so session-level replay parity remains unclaimable; and the anchor
offset means a joined outcome measures forward from the *episode's* signal
instant, not the score instant — which is why `anchorOffsetSecs` is on every
record and why forward/backward anchors are counted separately.

---

ALPHA OPPORTUNITY INTELLIGENCE V1 — PRE-CHECKPOINT CORRECTION COMPLETE
