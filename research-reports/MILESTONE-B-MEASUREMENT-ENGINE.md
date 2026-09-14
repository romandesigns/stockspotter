# STOCKSPOTTER ALPHA MILESTONE B — MEASUREMENT ENGINE

**Date:** 2026-09-09
**Branch:** `gpt/audit-remediation-20260909` @ `0544790`
**Status:** implemented and validated — **not committed, not pushed, not deployed**

**Behavior preservation: proven structurally.** The only modification to pre-existing code is
**16 lines of module declarations and re-exports** in `lib.rs`. No detector, scorer, auto-trader or
`market-data` file was touched — verified by `git status --porcelain` against those paths. No
threshold, gate, stop, target, timeout or sizing constant was read, let alone changed.

---

## 1. Architecture implemented

Three new modules in `crates/backtest-metrics`, all purely observational:

```
context.rs   SignalContext + FeatureCache
             ├─ folds ScanEvents forward into per-symbol state
             └─ cuts causally-bounded snapshots at signal moments

episode.rs   OpportunityEpisode + EpisodeTracker
             ├─ owns context.rs's FeatureCache
             ├─ applies the five boundary rules
             ├─ accumulates confirmations / momentum track / trader linkage
             └─ assigns research-only rank

horizon.rs   HorizonOutcome (additive to outcome.rs, which is untouched)
             ├─ 7-point forward-return grid
             ├─ MFE / MAE / drawdown-before-MFE
             ├─ time-to-target
             └─ explicit censoring
```

**Why `backtest-metrics` rather than a new crate:** this is where `signals.rs`, `live_signals.rs`
and `outcome.rs` already live, and where the live/backtest definitional parity is deliberately
maintained. A separate crate would have split that contract across a boundary.

**Why the tracker is an owned value, not a global:** the brief warned against a giant mutable global.
`EpisodeTracker` is an ordinary struct the caller owns, so a replay can build episodes from
historical events through the identical code path that live capture uses — which is the same
live/backtest-parity principle `live_signals.rs` already follows.

---

## 2. Episode boundary definition

**Fixed before any outcome was examined.** Every rule mirrors a mechanism the system already relies
on, rather than being invented for this milestone:

| # | Rule | Trigger | Precedent it matches |
|---|---|---|---|
| 1 | **Open** | first *edge-triggered qualifying* observation for a symbol in a session | `signals.rs` / `live_signals.rs` edge-trigger definitions exactly |
| 2 | **Continue** | any further observation of that symbol | — |
| 3 | **Close: inactivity** | 300s with no observation | `UNIVERSE_MONITOR_IDLE_SECS` and `QUIET_TRANSITION_GRACE`, both already 300s |
| 4 | **Close: invalidated** | `IgnitionEventKind::FollowThroughRejected` | the detector itself declaring the move over |
| 5 | **Close: session boundary** | UTC date change | `AlreadyEnteredToday`; discovery protocol's per-session labelling |
| — | **Close: capture ended** | tracker `finish()` | censoring, not conclusion — see §6 |

Diagnostic-only phases (`SurgeDetected`, `ConsolidationConfirmed`, `CandidateOpened`) **do not open**
an episode; they are recorded once one is open. This matches how `signals.rs` already refuses to
count them as signals.

**Identity:** `symbol + session_date + sequence` (1-based). A second distinct move in the same symbol
on the same day is sequence 2, not a continuation.

**Precedence detail found during implementation:** session boundary must be checked *before*
inactivity, because an overnight gap always exceeds 300s — otherwise every cross-session closure
would be mislabelled `Inactivity`, and the two mean different things for analysis. My own test caught
this.

**No rule consults future information.** `boundaries_never_depend_on_future_prices` asserts this
directly: the same event sequence, differing only in what happens *after* the opening moment
(+1% vs +890%), must produce identical boundaries, identical IDs and byte-identical opening context.

---

## 3. Signal-time feature schema

`SignalContext`, `schemaVersion: 1`, camelCase per project convention. Every feature group is
`Option` — **absent means unknown, never zero**. A universe-tier ignition candidate genuinely may
never have been momentum-scored, and a fabricated `0.0` would be indistinguishable from a real one.

