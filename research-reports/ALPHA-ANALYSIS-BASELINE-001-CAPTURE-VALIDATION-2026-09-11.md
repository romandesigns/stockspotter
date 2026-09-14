# ALPHA ANALYSIS BASELINE 001 — CAPTURE & VALIDATION REPORT

**Session:** 2026-09-11 · **Report written:** 2026-09-11 23:55 UTC
**Strategy baseline:** `ba722698af4fa2b387339eb017a2d58734f969c7` — **FROZEN, unchanged**
**Capture code (deployed):** `5b5da9b74a1769ea5462637372e2fc499db47a74`

---

## VERDICT (item 49 answered up front)

**NO — this session does not qualify as `ANALYSIS BASELINE 001`.**

It is classified **`INSTRUMENT VALIDATION SESSION 002`**, preserved unchanged.

The five approved repairs all work and are verified live. Validation nevertheless found a **new
defect of the same shape as F1**: `MAX_PENDING_OUTCOMES = 4096` saturates at regular-session episode
rates and force-settles every episode early, truncating the 600 s / 900 s / 1800 s horizons for the
window that matters most. Regular-session 1800 s observability is **0.26%**.

This is not a failure of R1 — R1 is demonstrably correct (premarket reaches **58.6%** at 1800 s).
A different bound now binds. The truncation is **non-random**: it scales with episode density, which
scales with market activity, which is correlated with essentially every outcome worth measuring.

**The session is genuinely valuable and must not be discarded.** It supports short-horizon analysis
(30–300 s) and — for the first time — **recall analysis**. §50/§51 state the boundaries precisely.

---

## 1. Measurement-repair commit SHA

`5b5da9b74a1769ea5462637372e2fc499db47a74` — parent `ba722698…` (frozen baseline). Six files staged
by explicit pathname. `git add .`/`-A` not used. `.claude/` and `AUDIT-2026-09-09.md` never staged.

A second commit, `6247dc03b06ce6619b9c6bf7af99de75a609f3e1`, was made **during** this task to fix a
defect deployment validation exposed (see §48-B). It is pushed but **not deployed** — it touches only
offline export tooling, and deploying it would have made the manifest's `gitCommit` disagree with the
code that captured the data.

## 2. CI results

Run **`34546969225`**, `Validate`, event `push`, head `5b5da9b…`.

| Job | Result |
|---|---|
| **Tests, lint and build** | ✅ **SUCCESS** |
| Dependency advisories | ❌ **JavaScript advisories step only** |

Rust advisories (RustSec) green. The Bun failure is the documented accepted exception — not
weakened, no threshold lowered, no ignores added, no upgrades. No new functional/test/build failure,
so §3's stop-condition did not trigger.

## 3. Exact deployed SHA

```
5b5da9b74a1769ea5462637372e2fc499db47a74
```
`.deployed-commit` == VPS `HEAD` == capture code. VPS working tree clean (0 modified files) at
report time.

**Honest note on how it deployed.** My manual `deploy.sh` returned exit 0 with **no output** — the
`flock -n 9 || exit 0` path, because the systemd timer held the lock. I verified rather than trusted
it, found `.deployed-commit` still at `ba722698…` with containers at 27 h uptime, and confirmed the
timer was mid-`docker compose build`. The project's own timer completed the deploy. Trusting that
exit code would have meant reporting a deployment that never happened.

## 4. Discovery storage configuration selected

Measured first: disk **387 GiB total / 258 GiB free (34% used)**; `discovery-audit` **25 GiB**
(10 files); `research` 312 MiB.

```
DISCOVERY_AUDIT_DAILY_BYTES = 25769803776   # 24 GiB
DISCOVERY_AUDIT_MAX_BYTES   = 85899345920   # 80 GiB
DISCOVERY_AUDIT_PER_FILE_BYTES = default    #  1 GiB
```
Appended to `.env` after backup to `.env.bak-pre-r5`. No credential read or printed.

**Daily budget raised deliberately, beyond what was asked.** R5 reserves half the daily allowance for
the regular session; at the 8 GiB default that leaves the session only 4 GiB, and Session 001's
~0.64 GiB/h burn said that would degrade `coverage` records mid-session. At 24 GiB/day the reserve
left ~12 GiB for the session.

