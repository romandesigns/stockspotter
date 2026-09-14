# ALPHA MEASUREMENT REPAIR — REPORT

**Baseline strategy commit:** `ba722698af4fa2b387339eb017a2d58734f969c7`
**Scope:** R1–R5 plus the six inverted episodes. **No strategy behaviour changed.**
**Not deployed, not pushed, not committed. No SSH or production access occurred.**

---

## 1. Root cause of F1 — the 1800-second settlement collision

Two constants had to agree, were declared independently, and silently stopped agreeing.

`crates/ws-server/src/measurement.rs:56` declared `OUTCOME_WINDOW_SECS = 1800`.
`crates/backtest-metrics/src/horizon.rs:33` declared `HORIZON_SECS = [.., 1800]`.

`settle_due` finalised a pending episode when `now - opened_at >= OUTCOME_WINDOW_SECS`, while
`sample_at` requires the **first observation at or after `signal_at + h`**, with `signal_at ==
opened_at`. For `h = 1800` the required observation and the settlement instant are the *same
moment*. Forward collection stopped exactly where the longest horizon began.

The only way it could ever be observed was a race inside a single `observe()` call: `extend_paths`
runs before `settle_due`, so a price arriving in that same call could land before finalisation. That
is precisely the shape of the data — **76 of 135,716 (0.056%) observed at 1800s, against 51.48% at
900s.** A 900× drop across one grid step is a boundary artifact, not decay.

Worth naming plainly: the surviving 76 are not a small sample of the same population. They are the
subset that won a race, so they are selected on exactly the property being measured. They should be
discarded, not modelled.

---

## 2. Exact R1 implementation

The duplicated relationship is gone. There is now one source of truth — the grid — and the deadline
is a compile-time consequence of it.

`crates/backtest-metrics/src/horizon.rs`:

```rust
pub const fn longest_horizon_secs() -> i64 { /* max over HORIZON_SECS */ }

pub const OBSERVATION_MARGIN_SECS: i64 = 120;

pub const SETTLE_AFTER_SECS: i64 = longest_horizon_secs() + OBSERVATION_MARGIN_SECS;

const _: () = {
    let mut index = 0;
    while index < HORIZON_SECS.len() {
        assert!(
            HORIZON_SECS[index] < SETTLE_AFTER_SECS,
            "every horizon must be strictly shorter than SETTLE_AFTER_SECS; \
             a horizon at the settlement boundary is unobservable (see F1)"
        );
        index += 1;
    }
};
```

`crates/ws-server/src/measurement.rs` no longer declares a deadline at all:

```rust
use backtest_metrics::horizon::SETTLE_AFTER_SECS as OUTCOME_WINDOW_SECS;
```

**Margin choice.** 120s, matching `MAX_GAP_SECS`. The reasoning is not "round number": a path dense
enough to avoid `DataGap` censoring at all is by definition dense enough to place an observation
inside a 120s window. Retention grows 1800 → 1920s, **6.7%**, which is the minimum that makes the
final horizon reachable rather than an arbitrary enlargement.

**The required invariant is enforced at compile time, and I verified it fires** rather than assuming
it. Temporarily setting the margin to `0` (making `SETTLE_AFTER_SECS == 1800 == max(HORIZON_SECS)`)
fails the build:

```
error[E0080]: evaluation panicked: every horizon must be strictly shorter than
SETTLE_AFTER_SECS; a horizon at the settlement boundary is unobservable (see F1)
```

The probe was reverted and the suite re-confirmed green.

---

## 3. Root cause of F4 — forward-price causality

`observed_price` mapped `ScanEvent::BarUpdate` to `(timestamp, close)`. For bar events `timestamp`
is the bar's **opening** instant while `close` is a fact about the bar's **end**, so a price was
recorded up to one full interval before it existed. Two distinct failures followed:

1. **Final bars.** A 60s bar opening at *t* had its close stamped at *t*, asserting knowledge 60
   seconds early. Meanwhile the bar-derived *detection* events (`FunnelSignal`, `MomentumUpdate`,
   `ConsolidationEvent`) are stamped `bar.timestamp + 1 minute` by `live.rs` — bar close. The path
   and the detections were on **two different conventions for the same bar.**