| Group | Captured |
|---|---|
| `market` | price, session volume, gap%, minute-of-day; bid/ask/spread as `Option` (see §15) |
| `funnel` | price, gap%, session volume, and **all four gate booleans** — recorded on rejection too, since near-misses are the recall sample |
| `ignition` | phase, plus per-session counters: candidates opened, confirmations, rejections ("first attempt or fourth" is a real feature) |
| `momentum` | **`overall` plus all four factors**, and the 0.60 verdict *as production computed it* |
| `consolidation` | strategy variant, phase, surge/confirmation/entry counters |
| `halt` | level, proximity ratio, band width, band-doubled, rel-vol, LULD-in-effect, **estimated-vs-official bands** |
| `catalyst` | tags, headline count, `observedAt`, `mostRecentPublishedAt`, `headlineAgeSecs` |
| `preDetection` | 1m/3m/5m-before anchors, session low, first observed price, **`moveBeforeDetectionPct`** |

**The momentum group is the highest-value addition.** Production computes `overall`, reduces it to a
boolean at 0.60, and discards the ordering — which is precisely why Milestone A found ranking
unmeasurable. Persisting the continuous value costs nothing and changes no gate.

`qualifies` is stored rather than recomputed, so a future threshold change cannot retroactively alter
what old records claim production decided.

**Freshness:** a feature group older than `FEATURE_FRESHNESS_SECS` (120s) is recorded as **absent**
rather than as a stale value pretending to be current. A two-minute-old momentum reading attached to
a fresh ignition would silently corrupt any interaction study.

---

## 4. Catalyst timestamp semantics — **resolved, and the flagged risk was real**

Milestone A flagged this as unverified. Traced end to end:

1. `python/app/news.py` fetches Alpaca `/v1beta1/news`.
2. `python/app/main.py` returns `most_recent_published_at = news_items[0]["created_at"]` — **genuine
   publication time**.
3. `crates/market-data/src/qualify.rs::SymbolQualification` **does** carry that field into Rust.
4. `crates/market-data/src/live.rs:1246` builds `CatalystRecord { timestamp: Utc::now(), … }` —
   **observation/fetch time** — and **`most_recent_published_at` is silently dropped**, never
   reaching `CatalystRecord` or `ScanEvent::CatalystUpdate`.

**So `CatalystUpdate.timestamp` is observation time, and publication time is available upstream but
discarded before it reaches anything durable.**

**Causality verdict:** observation time is the *safe* field, and the one this schema judges causality
against — we cannot have known a catalyst before we fetched it, so using fetch time can never leak
backward. `CatalystFeatures.observed_at` is that field, and attachment is gated on
`observed_at <= detected_at`, asserted by `a_later_catalyst_is_never_attached_retroactively`.

**Third finding, not previously noted:** `fetch_recent_news` requests `limit=10` with **no time
window**. A "catalyst" tag therefore carries **no recency guarantee** — `headlineCount: 10` could be
ten headlines spanning weeks. `headlineAgeSecs` exists to make that visible instead of every analysis
silently assuming freshness.

**Deliberately not done:** threading `most_recent_published_at` through `CatalystRecord` and
`ScanEvent` would touch `market-data` and the wire protocol. It is additive and low-risk, but this
milestone's absolute constraint was to leave strategy-path code alone. The field is plumbed in the
schema and populated as `None` until that one-line upstream change is authorized. **Flagged as the
top follow-up in §15.**

---

## 5. Outcome schema

`HorizonOutcome`, `schemaVersion: 1`. **Purely additive** — `outcome.rs`'s target/stop model is
untouched and still authoritative for existing metrics.

- **Forward returns** at 30s / 1m / 3m / 5m / 10m / 15m / 30m.
- **Excursion**: `mfePct`, **`maePct`** (absent from the codebase entirely before this),
  `secondsToMfe`, `secondsToMae`, **`drawdownBeforeMfePct`** — the heat endured to capture the
  favorable move.
- **Time-to-target** for +2% / +5% / +10%.
- `observedSpanSecs` and `observationCount`, so partial coverage is explicit rather than implicit.

**Causal sampling rule, stated once and applied uniformly:** for horizon *h*, the sampled price is
the **first observation at or after `signal_at + h`**. Never nearest, never interpolated, never the
last before. If no such observation exists, the horizon is censored. This makes a 30-second return
and a 30-minute return structurally the same kind of claim.

Observations before `signal_at` are filtered out — asserted by
`observations_before_the_signal_are_ignored`, so a pre-signal dip cannot become MAE.

---

## 6. Censoring semantics

The Milestone A ambiguity, fixed. `Observation<T>` is either `Observed(T)` or
`Censored(CensorReason)`:

| Reason | Meaning | Would more collection help? |
|---|---|---|
| `SessionEnded` | horizon fell past session end | **No** |
| `CaptureEnded` | observation stopped for our reasons | Yes |
| `InsufficientForwardData` | path does not extend that far | Yes |
| `DataGap` | a mid-window gap > 120s, so extremes inside it are unknown | Yes |

