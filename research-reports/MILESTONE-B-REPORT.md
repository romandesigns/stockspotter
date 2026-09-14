# ALPHA MILESTONE B — MAKE STOCKSPOTTER MEASURABLE

**Date:** 2026-09-09
**Branch:** `gpt/audit-remediation-20260909` @ `0544790`
**Status:** implemented and validated — **not committed, not pushed, not deployed**

**Invariant held, and proven rather than asserted.** No detector, scorer or auto-trader file was
touched at all. The two `market-data` files that changed contain **pure additions with zero
deletions**, all of it catalyst timestamp plumbing — every changed line is quoted in §13. No
threshold, gate, stop, target, timeout, sizing or concurrency constant appears anywhere in the diff.

---

## 1. Architecture implemented

```
crates/backtest-metrics/
  context.rs   SignalContext + FeatureCache      (new)
  episode.rs   OpportunityEpisode + Tracker      (new)
  horizon.rs   HorizonOutcome + censoring        (new)
  lib.rs       re-exports only                   (+16)

crates/market-data/
  events.rs    CatalystUpdate gains one optional field  (+49, incl. 2 compat tests)
  live.rs      publication time threaded, not dropped   (+16, zero deletions)

packages/shared-types/
  index.ts     CatalystUpdate gains one optional field  (+15)

python/
  export_session.py       portable research export      (new)
  test_export_session.py  6 tests                       (new)
```

**Why `backtest-metrics`:** `signals.rs`, `live_signals.rs` and `outcome.rs` already live there, and
the deliberate live/backtest definitional parity is maintained inside that crate. A separate crate
would have split that contract across a boundary.

**Why the tracker is an owned value, not a global:** a replay can build episodes from historical
events through the identical code path live capture uses — the same parity principle
`live_signals.rs` already follows.

**Why compression lives in the exporter, not the capture writer:** see §10. This decision changed
after measurement.

---

## 2. Signal-time schema

`SignalContext`, `schemaVersion: 1`, camelCase per project convention. Every feature group is
`Option` — **absent means unknown, never zero**. A universe-tier ignition candidate may genuinely
never have been momentum-scored; a fabricated `0.0` would be indistinguishable from a real one.

| Group | Captured |
|---|---|
| `market` | price, session volume, gap%, minute-of-day; bid/ask/spread `Option` (§16) |
| `funnel` | price, gap%, session volume, **all four gate booleans** — recorded on rejection too, since near-misses are the recall sample |
| `ignition` | phase, plus per-session counters: candidates opened, confirmations, rejections |
| `momentum` | **`overall` plus all four factors**, and the 0.60 verdict *as production computed it* |
| `consolidation` | strategy variant, phase, surge/confirmation/entry counters |
| `halt` | level, proximity, band width, band-doubled, rel-vol, LULD-in-effect, **estimated-vs-official bands** |
| `catalyst` | tags, count, `observedAt`, `mostRecentPublishedAt`, `headlineAgeSecs` |
| `preDetection` | 1m/3m/5m anchors, session low, first observed price, `moveBeforeDetectionPct` |

**The momentum group is the point.** Production computes `overall`, reduces it to a boolean at 0.60,
and discards the ordering — exactly why Milestone A found ranking unmeasurable. Persisting the
continuous value costs nothing and changes no gate. `qualifies` is **stored, not recomputed**, so a
future threshold change cannot retroactively alter what old records claim production decided.

**Freshness:** a group older than 120s is recorded **absent** rather than stale. A two-minute-old
momentum reading attached to a fresh ignition would silently corrupt any interaction study.

---

## 3. Episode boundary definition

**Frozen before any outcome was examined.** Each rule mirrors a mechanism the system already relies
on rather than being invented here:

| # | Rule | Trigger | Existing precedent |
|---|---|---|---|
| 1 | **Open** | first edge-triggered *qualifying* observation in a session | `signals.rs` / `live_signals.rs` edge-trigger definitions exactly |
| 2 | **Continue** | any further observation of that symbol | — |
| 3 | **Close: inactivity** | 300s without observation | `UNIVERSE_MONITOR_IDLE_SECS`, `QUIET_TRANSITION_GRACE` — both already 300s |
| 4 | **Close: invalidated** | `IgnitionEventKind::FollowThroughRejected` | the detector declaring the move over |
| 5 | **Close: session boundary** | UTC date change | `AlreadyEnteredToday`; discovery protocol's per-session labelling |
| — | **Close: capture ended** | `finish()` | censored, not concluded |