**Outcome: the choice was correct and had real margin.** Actual spend was **16.4 GiB** of 24 GiB
(68%), never reaching the 19.2 GiB degradation trigger. At the 8 GiB default the day would have
exhausted **before the open**, repeating Session 001 exactly.

Ceiling deliberately not maximised: discovery ended at ~42 GiB against the 80 GiB ceiling, so
retention never engaged; disk went 258 → 237 GiB free.

## 5. Session date

**2026-09-11** (Friday).

## 6. Exact capture interval

Continuous from **2026-09-11 00:43:05 UTC** (deployment restart) to export at **23:35 UTC**.
Record-derived: `captureTimeRange` = `00:00:00.029Z → 23:30:40.451Z`.

## 7. Exact regular-session interval

**13:30:00 – 20:00:00 UTC** (09:30–16:00 ET).

## 8. Exact settlement/finalization interval

**20:00:00 – 20:31:59 UTC**, derived from the deployed `SETTLE_AFTER_SECS = longest_horizon_secs() +
OBSERVATION_MARGIN_SECS = 1800 + 120 = 1920 s`. Export ran at 23:35 UTC, ~3 h after settlement
completed — comfortably past, and not the "+30 min" §7 warned against assuming.

## 9. Service / data interruption

| Event | Detail |
|---|---|
| Deployment restart | 00:43:05 UTC — **planned**, 12.8 h before the open |
| Alpaca re-authentications | **2** over the day |
| `new session: reconnecting` | **1** (the documented UTC-date-change detector rebuild) |
| Measurement queue-full drop | **1 record**, 20:06 UTC |
| Discovery queue-full drops | **1,339 records** (see §33) |
| Container restarts during the session | **none** — ws up 22 h across the whole session |
| VPS interruption | none |
| Disk pressure | none |

Nothing was discarded. All of the above is recorded rather than treated as grounds to reject.

## 10–15. Episode population

Measured on the 23:06 UTC file state (128,144 episodes). The **exported artifact contains 128,312** —
168 after-hours episodes settled between analysis and export at 23:35. No conclusion changes; the
artifact is authoritative.

| # | Item | Value |
|---|---|---|
| 10 | Total OpportunityEpisodes | **128,312** (exported) / 128,144 (analysed) |
| 11 | Distinct symbols | **5,930** |
| 12 | By `openedBy` | IgnitionDetector **125,744** (98.13%) · MomentumScorer 1,532 · FastFunnel 863 · Micropullback 4 · **ConsolidationBreakout 1** |
| 13 | By `closeReason` | invalidated **117,593** · inactivity 10,540 · session_boundary 11 |
| 14 | Feature-context coverage | see below |
| 15 | Research-rank coverage | **59,740 / 128,144 = 46.62%** |

**Feature-context coverage** (denominator 128,144):

| Group | Present | Share |
|---|---:|---:|
| `preDetection`, `capturedAt`, `detectedAt` | 128,144 | **100%** |
| `ignition` | 127,945 | 99.84% |
| `halt` | 38,440 | 29.99% |
| `momentum` | 17,239 | 13.45% |
| `catalyst` | 1,783 | 1.39% |
| `funnel` | 1,489 | 1.16% |
| `market` / `minuteOfDayUtc` | 1,489 | 1.16% |
| `traderLinkage` | **0** | 0% |

Session-window distribution by `openedAt`: overnight 20 · premarket 4,493 · **regular 121,520
(94.8%)** · after-hours 2,111.

## 16–22. Horizon reachability (§8A)

**All episodes** (denominator 128,144):

| Horizon | Observed | Censored | Observed % |
|---:|---:|---:|---:|
| 30 s | 126,466 | 1,678 | **98.69%** |
| 60 s | 125,850 | 2,294 | **98.21%** |
| 180 s | 121,892 | 6,252 | **95.12%** |
| 300 s | 116,709 | 11,435 | **91.08%** |
| 600 s | 98,926 | 29,218 | 77.20% |
| 900 s | 68,951 | 59,193 | 53.81% |
| **1800 s** | **3,713** | 124,431 | **2.90%** |

All censoring is `InsufficientForwardData`; no `SessionEnded`, `CaptureEnded` or `DataGap` on any
horizon.