2. **In-progress buckets.** `live.rs` broadcasts a live bucket every 500ms with `timestamp =
   bucket_start`. Up to ~120 updates per minute, each carrying a different price, **all stamped with
   one identical artificial instant.**

Where this actually corrupts results is *attribution*, and it is worth being precise rather than
overstating it. The sampling rule is "first observation at or after `signal_at + h`", so a sparse
path legitimately answers a 30s horizon with a later price — coarse, but causal, and predating this
repair. What F4 broke is that a price which only existed at *t+60* was presented **as the price at
*t***, which directly falsifies excursion timing (`secondsToMfe`, `secondsToMae`), can admit a
price into MFE/MAE that was not yet observable, and can exclude legitimate forward data when
`take_path` filters on `t > opened_at`.

I initially wrote a test asserting a 30s horizon must be censored in this scenario. **It failed, and
it deserved to** — it asserted something the sampling rule never claimed. The corrected test pins
attribution, which is the real invariant.

---

## 4. The causal timestamp model after R2

Every path point is now `(the instant the price became observable to Stockspotter, price)`.

| Event | Price | Observable at | Clock |
|---|---|---|---|
| `IgnitionEvent` | trade price | `timestamp` | exchange |
| `ConsolidationEvent` | bar close | `timestamp` | exchange (already bar-close) |
| `FunnelSignal` | snapshot price | `timestamp` | exchange (already bar-close) |
| `HaltWarning` | `current_price` | `timestamp` | exchange |
| `BarUpdate { is_final: true }` | bar close | **`timestamp + interval_secs`** | exchange |
| `BarUpdate { is_final: false }` | running close | **`received_at`** | receipt |

**Which clock, and why.** Market time is preferred wherever it is semantically correct — moving
trade-stamped prices to a receipt clock would discard real precision. The receipt clock is used in
exactly one case: the in-progress bucket, where the only market timestamp available
(`bucket_start`) is both early and non-unique. `received_at` is the same `now` that already drives
episode lifecycle, so no new clock was introduced.

**Trade observations are already preferred.** `IgnitionEvent` and `HaltWarning` stream per trade and
supply the large majority of path points; the bar cases are corrected rather than removed, because
removing them would lose coverage on symbols whose only prints arrive as bars. **No duplicate
observations were added** — the set of contributing events is unchanged, only their timestamps.

**One consequence handled explicitly.** A path now carries two clocks, so arrival order no longer
implies time order. `evaluate_horizons` sorts chronologically before sampling, keeping the "first at
or after" rule sound at the single place it is applied rather than trusting every producer.

**Strategy inputs are untouched.** `ScanEvent`, the wire format, event ordering and `live.rs` are
unchanged; the correction lives entirely in the measurement consumer.

---

## 5. R3 — exporter changes

`python/export_session.py`:

- **Directory inputs match both suffixes.** `CAPTURE_SUFFIXES = ("*.jsonl", "*.ndjson")`, applied in
  a new `collect_inputs()`. The discovery recorder writes `.jsonl`; the measurement collector writes
  `.ndjson`. Matching one is what let a directory export drop 135,716 episodes at exit code 0.
- **`--expect GROUP`**, repeatable, validated against `classify()`. A missing expected dataset exits
  non-zero with both what was demanded and what was found.
- **Explicit-file input preserved** — a named file bypasses the glob regardless of extension.
- **SHA-256 provenance preserved** — `sha256` and `sourceSha256` unchanged in meaning.
- `alpaca_paper_ledger` added to the `trader` group (it previously classified as `other`).

**Verified against the real artifact.** Pointed at Session 001's `raw/` directory, the repaired
`collect_inputs` now returns both files (`episodes-2026-09-10.ndjson → episodes`,
`alpaca_paper_ledger.jsonl → trader`), and `--expect discovery` correctly evaluates False — which
would have caught Session 001's missing discovery data *at export time*. Read-only; no export was
written into the immutable session.

---

## 6. R4 — manifest schema and semantics

`schemaVersion` **1 → 2**. `captureStartedAt`/`captureEndedAt` are **removed**, not renamed — they
were filesystem mtimes labelled as capture, and on Session 001 they described a 2-minute *transfer*
window for a ~22-hour dataset.

