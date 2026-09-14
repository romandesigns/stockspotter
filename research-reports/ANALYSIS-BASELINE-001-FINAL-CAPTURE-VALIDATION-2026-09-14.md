# ALPHA ANALYSIS BASELINE 001 — FINAL CAPTURE & VALIDATION REPORT

**Session:** 2026-09-14 (Monday) · **Report written:** 2026-09-14 22:20 UTC
**Strategy baseline:** `ba722698af4fa2b387339eb017a2d58734f969c7` — **FROZEN, unchanged**
**Capture code:** `df95d630e47ae1dc5d437295258811855520e834`

---

# VERDICT: **ANALYSIS BASELINE 001 — YES**

Every blocking measurement condition in §14 passed.

| §14 requirement | Result |
|---|---|
| Zero pending-capacity evictions | ✅ **0** |
| 1800s regular-session horizon genuinely reachable | ✅ **76.60%** (was 0.26%) |
| Zero post-repair inverted episodes | ✅ **0** |
| Causal timestamp model intact | ✅ verified |
| No blocking measurement corruption | ✅ 0 malformed, 0 write errors |
| Continuous discovery sufficient for recall | ✅ 17 segments, unbroken |
| Export complete and checksum-verified | ✅ 20/20 both sides |

The two known limitations — absent `SkipReason` evidence and incomplete trader linkage — are
**explicitly not blockers** per §14, and are recorded in §48.

**Stopping here. No Alpha analysis performed. Awaiting GPT.**

---

## 1. Final measurement-repair commit SHA
`df95d630e47ae1dc5d437295258811855520e834`

## 2. Parent / exporter-fix SHA
`6247dc03b06ce6619b9c6bf7af99de75a609f3e1` — verified as the direct parent, not amended or squashed.

## 3. CI results
Run **`34670164037`**, event `push`, head `df95d63`. **Tests, lint and build: SUCCESS.** Rust advisory
scan green. Dependency-advisories job failed on the **JavaScript advisories step only** — the
documented exception, not weakened. No new functional/test/build failure.

## 4. VPS RAM before deployment
**31 GiB total · 26 GiB available** (4.8 GiB used, 26 GiB buff/cache).

## 5. Swap
**0 B — none configured.** Noted because it makes exhaustion a hard OOM rather than graceful
degradation; irrelevant at the observed magnitudes but the reason headroom mattered.

## 6. Container memory before deployment
ws-server **4.434 GiB** (14.15%) · discovery-review 3.09 GiB · qualify 45 MiB · auto-trader 12 MiB ·
web 3.2 MiB · convex stack ~286 MiB.

Expected repaired footprint (~200 MiB) was 0.8% of available; the pathological 1.80 GiB ceiling 6.9%.
**No Docker memory limit was added**, per instruction.

**Measured after the repair: ws-server peaked at ~1.07 GiB during the session** — *below* its
pre-repair baseline, because episodes now settle on age instead of accumulating.

## 7. Exact deployed SHA
```
ops/vps/.deployed-commit : df95d630e47ae1dc5d437295258811855520e834
VPS git HEAD             : df95d630e47ae1dc5d437295258811855520e834
```
Verified explicitly, not from an exit code — §5's `flock` warning. All five containers restarted on
the intended build.

## 8. Session date
**2026-09-14** (Monday).

## 9. Regular-session interval
**13:30:00 – 20:00:00 UTC** (09:30–16:00 ET).

## 10. Settlement completion
**~20:32 UTC**, derived from deployed `SETTLE_AFTER_SECS = 1920`. Export ran **22:14 UTC**, ~1h42m
after settlement completed.

## 11. Service interruptions

| Event | Count | Note |
|---|---:|---|
| Container restarts during the session | **0** | ws-server `Up 2 days` throughout |
| Alpaca re-authentications | **1** | |
| `new session: reconnecting` | **1** | documented UTC-date-change detector rebuild |
| Measurement queue-full drops | **3** | |
| Measurement write errors | **0** | |
| Capacity-eviction warnings | **0** | |
| Discovery queue-full log lines | 11 | power-of-two logging; true count in §37 |
| **`discovery-review` crash loop** | **3,801+ restarts** | **my defect — see §48.1. Does not touch the capture.** |