**Is 1800 s genuinely reachable, or still a race?** Genuinely reachable. Two independent proofs:

1. **3,713 of the 3,714 episodes whose observed span reaches 1800 s do observe the 1800 s horizon —
   99.97%.** The sampling boundary is no longer the constraint at all.
2. **Broken out by window**, the repaired settlement performs exactly as designed where the pending
   set is not saturated:

| Window | Episodes | Median span | 900 s obs | **1800 s obs** | Episodes/s |
|---|---:|---:|---:|---:|---:|
| premarket | 4,493 | **1,870 s** | 86.3% | **58.60%** | 0.23 |
| **regular** | 121,520 | **923 s** | 52.6% | **0.26%** | **5.19** |
| after-hours | 2,118 | 1,195 s | 57.5% | 36.17% | 0.15 |

Against Session 001's 1800 s figure of **0.056%**, the all-day rate improved **52×** and premarket
improved **1,046×**. R1 is correct.

**But the regular session is truncated by a different bound — see §48-A.** `MAX_PENDING_OUTCOMES =
4096` at 5.19 episodes/s fills in a predicted **789 s**; the observed regular-session median span is
**923 s**. That is the signature, and it is why regular-session 1800 s is 0.26% while premarket is
58.6%.

## 23. MFE / MAE coverage

| Outcome | Count | Share |
|---|---:|---:|
| Observed | 96,097 | **74.99%** |
| Censored `DataGap` | 32,047 | 25.01% |

`DataGap` remains excursion-only; no horizon carries it. Time-to-target: 2% → 7,887 observed (6.15%);
5% → 4,694 (3.66%); 10% → 3,981 (3.11%) — all target censoring is confounded with §48-A truncation.

## 24. Inverted episode count

**11 total — but 0 attributable to the deployed code.**

| Population | Inverted |
|---|---:|
| Opened before the 00:43:05 restart (old binary) | **11** |
| **Opened after the restart (repaired binary)** | **0** |
| **Within the regular session** | **0** |

All 11 are `session_boundary` with `openedAt 2026-09-11T00:00:00Z` / `closedAt
2026-09-10T23:59:00Z`, written by the pre-repair binary between 00:00 and 00:43 UTC and settled at
~00:30. **R6 holds for everything it governed.** The session file spans a binary change; the
analytically relevant window is entirely post-repair.

## 25–28. Measurement integrity (§8D)

| # | Item | Value |
|---|---|---|
| 25 | Measurement dropped | **1** (20:06 UTC, queue full) |
| 26 | Measurement write errors | **0** |
| 27 | Pending-cap incidence | **NOT RECORDED — inferred pervasive** (see §48-A) |
| 28 | Path-cap incidence (`observationCount` ≥ 2048) | **1,153** (0.90%) |

Episodes written 128,312 · malformed **0** · partial lines **0**.

**Item 27 is itself a defect.** Early eviction calls `finalize(entry, None)`, producing
`InsufficientForwardData` — identical to genuine data exhaustion. There is no counter and no marker,
so pending-cap truncation can only be *inferred* from the span distribution. It cannot be filtered
out of the dataset.

## 29–36. Discovery integrity (§8E)

| # | Item | Value |
|---|---|---|
| 29 | Discovery records (exported) | **5,390,775** |
| 30 | Segments (2026-09-11) | **16** (+1 pre-repair file for the same UTC day) |
| 31 | Rotations | **15** (`capture_rotated` ×15) |
| 32 | Sampled-out | **0** |
| 33 | Lost / dropped | **1,339** — all queue-full, **zero budget drops** |
| 34 | Degradation intervals | **0** (`capture_degraded_start` absent) |
| 35 | Drop intervals | **0** (`capture_drop_start` absent) |
| 36 | Retention events | **0** (`capture_retention_removed` absent) |

Day spend **16.4 GiB** of the 24 GiB budget (68%), below the 19.2 GiB degradation trigger. Segments
capped at ~1 GiB each, exactly as configured.

**R5 did the thing it was built to do.** The pre-repair recorder would have hit its 8 GiB wall and
gone silent around midday; instead it rotated 15 times and recorded continuously.

## 37. Does regular-session discovery support recall analysis?

# YES.

**Evidence.** Contiguous coverage across the entire regular session, verified by reading the first
and last record of alternating segments:

