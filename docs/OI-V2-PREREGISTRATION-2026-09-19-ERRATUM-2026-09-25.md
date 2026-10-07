# Erratum to the OI V2 formula preregistration (2026-09-25)

- **Erratum to:** `OI-V2-PREREGISTRATION-2026-09-19.md`, repository root, as committed in `7e36586`.
- **SHA-256 of that file** (unchanged, and must stay unchanged): `4c7e09ccdeebed9095f714f2fed3ef4ed14c7ceae7404e7187b6efac77265eaa`.
- **The original is not edited.** It closes with: *"This document is frozen on hashing. If an implementation detail proves impossible, the milestone stops and reports the conflict rather than silently amending this file."* This erratum is that report.
  - It amends nothing retroactively.
  - It records where a preregistered statement was false in production.
  - It records what replaces that statement for future sessions.

## 1. The statement in error

`OI-V2-PREREGISTRATION-2026-09-19.md`, section "19–21. Cadence, cohort, ties", lines 245–246:

> - Cohort: the contemporaneous ranking cohort at each window, identical for
>   V1 and V2. `max_rank_cohort = 4096` unchanged.

Related statements that depend on it:

- Lines 247–248: *"V2 scoring an opportunity that V1 cannot is reported as **coverage improvement**, never by changing the denominator."* The denominator named there is the capped cohort.
- Line 313 (§25): *"`V2Config` includes … cadence and cohort size."* `V2Config.max_rank_cohort` is `4_096` (`crates/backtest-metrics/src/opportunity_v2.rs:242`). It enters the V2 fingerprint.

The statement made two claims:

1. The V1 and V2 cohorts are identical.
2. A cap of 4,096 is harmless: "unchanged", with the implied premise that the cap never binds.

**Claim 2 was false in production, and when it failed, the cohort was no longer the contemporaneous ranking cohort.**

## 2. What was observed

Production ran `7e36586` with fingerprint `oi-cfg-b4f21c8b311a1b99` from 2026-09-19 to 2026-09-25. Over that run, `/research/completeness` reported:

- `opportunityEngine.cohortTruncations = 69`;
- an open peak of about **4,678** against the 4,096 cap;
- `capacityEvictions 0`, so the loss was ranking coverage, not capture.

Full scans of the preserved captures attribute every truncation (`stockspotter-research/audit-2026-09-24/05-sessions.md`):

| UTC day | Truncated windows | Regular session (13:30–20:00Z) | Outside regular | Rows scored but unranked |
|---|---|---|---|---|
| 09-21 | 16 | 7 windows / 1,310 rows | 9 / 4,545 | 5,855 |
| 09-22 | 18 | 8 / 1,431 | 10 / 5,099 | 6,530 |
| 09-23 | 18 | 9 / 1,949 | 9 / 4,724 | 6,673 |
| 09-24 | 17 | 7 / 1,338 | 10 / 5,398 | 6,736 |
| **total** | **69** | 31 | 38 | |

**Surface.** On 09-21 the continuation surface was truncated and the early-quality surface was not: 16 continuation windows, a maximum scored cohort of 4,634, and no early-quality truncation. On 09-22 the 8 regular-session windows were continuation truncations at 19:56–19:59:32Z, the last four minutes before the close (`sep22-ranking-scan-20260923T040918Z/SCAN-RESULT.md`, `SEP22-CLOSEOUT-AGREEMENT-20260923.md`). The surfaces were ranked independently, each capped at 4,096 of its own scored set. Continuation needs only `moveFromStartPct`. Early quality needs the momentum core. So the continuation scored set is the larger one, late in the day, as the open population peaks toward the close.

The production counter was the OR of the two surfaces and could not name which one was cut. For 09-23 and 09-24 the audit records window and row counts but no per-surface split. They are *consistent with* continuation-only truncation, but that is not proven here.

In those windows:

- rows past rank 4,096 carried a score with `continuationRank = null`, indistinguishable at the rank field from unscorable rows;
- `continuationCohortSize` was reported as 4,096 instead of the true N;
- every normalised quantity in those windows (percentiles, `shadowState`, TopPercent thresholds) was computed on the wrong N.

