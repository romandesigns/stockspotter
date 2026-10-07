# ALPHA OPPORTUNITY INTELLIGENCE V1 — Complete Implementation Report

**Date:** 2026-09-15, 9:19 AM EDT
**Project:** Stockspotter
**Workstream:** Alpha Opportunity Intelligence V1
**Branch:** `research/alpha-reports`
**HEAD at start and end:** `c8f78dc2677591c6b9de37d2dfc034b506810ade` (unchanged)
**State:** implemented locally, validated, **uncommitted and unstaged**
**Deployed production commit (untouched):** `df95d63` on `release/operating-run-20260907`

---

## 1. Objective and scope

Implemented an independent, causal research layer that:

1. collapses repeated detector events into evolving *opportunities*;
2. distinguishes early opportunity quality from established continuation;
3. preserves continuous information the current architecture reduces to booleans;
4. ranks contemporaneous opportunities;
5. classifies opportunity regimes and price regimes;
6. provides a prioritization-layer reduction that does not destroy raw recall;
7. makes earliness, ranking, precision and recall measurable against the frozen baseline.

It is a shadow layer. It adds no gate, changes no threshold, and is invisible to
production. Its only output is research NDJSON.

Explicitly not attempted: threshold tuning, detector replacement, Auto-Trader
modification.

## 2. Starting state

Working tree clean at `c8f78dc`; `df95d63` (the deployed release commit)
confirmed an ancestor; diff versus the release lineage reports-only. Phases A–D
were implemented and green (35 tests) from the prior session. This report covers
the whole milestone, with Phases E, F and G completed in this session.

## 3. Actual architecture implemented

```
market_data live scan
        │
        ▼
   ScanEvent  ──►  tokio::sync::broadcast (BROADCAST_CAPACITY = 16,384)
                        │
     ┌──────────────────┼──────────────────┬────────────────────┐
     ▼                  ▼                  ▼                    ▼
server::run        LiveSignalTracker   measurement::        opportunity_shadow::
(client sockets)   (efficiency)        MeasurementCollector ShadowDriver   ◄── NEW
     │                                     │                    │
     ▼                                     ▼                    ▼
  clients /                          episode artifacts    OpportunityIntelligence
  auto-trader                        data/research/       (backtest_metrics)
                                                               │
                                                               ▼
                                                     bounded sync_channel(64)
                                                               │
                                                               ▼
                                                     writer thread → NDJSON
                                                data/research/opportunity-
                                                intelligence-<UTC date>.ndjson
```

Key architectural decisions:

- **Fourth independent subscriber, not an extension of `MeasurementCollector`.**
  The two answer different questions: measurement records what happened to an
  episode; this records how an evolving opportunity was *ranked at the time*.
  Merging them would have required editing a subsystem whose output is already a
  frozen analysis baseline.
- **One engine, two drivers.** `OpportunityIntelligence::observe` is the single
  entry point for both the live subscriber and offline replay
  (`replay_events`). Replay parity is therefore a property of there being one
  implementation, not of two being kept in step.
- **Read-only consumer.** It observes events already broadcast. It cannot
  reorder, suppress, delay or mutate anything a client or the trader sees.
- **Off by default.** Gated on `OPPORTUNITY_INTELLIGENCE_SHADOW=1`.

## 4. Every file changed, and why

### New files (9)

| File | Lines | Why |
|---|---|---|
| `crates/backtest-metrics/src/opportunity.rs` | 1,491 | The engine: identity, lifecycle, regimes, transforms, both scores, ranking, persistence record, health, offline driver. Placed in `backtest-metrics` because that is where the episode/context/horizon research types already live, so the opportunity and episode units stay comparable. |
| `crates/backtest-metrics/src/opportunity_tests.rs` | 914 | Groups A–H, K, P. Separate file via `#[path]` to keep the engine readable. |
| `crates/backtest-metrics/src/attribution.rs` | 323 | Discovery attribution (§16). Separate module because the rules are independent of the engine and must be unit-testable without a 500 MB fixture. |
| `crates/backtest-metrics/src/attribution_tests.rs` | 217 | Group L. |
| `crates/backtest-metrics/src/bin/oi_replay.rs` | 88 | Offline replay CLI. Placed beside the existing research bins (`backtest`, `tune`, `live_efficiency`). |
| `crates/backtest-metrics/src/bin/oi_attribute.rs` | 165 | Offline attribution join CLI. |
| `crates/ws-server/src/opportunity_shadow.rs` | 240 | Live wiring: bounded recorder, driver, health. In `ws-server` because that is where the broadcast lives; sibling to `measurement.rs`. |
| `crates/ws-server/src/opportunity_shadow_tests.rs` | 501 | Groups I, J, K. |
| `python/discovery_evidence.py` | 208 | Extracts stage evidence from a reduced discovery archive. In `python/` with the other research scripts; owns the change-based decoding because the reducer that produced that encoding is also Python. |

### Modified files (2), both purely additive

**`crates/backtest-metrics/src/lib.rs`** (+15, −0) — two `pub mod` declarations
(`attribution`, `opportunity`) and two `pub use` re-export blocks. No existing
line altered. Re-export list kept alphabetical to match the file's convention.

**`crates/ws-server/src/main.rs`** (+54, −0) — one `mod` declaration, the
default-off subscriber block, and one `handle.abort()` in the existing shutdown
sequence. No existing line altered.

### Not changed

No detector, strategy, Auto-Trader, market-data, replay-engine, Python service,
ops, compose or configuration file appears anywhere in the diff. Verified
mechanically in §25.

## 5. Opportunity identity and lifecycle rules

### Identity

`OpportunityId { symbol, session_date, sequence }`, key format
`"{symbol}:{session_date}:{sequence}"`. Mirrors `EpisodeId` deliberately, so the
two units can be joined without a translation table. Sequence is a per
`(symbol, session_date)` counter.

### Opening rule

An opportunity opens when no opportunity is open for that symbol **and** the
event is a qualifying edge-trigger. `qualifying_strategy` — identical semantics
to `episode::qualifying_strategy`:

| Event | Strategy |
|---|---|
| `FunnelSignal { passed: true }` | `FastFunnel` |
| `MomentumUpdate { qualifies: true }` | `MomentumScorer` |
| `IgnitionEvent { kind: FollowThroughConfirmed }` | `IgnitionDetector` |
| `ConsolidationEvent { kind: EntryTriggered, strategy: ConsolidationBreakout }` | `ConsolidationBreakout` |
| `ConsolidationEvent { kind: EntryTriggered, strategy: Micropullback }` | `Micropullback` |

If the opening event carries no price, the last known price from `FeatureCache`
is used; if neither exists, the opportunity does **not** open (no fabricated
price).

### The single deliberate divergence from `EpisodeTracker`

> An `Opportunity` differs from an `OpportunityEpisode` in exactly one rule:
> **invalidation does not end an opportunity.**

`EpisodeTracker` Rule 4 closes an episode on `FollowThroughRejected`. That
fragments one continuing move into several episodes, and is the mechanical
explanation for the 129,655 → 67,032 collapse observed independently in the
baseline analysis. This layer **absorbs** invalidation and counts it:

- `invalidations_absorbed` — rejections folded in without closing
- `episode_fragments` — incremented on each absorbed invalidation, so the
  episode-to-opportunity ratio stays measurable instead of assumed

`EpisodeTracker` itself is untouched.

### Closing rules

| Reason | Condition |
|---|---|
| `Inactivity` | `(now − last_seen_at) >= inactivity_secs` (300s), **same calendar date** |
| `SessionBoundary` | `opened_at.date() != now.date()` |
| `CaptureEnded` | `finish(at)` at shutdown |
| `CapacityReached` | open set at `max_open_opportunities()`; least-recently-active evicted |

The date check is required and was a test failure until added: an overnight gap
exceeds any inactivity timeout, so without it `expire_inactive` fires first and
labels the closure `Inactivity`. The two mean different things for analysis.
This mirrors a comment `EpisodeTracker::expire_inactive` already carries.

Capacity eviction selects the victim by `min_by_key((last_seen_at, symbol))` —
LRU, with the symbol tiebreak making it deterministic.

`close()` clamps `closed_at = at.max(opened_at)`. An opportunity can never close
before it opened — the same clamp the episode tracker needed after Session 002's
inverted records.

### Causal timestamp correction (R2)

`event_symbol_time_price` stamps a **finalised** bar at
`timestamp + interval_secs`, because a 60s bar's close is only knowable one
interval after the bar's opening timestamp. Non-final bars and trade-stamped
events keep their own timestamp. Identical to the correction the measurement
collector applies.