**"Failed to reach target" and "we do not know" are now different values.** A target unreached on a
*fully observed* longest window returns `Observed(None)` — a real miss. Unreached on a short window
returns `Censored(...)` — unknown. Two tests assert exactly this pair.

`EpisodeCloseReason::CaptureEnded` carries the same meaning at episode level: still-open at capture
end says something about us, not about the opportunity.

**Aggregation contract:** censored samples must never be counted as ordinary misses. The type system
enforces it — `Observation::observed()` returns `Option<T>`, so a caller cannot silently coerce
censored to zero.

---

## 7. Research-ranking telemetry

`ResearchRank { rank, cohortSize, score, rankedAt, windowId }`, attached per episode.

`EpisodeTracker::assign_research_rank` orders currently-open episodes by their most recent
`momentum.overall` — the score production already computes and discards at the gate. It answers one
question: **does that score already contain useful ordering information?**

**Isolation guarantees:** it is computed *after* episodes are built, writes only to a field nothing
else reads, and is not emitted to clients, not consulted by the auto-trader, and cannot suppress or
reorder anything. `windowId` lets a contemporaneous cohort be reassembled at analysis time without
re-deriving it from timestamps.

**Episodes with no momentum score are left unranked, not ranked last.** Ranking them last would
assert an ordering the data does not support — asserted by
`an_episode_without_a_momentum_score_is_left_unranked`.

---

## 8. Auto-trader linkage

`TraderLinkage { considered, skipReason, enteredAt, entryPrice, exitedAt, exitPrice, exitReason }`
per episode, attached via `EpisodeTracker::link_trader(symbol, closure)`.

Deliberately a closure over the linkage struct rather than a trader dependency: `backtest-metrics`
does not depend on `auto-trader`, and inverting that would couple measurement to the thing being
measured. The journal's existing `SkipReason` / `ExitReason` values are carried as strings, so the
trader's taxonomy can evolve without a lockstep schema change here.

This is what finally separates **detector quality** from **execution quality** — Milestone A found
that architecturally possible but empirically blocked. Two hypotheses it now makes testable:
whether `MomentumDeteriorated` exits are destroying good detector expectancy, and whether
`MaxConcurrentPositions = 4` or the signal is the binding constraint.

---

## 9. Discovery capture storage and capacity

**Analysis only — no code changed.** Grounded in the one real capture
(`docs/discovery-coverage-results-2026-09-07.md`) plus the Milestone A live rate.

| Quantity | Value | Basis |
|---|---|---|
| One snapshot census | 2,678,962 bytes / 12,628 symbols | **measured**, 2026-09-07 |
| Bytes per symbol-snapshot | ≈ 212 | derived |
| Snapshot cadence | 15s | `UNIVERSE_RESCAN_INTERVAL` |
| Regular session | 6.5h = 1,560 cadences | — |
| **Snapshots alone, one session** | **≈ 3.9 GB** | 212 × 12,628 × 1,560 |
| Plus coverage heartbeats / ignition / receipts | +10–20% | shape of the 67-record sample |
| **Realistic one session** | **≈ 4.3–4.7 GB** | matches the doc's ~4 GB estimate |
| **Five sessions** | **≈ 22–24 GB** | |

The repo's own ~4 GB/session estimate is **independently confirmed** by this derivation.

**Recommendation — gzip at the writer, not rotation.** JSONL of repetitive snapshot records
compresses roughly 8–12×, taking one session to **≈400–500 MB** and five sessions to ≈2.5 GB. That
is the difference between "needs a capacity plan" and "fits anywhere".

**Not implemented, deliberately.** The writer sits on the tick-dispatch path
(`discovery_audit.rs`: *"Never block market dispatch on disk I/O"*, 32-record `sync_channel`).
Introducing compression there changes flush timing and back-pressure characteristics of a component
that must not stall detection. That is a change to make with a measurement in hand, not speculatively
inside an instrumentation milestone whose entire premise is not perturbing the running system.
Rotation is already handled: per-process, per-UTC-day files with an 8 GiB cap.

**Realtime impact assessment:** the existing design is sound — bounded queue, dedicated writer
thread, dropped-record counter surfaced as `lost_records` so gaps invalidate rather than silently
corrupt completeness claims.

---

## 10. Export format

**Specified, not implemented** (the brief says define, and forbids automated upload).

```
stockspotter-capture-<session_date>/
  manifest.json
  episodes.ndjson.gz
  contexts.ndjson.gz
  horizons.ndjson.gz
  discovery-audit.ndjson.gz
  SHA256SUMS
```