The V2 replay (`oi_v2_replay`) takes V1's persisted `earlyCohortSize` and `continuationCohortSize` as its denominators. It therefore inherited the capped N in exactly those windows. So, when the cap bound, the "contemporaneous ranking cohort" was a cohort **truncated by score order**: the top 4,096 of the scored set. It was not the set of opportunities open at the window.

## 3. The correction: D6 (measurement-correctness contract, 2026-09-25)

Implemented on `p2/d4-d6-observability` (`4fac647`) and specified in `docs/measurement-correctness-contract-2026-09-25.md` §D6.

- **Cap = open capacity.** `max_rank_cohort` = `DEFAULT_MAX_RANK_COHORT` = `DEFAULT_MAX_OPEN_OPPORTUNITIES` = **16,375**. A const-assert and `OiConfig::capacity_invariant()` require `max_rank_cohort >= max_open_opportunities()`. Every scored opportunity is ranked on each surface in every window, so truncation is structurally unreachable. The fingerprint moves to `oi-cfg-15861d6d0b263f12`. `RANKING_VERSION` does not change: the rule is the same and only the configured bound moved.
- **Per-surface counters.** `earlyCohortTruncations` and `continuationCohortTruncations` are recorded, and `cohortTruncations` is kept as their OR. Also recorded: per-surface cohort last/peak, `rankCohortCapacity`, `truncationMarkersDropped`, and a `ranking_cohort_truncated {windowId, surface, scored, cap}` marker that is written only if the invariant is ever violated. A non-zero count is blocking in `completeness::check`, and in the qualification v5 gate `ranking-truncation` (`docs/qualification-v5-gates-2026-09-25.md`).
- **Fraction field.** `earlyQualityRankFraction` and `continuationRankFraction` equal `(rank − 1) / N`, where N is that surface's contemporaneous cohort size in the same window. The value is omitted when the rank is absent. For any p, `fraction < p ⇔ rank ≤ ceil(N·p)`. The alpha dataset now uses per-surface cohort sizes. Previously it used `max(early, continuation)` as the denominator for both surfaces, which was a separate bug.

### Absolute rank versus percentile rank

- **An absolute rank** (`*Rank`, 1..N) answers "how many opportunities scored above this one at this instant". It is comparable across windows only when N is comparable. N ranged from about 27 names before 09:00 ET to 4,000+ near the close. An early top-10 among 27 is not the same event as a top-10 among 4,600.
- **A percentile rank** (`*RankFraction`) normalises by the contemporaneous N of the same surface. It is the quantity a TopPercent criterion means.
- Neither is meaningful in a window whose N was truncated. The truncated absolute ranks 1..4,096 were correct. The ranks above 4,096 were missing. Every fraction or percentile in that window was computed on N = 4,096 instead of the true N.

## 4. Historical evidence stays historical

- The 09-21..09-24 captures (schema 2, `oi-cfg-b4f21c8b311a1b99`, `opportunity-outcome-v1`) are **immutable evidence of the old contract**. Their truncated ranks and capped cohort sizes are what that instrument measured, and they stay in the record as such.
- **No V2 or V2.1 result is retroactively repaired** by this erratum. That includes the development readings of 09-17/09-18 and any replay over 09-21..09-24. A result computed over truncated windows keeps the truncation, and must be reported as computed on a capped cohort wherever `cohortTruncations > 0`.
- An uncapped re-rank of those windows is possible from the persisted scores. Per window, re-rank every score-present row by `(value desc, symbol, sequence)`, and only where there was no OI writer loss. This is a **derived** quantity. It must carry its own tag (for example `repair: "d6-rerank"`), live in its own files, and never overwrite the capture. Nothing in this erratum produces one.
- Sessions captured under `oi-cfg-b4f21c8b311a1b99` remain evaluable only under the contract they were captured for. They are INVALID under it wherever `cohortTruncations > 0`. That applies to all four days above.