## 6. Detector-confluence representation

Recorded as **information, never a gate** (§9).

```rust
detectors_seen: BTreeMap<String, DetectorArrival>

struct DetectorArrival {
    first_seen_at: DateTime<Utc>,
    first_confirmed_at: Option<DateTime<Utc>>,  // None until a confirming event
    arrival_index: u32,                          // 1-based, order of first appearance
    events: u32,
}
```

- Keyed by `String` (via `strategy_name(strategy) = format!("{strategy:?}")`)
  rather than by `Strategy`, because `Strategy` is not `Ord` and this module
  will **not** add trait derives to a type shared with `strategy_config`. Keeps
  the module self-contained; a `BTreeMap` keeps serialization deterministic.
- `confluence_count()` = `detectors_seen.len()` — distinct detector families.
- `confirmation_span_secs()` = seconds between earliest and latest
  `first_confirmed_at`, `None` when fewer than two detectors have confirmed.
- `detector_transitions` counts how many times a *new* detector family arrived.
- A confirming event is `IgnitionEvent/FollowThroughConfirmed`,
  `ConsolidationEvent/EntryTriggered`, `FunnelSignal{passed:true}` or
  `MomentumUpdate{qualifies:true}`.

Confluence enters `ContinuationConfidence` as one weighted feature (0.10) and
gates nothing — pinned by `h44_confluence_alone_does_not_gate_anything`.

## 7. Continuous features retained

The feature surface is the existing `SignalContext`, carried **whole** and
refreshed on every observation. Nothing is reduced to a boolean.

Retained sub-structs: `MarketFeatures`, `FunnelFeatures`, `IgnitionFeatures`,
`MomentumFeatures`, `ConsolidationFeatures`, `HaltFeatures`,
`CatalystFeatures`, `PreDetectionContext`.

Momentum specifically keeps `overall`, `volume_confirmation`, `structure`,
`ma_slope`, `wick_rejection` as continuous `f64` — `qualifies` is **not** used
as a substitute. Pinned by
`d18_momentum_components_survive_without_boolean_reduction`.

Two contexts are held per opportunity:

| Field | Semantics |
|---|---|
| `latest_context` | Refreshed on every observation. Causal: `FeatureCache::snapshot` filters every sub-struct to values observed at or before the instant asked for, so a refresh at `t` can only contain information available at `t`. |
| `detection_context` | Frozen at open. Earliness is a question about what was knowable *at detection*; refreshing a single field would have destroyed the only record of it. |

The refresh is attributed to `first_detector`, not to whichever detector
produced the current event — the context belongs to the *opportunity*, whose
identity is the detector that opened it. Using the current event's strategy
would make the snapshot's `strategy` field oscillate between windows and turn a
stable key into noise.

Continuous scalars maintained directly on the opportunity:
`move_before_detection_pct`, `move_from_start_pct`, `max_move_pct`,
`min_move_pct`, `observed_high`, `observed_low`, `opening_price`,
`latest_price`.

## 8. Missingness semantics

`Option` throughout; **unknown is never zero**.

1. **`ScoreComponent`** — a missing input records `raw: None`,
   `transformed: None`, its weight, and `contribution: 0.0`. The zero
   contribution is visible *as* a missing input, not disguised as a neutral
   value. Pinned by `f32_missing_inputs_do_not_become_fabricated_neutral_values`.
2. **`ShadowScore.missing: Vec<String>`** — the explicit missingness mask, by
   feature name.
3. **`present_inputs` / `required_inputs`** — how many of the model's inputs
   actually existed.
4. **`MIN_PRESENT_INPUTS = 2`** — below this, `value` is `None`. A missing score
   is **not** a low score.
5. **`Ranking.unranked: Vec<String>`** — unscorable opportunities are listed
   explicitly, never ranked worst. Pinned by
   `g37_unscorable_candidates_are_unranked_not_ranked_worst`.
6. **`Regime::Unclassified`** — returned whenever prior move is unknown, or when
   the move is deeply negative without recovery. The evidence does not
   distinguish the cause, so no class is claimed.
7. **`classify_price_regime` returns `None`** for non-finite or non-positive
   price.
8. **Attribution is three-valued** — observed present / observed absent /
   unknown (§17).
9. **`detection_features` is carried once per opportunity**, on its first
   ranking row. Absence on a later row means *carried forward*, not *unobserved*
   — the same change-based convention the discovery reduction uses, documented
   on the field. Absence on every row means the first window was lost to a
   counted writer drop, or the opportunity closed before ever being ranked.

Pinned by `d19_unknown_is_absent_and_never_serialized_as_zero`,
`c16_missing_evidence_produces_unclassified`,
`e26_missing_features_follow_an_explicit_tested_policy`.

## 9. Exact regime classifier — `regime-v1`

```rust
pub fn classify_regime(
    prior_move_pct: Option<f64>,      // move_before_detection_pct
    move_from_start_pct: Option<f64>,
    config: &OiConfig,
) -> Regime
```

Evaluated strictly in this order:

| # | Condition | Result |
|---|---|---|
| 1 | `prior_move_pct` is `None` | `Unclassified` |
| 2 | `prior <= reversal_max_prior_move_pct` (**−5.0**) and `move_from_start_pct > 0.0` | `ReversalRecovery` |
| 3 | `prior <= −5.0` and `move_from_start_pct <= 0.0` or `None` | `Unclassified` |
| 4 | `prior >= continuation_min_prior_move_pct` (**+2.0**) | `ContinuationAcceleration` |
| 5 | `prior <= early_max_prior_move_pct` (**+2.0**) | `EarlyEmerging` |
| 6 | otherwise | `Unclassified` |

Consequences worth noting for an auditor:

- `−5.0 < prior < +2.0` → `EarlyEmerging`.
- `prior == +2.0` → `ContinuationAcceleration` (rule 4 is checked first).
- Only a *recovering* opportunity is a reversal; still-falling is `Unclassified`
  rather than forced into a class. Pinned by
  `c15_reversal_and_continuation_do_not_collapse_together`.
- Inputs are contemporaneous state only — no future observation participates.
  Pinned by `c14_identical_causal_state_always_produces_identical_regime`.

## 10. Exact price regimes — `price-regime-v1`

`config.price_regime_bounds = [0.50, 1.00, 5.00, 20.00]` (ascending, dollars).

```rust
pub struct PriceRegime { band: usize, lower: f64, upper: Option<f64> }
```

| Band | Range (lower inclusive, upper exclusive) |
|---|---|
| 0 | `[0.00, 0.50)` |
| 1 | `[0.50, 1.00)` |
| 2 | `[1.00, 5.00)` |
| 3 | `[5.00, 20.00)` |
| 4 | `[20.00, ∞)` — `upper: None` |

`None` is returned for non-finite or `<= 0.0` price. Classified against
`latest_price` at ranking time. **A band never suppresses detection** — it
exists because fixed-% outcome distributions are not comparable across price
bands. Pinned by
`c17_price_regime_bands_are_deterministic_and_reject_unknown`.

## 11. Exact `EarlyQualityScore` V1 — `early-quality-v1-transparent`

Question answered: *does this deserve priority before acceleration is obvious?*

**Deliberately excludes move magnitude.** Mixing magnitude in is what makes the
existing research rank a confirmation signal rather than an early one.

```
score = Σ  transform_i(raw_i) × weight_i     over present inputs
value = Some(score)  if present_inputs >= 2
        None         otherwise
required_inputs = 5
```

| # | Feature | Transform | Weight |
|---|---|---|---|
| 1 | `momentum.maSlope.presence` | `Presence { threshold: 0.0 }` | **0.30** |
| 2 | `momentum.structure` | `Raw` | **0.20** |
| 3 | `momentum.volumeConfirmation` | `Bins { bounds: [0.40, 0.60, 0.80, 1.00], weights: [0.0, 0.35, 0.80, 1.00, 0.70] }` | **0.20** |
| 4 | `momentum.wickRejection` | `Bins { bounds: [0.50, 0.75, 1.00], weights: [0.0, 0.50, 1.00, 0.75] }` | **0.15** |
| 5 | `earliness.priorMovePct` | `Piecewise { knots: [(−5.0, 0.2), (0.0, 1.0), (2.0, 0.8), (5.0, 0.4), (10.0, 0.1), (20.0, 0.0)] }` | **0.15** |

Weights sum to 1.00, so a fully-present score lies in `[0, 1]`.

### Transform semantics (exact)

**`Presence { threshold }`** → `1.0` if `value > threshold`, else `0.0`.
Encodes "positive MA slope is materially more informative than zero slope, while
increasing it further is not cleanly monotonic" — presence, not level.