| Field | Meaning |
|---|---|
| `exportedAt` | when the export ran (unchanged, already correct) |
| `sourceFileModifiedRange` | filesystem mtime range — metadata only, explicitly not the capture |
| `eventTimeRange` | derived from record event times (`openedAt`/`closedAt`) |
| `captureTimeRange` | derived from receipt times (`openingContext.capturedAt`) |
| `timestampSemantics` | prose definition of all four, **embedded in every manifest** |
| `groupsPresent` / `expected` | what was found and what was demanded |

Ranges are also emitted **per file**, not only in aggregate. A dataset lacking the necessary
semantic timestamp yields `null` — never a substituted clock. `captureTimeRange` is real for this
corpus: `openingContext.capturedAt` is present on 100% of Session 001's episodes.

---

## 7. R5 — discovery storage architecture

`crates/market-data/src/discovery_audit.rs`, rewritten around a `Writer` with explicit state.

**Rotation, not stop.** Per-file budget (default 1 GiB) rotates to `<day>-<run>-<seq>.jsonl`.
Reaching a file limit is never a reason to stop recording — the previous writer's defining flaw.

**Bounded retention.** Directory ceiling (default 32 GiB, `DISCOVERY_AUDIT_MAX_BYTES`) enforced by
deleting oldest-first. `reclaimable_segments` structurally excludes the segment being written, so
the current file can never be deleted. Each removal emits `capture_retention_removed`.

**Session reservation.** `applicable_budget()` reads `classify_session` from the record's own clock:
Premarket and Overnight may reach only `PRE_SESSION_BUDGET_FRACTION` (0.5) of the daily allowance;
Regular and AfterHours may use all of it. **A premarket flood can consume at most half the day**, so
the session the data exists to describe always has budget.

**Degrade before dropping.** At 80% of the applicable budget, the high-volume/low-value classes
(`coverage`, `snapshot_batch`, `snapshot_complete`) are downsampled 1-in-10. `CRITICAL_KINDS`
(`ignition`, `scan_completed`, `scan_started`, `stream_started`) and all `capture_*` markers are
**never** downsampled — the markers describing loss must outrank the loss.

**In-band markers.** `capture_degraded_start` / `capture_degraded_end` / `capture_drop_start` /
`capture_drop_end` / `capture_rotated` / `capture_retention_removed`, carrying class, counts, sample
rate, and budget state. **Completeness is now determinable from the dataset alone**, which was the
point: Session 001's loss was visible only in container logs.

Record schema `1 → 2`; `emit` now also carries `sampled_out` alongside `lost_records`, so intentional
downsampling is never confused with failure.

**One design change made for correctness and testability:** session classification reads the
record's own `recorded_at` rather than `Utc::now()`. The budget is a property of the data being
written, and it makes the reservation deterministically testable instead of wall-clock dependent.

---

## 8. Restart and budget behaviour

**Before:** the filename embedded `<day>-<run>` where `run` included the PID, so a restart produced a
new filename, `create_new` succeeded, and `bytes` reset to zero — **a restart handed the same UTC day
a fresh 8 GiB.** The `lost` counter reset too, since `RECORDER` is a per-process `OnceLock`.

**After:** `scan_disk_state(dir, day)` sums the on-disk sizes of every segment for that UTC day and
seeds `day_bytes` from it, and resumes sequencing from the highest existing `<seq>`. Budget is a
property of the files that exist, not of how many times the process has started. A UTC-day rollover
still grants a fresh allowance — correct, since that is a genuinely new day — and opens its own
segment.

Pinned by `a_restart_cannot_grant_the_same_day_a_fresh_budget` and
`disk_scan_rebuilds_the_days_spend_so_restart_cannot_reset_it`.

---

## 9. The six inverted episodes — root cause and repair

**Cause.** Both session-boundary close paths take their timestamp from an *event*, not from the
episode:

- `episode.rs:232` (`observe`) closes at the incoming event's `at`.
- `episode.rs:337` (`expire_inactive`) closes at `last_observed_at`.

