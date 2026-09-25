# Opportunity lifecycle `move-v1` — preregistered design record

- **Status: FROZEN BEFORE IMPLEMENTATION (2026-09-25).**
- **Change control:** the SHA-256 of this file is recorded in the commit that adds it and in the P3 report. Any semantic change during implementation must:
  1. be written into a new section, "Amendments", at the end of this file, with the reason;
  2. be re-hashed and committed **before** the code that depends on it.
- **Evidence used:** no outcome data (returns, MFE/MAE, labels, runner lists) was consulted to choose any boundary below. Every rule is derived from:
  - the unit the project documents already intended (P2 contract appendix D5.1, with quotations);
  - the detector event semantics as coded;
  - constants that already exist in the code.

- **Source design:** `docs/measurement-correctness-contract-2026-09-25.md`, sections "D5 — Opportunity lifecycle unit" and "Appendix — D5 design detail". This record fixes that design's open choices. It does not replace it.

## 1. Definition

An **opportunity** is **one causal move/setup on one symbol**. It is alive while detector evidence for that setup keeps arriving.
- Market data (bars, trades, halt proximity, catalysts, funnel health) updates an opportunity's **state**: price, extremes, features.
- Market data never extends its **life**.

At most one opportunity is open per symbol at a time, which is unchanged. A symbol may have many opportunities in one market day.

## 2. Per-symbol edge state

The engine keeps a per-symbol edge state that outlives individual opportunities. It is bounded by the symbol universe.
- `funnel_passing: bool`
- `momentum_qualifying: bool`

Semantics are identical to `live_signals::LiveSignalTracker`: the previous value is `false` when the symbol has never been seen. **The first `true` reading a process sees is therefore an edge.** This is the same rule the live signal path already uses.

## 3. Opening event (an edge)

An opportunity opens only when no opportunity is open for the symbol and one of these edges occurs:

| Edge | Strategy recorded |
|---|---|
| `FunnelSignal` with `passed` flipping `false → true` | FastFunnel |
| `MomentumUpdate` with `qualifies` flipping `false → true` | MomentumScorer |
| `IgnitionEvent FollowThroughConfirmed` | IgnitionDetector |
| `ConsolidationEvent EntryTriggered` (breakout or micropullback) | ConsolidationBreakout / Micropullback |

This is the V1 opening set, made edge-triggered. The level `passed: true` / `qualifies: true` repeated on every bar is **not** an opening event.

An edge that arrives while an opportunity is open joins that opportunity as **confluence**. It never opens a second one.

## 4. Relevant evidence (continuation)

Evidence refreshes `last_relevant_at` and `last_evidence_kind`:

| Event | Effect on life |
|---|---|
| Any opening edge above | **positive**: refresh |
| `MomentumUpdate { qualifies: true }` (level) | **positive**: refresh. The momentum detector re-asserts the setup on every bar. |
| `IgnitionEvent CandidateOpened` | **positive**: refresh (setup in progress) |
| `ConsolidationEvent SurgeDetected` / `ConsolidationConfirmed` | **positive**: refresh |
| `IgnitionEvent FollowThroughRejected` | **invalidation**: absorbed and counted (`invalidationsAbsorbed += 1`), **no refresh**. Sets `last_evidence_kind = invalidation`. |
| `FunnelSignal { passed: true }` (level, not an edge) | none (state only) |
| `MomentumUpdate { qualifies: false }`, `FunnelSignal { passed: false }` | none (they update edge state only) |
| `BarUpdate`, `HaltWarning`, `CatalystUpdate`, `FunnelHealth`, any other event | none (state only) |

## 5. Terminal conditions

`T_move = 300 s`. This is the existing inactivity constant, not a new tuned number.

Terminal conditions are evaluated on the causal clock of processed events. Each opportunity closes exactly once.

1. **`setup_inactivity`**: `now − last_relevant_at ≥ T_move` and `last_evidence_kind = positive`.
   - `closed_at = last_relevant_at + T_move`.
2. **`invalidated`**: the same condition when `last_evidence_kind = invalidation`, meaning the most recent evidence was a rejection and no positive evidence followed.
   - `closed_at = last_relevant_at + T_move`, where `last_relevant_at` is the time of the last *positive* evidence. The rejection does not refresh it.
   - A confirm → reject → confirm sequence inside `T_move` therefore stays **one** opportunity, which preserves V1's absorption rule.
3. **`session_boundary`**: the first event whose **market day** (`market_data::market_day`: 04:00 America/New_York, DST-aware, from P2) differs from the opportunity's opening market day.
   - This replaces the UTC-date rule, which split after-hours at 19:00 ET in winter.
   - Event-path close: `closed_at = at`.
   - Expiry-path close: `closed_at` is floored so it is never earlier than any anchor or ranking instant issued while the opportunity was open (the P2 D4 rule).
4. **`capacity_reached`**: unchanged.
5. **`capture_ended`**: unchanged. Graceful shutdown only; a crash leaves no terminal record.