Diagnostic-only phases (`SurgeDetected`, `ConsolidationConfirmed`, `CandidateOpened`) **do not open**
an episode — matching `signals.rs`, which already refuses to count them as signals.

**Identity:** `symbol + session_date + sequence` (1-based). A second distinct move the same day is
sequence 2, never a continuation.

**Precedence found during implementation:** session boundary must be evaluated **before** inactivity,
because an overnight gap always exceeds 300s — otherwise every cross-session closure would be
mislabelled `Inactivity`, and the two mean different things. My own test caught this.

**No rule consults future information.** `boundaries_never_depend_on_future_prices` asserts it
directly: identical event sequences differing only in what happens *after* the opening moment
(+1% vs +890%) must produce identical boundaries, identical IDs and byte-identical opening context.

---

## 4. Catalyst timestamp semantics — resolved, **and the flagged risk was real**

Traced end to end, then fixed:

1. `python/app/news.py` fetches Alpaca `/v1beta1/news`.
2. `python/app/main.py` returns `most_recent_published_at = news_items[0]["created_at"]` — **genuine
   provider publication time**.
3. `crates/market-data/src/qualify.rs` **does** carry it into Rust.
4. `live.rs` built `CatalystRecord { timestamp: Utc::now(), … }` — **observation time** — and
   **silently discarded the publication time**.

So the three timestamps the brief asked me to separate are:

| Timestamp | Source | Now recorded as |
|---|---|---|
| **Publication** | provider `created_at` | `mostRecentPublishedAt` (was discarded) |
| **Provider/fetch** | — | not separately exposed by the provider |
| **Observed by Stockspotter** | `Utc::now()` at qualify response | `observedAt` |

**Causality is judged against `observedAt`, never publication.** We cannot have known a headline
before fetching it, so using observation time can never leak backward. Attachment is gated on
`observed_at <= detected_at`, asserted by `a_later_catalyst_is_never_attached_retroactively`.

**Third finding, new:** `fetch_recent_news` requests `limit=10` with **no time window**. A catalyst
tag therefore carries **no recency guarantee** — `headlineCount: 10` could span weeks.
`headlineAgeSecs` makes that visible instead of every analysis silently assuming freshness. Asserted
by `catalyst_recency_is_recorded_separately_from_observation_time`, which checks a day-old headline
reports 86,400 seconds.

**This is the one strategy-path change**, and it is observational: two `Option` fields and a parse.
Zero deletions.

---

## 5. Outcome schema

`HorizonOutcome`, `schemaVersion: 1`. **Purely additive** — `outcome.rs`'s target/stop model is
untouched and still authoritative.

- **Forward returns** at 30s / 1m / 3m / 5m / 10m / 15m / 30m
- **Excursion**: `mfePct`, **`maePct`** (absent from the codebase entirely before), `secondsToMfe`,
  `secondsToMae`, **`drawdownBeforeMfePct`** — the heat endured to capture the favorable move
- **Time-to-target** for +2% / +5% / +10%
- `observedSpanSecs`, `observationCount` — partial coverage explicit, not implicit

**Sampling rule, stated once and applied uniformly:** for horizon *h*, the sampled price is the
**first observation at or after `signal_at + h`**. Never nearest, never interpolated, never the last
before — interpolation would use a price unavailable in real execution, which the brief explicitly
prohibits. No such observation ⇒ censored.

Observations before `signal_at` are filtered out, so a pre-signal dip cannot become MAE.

---

## 6. Censoring rules

`Observation<T>` is `Observed(T)` or `Censored(CensorReason)`:

| Reason | Meaning | Would more collection help? |
|---|---|---|
| `SessionEnded` | horizon fell past session end | **No** |
| `CaptureEnded` | observation stopped for our reasons | Yes |
| `InsufficientForwardData` | path does not extend that far | Yes |
| `DataGap` | mid-window gap > 120s; extremes inside unknown | Yes |

