# Opportunity Intelligence V2 — Formula Preregistration

## 1. Baseline commit

`143cb8ad44a143006c5ba26d9bab58db5955d1a7`

Worktree `H:\wavystack\claude_stockspotter`, branch `oi-v2-ranking-intelligence`,
tracked diff against the baseline: empty at the time of writing.

## 2. Development sessions

- Thursday 2026-09-17, regular session 13:30:00Z–20:00:00Z
- Friday 2026-09-18, regular session 13:30:00Z–20:00:00Z

Frozen independent +10% reference artifacts:

- THU `2fae868aac37c8b2a7a030a0bc37f965cf7aed7846bb680a3dd195cd7a3fb90b`
- FRI `9b930d1893c17187e8be6da9a724ebebbc7e4ce6b465fdba34e811652117e603`

## 3. Statement on outcome independence

**No outcome value was consulted while constructing this formula.**

Inputs used for construction were, exclusively:

- source semantics at `143cb8a`;
- the accepted §28 causal feature inventory;
- the accepted §§52–55 observed density report (availability only);
- the established V1 architectural failure;
- general structural reasoning.

Not consulted: forward returns, MFE, MAE, +10% reference membership
performance, top-k retention of any trial formula. No weight search was
performed. No alternative formula was scored and discarded.

Every transform shape reused from V1 is reused *because it was already
preregistered in V1*, not because it performed well on these sessions.

## 4. Model and version identities

| Quantity | Identity |
|---|---|
| Early V2 | `early-quality-v2-transparent` |
| Continuation V2 | `continuation-v1-transparent` (identity-preserving, see §14) |
| RiskQuality | `risk-quality-v1-transparent` |
| Evidence confidence | `evidence-confidence-v1` |
| Priority | `opportunity-priority-v2-transparent` |
| Score policy | `score-policy-v3-availability-aware` |
| Ranking | `opportunity-rank-v2` |
| V2 schema | `1` |

Regime classifier, price regime, opportunity schema and feature schema are
**unchanged** from V1 and keep their V1 identities.

## 5–8. Early V2 — Mode A (momentum-independent)

Mode name: `EARLY_V2_MOMENTUM_INDEPENDENT`.

All components draw on §53 Class-A universal evidence. Momentum appears
nowhere in Mode A.

| # | Component | Source | Transform | Weight |
|---|---|---|---|---|
| A1 | `earliness.moveBeforeDetectionPct` | `opportunity.moveBeforeDetectionPct` | Piecewise `(-5,0.2) (0,1.0) (2,0.8) (5,0.4) (10,0.1) (20,0.0)` | 0.20 |
| A2 | `earliness.consumedFromSessionLow` | `preDetection.sessionLowObserved`, `currentPrice` | Piecewise `(0,1.0) (10,0.8) (25,0.5) (50,0.2) (100,0.0)` | 0.15 |
| B1 | `ignition.confirmationRatio` | `ignition.confirmations / (confirmations + rejections)` | Raw | 0.15 |
| B2 | `ignition.attemptQuality` | `ignition.candidatesOpened` | Bins bounds `[1,2,4]` weights `[1.0,1.0,0.6,0.3,0.1]` | 0.10 |
| B3 | `ignition.phaseQuality` | `ignition.phase` | Map: `FollowThroughConfirmed`=1.0, `CandidateOpened`=0.5, `FollowThroughRejected`=0.0 | 0.05 |
| C1 | `stability.invalidationBurden` | `opportunity.invalidationsAbsorbed` | Bins bounds `[0,1,3,8]` weights `[1.0,0.8,0.55,0.25,0.05]` | 0.10 |
| C2 | `stability.fragmentation` | `opportunity.episodeFragments` | Bins bounds `[1,2,4]` weights `[1.0,0.7,0.4,0.15]` | 0.10 |
| D1 | `maturity.opportunityAge` | `opportunity.opportunityAgeSecs` | Piecewise `(0,0.35) (60,0.85) (300,1.0) (900,0.9) (1800,0.6) (3600,0.3) (7200,0.1)` | 0.15 |

Total configured weight 1.00.

**Required (core) inputs — deliberately minimal:**

```
EARLY_V2_CORE = [ "earliness.moveBeforeDetectionPct",
                  "maturity.opportunityAge" ]
```

Both measured at 100% ever / 100% of ranking windows on both sessions.
**Momentum is not, and may never become, a member of this set.**

Gates:

- `MIN_PRESENT_INPUTS_V2 = 3` (of 8)
- `MIN_COMPARABLE_COVERAGE_V2 = 0.40`
- `value = raw_weighted / present_weight` when no core input is missing and
  both gates pass; otherwise `None` with an explicit `unrankable_reason`.

### Structural rationale

- **A1** is the only non-momentum component V1 EarlyQuality had, and its
  knots are carried over unchanged. It encodes "less prior move is better,
  flattening once the move is obvious".
- **A2** answers a question A1 cannot: A1 is anchored at *detection*, A2 at
  the *session low*, so an opportunity detected early in a move that has
  nonetheless already run far from its base is distinguishable.
- **B1–B3** use the ignition lifecycle, which is 100%-available at ranking
  windows. B1 separates constructive progression from repeated failure; B2
  encodes "is this the first attempt or the fourth", which the source
  comment on `candidates_opened` explicitly names as a real feature; B3 is
  the current phase, ordered by the only sensible ordering of the enum.
- **C1–C2** prevent the failure §3C of the brief names: repeated
  fragmentation and absorbed invalidation must not be mistaken for
  conviction merely because event count is high. Note `rawEventCount` is
  deliberately **not** a positive component for exactly this reason.
- **D1** treats age as context, not merit. Very young is unproven, the
  60–900 s band is prime, and stale decays. Bounded at both ends.

## 9–11. Early V2 — Mode B (momentum-informed)

Mode name: `EARLY_V2_MOMENTUM_INFORMED`. Applies **only** when the complete
momentum family is `CURRENTLY_AVAILABLE` (§12 below).

Momentum term, normalized over present weight:

| Component | Transform | Weight |
|---|---|---|
| `momentum.maSlope.presence` | Presence `threshold = 0.0` | 0.35 |
| `momentum.structure` | Raw | 0.25 |
| `momentum.volumeConfirmation` | Bins bounds `[0.40,0.60,0.80,1.00]` weights `[0.0,0.35,0.80,1.00,0.70]` | 0.25 |
| `momentum.wickRejection` | Bins bounds `[0.50,0.75,1.00]` weights `[0.0,0.50,1.00,0.75]` | 0.15 |

All four transforms are V1's, unchanged.

**Combination — the bounded adjustment:**

```
BETA_MOMENTUM_MAX = 0.25

earlyV2 = modeATerm * (1 - BETA_MOMENTUM_MAX)
        + momentumTerm * BETA_MOMENTUM_MAX
```

Momentum's maximum possible contribution to Early V2 is **0.25**, against
V1's **0.85**. This is chosen structurally, not fitted: momentum is
supplemental evidence available at only 3.5–4.9% of ranking windows, so it
cannot be permitted to dominate a score that must rank the other ~95%.

Consequence, stated as a preregistered invariant: an opportunity with
`modeATerm = 0.80` and no momentum scores `0.80`, while one with
`modeATerm = 0.20` and perfect momentum scores `0.20·0.75 + 1.0·0.25 =
0.40`. Strong early evidence without momentum outranks weak early evidence
with perfect momentum. This is required by §6 of the brief.

Persisted on every record: `scoringMode`, `momentumAvailabilityState`,
`momentumFirstAvailableAt`, `modeATerm`, `momentumTerm`,
`momentumAdjustment` (= `momentumTerm · BETA − modeATerm · BETA`),
`finalEarlyV2`.

## 12. Momentum availability state (live semantics)

Three states, persisted:

| State | Meaning |
|---|---|
| `NEVER_SEEN_YET` | no momentum context observed for this opportunity so far |
| `CURRENTLY_AVAILABLE` | complete momentum family present in this snapshot |
| `SEEN_PREVIOUSLY_BUT_STALE` | momentum seen earlier, absent now |

`FEATURE_FRESHNESS_SECS = 120` remains authoritative and is the mechanism by
which `CURRENTLY_AVAILABLE` decays to `SEEN_PREVIOUSLY_BUT_STALE`.
`SEEN_PREVIOUSLY_BUT_STALE` **must not** be treated as
`CURRENTLY_AVAILABLE`; Mode B does not apply in that state.

The offline lifecycle classes (`NEVER_AVAILABLE`, `ACQUIRED_LATER`,
`AVAILABLE_FROM_FIRST_EVALUATION`) are derived later from these, not stored.

## 13. RiskQuality V1

Direction: **higher = more favourable / better-controlled risk.** Range
`0.0 .. 1.0`. Pinned by test.