**`Bins { bounds, weights }`** → `index` = the first `i` with
`value < bounds[i]`, else `bounds.len()`; result is `weights[index]`.
Requires `weights.len() == bounds.len() + 1`.

Expanded for feature 3 (`volumeConfirmation`):

| Value | Weight |
|---|---|
| `< 0.40` | 0.0 |
| `[0.40, 0.60)` | 0.35 |
| `[0.60, 0.80)` | 0.80 |
| `[0.80, 1.00)` | **1.00** |
| `>= 1.00` | **0.70** |

Expanded for feature 4 (`wickRejection`):

| Value | Weight |
|---|---|
| `< 0.50` | 0.0 |
| `[0.50, 0.75)` | 0.50 |
| `[0.75, 1.00)` | **1.00** |
| `>= 1.00` | **0.75** |

Both peak strictly *below* the extreme and fall at exactly `1.00`. This is the
structural encoding of the observed non-monotonicity: `volumeConfirmation == 1.0`
and `wickRejection == 1.0` are **not** treated as automatically superior,
because the descriptive evidence says they are not. A linear term cannot
represent that, which is why `Transform` is a first-class swappable object.

**`Piecewise { knots }`** → linear interpolation between `(x, y)` knots, clamped
at both ends (`value <= knots[0].0` → `knots[0].1`; beyond the last knot →
last `y`). Feature 5 peaks at `0.0%` prior move and decays to `0.0` by `+20%`:
less prior movement is better here, flattening once the move is already obvious.

Pinned by `e25`, `e27_transform_layer_represents_nonlinear_relationships`,
`b10_future_prices_cannot_alter_an_earlier_early_quality_score`.

## 12. Exact `ContinuationConfidence` V1 — `continuation-v1-transparent`

Question answered: *a move is already underway — how likely is further
continuation?* It **may** use move magnitude, which `EarlyQualityScore` may not.

Kept a separate quantity rather than folded into one global score, because the
continuation and reversal populations behave differently and a single global
score is not justified by the evidence.

```
required_inputs = 6
value = Some(score) if present_inputs >= 2, else None
```

| # | Feature | Transform | Weight |
|---|---|---|---|
| 1 | `continuation.priorMovePct` | `Piecewise { knots: [(0.0, 0.10), (2.0, 0.25), (5.0, 0.50), (7.5, 0.65), (10.0, 0.80), (15.0, 1.00), (20.0, 1.00)] }` | **0.35** |
| 2 | `continuation.moveFromStartPct` | `Piecewise { knots: [(−5.0, 0.0), (0.0, 0.2), (2.0, 0.6), (5.0, 0.9), (10.0, 1.0)] }` | **0.20** |
| 3 | `momentum.overall` | `Raw` | **0.15** |
| 4 | `momentum.maSlope.presence` | `Presence { threshold: 0.0 }` | **0.10** |
| 5 | `confluence.detectorCount` | `Bins { bounds: [2.0, 3.0], weights: [0.0, 0.5, 1.0] }` | **0.10** |
| 6 | `halt.proximityRatio` | `Piecewise { knots: [(0.0, 1.0), (0.5, 0.6), (0.8, 0.2), (1.0, 0.0)] }` | **0.10** |

Weights sum to 1.00.

Notes for an auditor:

- Feature 1 is **rise-then-flatten**, matching the observed earliness/quality
  frontier shape, and is explicitly **not extrapolated past +20%** — the last
  two knots are both `1.00`, so the curve is flat rather than continuing to
  climb outside the observed range.
- Feature 5 expanded: `< 2` detectors → 0.0; exactly 2 → 0.5; `>= 3` → 1.0.
  Confluence is a *feature*, never a gate.
- Feature 6 is **inverse**: halt proximity is a risk signal, not an opportunity
  signal, so closeness to a band *reduces* continuation confidence.
- Feature 4 is shared with `EarlyQualityScore` but at a different weight; the
  two scores are otherwise independent quantities. Pinned by
  `f29_continuation_is_independent_from_early_quality` and
  `g40_early_and_continuation_ranks_are_distinct_quantities`.
- Pinned by `b11_continuation_may_use_move_magnitude_without_rewriting_history`.

## 13. Cross-sectional ranking implementation — `opportunity-rank-v1`

`rank(now)` produces **two independent rankings** per window. Gating:

```rust
let due = last_ranked.is_none_or(|last| (now − last).num_seconds() >= ranking_cadence_secs);
if !due || open.is_empty() { return None; }
```

`ranking_cadence_secs = 30`. `window_id = format!("oiw-{}", ranking_windows)`,
a monotonically increasing per-engine counter.

### Determinism

1. Cohort construction sorts the open-set symbol keys before iterating.
   Iterating the `HashMap` directly produced deterministic *ranks* but
   non-deterministic *output order*, which would have broken replay parity —
   this was a real test failure (§24, defect c).
2. `rank_cohort` sorts by `(score DESC, symbol ASC, sequence ASC)`. Ties are
   therefore fully determined. Pinned by `g36_ties_are_deterministic`.
3. Output snapshots are sorted by `opportunity_id` before return.

### Cohort rules

- One opportunity occupies **at most one slot in each** ranking. Pinned by
  `g35_g39_one_opportunity_occupies_at_most_one_slot`.
- Ranks are 1-based.
- Unscorable opportunities go to `Ranking.unranked` — never ranked worst.
- `max_rank_cohort = 4,096`. Truncation sets `cohort_truncated: true` and
  increments `OiHealth.cohort_truncations`. Explicit, never silent.
- Ranking operates on **opportunities, not raw detector events**. Pinned by
  `g34_ranking_operates_on_opportunities_not_raw_events`.

Every snapshot carries `early_quality_rank`, `continuation_rank`,
`early_cohort_size` and `continuation_cohort_size`. A rank without its cohort
size is not reproducible information, which is why both are persisted and both
are compared under replay.

### Research alert semantics — `ShadowState`

Top-decile within the contemporaneous cohort:
`decile(rank, cohort) = rank <= max(cohort × 0.10, 1.0)`.

| Regime | Condition | `ShadowState` |
|---|---|---|
| `EarlyEmerging` | early top-decile | `EarlyWatch` |
| `ContinuationAcceleration` | continuation **and** early top-decile | `HighConfidenceContinuation` |
| `ContinuationAcceleration` | continuation top-decile | `Accelerating` |
| `ReversalRecovery` | continuation top-decile | `Accelerating` |
| any | otherwise | `None` |

Research output only. Never surfaced to production users; no existing push-alert
policy is touched.

## 14. Shadow persistence schema

**Path:** `data/research/opportunity-intelligence-<UTC date>.ndjson`, where the
date comes from `snapshot.timestamp.date_naive()`. Same `data/` mount every
other durable capture uses, so it survives a redeploy for the same reason those
do. Append-only, one JSON object per line, LF-terminated.

`OpportunityScoreSnapshot`, camelCase on the wire:

| Field | Type | Notes |
|---|---|---|
| `schemaVersion` | `u32` | 1 |
| `versions` | `OiVersions` | full version block, every row |
| `timestamp` | RFC3339 | ranking-window instant |
| `windowId` | `String` | `oiw-N` |
| `opportunityId` | `String` | `symbol:sessionDate:sequence` |
| `symbol`, `sessionDate` | `String` | |
| `regime` | `Regime` | snake_case |
| `priceRegime` | `PriceRegime?` | omitted when price unknown |
| `opportunityAgeSecs` | `i64` | |
| `currentPrice` | `f64` | |
| `moveFromStartPct` | `f64?` | |
| `moveBeforeDetectionPct` | `f64?` | |
| `rawEventCount` | `u32` | redundancy measure |
| `episodeFragments` | `u32` | `> 1` exactly when invalidation fragmented the move |
| `invalidationsAbsorbed` | `u32` | |
| `detectorsSeen` | `Vec<String>` | |
| `confluenceCount` | `usize` | |
| `confirmationSpanSecs` | `i64?` | |
| `features` | `SignalContext?` | as of `timestamp`, refreshed (feature schema 2) |
| `detectionFeatures` | `SignalContext?` | **first row per opportunity only**; forward-fill by `opportunityId` |
| `earlyQuality` | `ShadowScore` | value + full component decomposition + missingness |
| `earlyQualityRank` | `usize?` | |
| `continuation` | `ShadowScore` | |
| `continuationRank` | `usize?` | |
| `earlyCohortSize`, `continuationCohortSize` | `usize` | |
| `shadowState` | `ShadowState` | |