Nothing discarded; imperfections recorded.

## 12. Episode count
**137,398** in the exported artifact. (Validation analysis ran at 137,157; 241 after-hours episodes
settled between analysis and export. The artifact is authoritative.)

## 13. Distinct symbols
**6,023**

## 14. `openedBy` distribution
IgnitionDetector **135,089 (98.49%)** · MomentumScorer 1,293 · FastFunnel 774 · Micropullback 1 ·
**ConsolidationBreakout 0**

## 15. `closeReason` distribution
invalidated **126,170** · inactivity **10,987** · **session_boundary 0**

## 16. Feature-context coverage (denominator 137,157)

| Group | Present | Share |
|---|---:|---:|
| `preDetection`, `capturedAt`, `detectedAt` | 137,157 | **100%** |
| `ignition` | 137,066 | 99.93% |
| `halt` | 41,490 | 30.25% |
| `momentum` | 17,664 | 12.88% |
| `catalyst` | 1,636 | 1.19% |
| `market` / `funnel` | 1,460 | 1.06% |

## 17. Research-rank coverage
**60,785 / 137,157 = 44.32%**

## 18–24. Horizon observed / censored

**All episodes** (denominator 137,157):

| Horizon | Observed | Censored | Observed % |
|---:|---:|---:|---:|
| 30 s | 136,157 | 1,000 | 99.27% |
| 60 s | 135,668 | 1,489 | 98.91% |
| 180 s | 134,342 | 2,815 | 97.95% |
| 300 s | 133,274 | 3,883 | 97.17% |
| 600 s | 130,315 | 6,842 | 95.01% |
| 900 s | 124,703 | 12,454 | 90.92% |
| **1800 s** | **103,954** | 33,203 | **75.79%** |

**Regular session only** (denominator 129,655) — the decisive table:

| Horizon | Observed | Censored | Observed % | Session 002 |
|---:|---:|---:|---:|---:|
| 30 s | 128,837 | 818 | 99.37% | 98.69% |
| 60 s | 128,413 | 1,242 | 99.04% | 98.21% |
| 180 s | 127,174 | 2,481 | 98.09% | 95.12% |
| 300 s | 126,194 | 3,461 | 97.33% | 91.08% |
| 600 s | 123,885 | 5,770 | 95.55% | 77.20% |
| 900 s | 118,507 | 11,148 | 91.40% | 52.62% |
| **1800 s** | **99,314** | 30,341 | **76.60%** | **0.26%** |

All censoring is `InsufficientForwardData` — **no `PendingCapacityReached`, no `SessionEnded`, no
`CaptureEnded`, no `DataGap` on any horizon.**

**The critical question answered:** 1800s now behaves as a genuine horizon. The curve decays smoothly
(99.4 → 99.0 → 98.1 → 97.3 → 95.6 → 91.4 → 76.6) instead of collapsing off a cliff, remaining
censoring is attributable to real data availability, and at **76.60%** it exceeds Session 002's 58.6%
premarket reference — which §9 correctly framed as a reference, not a target.

Time-to-target coverage improved in the same way: 2% **77.26%** (was 3.89%), 5% **76.20%** (1.02%),
10% **75.93%** (0.33%).

## 25. MFE / MAE coverage
Observed **87,341 (63.68%)** · censored `DataGap` **49,816 (36.32%)**.

Honest note: `DataGap` censoring **rose** from Session 002's 24.73%. That is a direct and expected
consequence of the repair — median retained observations went 87 → **179**, so paths span far more
time and therefore encounter more >120s holes. Longer observation buys horizon coverage and costs
excursion coverage on thin symbols. Not a regression.

## 26. Inverted episodes
**0.** `closedAt >= openedAt` holds for every one of the 137,157 episodes analysed.

## 27. Measurement drops
**3** (queue-full, logged).

## 28. Measurement write errors
**0**

## 29. `pending_capacity`
**38,400** — derived `(16.00/s × 1920s × 1.25)`, verified in the deployed source and recomputed.