| Component | Source | Transform | Weight |
|---|---|---|---|
| `risk.drawdownFromHigh` | `(observedHigh − currentPrice) / observedHigh · 100` | Piecewise `(0,1.0) (2,0.85) (5,0.6) (10,0.3) (20,0.1) (40,0.0)` | 0.30 |
| `risk.adverseExcursion` | `opportunity.minMovePct` | Piecewise `(-40,0.0) (-20,0.1) (-10,0.3) (-5,0.6) (-2,0.85) (0,1.0)` | 0.20 |
| `risk.instability` | `invalidationsAbsorbed` | Bins bounds `[0,1,3,8]` weights `[1.0,0.8,0.5,0.2,0.0]` | 0.20 |
| `risk.fragmentation` | `episodeFragments` | Bins bounds `[1,2,4]` weights `[1.0,0.7,0.35,0.1]` | 0.15 |
| `risk.ignitionRejectionRate` | `rejections / (confirmations + rejections)` | Piecewise `(0,1.0) (0.25,0.8) (0.5,0.5) (0.75,0.2) (1.0,0.0)` | 0.15 |
| `risk.haltProximity` *(optional)* | `halt.proximityRatio` | Piecewise `(0,1.0) (0.25,0.9) (0.5,0.6) (0.75,0.3) (1.0,0.0)` | 0.15 |

`RISK_QUALITY_CORE = [ "risk.drawdownFromHigh" ]` — derived from
`observedHigh` and `currentPrice`, both 100%-available.

`risk.haltProximity` is **optional** (§53 Class B, 37.7–38.0% of ranking
windows). RiskQuality must remain scoreable without halt context; when halt
is absent the score renormalizes over present weight and records the miss.

Deliberately **excluded**, on density grounds: `market.spreadPct`,
`market.bid`/`ask`, `funnel.*`. All measured 0.00–0.97% and cannot carry a
component.

## 14. Continuation V2

```
continuation-v2 := continuation-v1-transparent   (identity-preserving)
```

Continuation V1 is the strongest measured baseline and §8 of the brief
directs that it be preserved aggressively. No component, transform, weight,
core requirement or gate is altered. The V1 version identity is retained
rather than minted as a new one, because presenting an unchanged model
under a new name would be misleading.

## 15. Evidence confidence V1

```
evidenceConfidence =
      0.60 · (early_core_present / early_core_total)
    + 0.25 · optionalCoverage
    + 0.15 · momentumConfidence
```

where `optionalCoverage = present_weight / total_weight` for Mode A, and

```
momentumConfidence = 1.0   if CURRENTLY_AVAILABLE
                   = 0.5   if SEEN_PREVIOUSLY_BUT_STALE
                   = 0.0   if NEVER_SEEN_YET
```

An opportunity with full universal evidence and no momentum reaches
**0.85**. Absence of momentum therefore does not by itself imply low
confidence, which §11 of the brief requires — Mode A exists precisely to
operate before momentum.

## 16–18. Optional / missing / stale semantics

- **Missing** input: excluded from both `raw_weighted` and `present_weight`,
  recorded in `missing[]`. **Never coerced to 0.0.**
- **Optional** input: may raise or lower a score when present; may never
  gate rankability.
- **Stale** input (older than `FEATURE_FRESHNESS_SECS`): treated as missing
  for scoring, but distinguished in `momentumAvailabilityState` so the two
  are not conflated.
- A component whose denominator would be zero (e.g. `confirmations +
  rejections = 0`) is **missing**, not 0.0.

## 19–21. Cadence, cohort, ties

- Cadence: **30 s**, unchanged. Measured median 30.00 s, p95 30.01–30.02 s.
- Cohort: the contemporaneous ranking cohort at each window, identical for
  V1 and V2. `max_rank_cohort = 4096` unchanged.
- V2 scoring an opportunity that V1 cannot is reported as **coverage
  improvement**, never by changing the denominator.
- Ties: ascending `opportunityId`, matching V1's deterministic tie rule.

## 22. Rank persistence

All §39 fields are computed and persisted:
`firstScoreAt`, `firstRankAt`, `bestRank`, `bestPercentile`,
`currentPercentile`, `consecutiveTop25/10/5`, `totalTop25/10/5Windows`,
`previousScore`, `scoreDelta`, `previousRank`, `rankDelta`, `bestRankAt`,
`timeSinceBestRank`.