A close is reported to the outcome collector through the P2 D4 pipeline, with **exactly one** canonical disposition token per opportunity: `setup_inactivity | invalidated | session_boundary | capacity_reached | capture_ended`. `still_open` means not closed as of settlement.
- The legacy `inactivity` token stays deserializable for schema ≤ 2 rows.

## 6. Re-arm / new opportunity

After a close, the next **opening edge** (section 3) for the symbol opens a new opportunity with a new identity.
- Positive level evidence alone, such as momentum still `qualifies: true`, does **not** re-open. The momentum edge has to flip false → true again, or another opening edge has to occur.
- Detection-time state is frozen at the **new** open: `moveBeforeDetectionPct`, `detection_context`, the extremes, and the D3 baseline, which remains market-day scoped.

## 7. Identity

- `opportunityId = SYM:UTC-date:ms-since-UTC-midnight` of `opened_at`. The format is unchanged. The UTC date remains the partition and identity key; `marketDay` is a separate field.
- **Uniqueness argument:**
  - A symbol's next open follows a close.
  - `setup_inactivity` and `invalidated` require `T_move` of evidence silence, so the next open is at least `T_move` later.
  - `session_boundary` changes the market day.
  - Capacity evicts a different symbol.
  - The remaining risk is an out-of-order event exactly at a previous `opened_at`. It is guarded by a duplicate-identity check that **refuses to open** and counts the refusal (`duplicateIdentityRefused`), rather than emitting a colliding id. That counter is a qualification gate.
- **`OPPORTUNITY_SCHEMA_VERSION` 2 → 3.** An id now denotes a move. Schema ≤ 2 ids denote symbol-activity containers, and the two are not comparable.

## 8. Relationships

- **Episodes:** the episode lifecycle is **unchanged** (it is a separate unit with its own tracker). Episode↔opportunity membership keeps its existing algorithm. Ambiguity is expected to rise, and it is reported rather than hidden.
- **Outcome anchors:** one anchor per ranking window per open opportunity (unchanged mechanism). Each anchor carries the move's id, and settlement stays independent of the close.
- **Ranking:** only open opportunities are ranked (unchanged). Cohorts contain moves, not symbol-days. D6 capacity (16,375) is unchanged.

## 9. Session phases

- **Premarket → regular, and regular → after-hours:** the same opportunity continues if relevant evidence keeps arriving across the bell. If evidence stops for `T_move`, the next opening edge starts a new one.
- A new field, `openedPhase` (`classify_session` at open: premarket / regular / after_hours / overnight), lets analysis filter by phase without splitting at the bell.

## 10. Restart and reconnect

- **Process restart:**
  - opportunities and edge state are empty (nothing is persisted across processes);
  - the first `true` funnel or momentum reading after start is an edge (section 2);
  - `baselineTruncated` (P2 D3) marks the market day as incomplete.
- **Feed reconnect without a process restart:** the engine is untouched. Edge state and open opportunities persist. A reconnect gap is just a period without events, subject to the ordinary `T_move` clock.

## 11. Capacity

Unchanged: at most one open opportunity per symbol, and the open capacity is 16,375. Eviction is explicit, counted, and reported as `capacity_reached`.

## 12. Versioning and selector

- `OiConfig.lifecycle ∈ { symbol-activity-v1, move-v1 }`. The **default is `move-v1`**.
- `symbol-activity-v1` stays selectable so that historical replay and the model-freeze proof keep their meaning.
- `OiConfig.move_inactivity_secs = 300`.
- Both are included in the config fingerprint, so the fingerprint changes.
- `OiVersions.lifecycle` = `"opportunity-lifecycle-move-v1"` or `"opportunity-lifecycle-symbol-activity-v1"`.
- `OPPORTUNITY_SCHEMA_VERSION = 3`.
- V2 was preregistered against schema-2 semantics. Its inputs, such as `opportunityAge` and `invalidationsAbsorbed`, change meaning under `move-v1`. Evaluating V2 on schema-3 data needs its own re-preregistration, and nothing here authorizes that.

## 13. Explicitly not decided here

- Dual-lifecycle shadow, meaning both lifecycles run on one stream into two artifacts. It is **not** implemented; the selector makes it possible later.
- Any change to detector thresholds, the ranking formula, or the episode lifecycle.

## Amendments

### A1 — Implementation clarifications (2026-09-25, before any move-v1 code)

**No rule in sections 1–12 is changed.** Implementing them exposed details those sections do not state. Each is fixed here, before the code that depends on it, so the choice is part of the frozen record rather than of the diff. As with the rest of this record, no outcome data was consulted: each choice follows from the text above, from `LiveSignalTracker`, or from the V1 code it replaces.