## 30. `pending_peak`
**24,882** — **64.8% of capacity**, comfortably below the ~30,000 flag threshold. Computed from the
artifact as the peak rolling-1920s population, the same method that measured Session 002's 24,217.

## 31. `capacity_evictions`
**0**

## 32. `PendingCapacityReached` count
**0** — direct, authoritative proof that capacity never bound.

## 33. Path-cap incidence
**1,761 (1.28%)** at `MAX_PATH_POINTS = 2048`. Observation counts: median **179**, p95 805, max 2,049.

## 34. Discovery records
**6,008,848** exported.

## 35. Discovery segments / rotations
**17 segments · 16 rotations** (`capture_rotated` ×16) · **16.32 GiB**, each capped at ~1 GiB.

## 36. `sampled_out`
**0 for this session.**

This required care. The final record reads `sampled_out = 256,729`, but these are **process-lifetime**
atomics and ws-server has been up since Saturday. Differencing the session's own first and last
records:

| Counter | First (00:00:12) | Last (22:02:44) | **Session delta** |
|---|---:|---:|---:|
| `sampled_out` | 256,729 | 256,729 | **0** |
| `lost_records` | 0 | 1,881 | **1,881** |

The 256,729 accumulated Sat/Sun, when `classify_session` treats non-trading days as `Overnight` and
applies the 50% pre-session reserve. **Reading the final value alone would have reported a
quarter-million downsampled records that never happened today.** Any session-scoped analysis of these
counters must difference, not read.

## 37. Queue-loss count
**1,881 records** (0.031% of 6,008,848), all `queue_full` backpressure — **zero budget-induced drops.**

## 38. Queue-loss marker verification — **the 48-C repair works**

**179 records carry a `queue_loss` object**, and they decode exactly as designed:

```json
"queue_loss":{"classes":["ignition"],"lost":7,
  "onset":"2026-09-14T13:53:16.995316+00:00",
  "onsetMicros":1789393996995316,"reason":"queue_full"}
```

Onset, count, affected class and reason — all in-band. Session 002's 1,339 lost records existed only
in container logs; this session's loss spans are locatable in the dataset itself.

## 39. Degradation / drop intervals
**0 degradation intervals · 0 drop intervals.** No `capture_degraded_start`, `capture_degraded_end`,
`capture_drop_start` or `capture_drop_end` in any 2026-09-14 segment — consistent with §36's delta of
zero. Day spend 16.32 GiB against the 24 GiB budget (68%), never reaching the 19.2 GiB trigger.

## 40. Retention events
**0.** No `capture_retention_removed`; the directory stayed well under the 80 GiB ceiling.

## 41. Recall-analysis suitability — **YES**

Continuous regular-session coverage, verified by reading first/last records of alternating segments:

```
seg  4: 04:43:43 -> 06:18:13
seg  8: 10:56:51 -> 12:27:06
seg 12: 15:47:06 -> 16:49:35
seg 16: 19:52:33 -> 21:13:52
```

No gap across 13:30–20:00 UTC. Zero degradation, zero budget drops, zero retention deletions.
Qualification: 0.031% queue-loss, and **that loss is now locatable in-band** (§38), so recall claims
carry a bounded, self-describing uncertainty rather than an unknown one.

## 42. Trader evidence availability

| Measure | Value |
|---|---:|
| Ledger snapshot lines | 1,598 |
| Trades in final snapshot | **150** |
| Entries (`buy` present) | **150** |
| Exits (`sells` present) | **139** |
| `ExitReason` | timeout 54 · stop_hit 45 · momentum_deteriorated 23 · target_hit 17 · 11 open |
| **Skip fields anywhere in ledger** | **NONE** |
| Journal records | 8,118 — **all stale, last `2026-09-04T23:59:32Z`** |
| `trader.considered` | `false` on all 137,157 episodes |

Reconstruction chain:
```
episode → considered → skipped OR entered → SkipReason → entry → exit → ExitReason
            ✗            ✗ (skipped)         ✗            ✓        ✓        ✓
```
Both trader files were exported regardless, per §12 — **stale skip evidence is reported as stale, not
presented as current.**