Detector events do not arrive in timestamp order across UTC midnight — bar-derived events are
stamped bar-close, trade events are stamped by the trade. An episode opened at `00:00:00Z` could
then see an event stamped `23:59:00Z` on the previous date, which both satisfies "different date"
and **precedes the episode's own open**. All six Session 001 cases match exactly: `openedAt
2026-09-10T00:00:00Z`, `closedAt 2026-09-09T23:59:00Z` (FTFT, YMAT, SUNE, CULP, UFG, MGN).

**Repair.** A single clamp in `close()`, the choke point every path funnels through:

```rust
let at = at.max(episode.opened_at);
```

Clamping rather than rejecting: the boundary genuinely occurred and the episode genuinely must
close; only the recorded instant was wrong. A zero-length episode is honest — "opened and closed at
the boundary" — where a negative one is not representable.

**Invariant `closedAt >= openedAt` now holds for every persisted episode**, across inactivity,
invalidation, boundary and shutdown alike.

---

## 10. Files changed

```
 crates/backtest-metrics/src/episode.rs    |  74 +++
 crates/backtest-metrics/src/horizon.rs    | 179 ++++++-
 crates/market-data/src/discovery_audit.rs | 757 ++++++++++++++++++++++++++++--
 crates/ws-server/src/measurement.rs       | 181 ++++++-
 python/export_session.py                  | 184 +++++++-
 python/test_export_session.py             | 197 +++++++-
 6 files changed, 1502 insertions(+), 70 deletions(-)