`OiVersions` on every row: `opportunitySchema: 1`, `featureSchema: 2`,
`regimeClassifier: "regime-v1"`, `priceRegime: "price-regime-v1"`,
`earlyQualityModel: "early-quality-v1-transparent"`,
`continuationModel: "continuation-v1-transparent"`,
`ranking: "opportunity-rank-v1"`,
`configFingerprint: "oi-cfg-a7b2bf07d227c55e"` (defaults).

The fingerprint is FNV-1a over the canonical JSON of `OiConfig` —
dependency-free and stable across runs and platforms, unlike `DefaultHasher`.

**Every score is re-derivable from its own record**: `ShadowScore.components`
carries `feature`, `raw`, `transformed`, `weight`, `contribution` per term.

The existing `researchRank` field was **not** touched or reinterpreted.

### Writer

Bounded `sync_channel(64)` to a dedicated thread. The market-facing side only
ever `try_send`s — a full queue **drops and counts**, never waits, so disk
latency cannot reach dispatch. Every disk error is logged (at powers of two, to
avoid log flooding) and counted; nothing propagates. An unusable directory
returns `None` from `start` and capture is simply off — research capture must
degrade to off, never take the service down.

`ShadowHealth { dropped, write_errors, snapshots_written }` plus
`is_degraded()`.

## 15. Replay architecture and parity results

### Architecture

```rust
pub struct ReplayObservation { received_at: DateTime<Utc>, event: ScanEvent }
pub fn replay_events(config: OiConfig, events: &[ReplayObservation])
    -> Vec<OpportunityScoreSnapshot>
```

`received_at` is carried **separately** from the event's own timestamp and is
not derivable from it. That separation is what makes the R2 bar correction
meaningful; collapsing the two would quietly make replay acausal.

`replay_events` drives the same `OpportunityIntelligence::observe` /
`rank` the live subscriber drives. There is no second implementation of the
research logic.

CLI: `oi_replay <events.ndjson>` reads
`{"receivedAt":"…","event":{…}}` per line, writes snapshots as NDJSON to
stdout. **Input order is authoritative and is not re-sorted by timestamp** — the
engine's whole subject is the order in which information actually arrived, and
reordering would manufacture a sequence that never occurred. Unparseable lines
are counted and the run **refuses** rather than replaying a partial sequence.

### Parity results — all pass

| Test | Claim |
|---|---|
| `j51_replay_reproduces_live_output_exactly` | snapshot count equal; **byte-identical** serialization, not merely equivalent |
| `j52_scores_are_identical_under_replay` | `early_quality.value`, `continuation.value`, **and** per-component `feature`/`raw`/`transformed`/`contribution` — so a compensating pair of errors cannot pass |
| `j53_regimes_are_identical_under_replay` | `regime`, `priceRegime`, `shadowState`; asserts at least one classified regime in the fixture |
| `j54_opportunity_ids_are_identical_under_replay` | `opportunityId` sequence **and** `windowId` alignment — the sequence suffix makes IDs order-dependent, so this catches a replay driver folding events in a different order |
| `j55_ranks_are_identical_under_replay` | both ranks **and** both cohort sizes; asserts at least one real rank exists |
| `j56_missingness_is_identical_under_replay` | `missing`, `present_inputs` for both scores, plus `moveBeforeDetectionPct`, `moveFromStartPct`, `confirmationSpanSecs`; asserts the fixture contains at least one genuinely missing input, so the test can distinguish absence from zero |

Replay in these tests runs through the **crate-level** offline driver, not a
second copy of the live loop, so the parity claim is not this test file testing
itself.

### End-to-end run

`oi_replay` on a 200-event synthetic file: **31 snapshots across 7 ranking
windows**, `invalidationsAbsorbed` reaching 9 and `episodeFragments` 10 on a
single opportunity — the divergence from `EpisodeTracker` working on
realistic-shaped input. `detectionFeatures` present on exactly 5 rows for 5
distinct opportunities. `featureSchema: 2` on every row. Config fingerprint
`oi-cfg-a7b2bf07d227c55e`.

## 16. Bounds and memory/resource limits

All bounds **derived**, none picked.

```rust
pub fn max_open_opportunities(&self) -> usize {
    ((supported_open_rate_centi × inactivity_secs × bound_safety_num)
      / (100 × bound_safety_den)) as usize
}
```

With defaults: `(1_000 × 300 × 5) / (100 × 4)` = **3,750** simultaneously-open
opportunities.

`supported_open_rate_centi = 1_000` (10.00/s) is the design envelope: 67,032
opportunities over the regular session is ~2.86/s, and the envelope is ~3.5×
that to absorb a busier session. The safety factor is 5/4.

Derived rather than picked for the same reason the measurement collector's
pending capacity had to be: a flat constant that happens to sit below the real
population silently truncates the thing being measured. The
`PendingCapacityReached` incident is the precedent.

| Bound | Value | Enforcement |
|---|---|---|
| Open opportunities | 3,750 (derived) | LRU eviction → `CapacityReached`, counted |
| Ranking cohort | 4,096 | truncate, `cohort_truncated: true`, counted |
| Per-opportunity price history | 512 | `max_history_per_opportunity` |
| Writer queue depth | 64 | `try_send`, drop + count |
| Broadcast subscriber | 16,384 (existing) | `Lagged(n)` logged with `skipped` |

Per-opportunity state that does **not** grow with event count: `detectors_seen`
is bounded by the number of strategy families (≤ 8); `raw_event_count`,
`episode_fragments`, `invalidations_absorbed`, `detector_transitions` are
counters, not collections; `max/min_move_pct`, `observed_high/low` are scalars.
Pinned by `p77_per_opportunity_history_is_capped` (50,000 events on one symbol:
exact count, bounded per-detector state, one open opportunity).

Open opportunities are keyed by symbol in a `HashMap`, so a price event for an
unrelated symbol is O(1) and never scans the open set. Pinned by
`k65_unrelated_symbol_events_do_not_disturb_the_open_set`.

### Saturation accounting

`OiHealth { open_opportunities, peak_open_opportunities, capacity_evictions,
history_truncations, cohort_truncations, opportunities_opened,
opportunities_closed, raw_events_observed, scores_emitted }`.

Reported at shutdown **whether or not it is zero**, because an operator must be
able to establish saturation without inferring it from the data afterwards —
the lesson of the pending-capacity incident. Pinned by
`k63_saturation_is_distinguishable_from_ordinary_absence`.

## 17. Discovery attribution implementation

### Stage ladder

```
Visible (0) → SelectionInput (1) → { Qualified | QuietSelected } (2)
            → DetectorProduced (3) → OpportunityRanked (4)
```

`Qualified` and `QuietSelected` sit at **equal depth** because they are two
parallel outcomes of selection (movers path, quiet-watch path), not a ranking
of each other. Ordering them would invent a hierarchy the platform does not
have.

### Three states, never two

Every stage is `Option<bool>`: `Some(true)` observed present, `Some(false)`
observed absent (the stage demonstrably ran and this symbol was not in it),
`None` no evidence either way.

Collapsing the last two is the single most tempting error here, and it
manufactures recall: "not in the qualified list" and "we never saw the qualified
list" would both read as "did not qualify". The type forbids it.

`observe(stage, present)` is **monotonic in the positive direction** — a later
`false` cannot retract an earlier `true`, because selection is re-evaluated
every scan and a symbol legitimately leaves a set it was previously in. Letting
a later absence overwrite would make attribution depend on which scan happened
to be last. Pinned by `l71`.

### Verdicts

| Verdict | Meaning |
|---|---|
| `Reached { stage }` | deepest stage positively observed |
| `NotVisible` | requested from the scan and returned **without** a snapshot (`nosnap_events > 0`). An observation about data availability, not an absence |
| `StoppedAtVisibility` | visible, and every deeper stage positively observed absent — the only one of the two visibility outcomes that supports "the funnel rejected it" |
| `Unknown { NoEvidence }` | no stage has any evidence at all |
| `Unknown { VisibilityUnknown }` | visibility itself unevidenced, so no depth statement is meaningful |
| `Unknown { Inconsistent { shallower, deeper } }` | a deeper stage is evidenced while **every** alternative at a shallower depth was observed absent |

`Inconsistent` is a **capture** contradiction, surfaced rather than resolved:
trusting the deeper stage would hide an incomplete discovery log, and trusting
the shallower one would erase real detector output. §16's instruction is to use
UNKNOWN when the evidence cannot distinguish causes, and this is that case.

The contradiction check runs **per depth**, not per stage: a depth contradicts
only when every parallel alternative at it was observed absent. Per-stage was
wrong and a real run caught it (§24, defect b).