`manifest.json` carries: `sessionDate`, `captureStartedAt`, `captureEndedAt`, per-file
`schemaVersion`, `recordCounts`, `droppedRecords` / `errorRecords`, `fileSizes`, per-file `sha256`,
and the producing `gitCommit`.

**Secret hygiene is structural, not procedural:** every record type in this milestone is built from
`ScanEvent` fields and derived measurements. None reads an environment variable, credential or
`.env` value, so there is no path by which a secret could enter an export. The manifest deliberately
records the git commit rather than the environment.

`SHA256SUMS` and the per-file manifest hashes mirror the provenance discipline
`analyze_discovery.py` already applies — it hashes both its inputs and its own source.

---

## 11. Backward compatibility

- Every new field is `Option` with `#[serde(default)]`, or a `Vec` defaulting to empty.
- `skip_serializing_if = "Option::is_none"` keeps records compact rather than littered with nulls.
- `schemaVersion` on `SignalContext`, `OpportunityEpisode` and `HorizonOutcome`, so a **meaning**
  change is detectable in already-written data — not merely inferred from field presence.
- `PendingSignal` and `SignalOutcome` are **completely untouched**; existing readers are unaffected.

**Tested against a representative legacy record**, and that test earned its place: my first fixture
used snake_case for `Strategy`, and the test failed — revealing that the real on-disk encoding is
**PascalCase** (`"IgnitionDetector"`), because `Strategy` carries no `rename_all`. The fixture now
matches reality, which is the only reason it proves anything.

---

## 12. Files changed

```
 M crates/backtest-metrics/src/lib.rs      (+16, module decls + re-exports only)
?? crates/backtest-metrics/src/context.rs  (new, ~640 lines incl. tests)
?? crates/backtest-metrics/src/episode.rs  (new, ~700 lines incl. tests)
?? crates/backtest-metrics/src/horizon.rs  (new, ~460 lines incl. tests)
```

No other tracked file was modified. `.claude/` and `AUDIT-2026-09-09.md` untouched.

---

## 13. Tests added (36 new)

Every requirement in the brief's §16 is covered by a named test:

| Requirement | Test |
|---|---|
| Signal-time causality | `a_snapshot_cannot_contain_information_observed_after_its_timestamp` |
| Momentum round-trip exact | `momentum_score_and_every_factor_round_trip_exactly` |
| Catalyst causality | `a_later_catalyst_is_never_attached_retroactively` |
| Episode: open | `the_first_qualifying_observation_opens_an_episode` |
| Episode: continue | `repeated_updates_stay_in_the_same_episode` (200 updates → 1 episode) |
| Episode: inactivity | `inactivity_closes_an_episode` |
| Episode: invalidation | `explicit_invalidation_closes_an_episode` |
| Episode: new move | `a_new_move_after_closure_creates_a_new_episode` |
| Episode: session boundary | `a_session_boundary_closes_an_episode` |
| **No lookahead** | `boundaries_never_depend_on_future_prices`, `future_prices_beyond_a_horizon_do_not_change_that_horizons_return` |
| Outcome horizons exact | `a_known_path_produces_exact_horizon_returns` |
| MFE / MAE / drawdown exact | `mfe_mae_and_drawdown_before_mfe_are_exact_on_a_known_path` |
| Time-to-target | `time_to_target_is_the_first_touch_at_or_after_the_threshold` |
| Censoring ≠ failure | `an_incomplete_window_censors_rather_than_reporting_a_miss`, `an_unreached_target_on_a_complete_window_is_a_real_miss_not_censoring` |
| Backward compatibility | `a_legacy_pending_signal_record_still_deserializes`, `a_context_missing_every_optional_group_deserializes` |
| Trader linkage | `trader_decisions_attach_to_the_intended_episode` |

Plus hardening tests: stale-feature exclusion, unobserved-symbol-yields-unknowns, bounded price
trail under 5,000 observations, session-vs-capture censoring distinction, mid-window data-gap
censoring, pre-signal-price exclusion, empty-path censoring, and three JSON round-trips.

**Two real bugs my own tests caught during implementation:**

1. **NaN poisoned serialization.** `MomentumUpdate` carries no price; an early revision used
   `f64::NAN`, which `serde_json` writes as `null` and then cannot read back — silently corrupting
   any capture containing a momentum-opened episode. Fixed by making price `Option<f64>` and falling
   back to the last observed price, declining to open rather than fabricating one. Guarded by
   `an_episode_opened_by_a_priceless_event_still_serializes` (which asserts the JSON contains no
   `null` at all) and `an_episode_is_declined_rather_than_opened_at_an_invented_price`.