**"Did not reach target" and "we stopped observing" are now different values.** Unreached on a
*fully observed* longest window ⇒ `Observed(None)` — a real miss. Unreached on a short window ⇒
`Censored(…)` — unknown. Two tests assert exactly that pair.

`EpisodeCloseReason::CaptureEnded` carries the same meaning at episode level.

**Aggregation contract is type-enforced:** `Observation::observed()` returns `Option<T>`, so a caller
cannot silently coerce censored into zero.

---

## 7. Earliness / pre-detection design

Bounded anchors, not tick history: 1m / 3m / 5m before detection, session low observed, first
observed price, and `moveBeforeDetectionPct`.

**A missing anchor is `None`, never substituted.** A symbol observed for ten seconds genuinely has no
one-minute-ago price; substituting the oldest available would understate how much move preceded
detection — precisely the number this exists to measure. Asserted by
`a_missing_anchor_is_absent_rather_than_substituted`.

The price trail is bounded two ways (512 entries, 400s age), verified under 5,000 observations.

This answers the brief's question directly: a +20% runner first detected after +17% will show
`moveBeforeDetectionPct ≈ 17`, against subsequent MFE.

---

## 8. Research ranking design

`ResearchRank { rank, cohortSize, score, rankedAt, windowId }` per episode, ordered by the most
recent `momentum.overall` — the score production already computes and discards.

**Isolation:** computed *after* episodes are built, writes to a field nothing else reads, never
emitted to clients, never consulted by the auto-trader, cannot suppress or reorder anything.
`windowId` lets a contemporaneous cohort be reassembled without re-deriving it from timestamps.

**Episodes with no momentum score are left unranked, not ranked last** — ranking them last would
assert an ordering the data does not support.

Tests the single hypothesis: *does the continuous momentum score already rank future opportunity
quality better than the remainder?*

---

## 9. Auto-Trader linkage

`TraderLinkage { considered, skipReason, enteredAt, entryPrice, exitedAt, exitPrice, exitReason }`,
attached via `link_trader(symbol, closure)`.

Deliberately a closure rather than a dependency: `backtest-metrics` does not depend on `auto-trader`,
and inverting that would couple measurement to the thing measured. The journal's `SkipReason` /
`ExitReason` values travel as strings, so the trader's taxonomy can evolve without a lockstep schema
change.

This separates **detector expectancy** from **execution expectancy**. Two hypotheses it makes
testable: whether `MomentumDeteriorated` exits destroy good detector expectancy, and whether
`MaxConcurrentPositions = 4` or the signal is the binding constraint.

---

## 10. Discovery capture capacity — **measured, and it changed my recommendation**

Milestone A projected ~4 GB/session from the real 2026-09-07 census. I re-derived it and then
measured compression on realistic capture shape (12,628 snapshot records with true field structure):

| Quantity | Value |
|---|---|
| Measured bytes/symbol-snapshot | **258** |
| Snapshot cadence | 15s (`UNIVERSE_RESCAN_INTERVAL`) |
| Cadences per regular session | 1,560 |
| **Raw, one session** | **5.07 GB** |
| **Raw, five sessions** | **25.4 GB** |
| **Measured gzip ratio** | **31.5×** |
| **Compressed, one session** | **0.16 GB** |
| **Compressed, five sessions** | **0.81 GB** |

31.5× is far better than the 8–12× I assumed before measuring — JSONL of near-identical snapshot
records is extremely redundant.

**Decision: compress at export, not at capture.** Reasons, in order:

1. The capture writer lives in `market-data`, a **strategy-path crate**. No gzip crate exists in the
   dependency tree, so this would mean adding `flate2` there — new code between tick dispatch and
   disk, for a component whose own header says *"Never block market dispatch on disk I/O"*.
2. **Compression at export achieves the same 31.5×** on the artifact that actually gets copied. The
   only cost is transient disk during the session.
3. 5 GB of transient disk for one session is unremarkable; 25 GB for five is manageable and only
   arises if five sessions are captured before any export.
4. **Rotation already exists** — per-process, per-UTC-day files with an 8 GiB cap.

**Write-amplification/performance assessment:** the existing design is sound — bounded 32-record
`sync_channel`, dedicated writer thread off the dispatch path, dropped-record counter surfaced as
`lost_records` so gaps invalidate rather than silently corrupt completeness claims.

**Nothing in the capture path was changed.**