```

Six files, all measurement, capture, or export. **No strategy crate appears in the diff.**

---

## 11. Tests added and modified

**39 new tests** (28 Rust, 11 Python); 1 modified.

`horizon.rs` (7): settlement exceeds every horizon · deadline tracks the grid automatically · path to
longest+margin observes every horizon · **settling exactly at the longest horizon censors it (F1
regression, deliberately retained)** · observed coverage monotone non-increasing · out-of-order
points sorted before sampling · excursion excludes pre-signal prices.

`measurement.rs` (6): final bar close observable at bar end · in-progress bucket uses receipt time ·
live updates in one bucket never share a timestamp · trade-stamped events keep market time · bar
close attributed to the instant it became knowable · collector settles on the derived deadline.

`episode.rs` (2): never closes before it opened across UTC midnight (regression) · every close path
upholds the invariant.

`discovery_audit.rs` (13): premarket cannot spend the whole budget · overnight held to the reserve ·
after-hours may use the full budget · critical classes never downsampled · disk scan rebuilds the
day's spend · retention never offers the current segment · **premarket flood cannot make
regular-session capture impossible** · file budget rotates instead of stopping · directory ceiling
bounds growth and spares the current segment · degradation downsamples and says so in-band ·
critical records survive exhaustion with self-describing drops · restart cannot grant a fresh budget
· UTC-day rollover starts a fresh allowance and segment.

`test_export_session.py` (11 new): directory with only JSONL · **only NDJSON** · both · explicit
JSONL · explicit NDJSON · empty directory fails · expected episodes absent fails · expected discovery
absent fails · present expectation succeeds · manifest counts and checksums match independent
computation · four clocks stay distinct and correct · missing capture timestamps yield null.
**Modified:** `schemaVersion` assertion 1 → 2.

---

## 12. Validation results

| Suite | Result |
|---|---|
| `cargo test -p backtest-metrics` | **112 passed**, 0 failed |
| `cargo test -p market-data` | **89 passed**, 0 failed |
| `cargo test -p ws-server` | **70 passed**, 0 failed |
| `cargo test -p auto-trader` | **45 passed**, 0 failed |
| `cargo test --workspace --no-fail-fast` | **427 passed, 0 failed** |
| `python -m unittest discover -s python -p 'test_discovery*.py'` | 10 passed, OK |
| `python -m unittest python.test_export_session` | 17 passed, OK |
| `bun --cwd=apps/client run test` | **60 pass, 0 fail** (500 assertions) |
| `tsc --noEmit -p apps/client/tsconfig.app.json` | exit **0** |
| `tsc --noEmit -p apps/mobile/tsconfig.json` | exit **0** |

`pytest` was not run locally — it is not installed on this machine, exactly as reported previously.
CI runs it, and the Python suites above were executed with `unittest`, which covers the same files.

No formatting churn: `cargo fmt` was not run (it fails repo-wide on 63 pre-existing files, unrelated
to this work).

---

## 13. Adversarial review results

Each invariant was attacked, not assumed.

| Invariant | Attack | Result |
|---|---|---|
| **Causality** | Feed a final bar whose close is far above the signal; check whether it is dated to bar open | **Holds** — dated to bar end; `secondsToMfe == 60` |
| | Feed repeated live updates in one bucket | **Holds** — strictly increasing stamps, `bucket_start` never used |
| | Place a price before `signal_at` in the path | **Holds** — excluded from MFE/MAE and from `observationCount` |
| | Shuffle path arrival order | **Holds** — sorted before sampling; identical results |
| **Horizon reachability** | Set margin to 0 so a horizon meets the deadline | **Build fails** with the intended message |
| | Path running to the deadline | **Holds** — every horizon observed |
| | Path stopping short | **Censors correctly** (F1 regression retained) |
| **Temporal consistency** | Out-of-order event across UTC midnight; `finish()` at an earlier instant | **Holds** — `closedAt >= openedAt` on every path |
| **Bounded memory** | Pending set saturation | **Holds** — `MAX_PENDING_OUTCOMES` 4096 and `MAX_PATH_POINTS` 2048 unchanged; window grew 6.7%, still bounded |
| **Bounded disk** | 200 records against a 2048-byte ceiling | **Holds** — bounded; current segment never deleted |
| **Session preservation** | 400-record premarket flood, then regular-session writes | **Holds** — premarket capped at its share; `scan_completed` records survive |
| **Restart safety** | Second writer over the same directory and day | **Holds** — inherits prior spend |
| **Export completeness** | Directory with only `.ndjson`; expectations unmet | **Holds** — exported; non-zero exit on unmet expectations |
| **Provenance** | Independently recompute both digests and counts | **Holds** — exact match |

**One invariant failed during development and was fixed before reporting:** my first causality test
asserted a 30s horizon must be censored when only a later bar close exists. That is not what the
sampling rule claims, and the failure exposed that I had mis-stated the invariant, not that the code
was wrong. The test was rewritten to pin attribution — the property F4 actually violates.

---

## 14. Session 001 unchanged

Checksums captured before any edit and re-verified after all work:

```
d365bd5bcf70c357c1378a023c8538238af97e0c48f334ccd1c979027f615c3c  export/session-2026-09-10/SHA256SUMS
888ae168cd3a36b232f04f3ad716576c1e64a6c1b95ea51b01eb4c783ea59802  export/session-2026-09-10/alpaca_paper_ledger.ndjson.gz
34245f0d011bd65d8c04b6422cdc319b3a1ee118bbe1a7b4bdcfd0660a42f16e  export/session-2026-09-10/episodes-2026-09-10.ndjson.gz
a0440188d5e63fcda4a599ad256d07ed099393b75c99a3ba08bdac2d567f4014  export/session-2026-09-10/manifest.json
f70299130079bc57eac9479f8cfe4bc3cdd324b3b6897aeb552505d3d831c23e  raw/alpaca_paper_ledger.jsonl
9b63027bb6752c46affbf6409505c1647a2c9acd562f96f5b8b0558bbe084204  raw/episodes-2026-09-10.ndjson
```

`diff` of before/after: **identical, all six.** No file under
`~/Desktop/wavystack/stockspotter-research/sessions/2026-09-10/` was modified, regenerated, renamed,
filtered, or overwritten. No "corrected Session 001" exists. Reads against it were read-only.

**Classification recorded outside the artifact:** Session 001 is **INSTRUMENT VALIDATION SESSION
001** — it validated the instrument and found it defective. Its measurements are not analysis-grade,
and it should not be used for outcome analysis beyond that purpose.

---

## 15. No strategy logic changed

Untouched, verified by the diff containing none of these files:

Fast Funnel · relative-volume formula · Ignition detector and its cooldown · Momentum Scorer,
weights and thresholds · Consolidation/Micropullback configs · Halt Detector · catalyst behaviour ·
universe tiers · Auto-Trader gates, sizing and exits · `outcome.rs` target/stop definitions ·
`strategy_config.rs` enablement.

`live.rs` is **not** in the diff, so the `ScanEvent` wire, event ordering, and every strategy input
are byte-for-byte unchanged. The R2 correction lives entirely in the measurement consumer, which is
why it can fix causality without altering what production sees.

Architecture findings from the earlier audit (relative-volume normalisation, ignition dominance,
quote dead-end, flat-base band) were **deliberately left alone**. A valid ruler first.

---

## 16. `git diff --stat`

```
 crates/backtest-metrics/src/episode.rs    |  74 +++
 crates/backtest-metrics/src/horizon.rs    | 179 ++++++-
 crates/market-data/src/discovery_audit.rs | 757 ++++++++++++++++++++++++++++--
 crates/ws-server/src/measurement.rs       | 181 ++++++-
 python/export_session.py                  | 184 +++++++-
 python/test_export_session.py             | 197 +++++++-
 6 files changed, 1502 insertions(+), 70 deletions(-)