### Tooling

`python/discovery_evidence.py <reduced.ndjson.gz> --session-date D --out evidence.ndjson`
decodes the change-based reduction (sym/dt/stage interning, `snap`/`nosnap`/
`sel`/`scanc`/`ign` rows) and emits `SymbolEvidence` per symbol. A stage is
written `false` **only** when the archive contains positive proof the stage
executed (`scanc_rows > 0` for qualification, `scanc_rows > 0 && sel_rows > 0`
for selection input); otherwise it is omitted. Unknown record kinds and
unresolved symbol ids are counted, never silently dropped, and a referenced id
with no interning row is reported rather than given a fabricated ticker.

`detectorProduced` is emitted `true` or omitted, **never `false`** — the
discovery capture records ignition events only, so the absence of an `ign` row
is not evidence that no detector fired.

`oi_attribute <evidence.ndjson> [snapshots.ndjson …]` merges evidence rows
(keyed by `(sessionDate, symbol)`, so the same symbol on two sessions is two
independent attributions), adds `OpportunityRanked` + `DetectorProduced` +
`rankingWindows` from shadow logs, classifies, and writes `AttributedSymbol`
NDJSON plus an `AttributionCoverage` summary on stderr. Snapshot absence
contributes **only** `true`, never `Some(false)`: a symbol missing from the
shadow log is not evidence it was never ranked.

### End-to-end run

9 symbols attributed: 5 `OpportunityRanked`, 2 `DetectorProduced`, 1 `Visible`,
1 `NotVisible`, 0 unknown, 0 inconsistent. The tool also correctly **refused** a
run where the snapshot file had been polluted with two stderr lines, rather than
attributing from a partial join.

### Scope discipline

`AttributionCoverage` is the only aggregate either tool emits, and it describes
coverage of the *evidence*, not behaviour of the platform. No hit rate,
precision, expectancy or effectiveness measure is computed anywhere.

## 18. Performance results — observed, 2×, overload, recovery

Measured, not asserted. Release build (`cargo test --release`), this development
workstation. Reference load: production was observed sustaining ~650
ScanEvents/second during market hours — the figure `BROADCAST_CAPACITY` is
sized against.

| Test | Load | Result |
|---|---|---|
| `p73` **observed** | 650 ev/s × 120s, 400 symbols (78,000 events) | **1,519 ns/event**; peak open **200**; evictions **0**; 601 snapshots |
| `p74` **2× observed** | 650 vs 1,300 ev/s × 60s, 400 symbols | **1,499 → 1,485 ns/event** — flat in rate; evictions 0 |
| `p75` **overload** | 650 ev/s × 60s, 20,000 symbols (39,000 events) | peak open **3,750 = the bound exactly**; **15,750 evictions counted**; 69.6 µs/event |
| `p76` **recovery** | overload → quiet (> 300s) → 600 events / 50 symbols | open set clears (`< 100`), scoring resumes, `capacity_evictions` unchanged from the overload total |
| `p77` adversarial | 50,000 events on one symbol | exact `raw_event_count`; per-detector state ≤ 8; one open opportunity |
| `p78` scaling | 100 vs 4,000 symbols × 30s | **655 → 12,104 ns/event** (18.5× cost for 40× the open set) |

Debug-build figures for comparison, since they are what a normal `cargo test`
run shows: `p73` 15,634 ns/event; `p75` ~323 µs/event.

### The honest characterisation

Per-event cost is **flat in event rate but linear in open-set size**, because
`expire_inactive` scans the open set on every event. At 650 events/second that
is **~0.1% of a core at normal load and ~4.5% saturated**. Bounded, because the
open set is bounded (`p75`).

`p74` structurally **cannot** see this — it holds symbol count fixed while
varying rate. `p78` therefore pins it explicitly, as a ratio rather than a
wall-clock number so it survives a different machine while still failing if the
scan becomes quadratic. This is a *stated known characteristic*, not an
unnoticed one. It was not optimised away: 4.5% of a core at full saturation for
a subscriber that is off the dispatch path did not justify introducing a
time-ordered index.

The layer also cannot block dispatch regardless of cost: it reads from a
broadcast subscriber and writes via `try_send`. Under sustained inability to
keep up it lags (`RecvError::Lagged(n)`, logged with `skipped`) and undercounts
an opportunity's raw events — it cannot corrupt one, because every field is
derived from events actually seen.

## 19. Every test added — 67 total, all passing

### Opportunity engine — 44 (`crates/backtest-metrics/src/opportunity_tests.rs`)

**A — identity and collapse (7)**
- `a1_repeated_detector_events_remain_one_opportunity`
- `a2_inactivity_boundary_closes_the_opportunity`
- `a3_a_subsequent_move_creates_sequence_plus_one`
- `a4_different_symbols_never_share_an_opportunity`
- `a5_session_boundary_closes_deterministically`
- `a6_out_of_order_timestamps_cannot_invert_the_lifecycle`
- `a7_repeated_ignition_cannot_inflate_the_candidate_count`

**B — causality (3)**
- `b10_future_prices_cannot_alter_an_earlier_early_quality_score`
- `b11_continuation_may_use_move_magnitude_without_rewriting_history`
- `b13_replaying_a_prefix_is_identical_regardless_of_later_events`

**C — regimes (4)**
- `c14_identical_causal_state_always_produces_identical_regime`
- `c15_reversal_and_continuation_do_not_collapse_together`
- `c16_missing_evidence_produces_unclassified`
- `c17_price_regime_bands_are_deterministic_and_reject_unknown`

**D — continuous features and missingness (6)**
- `d18_momentum_components_survive_without_boolean_reduction`
- `d19_unknown_is_absent_and_never_serialized_as_zero`
- `d20_features_are_refreshed_as_the_opportunity_evolves` *(new, defect a)*
- `d21_the_detection_time_surface_survives_a_refresh` *(new, defect a)*
- `d21b_the_detection_surface_is_persisted_once_not_every_window` *(new, defect a)*
- `d22_detector_first_seen_and_order_survive_serialization`

**E — determinism and transforms (3)**
- `e25_deterministic_config_produces_deterministic_score`
- `e26_missing_features_follow_an_explicit_tested_policy`
- `e27_transform_layer_represents_nonlinear_relationships`

**F — score independence (2)**
- `f29_continuation_is_independent_from_early_quality`
- `f32_missing_inputs_do_not_become_fabricated_neutral_values`

**G — ranking (6)**
- `g34_ranking_operates_on_opportunities_not_raw_events`
- `g35_g39_one_opportunity_occupies_at_most_one_slot`
- `g36_ties_are_deterministic`
- `g37_unscorable_candidates_are_unranked_not_ranked_worst`
- `g38_rank_metadata_is_stable_under_replay`
- `g40_early_and_continuation_ranks_are_distinct_quantities`

**H — confluence (2)**
- `h41_h43_confluence_is_recorded_with_order_and_timing`
- `h44_confluence_alone_does_not_gate_anything`

**K — bounds (4)**
- `k57_open_opportunities_stay_bounded_under_symbol_churn`
- `k63_saturation_is_distinguishable_from_ordinary_absence`
- `k64_overload_then_normal_load_recovers`
- `k65_unrelated_symbol_events_do_not_disturb_the_open_set`

**Config (1)**
- `config_bound_is_derived_and_fingerprint_is_stable`

**P — performance, §21 (6, new)**
- `p73_observed_production_load_stays_within_its_derived_bound`
- `p74_double_observed_load_degrades_proportionally_not_catastrophically`
- `p75_overload_is_bounded_and_counted_not_unbounded`
- `p76_the_engine_recovers_after_overload`
- `p77_per_opportunity_history_is_capped`
- `p78_cost_scales_with_the_open_set_and_that_ceiling_is_stated`

### Shadow layer — 15 (`crates/ws-server/src/opportunity_shadow_tests.rs`, all new)

**I — shadow isolation, BLOCKING (6)**
- `i45_production_events_are_byte_identical_with_and_without_shadow`
- `i46_auto_trader_decisions_are_byte_identical_with_and_without_shadow`
- `i47_shadow_output_is_research_records_only`
- `i48_unusable_capture_directory_disables_capture_silently`
- `i49_write_failures_are_counted_and_never_propagate`
- `i50_disabled_shadow_writes_nothing`

**J — replay parity (6)**
- `j51_replay_reproduces_live_output_exactly`
- `j52_scores_are_identical_under_replay`
- `j53_regimes_are_identical_under_replay`
- `j54_opportunity_ids_are_identical_under_replay`
- `j55_ranks_are_identical_under_replay`
- `j56_missingness_is_identical_under_replay`