```
seg  8: 11:40:21 -> 13:11:05   (premarket into the open)
seg 10: 14:18:07 -> 15:21:20
seg 12: 16:47:20 -> 18:04:40
seg 14: 19:09:05 -> 20:08:05   (through the close)
seg 16: 21:40:36 -> 23:13:21
```

No gap across 13:30–20:00 UTC. Zero degradation intervals, zero budget drops, zero retention
deletions. Session 001 had **no regular-session coverage whatever** (cap reached 12:30, an hour
before the open).

**Qualification:** 1,339 records were lost to channel backpressure and are **not** marked in-band
(§48-C). Against 5.39 M records that is 0.025%, but the loss is not locatable within the stream, so
recall claims should carry that stated uncertainty rather than being treated as exhaustive.

## 38–41. Auto-Trader evidence (§10)

| # | Item | Value |
|---|---|---|
| 38 | Auto-Trader journal records | **8,118 — all stale, last record `2026-09-04T23:59:32Z`** |
| 39 | Skip records / `SkipReason` for this session | **NONE — 0** |
| 40 | Entry records | **113** (final ledger snapshot) |
| 41 | Exit records / `ExitReason` | **104** exits — timeout 44 · stop_hit 31 · target_hit 15 · momentum_deteriorated 14 · 9 still open |

Ledger: 1,212 cumulative account snapshots; trade objects carry
`proposal / buy / sells / adjustments / exit_reason` and **no skip field of any kind**.

**The reconstruction chain is broken at the second step:**

```
episode → considered → skipped OR entered → SkipReason → entry → exit → ExitReason
            ✗            ✗ (skipped)         ✗            ✓        ✓        ✓
```

`trader.considered` is `false` on **all** 128,312 episodes. Production runs the paper-runtime path,
which writes the Alpaca ledger; the `Skipped` journal variant belongs to the older dry-run mode and
has not been written since 2026-09-04, so skips exist only as container stdout, which was not
captured.

**I flagged this risk before the capture and it materialised.** Both trader files were exported
anyway — Session 001's mistake was omitting the journal, and that is not repeated, even though the
journal turns out to be stale. `SkipReason` analysis is **not possible** for this session.

## 42–46. Artifact and verification

| # | Item | Value |
|---|---|---|
| 42 | Raw session size | **18,300,490,951 B (17.04 GiB)** across 21 files |
| 43 | Compressed / exported size | **3,493,290,145 B (3.25 GiB)** — ratio **5.24×** |
| 44 | Manifest schema-v2 summary | below |
| 45 | Production checksum verification | ✅ 21/21 gzip OK, 21/21 SHA-256 OK |
| 46 | Local checksum verification | ✅ 21/21 gzip OK, 21/21 SHA-256 OK — **byte-identical** |

**Manifest (schema 2):**

```
schemaVersion            2
sessionDate              2026-09-11
gitCommit                5b5da9b74a1769ea5462637372e2fc499db47a74   (the CAPTURE code)
exportedAt               2026-09-11T23:36:39.689760+00:00
eventTimeRange           2026-09-10T23:59:00Z -> 2026-09-11T23:31:21.479Z
captureTimeRange         2026-09-11T00:00:00.029Z -> 2026-09-11T23:30:40.451Z
sourceFileModifiedRange  2026-09-04T23:59:32.892Z -> 2026-09-11T23:35:00.016Z
expected                 [discovery, episodes, trader]
groupsPresent            [discovery, episodes, trader]      <- fail-loud satisfied
totals                   5,528,417 records · 0 errors · 0 partialLines · 21 files
```

**All four clocks are genuinely distinct**, which was R4's whole purpose. Note
`sourceFileModifiedRange` starts **2026-09-04** — the stale journal's mtime — and is correctly
labelled filesystem metadata. Under schema 1 that value would have been published as
`captureStartedAt`, asserting the session began a week early. No field labelled capture time derives
from mtime.

Records by group: discovery 5,390,775 · episodes 128,312 · trader 9,330. Episode count re-derived by
decompressing locally: **128,312 — matches the manifest exactly**. All three groups parse.