1. **Schema number per lifecycle.** `OPPORTUNITY_SCHEMA_VERSION` is 3 and every `move-v1` row carries 3. A row produced under `symbol-activity-v1` keeps carrying **2**. Section 7 says schema ≤ 2 ids denote symbol-activity containers; stamping 3 on one would make "schema 3" stop meaning "an id denotes a move". It also keeps the model-freeze proof (section 12) comparing like with like.
2. **Confluence is recorded from opening edges only.** Under `move-v1`, `detectorsSeen`, `detectorTransitions` and each `DetectorArrival` are updated only by the section-3 edges ("An edge that arrives while an opportunity is open joins that opportunity as confluence"). Level readings (`passed: true`, `qualifies: true` without a flip) and setup-phase evidence (`CandidateOpened`, `SurgeDetected`, `ConsolidationConfirmed`) act on life as section 4 says. They do not add to confluence. V1 counted the level funnel on every bar as a detector arrival, which is the symbol-activity artifact this lifecycle removes.
3. **Edge state is never reset by the engine.** It persists for the life of the process, as in `LiveSignalTracker` (section 2), across closes and across market days. The design appendix's "evict entries on a date change" is not adopted. It would make the first `true` reading of each market day an edge, and that is not `LiveSignalTracker`'s rule. The map is keyed by symbol, so it stays bounded by the universe.
4. **An opening edge with no known price opens nothing.** The edge is still consumed. This is the existing V1 rule (the fall back is the latest price in the feature cache) and `LiveSignalTracker`'s documented gap for a momentum flip with no prior bar. Positive level evidence cannot re-open (section 6), so the symbol waits for its next edge.
5. **`last_relevant_at` never moves backwards.** Positive evidence sets it to `max(previous, event time)`. An out-of-order older event therefore cannot shorten a live move. `last_evidence_kind` follows processing order, which is the order the engine learns things in.
6. **The clocks are V1's.** The deadline compares `last_relevant_at` (event time) against the receipt instant of the event being processed, exactly as V1 compared `last_seen_at`. The expiry-path session-boundary test compares the opening market day with the market day of that receipt instant; V1 compared UTC dates on the same clock. An expiry-path session-boundary close takes `closed_at = last_relevant_at`, then the P2 D4 ranking floor (section 5.3).
7. **Scope of the duplicate-identity guard.** Per symbol, the engine remembers the `(UTC date, sequence)` of each opportunity it issued on that symbol's current market day. The list is cleared when the symbol opens on a later market day. Two ids collide only if they open at the same instant to the millisecond, and one instant has one market day, so this is exact for every collision that can arise within the process. A refused open still consumes its edge, is counted in `duplicateIdentityRefused`, and opens nothing.
8. **`openedPhase` is populated under `move-v1` only.** It is written on the opportunity and on every ranking row as `openedPhase` (`premarket | regular | after_hours | overnight`). Under `symbol-activity-v1` it is absent, so that lifecycle's output differs from the pre-D5 engine only in `versions`.
9. **Reading old configurations.** `OiConfig::default()` is `move-v1`. A serialized `OiConfig` without a `lifecycle` field deserializes as `symbol-activity-v1`, and an `OiVersions` without one reads as `opportunity-lifecycle-symbol-activity-v1`: both predate this lifecycle. `move_inactivity_secs` is the `move-v1` clock. `inactivity_secs` stays the `symbol-activity-v1` clock, and `move-v1` does not read it.
10. **The capacity victim is the least recently relevant.** Under `move-v1` the expiry index is keyed by `last_relevant_at`, so "least recently active" in section 11 means least recent evidence. Market data no longer counts as activity.
11. **Where the qualification gate lives.** `duplicateIdentityRefused` is reported in `opportunityEngine`. It is blocking in the completeness verdict, compared to zero by the preflight, and listed in the qualification contract's required completeness, as section 7 requires. The contract's hash moves with it; it moves anyway for schema 3.
12. **The outcome version does not move.** `setup_inactivity` and `invalidated` are new disposition tokens under the existing `opportunity-outcome-v2` rule. The rule is unchanged, and `inactivity` stays readable.

### A2 — Edge state is scoped to the market day (2026-09-25, integrator review, before the code that implements it)

**Supersedes A1.3.** A1.3 kept funnel/momentum edge state for the life of the process, by analogy with `LiveSignalTracker`. On review that reintroduces the defect D3 removed. Under A1.3 a symbol still passing the funnel at 19:59 ET gets **no** FastFunnel opening edge at 04:00 ET the next market day, unless a `passed: false` reading happens to arrive in between. A process started overnight **does** get that edge, because its first `true` reading is an edge (section 2). The opportunities a market day produces would therefore depend on process uptime, and two identical market days could segment differently. That is the "features depend on uptime" failure the D3 contract names. The live scanner itself also rebuilds all detector state on each market day (`live.rs` new-session bail).

**Rule.** Each symbol's `funnel_passing` / `momentum_qualifying` belongs to one market day (`market_data::market_day` of the **event's** timestamp, as in D3).
- The first funnel or momentum reading of a later market day starts from `false` for both, so a `true` reading then is an edge. This is exactly what a freshly started process sees.
- A reading from an **earlier** market day than the stored one changes nothing (late correction).
- Within a market day, section 2 applies unchanged.

**Effect.**
- Segmentation of a market day no longer depends on whether the process was running the previous day.
- A continuously running process and one restarted before 04:00 ET open the same opportunities.
- No outcome data was consulted. The change follows from D3's market-day scoping and the section-10 restart semantics.

**Unchanged:**
- the duplicate-identity memory (A1.7), which was already market-day scoped
- every other section and amendment