---

## 11. Research export format

`python/export_session.py` — implemented and tested. Read-only: no network, no strategy state, no
orders.

```
session-YYYY-MM-DD/
  manifest.json
  <name>.ndjson.gz     …grouped: discovery / signals / episodes / outcomes / trader / other
  SHA256SUMS
```

`manifest.json` carries: `sessionDate`, `captureStartedAt/EndedAt`, `exportedAt`, `gitCommit`,
`schemaVersion`, and per file — group, record count, **errorRecords**, **partialLines**, source and
compressed bytes, and **both** source and output SHA-256.

Design decisions worth stating:

- **Counts come from the bytes actually written**, in the same pass, so a manifest count can never
  disagree with the file it describes.
- **A malformed line is counted and still exported.** Dropping it would create a silent gap — exactly
  what the capture format's own `lost_records` accounting exists to prevent.
- **Session date comes from the data, never from `today`.** An export run days later must not
  relabel the session. Asserted by test.
- **Unrecognised files are exported as `other`**, never silently skipped.
- **No secrets by construction:** every record type is derived from market events; nothing reads an
  environment variable or credential. The manifest records the git commit, not the environment.
- Python, not Rust: `hashlib` and `gzip` are stdlib, so this adds **no dependency anywhere**, and it
  matches the provenance discipline `analyze_discovery.py` already established (it hashes its inputs
  *and itself*; so does this).

---

## 12. Backward compatibility

- Every new field is `Option` with `#[serde(default)]`; `skip_serializing_if` keeps records compact
  rather than littered with nulls.
- `schemaVersion` on `SignalContext`, `OpportunityEpisode`, `HorizonOutcome` and the export manifest,
  so a **meaning** change is detectable in written data — not merely inferred from field presence.
- `PendingSignal` and `SignalOutcome` are **untouched**.
- The TypeScript `mostRecentPublishedAt` is optional, so existing clients and older servers are
  unaffected in both directions.

**Tested against representative legacy records**, and one of those tests earned its keep: my first
fixture used snake_case for `Strategy` and failed, revealing the real on-disk encoding is
**PascalCase** (`"IgnitionDetector"`) because `Strategy` carries no `rename_all`. A fixture that
matches reality is the only kind that proves anything.

`a_catalyst_record_without_a_publication_time_still_parses` covers every catalyst record already on
the VPS.

---

## 13. Files changed

```
 M crates/backtest-metrics/src/lib.rs           +16   (re-exports only)
 M crates/backtest-metrics/src/live_signals.rs   +1/-1 (test fixture field)
 M crates/market-data/src/events.rs             +49   (1 optional field + 2 compat tests)
 M crates/market-data/src/live.rs               +16   (catalyst plumbing, ZERO deletions)
 M packages/shared-types/src/index.ts           +15   (1 optional field + docs)
?? crates/backtest-metrics/src/context.rs       new
?? crates/backtest-metrics/src/episode.rs       new
?? crates/backtest-metrics/src/horizon.rs       new
?? python/export_session.py                     new
?? python/test_export_session.py                new
```

**Every changed line in `live.rs`** — the only strategy-path file with logic in it:

```rust
+ /// Observation time -- when this process received the qualify response.
+ /// Provider publication time of the newest headline, when the qualitative
+ /// layer supplied one. Carried through rather than dropped so a catalyst
+ /// tag's recency is knowable; see `ScanEvent::CatalystUpdate`.
+ #[serde(default, skip_serializing_if = "Option::is_none")]
+ pub most_recent_published_at: Option<DateTime<Utc>>,
+ // `most_recent_published_at` arrives as a provider string;
+ // an unparseable value becomes `None` rather than failing
+ // the record or guessing a time.
+ let published_at = q
+     .most_recent_published_at
+     .as_deref()
+     .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
+     .map(|dt| dt.with_timezone(&Utc));
+         most_recent_published_at: published_at,
+         most_recent_published_at: record.most_recent_published_at,
```

Pure additions. No detector crate (`fast-funnel`, `ignition-detector`, `momentum-scorer`,
`consolidation-breakout`, `halt-detector`, `auto-trader`) was touched at all.

---

## 14. Tests added (44 new)

Every requirement in the brief's testing section maps to a named test:

| Requirement | Test |
|---|---|
| Causality | `a_snapshot_cannot_contain_information_observed_after_its_timestamp` |
| Momentum exact round-trip | `momentum_score_and_every_factor_round_trip_exactly` |
| Catalyst not retroactive | `a_later_catalyst_is_never_attached_retroactively` |
| Catalyst recency | `catalyst_recency_is_recorded_separately_from_observation_time` |
| Episode: open | `the_first_qualifying_observation_opens_an_episode` |
| Episode: continue | `repeated_updates_stay_in_the_same_episode` (200 updates → 1 episode) |
| Episode: inactivity | `inactivity_closes_an_episode` |
| Episode: invalidation | `explicit_invalidation_closes_an_episode` |
| Episode: session boundary | `a_session_boundary_closes_an_episode` |
| Episode: new move | `a_new_move_after_closure_creates_a_new_episode` |
| **No lookahead** | `boundaries_never_depend_on_future_prices`, `future_prices_beyond_a_horizon_do_not_change_that_horizons_return` |
| Horizon returns exact | `a_known_path_produces_exact_horizon_returns` |
| MFE/MAE/drawdown exact | `mfe_mae_and_drawdown_before_mfe_are_exact_on_a_known_path` |
| Target timing | `time_to_target_is_the_first_touch_at_or_after_the_threshold` |
| Censoring ≠ failure | `an_incomplete_window_censors_rather_than_reporting_a_miss`, `an_unreached_target_on_a_complete_window_is_a_real_miss_not_censoring` |
| Legacy compatibility | `a_legacy_pending_signal_record_still_deserializes`, `a_context_missing_every_optional_group_deserializes`, `a_catalyst_record_without_a_publication_time_still_parses` |
| Auto-Trader linkage | `trader_decisions_attach_to_the_intended_episode` |
| Export integrity | 6 tests incl. round-trip, honest counts, malformed-line retention, date-from-data, credential-shaped-value scan, measured compression |

**Three real bugs my own tests caught during implementation:**

1. **NaN poisoned serialization.** `MomentumUpdate` carries no price; using `f64::NAN` produced
   episodes whose `openingPrice` serialized as `null` and could not be read back — silently
   corrupting any capture containing one. Fixed to `Option<f64>` with fallback to last observed
   price, declining to open rather than fabricating. Guarded by a test asserting the JSON contains
   **no `null` at all**.
2. **Inactivity pre-empted session boundary** (§3).
3. **My own secret-scan test was naive** — it matched the word "secret" in the manifest's own
   *limitations* prose. Rewritten to scan for credential-shaped **values** (token prefixes, key
   patterns) rather than English words, which is both correct and actually capable of catching a
   real leak.

---

## 15. Validation results

| Command | Result |
|---|---|
| `cargo test -p backtest-metrics` | **103 passed, 0 failed** |
| `cargo test -p ws-server` | **51 passed, 0 failed** |
| `cargo test -p auto-trader` | **45 passed, 0 failed** |
| `cargo test -p market-data` | **76 passed, 0 failed** |
| `cargo test --workspace --no-fail-fast` | **386 passed, 0 failed**, 24 binaries, **0 SIGKILLs, 0 errors** |
| `bun --cwd=apps/client run test` | **60 passed, 0 failed** |
| `tsc --noEmit -p apps/client/tsconfig.app.json` | **exit 0** |
| `tsc --noEmit -p apps/mobile/tsconfig.json` | **exit 0** |
| `python -m unittest discover -s python -p 'test_discovery*.py'` | **10 passed** |
| `python -m unittest python.test_export_session` | **6 passed** |
| `python -m pytest -q tests` | **could not run — `No module named pytest`** |

Workspace rose 347 → 386, matching the Rust tests added.

**Two environmental anomalies, reported rather than worked around:**

- **pytest is not installed locally**, so `python/tests` could not run. CI installs
  `python/requirements.txt` and does run it. I did not install a toolchain to work around this.
- An earlier workspace run failed with `failed to create dependency graph … dep-graph.part.bin` — a
  corrupted incremental-compilation cache, not a code fault. `CARGO_INCREMENTAL=0` produced clean
  runs throughout.

No repository-wide rustfmt churn.

---

## 16. Remaining blockers to a scientifically useful session capture