**Excluded by construction:** no `.env`, credentials, Alpaca secrets, GitHub credentials, SSH
material or unrelated logs. Credential-shaped scan of the manifest: **0 matches**; no
`env`/`credentials`/`secrets` keys; all 21 sources are capture files.

## 47. Local immutable artifact path

```
~/Desktop/wavystack/stockspotter-research/sessions/2026-09-11/session-2026-09-11/
```
23 entries, 3.3 GB, outside the Git repository. Not modified after verification.

## 48. Remaining measurement defects

### 48-A — `MAX_PENDING_OUTCOMES` truncates regular-session horizons (NEW, significant)

`crates/ws-server/src/measurement.rs`: on reaching 4,096 pending episodes the oldest is settled
early with whatever path it has. At 5.19 episodes/s the set fills in ~789 s, so **every**
regular-session episode is force-settled around 800–950 s regardless of the 1,920 s deadline.

Consequence: regular-session 900 s observability 52.6% (premarket 86.3%) and 1800 s **0.26%**
(premarket 58.6%). The same shape of bug as F1 — a bound that silently binds before measurement
completes — and **the truncation correlates with episode density**, hence with market activity, hence
with the very things analysis wants to compare.

Aggravating: early eviction is **unrecorded and indistinguishable** from genuine data exhaustion
(§27), so it cannot be conditioned on or filtered out.

### 48-B — Exporter misclassified discovery segments (FOUND AND FIXED in this task)

`classify()` matched the filename only, but the discovery recorder names segments
`<day>-<run>-<seq>.jsonl`, which contains no group word. Real discovery data classified as `other`
and `--expect discovery` failed on a complete capture — the exact failure R3 exists to prevent,
inverted. Fixed in `6247dc0` by also matching the containing directory, with three regression tests.
Pushed; not deployed (offline tooling only).

### 48-C — Queue-full discovery loss is not marked in-band (NEW, minor)

R5 made **budget** loss self-describing but left the `emit()` `try_send` backpressure path
counter-and-log only. The 1,339 lost records therefore have no `capture_*` marker and are not
locatable in the stream. Completeness is still determinable in aggregate (`lost_records` rides on
every record) but not positionally.

### 48-D — Session file spans a binary change (inherent, documented)

`episodes-2026-09-11.ndjson` contains 11 pre-repair episodes written before the 00:43 restart,
including the 11 inverted ones. The regular session is entirely post-repair. Analysis restricted to
`openedAt >= 2026-09-11T00:43:05Z` is uniform in measurement semantics.

## 49. Does the session qualify as ANALYSIS BASELINE 001?

**NO.** Classified **`INSTRUMENT VALIDATION SESSION 002`**, preserved unchanged.

Passed: §8B temporal consistency (0 post-repair inversions) · §8C causal timestamps · §8D integrity
(0 malformed, 1 dropped, 0 write errors) · §8E discovery and recall support · §9–12 export,
provenance and byte-identical transfer.

Failed: **§8A for the regular session.** 1800 s at 0.26% and 900 s at 52.6% are not a real
measurement of those horizons, and the cause is non-random and unrecorded. Calling this an
analysis-grade baseline would hand GPT long-horizon numbers that are artifacts of a queue bound.

Per §13 the dataset was **not** repaired, filtered or manipulated to make it pass.

## 50. Analyses this artifact supports

- **Short-horizon forward returns, 30 s – 300 s** — 91.1–98.7% observed, uniformly across windows,
  unaffected by 48-A (eviction happens at ~800 s+). Sound.
- **Recall / missed-runner analysis** — for the first time. Continuous regular-session discovery with
  `selection_inputs` and `qualified` per scan, 0.025% stated loss.
- **Episode composition and lifecycle** — counts, `openedBy`, `closeReason`, timing, session windows.
- **Feature-availability structure** — a finding in itself, fully measurable.
- **Time-of-day detection density** (audit hypothesis H1) via `openedAt` (100% coverage). **Not** via
  `minuteOfDayUtc`, which is present on 1.16% only.
- **Entry/exit and `ExitReason` composition** — 113 entries, 104 exits.
- **Instrument validation of R1/R4/R5/R6** — its designed purpose, which it served.

## 51. Analyses still prohibited