## 43. Raw / export sizes
Raw **18,354,856,426 B (17.09 GiB)** across 20 files → compressed **3,482,586,954 B (3.24 GiB)**,
ratio **5.27×**.

## 44. Manifest summary (schema v2)
```
schemaVersion            2
sessionDate              2026-09-14
gitCommit                df95d630e47ae1dc5d437295258811855520e834   (the capture code)
exportedAt               2026-09-14T22:14:13.973941+00:00
eventTimeRange           2026-09-14T08:00:00.035Z -> 2026-09-14T22:10:15.189Z
captureTimeRange         2026-09-14T00:00:12.249Z -> 2026-09-14T22:08:54.069Z
sourceFileModifiedRange  2026-09-04T23:59:32.892Z -> 2026-09-14T22:14:00.015Z
expected                 [discovery, episodes, trader]
groupsPresent            [discovery, episodes, trader]     <- fail-loud satisfied
totals                   6,155,962 records · 0 errors · 0 partialLines · 20 files
records by group         discovery 6,008,848 · episodes 137,398 · trader 9,716
```
All four clocks distinct. `sourceFileModifiedRange` starts 2026-09-04 — the stale journal's mtime —
correctly labelled filesystem metadata; under schema 1 that would have been published as
`captureStartedAt`, asserting the session began ten days early.

## 45. Production checksum verification
**20/20 gzip OK · 20/20 SHA-256 OK · 0 failures.**

## 46. Local checksum verification
**20/20 gzip OK · 20/20 SHA-256 OK · 0 failures** — byte-identical to production. All three groups
parse; episodes decompress to **137,398**, matching the manifest exactly. Discovery records carry the
`queue_loss` key.

## 47. Immutable local artifact path
```
~/Desktop/wavystack/stockspotter-research/sessions/2026-09-14/session-2026-09-14/
```
22 entries, 3.3 GB, outside the Git repository. Not modified after verification.

## 48. Remaining measurement limitations

**48.1 — `discovery-review` is crash-looping (my defect, introduced by R5).**
3,801+ restarts since the 2026-09-12 deploy:
```
File "/app/analyze_discovery.py", line 44, in analyze
    raise ValueError("unsupported discovery audit schema")
```
R5 bumped discovery records from `schema: 1` to `schema: 2`; `analyze_discovery.py:41` hard-rejects
anything that is not exactly 1. I changed the writer and never updated the reader.

**It cannot affect the capture** — separate container, reads `discovery-audit/`, writes only to
`discovery-reports/`; ws-server ran untouched for two days. What is lost is the daily review report
(last one 2026-09-11). My §12 validation missed it because `test_discovery*.py` uses schema-1
fixtures. Not fixed mid-session: that would require a deploy and a container restart, destroying 32
minutes of pending episodes to repair a monitoring convenience.

**48.2 — Capacity telemetry emits only at shutdown.** `pending_peak` / `capacity_evictions` /
`pending_capacity` are logged when the collector's receiver closes. A healthy long-running container
reports none of it. Worked around here by deriving both decisively from the artifact
(`PendingCapacityReached` count, rolling-population computation); I did **not** restart ws-server to
harvest a log line.

**48.3 — In-band counters are process-cumulative** (§36). Session-scoped analysis must difference
first/last records.

**48.4 — `SkipReason` evidence absent** (§42). Not a §14 blocker.

**48.5 — Excursion `DataGap` rose to 36.32%** (§25) as a direct consequence of longer retention.

**48.6 — Local disk reached 97%** during transfer (15 GiB free). Not a data risk this time; a real
constraint for the next session.

## 49. Does the session qualify as ANALYSIS BASELINE 001?

# YES.

All seven §14 conditions met. The session is promoted from `CANDIDATE ANALYSIS BASELINE 001` to
**`ANALYSIS BASELINE 001`**. The dataset was not repaired, filtered or manipulated to make it pass.

## 50. Alpha analyses now supported