**K — writer queue (3)**
- `k60_a_full_queue_drops_and_counts_without_blocking`
- `k61_saturation_is_reported_not_hidden`
- `k62_persisted_records_are_one_parseable_ndjson_line_each`

### Attribution — 8 (`crates/backtest-metrics/src/attribution_tests.rs`, all new)

**L — discovery attribution**
- `l66_attribution_is_the_deepest_stage_actually_evidenced`
- `l67_no_snapshot_is_distinct_from_no_evidence`
- `l68_unknown_never_degrades_to_observed_absent`
- `l69_a_contradictory_chain_is_reported_not_resolved`
- `l70_the_two_selection_paths_do_not_contradict_each_other`
- `l70b_one_absent_parallel_path_does_not_contradict_a_deeper_stage` *(new, defect b)*
- `l71_leaving_a_selection_set_does_not_retract_having_been_in_it`
- `l72_attribution_is_deterministic_and_serializes_stably`

## 20. The blocking test, in detail (§19 group I)

`i46_auto_trader_decisions_are_byte_identical_with_and_without_shadow` is the
test that decides whether this milestone is allowed to exist.

It drives the **real** `auto_trader::engine::Engine` — constructed with a real
`Config`, seeded from `backtest_metrics::default_enabled` exactly as production
does — over the identical 240-event stream twice: once with no shadow layer,
once with `ShadowDriver::observe` called on every event before
`engine.on_event`. Both runs' `Vec<JournalEntry>` are serialized and compared.

An import audit proves nothing about a value smuggled in through shared mutable
state; this drives the actual decision path. The fixture is asserted
**non-empty**, so the test cannot pass vacuously by producing no decisions in
either run.

`i45` uses the same construction for the client-facing event stream, with the
shadow layer observing *first*, so a mutation would appear in the later
serialization. This mirrors `measurement.rs`'s own
`measurement_does_not_alter_the_events_production_sees` deliberately: two
independent research subsystems now read the same broadcast, and each must be
independently provable innocent.

`i49` forces a write failure deterministically by occupying the exact filename
the writer will choose with a directory — which no `OpenOptions::append` can
open on any platform — then asserts through the driver that
`write_errors > 0`, `snapshots_written == 0`, and `is_degraded()`. The point is
the counter, not the absence of a panic: a swallowed error with no record of it
is the failure mode being guarded against.

`k60`/`k61` hold the writer thread at a `std::sync::Barrier` so the queue bound
is actually **reachable**. With a live writer a 64-deep channel never fills in a
test, and a drop counter no test can reach is indistinguishable from a drop
counter that does not work. That required a test-only `start_inner(dir, depth,
gate)`; `start` delegates to it with the production depth and no gate.

## 21. Complete validation results

```
cargo test --workspace
  TOTAL passed=506 failed=0

cargo build --workspace --all-targets
  0 warnings
```

Per-suite:

| Suite | Result |
|---|---|
| `backtest-metrics --lib opportunity::` | 44 passed, 0 failed |
| `backtest-metrics --lib attribution::` | 8 passed, 0 failed |
| `ws-server --bin ws-server opportunity_shadow` | 15 passed, 0 failed |
| `ws-server` (whole crate) | 92 passed, 0 failed |
| Workspace | **506 passed, 0 failed, 0 ignored** |

`cargo test --release -p backtest-metrics --lib opportunity::tests::p7` — 5
passed with the figures in §18; `p78` separately, passed.

`git diff --check` — clean.

No test was weakened, disabled, or `#[ignore]`d to make the implementation pass.
Two pre-existing tests required mechanical updates for a new struct field
(`Opportunity` gained `detection_context` / `detection_context_emitted`, so the
one hand-built fixture in `opportunity_tests.rs` gained two `None`/`false`
initializers). No assertion was changed.

## 22. Bugs discovered during implementation

Four real defects, **all found by running the code rather than reading it**.

### (a) Frozen feature surface — the significant one

`record()` never refreshed `latest_context`, so the feature surface was captured
once at open and never again, despite the field being named and documented as
the *latest* snapshot. The field's own doc comment contradicted the code.

**Impact, measured:** on a 200-event replay this left **25 of 31 ranking rows
with no momentum inputs at all**, so the early-quality score was unavailable for
80% of the cohort and only 6 of 31 rows carried a rank. Precisely the continuous
information this layer exists to preserve, reduced to absence by a wiring
defect. No test caught it: `d18` passes either way, because its fixture happens
to deliver momentum *before* the opening confirm.

**Fix:** refresh `latest_context` on every observation — causal, because
`FeatureCache::snapshot` filters every sub-struct to values observed at or
before the instant asked for — while preserving the detection-time surface
separately as `detection_context`, because earliness is a question about what
was knowable *at detection* and refreshing a single field would have destroyed
the only record of it.

**After:** early-quality availability **6/31 → 30/31** rows, ranked entries
**6 → 30** (full cohorts of 5 across 6 windows), with the one remaining gap
genuinely unobserved rather than zero-filled — missingness preserved, not
papered over.

`OI_FEATURE_SCHEMA_VERSION` bumped **1 → 2** rather than silently corrected,
because the two versions mean different things by the same field name (§24:
"Version/add rather than silently reinterpret"). The reason is recorded in the
constant's own doc comment.

Pinned by `d20`, `d21`, `d21b`.

### (b) Attribution false positive

The contradiction check ran per stage, so a symbol that qualified via the movers
path, was not quiet-selected, and then fired a detector — the **ordinary** path
through this platform — was reported as a capture contradiction. Found by
running `oi_attribute` on realistic data; the unit tests missed it because
`l70`'s fixture happens to have the deepest stage at the same depth as the
absent one.

**Fix:** check per *depth*; a depth contradicts only when every parallel
alternative at it was observed absent. Pinned by `l70b`, which also asserts the
converse still fires (both selection paths absent under a detector *is* a
contradiction).

### (c) Non-deterministic output order

The ranking cohort was built by iterating `self.open` (a `HashMap`), so ranks
were deterministic but output *order* was not — which would have broken replay
parity. Fixed by sorting symbol keys before building the cohort and sorting
output snapshots by `opportunity_id`. Caught by
`g38_rank_metadata_is_stable_under_replay`.

### (d) Session boundary pre-empted by inactivity

An overnight gap exceeds any inactivity timeout, so `expire_inactive` fired
first and labelled the closure `Inactivity`, never reaching the session-boundary
branch. Fixed by mirroring the date check `EpisodeTracker::expire_inactive`
already documents. The two reasons mean different things for analysis. Caught by
`a5_session_boundary_closes_deterministically`.

### (e) Process defect, not a code defect

My own edits converted two files from the checkout's CRLF to LF. Git normalizes
on commit so the committed content was unaffected (the diffstat stayed at 69
real insertions with no whole-file churn), but the working tree was left
inconsistent with every other file in the checkout. Normalized back to CRLF.
Same class as the `gzip.open(…, "wt")` defect that earlier cost one artifact its
`sha256sum -c`-verifiable checksum.

## 23. Production-isolation proof

Not asserted — demonstrated.

1. **`i46`** — Auto-Trader decisions byte-identical over the real engine, with a
   non-empty fixture. This is the §31 claim, tested.
2. **`i45`** — client-facing event stream byte-identical in content and order.
3. **`i47`** — the layer's only output is `OpportunityScoreSnapshot`; a
   `ScanEvent` cannot inhabit that type, so there is no value a caller could
   forward to clients or the trader.
4. **`i50`** — disabled capture creates no files.
5. **Structural** — the module reads from a `broadcast::Receiver`. It holds no
   handle to the sender, to `server::run`, to any detector, or to
   `auto_trader`. Its return value is discarded in `main.rs` with an explicit
   comment saying why.
6. **Default off** — `OPPORTUNITY_INTELLIGENCE_SHADOW` must be `1`/`true`/
   `yes`/`on`. A new research consumer costs real CPU on a box that also runs
   the live scan, so that cost is opted into rather than inherited by a
   deployment that did not ask for it.

### §24 checklist — none of these altered

Fast Funnel thresholds · Fast Funnel pass/fail behaviour · relative-volume
formula · universe construction · quiet-watch selection · movers selection ·
Ignition thresholds · Ignition cooldown · Ignition follow-through · flat-base
gate · Momentum production weights · Momentum 0.60 production gate · Momentum
0.40 deterioration exit · Consolidation thresholds · Micropullback thresholds ·
Halt Detector behaviour · Catalyst qualification · Auto-Trader entry conditions
· Auto-Trader exits · Auto-Trader sizing · Auto-Trader concurrency · targets ·
stops · timeouts · strategy enablement · production client event ordering ·
existing push-alert policy.