Ordered by how much each limits the next session's value:

1. **Nothing is wired into the live path.** `EpisodeTracker` and `FeatureCache` are complete and
   tested, but **no producer calls them**. This is the single blocker between "tested library" and
   "the next session produces episodes". Wiring it into `ws-server`'s existing subscriber task —
   alongside `LiveSignalTracker`, which already does exactly this shape of work — is a small
   additive change, deliberately deferred so this milestone stayed provably observational.
2. **Ignition and consolidation internals are not on the wire.** The brief asked for compression,
   trade-frequency acceleration, spread tightening, ask absorption, surge magnitude, volume ratio and
   consolidation length. **`IgnitionEvent` and `ConsolidationEvent` carry only phase and price** —
   those values live inside the detectors and are never emitted. Capturing them requires emitting
   them, which touches detector call sites. I recorded phase and per-session counters and left the
   rest **genuinely absent rather than fabricated**.
3. **Bid/ask/spread are unavailable live.** The `ScanEvent` stream carries trades and bars, not
   quotes. `quote_execution.rs` is the quote-aware path and works on backtests only.
4. **Relative volume is not on the wire as a value** — the funnel emits `relVolOk`, the boolean, not
   the ratio. Segmentation by rel-vol will be coarse.
5. **Discovery capture must actually be enabled and run.** `DISCOVERY_AUDIT_DIR` is set in the VPS
   `.env`, but no session has been recorded. Plan ~5 GB transient disk (§10).
6. **Still no data.** This milestone builds the instrument; it does not fill it.

Items 1 and 5 are the two that must happen before the next session for it to be useful. Items 2–4
degrade the analysis but do not prevent it.

---

## 17. `git diff --stat`

```
 crates/backtest-metrics/src/lib.rs          | 16 ++++++++++
 crates/backtest-metrics/src/live_signals.rs |  2 +-
 crates/market-data/src/events.rs            | 49 +++++++++++++++++++++++++++++
 crates/market-data/src/live.rs              | 16 ++++++++++
 packages/shared-types/src/index.ts          | 15 +++++++++
 5 files changed, 97 insertions(+), 1 deletion(-)
```

## 18. `git status --short`

```
 M crates/backtest-metrics/src/lib.rs
 M crates/backtest-metrics/src/live_signals.rs
 M crates/market-data/src/events.rs
 M crates/market-data/src/live.rs
 M packages/shared-types/src/index.ts
?? .claude/
?? AUDIT-2026-09-09.md
?? crates/backtest-metrics/src/context.rs
?? crates/backtest-metrics/src/episode.rs
?? crates/backtest-metrics/src/horizon.rs
?? python/export_session.py
?? python/test_export_session.py
```

---

## 19. Recommendation

**Ready to commit**, with the wiring gap recorded rather than hidden.

Against the brief's own success criterion — *"starting with the next captured market session,
Stockspotter will preserve enough causal information to measure signal quality, earliness, detector
interactions, candidate ranking, missed opportunities, and Auto-Trader execution separately"* — the
honest assessment is:

| Capability | Status |
|---|---|
| Signal quality | schema complete, needs wiring |
| Earliness | complete, needs wiring |
| Detector interaction | **partial** — momentum/funnel/halt/catalyst yes; ignition and consolidation internals not on the wire |
| Candidate ranking | complete, needs wiring |
| Missed opportunities | discovery recorder already existed; needs a session run |
| Auto-Trader separation | structure complete, needs wiring |

**So the criterion is not yet met, and I would not claim it is.** One additive change —
wiring `EpisodeTracker` into `ws-server`'s subscriber task — moves five of those six rows to
"complete", and it is deliberately not in this milestone because doing it here would have meant
touching the live path in the same change that defines the measurement contract.

**Suggested sequence:** commit this → wire the tracker into `ws-server` (small, additive, its own
review) → enable discovery capture and run one session → *then* the ignition/consolidation feature
emission, informed by what the first capture shows is actually missing rather than by speculation.

---

### Constraints observed

No thresholds tuned · no detector logic changed · no auto-trader entry/exit/sizing/limits changed ·
no strategy enablement changed · production event ordering and filtering untouched · raw event stream
preserved · nothing committed, pushed or deployed · no SSH or production access · no new
dependencies in any language.