```

## 17. `git status --short`

```
 M crates/backtest-metrics/src/episode.rs
 M crates/backtest-metrics/src/horizon.rs
 M crates/market-data/src/discovery_audit.rs
 M crates/ws-server/src/measurement.rs
 M python/export_session.py
 M python/test_export_session.py
?? .claude/
?? AUDIT-2026-09-09.md
```

Nothing staged, nothing committed. The two untracked entries pre-date this work.

---

## 18. Deployment recommendation

**Yes — ready for deployment, with two conditions.**

All five repairs are implemented, every stated invariant is enforced and adversarially tested, the
full workspace is green, and no strategy input changed. R5 in particular is the one that must ship
before another capture, because without it every future session loses discovery coverage from
mid-morning onward, exactly as Session 001 did.

**Condition 1 — deploy R5 with explicit storage settings for the target host.** Defaults are 1 GiB
per file, 8 GiB per day, 32 GiB directory ceiling. The VPS currently holds ~24 GiB of existing
discovery data, so the ceiling will engage and begin reclaiming oldest segments immediately. That is
correct behaviour, but it should be a decision, not a surprise. Set `DISCOVERY_AUDIT_MAX_BYTES`
deliberately against available disk.

**Condition 2 — verify capture health early in the first session, not at the close.** The in-band
markers now make degradation self-describing; confirm that `capture_degraded_start` does *not* appear
before the open, which is the signature of the failure being repaired.

What I cannot verify from here, stated plainly: this was validated locally only. Behaviour under real
universe-mode volume — the rate that exhausted 8 GiB in a morning — has not been observed. The
adversarial tests simulate pressure with small budgets and synthetic records; they establish the
logic is correct, not that the chosen defaults are right for production volume. Expect the first
session to inform the budget numbers.

---

## 19. Is the next session an analysis-grade Alpha baseline?

**Yes for outcome measurement, conditionally, and not for recall.**

**What is now sound.** Every configured horizon including 1800s is reachable. Forward prices are
attributed to the instant they became observable. Episodes cannot close before they open. Exports
cannot silently omit a dataset. Manifest times mean what their labels say. Discovery capture survives
a premarket flood and describes its own gaps.

**Three caveats, none repaired by this work because none is a measurement defect:**

1. **Comparability breaks at this commit.** R1 and R2 change measurement semantics, so pre-repair and
   post-repair data are not comparable. Session 001 cannot be pooled with the next session under any
   circumstances. This is the reason it is now classified as instrument validation.
2. **Recall depends on discovery capture actually working in production.** The mechanism is repaired
   and tested; that it holds under real volume is unverified (§18). Until a session completes with no
   pre-open degradation markers, missed-runner analysis should not be attempted.
3. **The structural properties the audit identified are unchanged, by instruction.** Ignition will
   still dominate ~98.5% of episodes under `IGNITION_UNIVERSE_MODE=1`; catalyst coverage will still
   track funnel membership at ~1.5%; episodes will still cluster heavily within symbols. These are
   strategy and architecture questions, and they will shape what the next session can support
   regardless of how good the ruler is. A correct instrument measuring a narrow population still
   yields a narrow result.

**Recommendation:** deploy, capture one complete session, and validate the instrument against *that*
session — total 1800s observability, zero inverted episodes, no pre-open degradation — before
treating it as the analysis-grade baseline. That check is cheap and it is the same discipline that
found these five defects.

**Stopping here. Nothing committed. Awaiting instructions.**