Also: the new scores are **not** exposed to Auto-Trader; the new rank suppresses
**no** `ScanEvent`; `researchRank` is **not** replaced or reinterpreted; **no**
raw detector evidence is deleted; **no** low-ranked opportunity is excluded from
measurement.

### §25 boundary

No required feature turned out to need a detector behaviour change. Every
feature is an already-computed value exposed through the existing
`SignalContext` surface. One boundary was approached and not crossed: the
`Strategy` enum needed ordering to be a `BTreeMap` key. Rather than add a derive
to a production type shared with `strategy_config`, the module keys by `String`
through a local `strategy_name()` helper and stays self-contained.

## 24. Explicit verification that strategy files remained unchanged

Mechanical check, working tree versus `HEAD`:

```
### Strategy / production crates: working tree vs HEAD
  fast-funnel              modified=0 untracked=0
  ignition-detector        modified=0 untracked=0
  momentum-scorer          modified=0 untracked=0
  consolidation-breakout   modified=0 untracked=0
  halt-detector            modified=0 untracked=0
  market-data              modified=0 untracked=0
  auto-trader              modified=0 untracked=0
  replay-engine            modified=0 untracked=0

### Python service + ops + config
  python/app               modified=0 untracked=0
  ops                      modified=0 untracked=0
  docker-compose.yml       modified=0 untracked=0
  deploy.sh                modified=0 untracked=0

### ws-server: which tracked files differ from HEAD
crates/ws-server/src/main.rs

### backtest-metrics: which tracked files differ from HEAD
crates/backtest-metrics/src/lib.rs
```

Exactly two tracked files differ from `HEAD`, both purely additive. No strategy
crate, detector crate, Auto-Trader file, market-data file, Python service file,
ops file or configuration file is modified or has an untracked addition.

Sessions 001 (2026-09-10) and 002 (2026-09-11) were not read, modified or
pooled. The 2026-09-14 ANALYSIS BASELINE 001 artifacts were not modified.

## 25. `git status --short`

```
 M crates/backtest-metrics/src/lib.rs
 M crates/ws-server/src/main.rs
?? crates/backtest-metrics/src/attribution.rs
?? crates/backtest-metrics/src/attribution_tests.rs
?? crates/backtest-metrics/src/bin/oi_attribute.rs
?? crates/backtest-metrics/src/bin/oi_replay.rs
?? crates/backtest-metrics/src/opportunity.rs
?? crates/backtest-metrics/src/opportunity_tests.rs
?? crates/ws-server/src/opportunity_shadow.rs
?? crates/ws-server/src/opportunity_shadow_tests.rs
?? python/discovery_evidence.py
?? research-reports/ALPHA-OPPORTUNITY-INTELLIGENCE-V1-2026-09-15.md
```

## 26. `git diff --stat`

```
 crates/backtest-metrics/src/lib.rs | 15 +++++++++++
 crates/ws-server/src/main.rs       | 54 ++++++++++++++++++++++++++++++++++++++
 2 files changed, 69 insertions(+)
```

Untracked files do not appear in `git diff --stat` by design; they are listed in
§25 and sized in §4. Nothing is staged:
`git diff --cached --name-only` → empty. `git rev-parse HEAD` →
`c8f78dc2677591c6b9de37d2dfc034b506810ade`, unchanged.

## 27. Known limitations

1. **No production raw-`ScanEvent` capture exists.** The measurement capture
   persists *episodes*; the discovery capture persists *scan* records. Neither
   is a raw event log. `oi_replay`'s real inputs today are therefore hand-built
   or test-generated sequences. Deliberately not accompanied by a new
   production raw-event writer: that would be a new high-volume write path on
   the market-dispatch side, which this milestone is not authorised to add.
   Defining the format now gives a future capture a target instead of letting
   this layer grow a second, divergent one.
2. **No opportunity↔outcome join is implemented.** The shadow log records
   *scoring decisions*; it carries **no outcome fields**. Outcomes live in the
   separate measurement capture, keyed on episodes. `OpportunityId` deliberately
   mirrors `EpisodeId` so the two are joinable, but **that join utility does not
   exist and is not tested**. Until it does, precision cannot be computed from
   the shadow log alone. This is the single largest remaining gap.
3. **The discovery archive is not a complete detector log** — ignition events
   only, so `detectorProduced` can never be evidenced absent from it.
4. **Per-event cost is linear in open-set size** (§18). Bounded, measured,
   stated.
5. **`detection_features` is persisted once per opportunity.** Forward-fill by
   `opportunityId`. If the first ranking window was lost to a counted writer
   drop, or the opportunity closed before ever being ranked, it is absent
   entirely.
6. **Model weights are hand-set, not fitted.** No result validates them (§28).
7. **`ShadowState`'s top-decile threshold (10%) is arbitrary** — a starting
   point for measurement, not a calibrated value.
8. **`history_truncations` is currently always zero, and
   `max_history_per_opportunity` (512) is never read.** Both are declared, and
   the config field is carried in the fingerprint, but the opportunity retains
   scalars and counters rather than a price track, so there is no history to
   truncate. Verified by grep: the counter has exactly one reference (its own
   declaration) and the config field two (declaration and default). Neither is
   load-bearing; both are reserved, and an auditor should not read the zero as
   evidence that a bounded history was exercised.
9. **Replay parity is proved against synthetic sequences**, not against a real
   production session — a direct consequence of (1).

Outstanding from earlier milestones, unchanged and non-blocking:
`discovery-review` is crash-looping (3,801+ restarts) because R5 bumped the
discovery schema to 2 while `python/analyze_discovery.py:41` still hard-rejects
anything but 1 (my defect, harmless to capture, deliberately not fixed here);
capacity telemetry emits only at shutdown; `H:\wavystack\CLAUDE.md` still claims
the VPS deploys `master` at `b4331df` when it deploys
`release/operating-run-20260907` at `df95d63`; two temporary SSH keys remain
installed on the VPS; Session 001 (2026-09-10) is staged on the VPS but not yet
pulled to this workstation.

## 28. What V1 can now measure prospectively

Once shadow capture is enabled on a **future** session, the log supports these
measurements directly:

1. **Episode-to-opportunity collapse ratio**, measured rather than inferred:
   `episodeFragments`, `invalidationsAbsorbed`, `rawEventCount` per opportunity,
   against the concurrent measurement capture's episode count.
2. **Firehose reduction at the prioritization layer**: how many opportunities
   exist per ranking window versus how many raw detector events occurred, and
   how many opportunities occupy top-decile. Raw recall is untouched, so the
   reduction is measurable without having destroyed anything.
3. **Earliness distribution**: `moveBeforeDetectionPct` per opportunity, and
   `opportunityAgeSecs` at which each `shadowState` first fires — separable by
   regime and by price band.
4. **Whether `EarlyQualityScore` ranks differently from `ContinuationConfidence`**
   — both ranks are on every row, so their rank correlation is directly
   computable, as is the overlap of their top deciles. This is the concrete test
   of whether an *early* signal was actually constructed or whether it collapsed
   back into a confirmation signal.
5. **Regime population shares** and how outcome distributions differ across
   price bands.
6. **Score availability and missingness structure**: which inputs are absent,
   how often, and whether absence concentrates in particular regimes or price
   bands. `present_inputs` / `missing` are on every row.
7. **Component attribution**: because every score carries its full
   decomposition, the contribution distribution of each feature is computable
   without re-running the engine.
8. **Confluence timing**: `confluenceCount`, `detectorsSeen`,
   `confirmationSpanSecs` and `DetectorArrival.arrival_index` — which detector
   arrives first, how often others follow, and how long confirmation takes.
9. **Discovery recall with causes**: via `oi_attribute`, how far every symbol
   got, with `NotVisible` separated from `Unknown` so a data outage cannot be
   read as a market fact.
10. **Capture completeness itself**: `OiHealth` + `ShadowHealth` + broadcast lag
    warnings establish whether the session's data is complete before any
    conclusion is drawn from it.

## 29. What V1 still cannot establish

Stated plainly, because these are the claims an auditor should refuse if they
appear:

1. **Nothing about precision or hit rate yet** — the shadow log carries no
   outcomes, and the opportunity↔episode-outcome join is not implemented
   (§27.2). Any precision number would require that join first.
2. **Nothing about whether the V1 weights are correct.** They are hand-set
   starting points. They were not fitted, and no result in this milestone
   validates them.
