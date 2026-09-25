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

(none)