- **600 s / 900 s / 1800 s outcomes for the regular session** — truncated by 48-A.
- **Any time-of-day or activity-level comparison of long-horizon outcomes** — directly confounded by
  48-A, since observation length is a function of episode density.
- **`SkipReason` / skip-rate analysis** — no skip evidence exists (§39).
- **Episode → trade attribution** — `trader.considered` false on all episodes, no linkage object.
- **Pooling with Session 001** — R1/R2 changed measurement semantics.
- **All Alpha analysis in this task** — none performed: no hit rates, precision, feature
  correlations, momentum/rank/catalyst effectiveness, threshold optimisation, MFE/MAE by feature,
  detector combinations, profitability, or new scoring formulas.

## 52. Strategy behaviour frozen — confirmed

Deployed diff (`ba722698…` → `5b5da9b…`) is six files, all measurement/capture/export. No strategy
file appears. Untouched: universe construction · time-of-day behaviour · relative-volume formula ·
Fast Funnel · float threshold and priority · Ignition · ignition cooldown · flat-base gate · Momentum
Scorer, weights, thresholds · Consolidation · Micropullback · Halt Detector · catalyst behaviour ·
Auto-Trader gates, sizing, exits · targets · stops · strategy enablement.

`live.rs` is not in the diff, so `ScanEvent`, the wire format and event ordering are unchanged.
Post-deployment behaviour confirms it: `IGNITION_UNIVERSE_MODE` on with `max_monitors=6000`, universe
**12,636**, movers `gainers=25 most_active=25`. Architecture findings were deliberately ignored.

## 53. Session 001 unchanged — confirmed

Re-verified after all work; all six checksums identical to the pre-task baseline.

```
d365bd5b…  export/session-2026-09-10/SHA256SUMS
888ae168…  export/session-2026-09-10/alpaca_paper_ledger.ndjson.gz
34245f0d…  export/session-2026-09-10/episodes-2026-09-10.ndjson.gz
a0440188…  export/session-2026-09-10/manifest.json
f7029913…  raw/alpaca_paper_ledger.jsonl
9b63027b…  raw/episodes-2026-09-10.ndjson
```

`INSTRUMENT VALIDATION SESSION 001` was not modified, regenerated, overwritten, pooled, or silently
compared against post-repair semantics.

## 54. `git log --oneline --decorate -5`

```
6247dc0 (HEAD -> release/operating-run-20260907, origin/release/operating-run-20260907) Classify discovery segments by their capture directory
5b5da9b Repair causal alpha measurement and discovery capture
ba72269 (origin/gpt/audit-remediation-20260909, gpt/audit-remediation-20260909) Build causal alpha measurement pipeline
0544790 Gate release deployments on remote CI lineage
725179b Harden authentication and realtime stream resilience
```

## 55. `git status --short`

```
?? .claude/
?? AUDIT-2026-09-09.md
```

Both untracked entries pre-date this work and were never staged. VPS working tree clean.

---

## Recommended next step

**One repair, then one more capture.** 48-A is a single bound, and premarket already proves the
measurement is correct once it is not saturated:

- Size the pending set from the observed episode rate rather than a fixed 4,096 — at 5.19 eps/s a
  1,920 s window needs ~10,000 slots — and/or key eviction on age rather than count.
- **Record early eviction** so it can never again be mistaken for data exhaustion: a distinct
  `CensorReason` (e.g. `PendingCapReached`) plus a counter.
- Extend R5's in-band marking to the `emit()` backpressure path (48-C).

With those, the next complete session should clear §8A and become the real `ANALYSIS BASELINE 001`.
Nothing about the strategy needs to change to get there.

---

## Standing confirmations

- **No Alpha analysis performed.** Every figure above is a data-quality measurement.
- **Strategy frozen** at `ba722698…`; no strategy file touched.
- **Session 001 unchanged**, checksum-verified.
- **Session 002 preserved unchanged**, not filtered or manipulated to pass validation.
- **Nothing fabricated.** Where evidence is absent (skip records, pending-cap counts) it is reported
  absent.
- Production changes: promote the checkout to the approved commit, append two storage keys to `.env`
  (backed up), run the standard deployment, and temporarily stage the fixed exporter for the export
  (restored immediately; checkout verified clean, HEAD unchanged).

**Stopping here. No Alpha analysis, no strategy changes. Awaiting instruction.**