3. **Nothing from ANALYSIS BASELINE 001.** §26 forbids evaluating a
   hand-designed V1 on the data used to design it and calling the result
   validation. 2026-09-14 is development evidence.
4. **No causal claim that ranking improves trading outcomes.** Nothing was
   traded on any of this, and the Auto-Trader cannot see it by construction.
5. **No claim that the earliness/quality frontier shape encoded in the
   transforms is correct** — the knots were seeded from descriptive buckets, not
   estimated.
6. **No comparative recall statement** until the attribution join has been run
   on a session where shadow capture was actually enabled. The tooling is built
   and tested; it has been run only on synthetic input.
7. **No replay-parity claim against real production traffic** (§27.1/27.9).
8. **No statement about whether `ShadowState`'s 10% decile is the right
   cutoff.**
9. **No calibration**: `earlyQuality` and `continuation` are ordinal research
   scores on `[0, 1]`. Nothing establishes that 0.8 means anything absolute.

## 30. Exact prospective shadow-evaluation procedure

The procedure below is the intended next step. **It has not been performed, and
no session has been started.**

### Phase 0 — pre-registration (before any capture)

1. Record `HEAD`, the config fingerprint (`oi-cfg-a7b2bf07d227c55e`) and the
   full `OiVersions` block. Every persisted row carries them, so the evaluation
   is against a **frozen, pre-registered** model rather than one adjusted after
   seeing results.
2. Record the evaluation questions from §28 **in advance**. Questions chosen
   after seeing the data are not measurements.
3. Confirm shadow capture is off in the current deployment and that no
   production behaviour differs from `df95d63`.

### Phase 1 — capture (requires explicit authorisation; not granted)

4. Choose a session date `D` strictly after 2026-09-15. `D` must **not** be
   2026-09-14, 2026-09-10 or 2026-09-11.
5. Set `OPPORTUNITY_INTELLIGENCE_SHADOW=1` for the `ws` service only. Change
   **nothing** else: no threshold, no strategy enablement, no Auto-Trader
   setting.
6. Let the existing measurement capture run unchanged and concurrently. Both
   write under `data/research/`, which is bind-mounted and survives a redeploy.
7. Do not restart mid-session. A restart splits the shadow log and resets
   `windowId` counters.

### Phase 2 — completeness gate (before any analysis)

8. Collect from the session-end logs: `ShadowHealth.dropped`,
   `write_errors`, `snapshots_written`; `OiHealth.peak_open_opportunities`,
   `capacity_evictions`, `cohort_truncations`; and every
   `"opportunity-intelligence lagged"` warning with its `skipped` count.
9. **Gate:** if `dropped > 0`, `write_errors > 0`, `capacity_evictions > 0` or
   `cohort_truncations > 0`, the session is *capacity-censored* and must be
   reported as such. It may still be analysed, but no completeness or recall
   claim may be made from it without that caveat attached. This gate exists
   because Session 002 required exactly this distinction retroactively.
10. Verify the file is LF-terminated NDJSON and every line parses
    (`k62` guarantees the writer's side; verify the artifact anyway).

### Phase 3 — artifact assembly

11. Collect three artifacts for `D`: the shadow log
    `opportunity-intelligence-D.ndjson`; the measurement capture's episode and
    outcome artifacts for `D`; the discovery archive for `D`.
12. Checksum all three (`sha256sum`, byte-mode, LF-verified) and record sizes.
13. Reduce the discovery archive, then
    `python/discovery_evidence.py <reduced> --session-date D --out evidence-D.ndjson`.
14. `oi_attribute evidence-D.ndjson opportunity-intelligence-D.ndjson > attributed-D.ndjson`.
    Record the `AttributionCoverage` summary. The tool refuses a partial join, so
    a non-zero exit is a finding, not a nuisance.

### Phase 4 — replay parity on real data

15. **Currently blocked** by §27.1 — there is no raw `ScanEvent` log to replay.
    Parity is established by the group J test suite against synthetic
    sequences only. Do not claim session-level replay parity until a raw event
    capture exists.

### Phase 5 — evaluation (GPT, not Claude)

16. Hand the three artifacts plus `attributed-D.ndjson` to GPT. **Claude
    computes no hit rate, precision, expectancy, feature correlation,
    effectiveness measure, threshold optimisation, detector-confluence analysis
    or missed-runner characterisation** — that division of labour is a standing
    constraint of this programme, not a preference.
17. The precision and recall questions additionally require the
    opportunity↔outcome join described in §27.2, which does not yet exist.
    Items 1–8 and 10 of §28 are answerable from the artifacts as they stand;
    item 9 requires step 14; precision requires the missing join.
18. Any V1 result must be reported with its `configFingerprint` and model
    versions, so a later configuration change cannot be confused with a
    behavioural change.

### Phase 6 — only then

19. Decide whether V1's weights warrant revision. Any revision bumps the model
    version rather than editing V1 in place, so prior artifacts stay
    attributable.

## 31. Compliance summary

**§26 — no training on the baseline.** Nothing in this milestone was fitted to,
trained on, or evaluated against ANALYSIS BASELINE 001. Every number in this
report comes from synthetic fixtures constructed for the tests, or from measured
execution cost. No V1 result on 2026-09-14 is claimed, because none was
produced.

**§28 — stop conditions, none triggered.** No production detector threshold
changed. No production strategy logic changed. No Auto-Trader behaviour changed.
No target/stop/timeout semantics touched. No universe or discovery selection
semantics changed. No new paid service or database. No destructive schema
migration. No persisted field silently reinterpreted — the one semantic change
(`features`) is version-bumped and documented. No unbounded in-memory state. No
blocking I/O on market dispatch. No outcome-dependent live scoring. No training
using future observations. No test weakened. No raw detector evidence deleted.
No measurement of low-ranked opportunities stopped.

Repository inspection did not contradict §§0–18. One thing it *explained*:
`EpisodeTracker`'s Rule 4 is the mechanical cause of the episode-to-opportunity
collapse ratio, which the specification described but did not attribute.

**§29 — git and deployment.** Nothing staged. Nothing committed — `HEAD` is
still `c8f78dc`. Nothing pushed. No PR. No merge. No deploy. No SSH. No
production configuration touched. No credentials touched. `git add .` and
`git add -A` were not used. No shadow or live session was started. The
implementation is left uncommitted in the working tree, as instructed.

## 32. Final recommendation

**READY FOR REVIEW / CHECKPOINT.**

Rationale:

- All seven phases (A–G) are implemented and green.
- The blocking isolation test passes, driving the real Auto-Trader engine rather
  than asserting an absence.
- 67 new tests; workspace at 506 passing, 0 failing, 0 warnings.
- Bounds are derived and measured under observed, 2×, overload and recovery
  load, with the one non-obvious cost characteristic identified and pinned
  rather than left to be discovered later.
- Four real defects were found and fixed during implementation, the most
  consequential of them mine and silently discarding 80% of the continuous
  information this layer exists to preserve.

Two things a reviewer should weigh before authorising a capture session:

1. **The opportunity↔outcome join does not exist** (§27.2). Without it the
   shadow log cannot yield a precision number, so a capture session would answer
   items 1–8 and 10 of §28 but not precision. That may still be worth doing
   first, since the join can be built against real captured data rather than
   against a guess about its shape — but it should be a deliberate choice, not a
   surprise.
2. **Offline replay has no production input** (§27.1). Session-level replay
   parity is not yet claimable, and closing that gap means a new raw-event
   capture — a change outside this milestone's authorisation and one that
   deserves its own decision.

Neither is a correction to what was built; both are scope boundaries that were
respected rather than crossed quietly. Nothing here requires rework before
review.

---

### §31 Final verification

- No production detector threshold changed — **confirmed**
- No production strategy behaviour changed — **confirmed**
- No Auto-Trader behaviour changed — **confirmed** (`i46`, byte-identical
  journal output over the real engine, non-empty fixture)
- No production ranking or filtering changed — **confirmed**
- No existing alert suppressed — **confirmed**
- No future information used — **confirmed** (feature refresh filtered to
  observations at or before the instant asked for; finalised bars attributed to
  bar end)
- No unbounded state introduced — **confirmed** (all bounds derived, §16)
- No blocking research I/O on dispatch — **confirmed** (`try_send` only, `k60`)
- No files staged — **confirmed**
- No commit — **confirmed** (HEAD unchanged at `c8f78dc`)
- No push — **confirmed**
- No deploy — **confirmed**
- No SSH — **confirmed**
- No production configuration changed — **confirmed**
- No credentials touched — **confirmed**

ALPHA OPPORTUNITY INTELLIGENCE V1 — LOCAL IMPLEMENTATION COMPLETE