## 5. The ranking-capacity contract from now on

For every session captured under `oi-cfg-15861d6d0b263f12` or later:

1. The ranked cohort on each surface is **every** opportunity scored on that surface at that window. N is reported exactly (`*CohortSize`), and each surface has its own N.
2. `rankCohortCapacity >= capacity` (open capacity). Qualification checks this before the session in the `session.sh` readiness gate `rank-capacity`, and after it in qualification v5 gate `ranking-truncation`.
3. Any truncation counter or dropped marker that is non-zero makes the session INVALID. No narrative override exists.
4. Percentile criteria use `*RankFraction`, or per-surface N, and never another surface's N.
5. "Identical cohorts for V1 and V2" can only hold if both use this bound. **`V2Config.max_rank_cohort` is still `4_096`** (`opportunity_v2.rs:242`). The code does not apply it to ranking: V2 inherits V1's persisted cohort sizes. But it is part of the V2 fingerprint and it states the old cap. Whoever re-preregisters V2 must set it to open capacity, or remove it, as a deliberate and fingerprint-moving change. This erratum does not change it.

## 6. move-v1 changes what V2's inputs mean

The move-v1 lifecycle (`docs/opportunity-lifecycle-move-v1-preregistration-2026-09-25.md`, frozen before implementation) changes the unit an opportunity denotes. Under V1's symbol-activity lifecycle it was roughly a symbol-day container. Under move-v1 it is one causal move or setup, and `OPPORTUNITY_SCHEMA_VERSION` becomes 3. That changes the meaning of preregistered V2 inputs **without renaming them**:

- **`opportunity.opportunityAgeSecs`** (D1 `maturity.opportunityAge`, weight 0.15, and a member of `EARLY_V2_CORE`). Under schema 2 this was time since the symbol's activity container opened, often hours. Under move-v1 it is time since *this move's* opening edge. The piecewise transform was drawn over the old distribution.
- **`opportunity.invalidationsAbsorbed`** (C1 `stability.invalidationBurden`, 0.10; RiskQuality `risk.instability`, 0.20). This was a whole container's accumulated rejections: for example 9 in both of the first two rows of the 09-24 OI file (AAPL, ADGM at 00:00:32Z). It becomes rejections absorbed within one move, which is bounded by that move's `T_move` window. Its bins `[0,1,3,8]` were drawn over the old distribution.
- **`episodeFragments`** (C2; `risk.fragmentation`) and every cohort size change too: cohorts now contain moves, not symbol-days.
- D3 already changed the meaning of `moveBeforeDetectionPct` and `sessionLowObserved` (A1, A2). They are now market-day scoped, with signal-context schema 2 and feature schema 3.

Consequently, **V2 on schema-3 data requires a new preregistration**, with its own transforms, core list, fingerprint and development evidence. As the move-v1 record (§12) states, nothing in this erratum or in P3 authorises evaluating the 2026-09-19 V2 formula on move-v1 captures. Doing so would score a different unit with coefficients drawn for another.

## 7. Summary

| Item | Status |
|---|---|
| "identical for V1 and V2" / "`max_rank_cohort = 4096` unchanged" (lines 245–246) | **False in production.** The cap bound in 69 windows over 09-21..09-24. The continuation surface was cut, near the close on the days examined. |
| The original preregistration file | Unedited. Hash `4c7e09cc…5eaa`. |
| Correction | D6: cap = open capacity (16,375), per-surface counters and markers, `*RankFraction = (rank−1)/N` |
| Historical V1/V2/V2.1 results over truncated windows | Unrepaired, and must be reported as capped. Any re-rank is a separately tagged, derived quantity. |
| Future sessions | Ranking-capacity contract in §5, enforced by `rank-capacity` (preflight) and `ranking-truncation` (qualification v5) |
| `V2Config.max_rank_cohort = 4096` | Stale, and fingerprint-bearing. For the V2 re-preregistration. |
| V2 on schema-3 (move-v1) | Requires a new preregistration. Not authorised here. |