2. **Inactivity pre-empted session boundary**, as described in §2.

---

## 14. Validation results

| Command | Result |
|---|---|
| `cargo test -p backtest-metrics` | **102 passed, 0 failed** (was 66) |
| `cargo test -p ws-server` | **51 passed, 0 failed** |
| `cargo test -p auto-trader` | passed, 0 failed |
| `cargo test --workspace --no-fail-fast` | **383 passed, 0 failed**, 24 binaries, **0 SIGKILLs** |
| `bun --cwd=apps/client run test` | **60 passed, 0 failed** |
| `tsc --noEmit -p apps/client/tsconfig.app.json` | **exit 0** |
| `tsc --noEmit -p apps/mobile/tsconfig.json` | **exit 0** |
| Python tests | **not run** — no Python file was changed |

Workspace total rose 347 → 383, matching the 36 tests added.

**One anomaly, resolved:** the first workspace run failed with
`failed to create dependency graph … dep-graph.part.bin (os error 2)` — a corrupted incremental
compilation cache, not a code fault. Re-running with `CARGO_INCREMENTAL=0` produced a clean
383/0. Reporting it rather than quietly re-running.

No repository-wide rustfmt churn was performed.

---

## 15. Remaining measurement limitations

**Honest inventory of what this milestone does *not* deliver:**

1. **Ignition's internal features remain unavailable.** The brief asked for lookback compression,
   trade-frequency acceleration, spread tightening and ask absorption. **`IgnitionEvent` carries only
   its phase** — those values live inside the detector and never reach the wire. Recording them
   requires emitting them, which is a `market-data` change this milestone's constraints forbid. I
   recorded phase and per-session counters, and left the rest genuinely absent rather than
   fabricating it.
2. **Consolidation's surge magnitude, volume ratio and consolidation length are likewise not on the
   wire.** Same cause, same treatment.
3. **Bid/ask/spread are unavailable live.** The `ScanEvent` stream carries trades and bars, not
   quotes. `quote_execution.rs` is the quote-aware path and works on backtests only. Recorded as
   `Option`, populated `None`.
4. **Catalyst publication time is plumbed but unpopulated** — see §4. A one-line `market-data` change
   unlocks it and is the **highest-value follow-up**.
5. **Relative volume is not directly on the wire**; the funnel emits `relVolOk` (the boolean), not the
   ratio. Segmentation by rel-vol therefore remains coarse.
6. **Nothing is wired into the live path yet.** `EpisodeTracker` and `FeatureCache` are complete and
   tested, but no producer calls them. Wiring them into `ws-server`'s existing subscriber task is a
   small, additive change — deliberately deferred so this milestone remains provably observational.
7. **Still no data.** This builds the instrument. It does not fill it.

---

## 16. `git diff --stat`

```
 crates/backtest-metrics/src/lib.rs | 16 ++++++++++++++++
 1 file changed, 16 insertions(+)
```

## 17. `git status --short`

```
 M crates/backtest-metrics/src/lib.rs
?? .claude/
?? AUDIT-2026-09-09.md
?? crates/backtest-metrics/src/context.rs
?? crates/backtest-metrics/src/episode.rs
?? crates/backtest-metrics/src/horizon.rs
```

---

## 18. Recommendation

**Ready to commit**, with two caveats worth recording rather than blocking on.

The measurement engine is complete, tested against synthetic paths with exactly-known answers, and
provably observational — the 16-line `lib.rs` diff is the whole footprint on existing code.

**Caveat 1 — it is not yet connected.** No producer calls `EpisodeTracker`. Committing this is
committing a tested library, not a running capture. That is the right shape for review, but it means
the milestone's value is not realized until a follow-up wires it into `ws-server`'s subscriber task.

**Caveat 2 — three feature groups are structurally unavailable** (§15 items 1–3). The ignition and
consolidation internals the brief asked for cannot be recorded without emitting them from
`market-data`. If detector-interaction analysis is the priority, that emission change should be its
own small, carefully-reviewed milestone — it touches the strategy path, which everything here
deliberately avoided.

**Suggested sequence:** commit this → one-line catalyst `published_at` change → wire the tracker into
`ws-server` → capture one session → *then* the ignition/consolidation feature emission, informed by
what the first capture shows is actually missing.

---

### Constraints observed

No thresholds tuned · no detector logic changed · no auto-trader entry/exit policy changed · no
strategy enablement, sizing, stops, targets, timeouts or position limits touched · raw event stream
preserved untouched · nothing committed, pushed or deployed · no SSH or production access · no
dependency changes.