- **Forward returns across the full horizon grid, 30 s – 1800 s** — 99.4% to 76.6% observed in the
  regular session, with correct censoring types. **Long-horizon analysis is available for the first
  time.**
- **Time-to-target** at 2/5/10% — ~76–77% observed.
- **Recall / missed-runner analysis** — continuous regular-session discovery, 0.031% self-describing
  loss.
- **MFE/MAE** — 63.68% observed; condition on availability, which correlates with symbol activity.
- **Research-rank evaluation** — 44.32% coverage.
- **Episode composition, lifecycle and feature-availability structure.**
- **Time-of-day analysis** via `openedAt` (100%); **not** via `minuteOfDayUtc` (1.06%).
- **Entry/exit and `ExitReason` composition** — 150 entries, 139 exits.

## 51. Alpha analyses still prohibited

- **`SkipReason` / skip-rate analysis** — no skip evidence exists.
- **Episode → trade attribution** — `trader.considered` false on every episode; no linkage object.
- **Pooling with Sessions 001 or 002** — R1/R2/48-A changed measurement semantics; the three are not
  comparable.
- **Treating `DataGap`-censored excursion as a miss** — it is unknown, not zero.
- **Any analysis assuming independence across episodes** — 137,398 episodes over 6,023 symbols.
- **All Alpha analysis by me.** None performed: no hit rates, precision, feature/outcome correlations,
  momentum/rank/catalyst effectiveness, threshold optimisation, MFE/MAE by feature, detector
  confluence, missed-runner characterisation, or new scoring/ranking models.

## 52. Strategy remained frozen — confirmed
Deployed diff is five files, all measurement/capture/telemetry. Eleven strategy sources were
explicitly verified unchanged before committing: `live.rs` · `universe.rs` · `engine.rs` ·
`paper_runtime.rs` · `filters.rs` · `monitor.rs` · `scorer.rs` · `surge.rs` · `halt-detector/lib.rs` ·
`outcome.rs` · `strategy_config.rs`. `live.rs` absent from the diff means `ScanEvent`, the wire format
and event ordering are untouched. `openedBy` and `closeReason` distributions match Session 002's
shape, confirming detection behaviour is unchanged.

## 53. Sessions 001 and 002 unchanged — confirmed
Re-verified after all work: **29 files, every checksum identical**. No modification, pooling,
corrected copies or regenerated artifacts.

## 54. `git log --oneline --decorate -6`
```
df95d63 (HEAD -> release/operating-run-20260907, origin/release/operating-run-20260907) Finalize bounded alpha measurement under production load
6247dc0 Classify discovery segments by their capture directory
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
Clean. Both untracked entries pre-date this work.

---

## The three sessions, side by side

| | 001 (09-10) | 002 (09-11) | **BASELINE 001 (09-14)** |
|---|---:|---:|---:|
| Episodes | 135,716 | 128,312 | **137,398** |
| Regular-session 1800s observed | — | **0.26%** | **76.60%** |
| Regular-session 900s observed | 51.5% | 52.62% | **91.40%** |
| Pending capacity | 4,096 | 4,096 | **38,400** |
| Capacity evictions | — | pervasive (unrecorded) | **0** |
| Inverted episodes | 6 | 11 (all pre-repair) | **0** |
| Discovery regular-session coverage | **none** | continuous | **continuous** |
| Discovery loss visible in-band | no | no | **yes** |
| Recall analysis | **NO** | YES | **YES** |
| Verdict | instrument validation | instrument validation | **ANALYSIS BASELINE 001** |

---

## Standing confirmations

- **No Alpha analysis performed.** Every figure is a data-quality or capacity measurement.
- **Strategy frozen** at `ba722698…`; no strategy file touched, eleven verified unchanged.
- **Sessions 001 and 002 unchanged**, checksum-verified after all work.
- **Nothing fabricated.** Where evidence is absent (skip records, live capacity telemetry) it is
  reported absent, with the method used to work around it stated.
- Production changes: promote the checkout to the approved commit and deploy it. Nothing else.

**ANALYSIS BASELINE 001 is banked. Stopping. Awaiting GPT.**