**Formula weight in V2.0: ZERO.** Persistence has no prospective evidence
yet; §14 of the brief warns against turning V2 into a persistence model
before persistence is tested. It is measured now, and may earn a weight in a
separately authorized V2.1.

State is bounded: a fixed-size struct per opportunity, no history vector,
reset only by opportunity lifecycle — never by a detector invalidation that
does not close the opportunity.

## 23. Priority V2 — regime-aware

```
priorityRaw = wE·earlyV2 + wC·continuation + wR·riskQuality
priorityV2  = priorityRaw · (0.85 + 0.15 · evidenceConfidence)
```

| Regime | wE | wC | wR |
|---|---|---|---|
| `early_emerging` | 0.50 | 0.25 | 0.25 |
| `continuation_acceleration` | 0.20 | 0.55 | 0.25 |
| `reversal_recovery` | 0.30 | 0.30 | 0.40 |
| `unclassified` | 0.30 | 0.40 | 0.30 |

Weights sum to 1.00 in every regime. The frozen regime classifier is
**unchanged**.

Rationale: early evidence should dominate where the move has not yet
matured; continuation should dominate once acceleration is the question;
reversal is the least-evidenced regime (n = 131/328) so it is weighted
conservatively toward risk; unclassified takes a conservative default.

The confidence multiplier spans `0.85 .. 1.00`: sparse evidence is
discounted at most 15%, so missing evidence is neither a free pass (§35) nor
converted into negative evidence.

If a component is unrankable its weight is removed and the remainder
renormalized; `priorityV2` is `None` only when Early V2, Continuation and
RiskQuality are all unrankable.

## 24. Score ranges

All of `earlyV2`, `continuation`, `riskQuality`, `evidenceConfidence`,
`priorityV2` are bounded `0.0 .. 1.0`. They are **not** mutually comparable:
`0.8` Early does not mean the same thing as `0.8` RiskQuality. Priority V2
is the single common ranking surface.

## 25. Config fingerprint

FNV-1a over the canonical JSON encoding of `V2Config`, formatted
`oi-v2-cfg-{hash:016x}` — the same algorithm as V1's `oi-cfg-`, applied to a
separate struct so V1 and V2 fingerprints move independently.

`V2Config` includes every weight, every transform, every core list, both
gates, `BETA_MOMENTUM_MAX`, all regime weight triples, the confidence
coefficients, cadence and cohort size. Changing any of them changes the
fingerprint. Pinned by test.

## 26. Expected causal invariants

1. No component reads any value timestamped after the score instant.
2. Momentum absent ⇒ Early V2 still rankable given core + ≥3 inputs.
3. Missing ≠ zero, anywhere.
4. `SEEN_PREVIOUSLY_BUT_STALE` ⇒ Mode A, not Mode B.
5. Momentum contribution to Early V2 ≤ `BETA_MOMENTUM_MAX` = 0.25.
6. RiskQuality is monotone non-increasing in drawdown, invalidations,
   fragmentation and halt proximity, all else equal.
7. Enabling V2 changes no production-visible event, ordering, or
   Auto-Trader decision.
8. V1 output is bit-identical with V2 enabled and disabled.
9. Replay and live produce identical V2 values from identical input.
10. Any `V2Config` change changes the V2 config fingerprint.

## 27. Explicit non-goals

- Not a trade-entry rule, threshold, or Auto-Trader input.
- Not a prediction of future MAE; RiskQuality describes *path quality to
  date*.
- Not a persistence model (V2.0 weight is zero).
- Not a confluence model (~99% of opportunities are single-detector).
- Not a catalyst, funnel or market-microstructure model — those failed the
  density gate.
- Not prospectively validated by anything in this milestone.

## 28. Development success / failure criteria

Inherited unchanged from §45 of the governing brief:

- **A** production event output byte/semantically identical;
- **B** Early V2 coverage materially exceeds V1's 56.2% / 58.6% without
  coercing missing to zero;
- **C** reduces the "+10 runner exists → opportunity exists → Early
  unavailable/low" failure;
- **D** no material deterioration against Continuation V1's
  100%/93.8% top-10 and 93.8%/75.0% top-5;
- **E** any MFE gain accompanied by materially worse MAE is reported as a
  tradeoff, not a win;
- **F** Priority V2 shows a substantially cleaner rank-quality ordering than
  Early V1, or the failure is reported rather than retuned.

Coverage improvement alone is **not** success (§27 of the governing brief).

---

**This document is frozen on hashing. If an implementation detail proves
impossible, the milestone stops and reports the conflict rather than
silently amending this file.**
