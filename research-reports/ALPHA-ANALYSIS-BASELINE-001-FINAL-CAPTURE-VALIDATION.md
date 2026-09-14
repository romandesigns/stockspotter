# ALPHA ANALYSIS BASELINE 001 — FINAL CAPTURE & VALIDATION REPORT

**Written:** 2026-09-12 03:35 UTC (2026-09-11 23:35 EDT)
**Strategy baseline:** `ba722698af4fa2b387339eb017a2d58734f969c7` — **FROZEN**
**Deployed measurement commit:** `df95d630e47ae1dc5d437295258811855520e834`

---

## STATUS: §§1–7 COMPLETE AND DEPLOYED. §§8–14 AWAIT MONDAY'S SESSION.

The capacity repair is committed, pushed, CI-validated, deployed and verified healthy in production.

**The capture cannot happen yet, and the reason is the calendar.** The VPS clock reads
**Saturday 2026-09-12 03:20 UTC**. US markets are closed Saturday and Sunday. The next **complete**
regular session is:

| Milestone | Time (UTC) | Time (ET) |
|---|---|---|
| Regular session opens | **Monday 2026-09-14 13:30** | 09:30 |
| Regular session closes | **Monday 2026-09-14 20:00** | 16:00 |
| Settlement completes (`SETTLE_AFTER_SECS = 1920`) | **~20:32** | ~16:32 |
| Earliest honest export | **~20:35** | ~16:35 |

Every session-dependent item below is marked **PENDING** with its reason. Per §14 the session is not
yet named — it cannot be `CANDIDATE ANALYSIS BASELINE 001` before it is captured, and **item 49 is
therefore NOT YET DETERMINED**, not YES.

---

## 1. Final measurement-repair commit SHA

```
df95d630e47ae1dc5d437295258811855520e834
```

Five files staged by explicit pathname. `git add .` / `-A` not used. `.claude/` and
`AUDIT-2026-09-09.md` never staged. `git diff --check` and `git diff --cached --check` both clean.

## 2. Parent / exporter-fix SHA

```
6247dc03b06ce6619b9c6bf7af99de75a609f3e1
```

Verified as the **direct parent** of `df95d63`. Not amended, not squashed, not rewritten — it remains
a distinct commit in the lineage, as §3 requires.

## 3. CI results

Run **`34670164037`**, `Validate`, event `push`, head `df95d63…` — the exact commit.

| Job | Result |
|---|---|
| **Tests, lint and build** | ✅ **SUCCESS** |
| Dependency advisories | ❌ **JavaScript advisories step only** |

Rust advisory scan green. The Bun failure is the previously documented exception — not weakened, no
threshold lowered, no ignores added, no dependencies upgraded. **No new functional/test/build
failure**, so §4's stop-condition did not trigger.

## 4. VPS RAM before deployment

| Metric | Value |
|---|---:|
| **Total RAM** | **31 GiB** |
| Used | 4.8 GiB |
| Free | 648 MiB |
| buff/cache | 26 GiB |
| **Available** | **26 GiB** |

## 5. Swap

**0 B — no swap configured**, no swap devices.

Worth stating plainly: with no swap, memory exhaustion would be a hard OOM kill rather than a graceful
degradation. At these magnitudes that is not a concern — but it is the reason the headroom margin
matters rather than merely being comfortable.

## 6. Container memory before deployment

| Container | Memory | % of 31.34 GiB |
|---|---:|---:|
| **ws-server** | **4.434 GiB** | 14.15% |
| discovery-review | 3.09 GiB | 9.86% |
| qualify | 45.01 MiB | 0.14% |
| auto-trader | 12.44 MiB | 0.04% |
| web | 3.20 MiB | 0.01% |
| scout-ntfy | 49.17 MiB | 0.15% |
| convex-dashboard | 132.2 MiB | (512 MiB limit) |
| convex-backend | 94.22 MiB | (2 GiB limit) |
| convex-postgres | 59.78 MiB | (1 GiB limit) |

### Decision against the repair's estimates

| Estimate | Value | Share of 26 GiB available |
|---|---:|---:|
| Expected repaired footprint | ~200 MiB | **0.8%** |
| Conservative | ~300 MiB | 1.2% |
| Theoretical pathological ceiling | 1.80 GiB | **6.9%** |

**Proceed.** Even the pathological ceiling leaves >24 GiB of host headroom, and ws-server would rise
from 4.43 GiB to ~6.2 GiB against 31 GiB total.

**No Docker memory limit was added**, per §1's explicit instruction not to add one merely because none
exists.

## 7. Exact deployed SHA

```
ops/vps/.deployed-commit : df95d630e47ae1dc5d437295258811855520e834
VPS git HEAD             : df95d630e47ae1dc5d437295258811855520e834
```

Identical, and **verified explicitly rather than inferred from an exit code** — §5 warns that
`deploy.sh` can return exit 0 with no output when `flock` is held, which is exactly what happened on
the previous deployment. All five containers restarted (`Up 16 seconds` at verification), confirming
the intended build is live.

Deployed by the project's normal mechanism: `git fetch` + `git merge --ff-only FETCH_HEAD` to promote
the release branch (the VPS checkout was clean, 0 modified files, before the merge), then the standard
`ops/vps/deploy.sh` path with every gate active — dirty-checkout protection, branch validation,
remote-presence validation, token-length assertion, `--wait` health gates, post-deploy probes and
`.deployed-commit` recording.

## 8–11. Session identity and interruptions

| # | Item | Value |
|---|---|---|
| 8 | Session date | **PENDING — target Monday 2026-09-14** |
| 9 | Regular-session interval | 2026-09-14 13:30–20:00 UTC (scheduled) |
| 10 | Settlement completion | ~20:32 UTC, derived from deployed `SETTLE_AFTER_SECS = 1920` |
| 11 | Service interruptions | **One so far, planned:** deployment restart of all five services at 2026-09-12 03:31:27 UTC — **~58 hours before the target session opens**, so it does not interrupt the capture |

## 12–47. Session measurements

**ALL PENDING — the session has not occurred.**

Covering: episode count · distinct symbols · `openedBy` · `closeReason` · feature-context coverage ·
research-rank coverage · all seven horizon observed/censored counts · MFE/MAE coverage · inverted
episodes · measurement drops · write errors · `pending_capacity` · `pending_peak` ·
`capacity_evictions` · `PendingCapacityReached` count · path-cap incidence · discovery records ·
segments/rotations · `sampled_out` · queue-loss count and marker verification · degradation/drop
intervals · retention events · recall suitability · trader evidence · raw/export sizes · manifest
summary · production and local checksum verification · immutable artifact path.

None of it exists. Reporting any of it now would be fabrication.

**One exception that is verifiable now — item 29, `pending_capacity`:**

```
SUPPORTED_EPISODE_RATE_CENTI = 1_600     (16.00 eps/s)
PENDING_SAFETY_NUM / DEN     = 5 / 4     (1.25)
(1600 x 1920 x 5) / (100 x 4) =          38,400
```

Read from the **deployed source at `df95d63`** and recomputed independently. **`pending_capacity` =
38,400**, compile-time asserted to hold the supported rate for one full settlement window.

## 48. Remaining measurement limitations

Known now, before the capture:

1. **Capacity telemetry is only emitted at shutdown.** `pending_peak`, `capacity_evictions` and
   `pending_capacity` are logged when the collector's receiver closes, plus a `warn!` on each
   power-of-two eviction. A long-running container that never restarts therefore reports none of it
   while healthy. This is a real gap in my §5 implementation and it matters for §9.

   **It does not block §9 validation**, because the decisive numbers are recoverable from the
   artifact itself and in one case more authoritatively:
   - `capacity_evictions` → exact count of episodes carrying `pending_capacity_reached` censoring
     (item 32). Zero such episodes is direct proof capacity never bound.
   - `pending_peak` → computed from `openedAt` as the peak rolling-1920s population, the same method
     that measured Session 002's 24,217.

   I will **not** restart ws-server after the session to harvest a shutdown line: that would destroy
   up to 32 minutes of pending episodes to obtain a number the data already contains.

2. **`SkipReason` evidence remains absent.** `auto_trader_journal.jsonl` has been stale since
   2026-09-04; production runs the paper-runtime path and skips go only to container stdout. Per §14
   this is explicitly **not** a blocker, and per §12 the journal will be exported anyway rather than
   pretending stale evidence is current.

3. **Structural properties are unchanged by instruction.** Ignition will still dominate ~98% of
   episodes under `IGNITION_UNIVERSE_MODE=1`; catalyst coverage will still track funnel membership at
   ~1.5%; episodes will still cluster heavily within symbols.

## 49. Does the session qualify as ANALYSIS BASELINE 001?

**NOT YET DETERMINED — the session has not been captured.**

Per §14 it is not yet even `CANDIDATE ANALYSIS BASELINE 001`. That name applies once captured;
`ANALYSIS BASELINE 001` only if every blocking measurement check passes.

## 50–51. Supported / prohibited analyses

**PENDING** — determined by §§9–11 validation. Unchanged regardless of outcome: **no Alpha analysis
was performed in this task**, and none will be before GPT has the artifact. No hit rates, precision,
feature/outcome correlations, momentum or rank or catalyst effectiveness, threshold optimisation,
MFE/MAE by feature, detector confluence, missed-runner characterisation, or new scoring/ranking
models were computed or read.

## 52. Strategy remained frozen — confirmed

The deployed diff is five files, all measurement/capture/telemetry:

```
crates/backtest-metrics/src/horizon.rs    | 108 ++++++++
crates/market-data/src/discovery_audit.rs | 133 ++++++++-
crates/ws-server/src/main.rs              |  20 ++
crates/ws-server/src/measurement.rs       | 431 ++++++++++++++++++++++++++++--
python/test_export_session.py             |  15 +-
```

Verified file-by-file before committing — every one of these reported **unchanged**:

`live.rs` · `universe.rs` · `engine.rs` (Auto-Trader) · `paper_runtime.rs` · `filters.rs`
(Fast Funnel) · `monitor.rs` (Ignition) · `scorer.rs` (Momentum) · `surge.rs` (Consolidation) ·
`halt-detector/lib.rs` · `outcome.rs` (targets/stops) · `strategy_config.rs` (enablement).

`live.rs` absent from the diff means `ScanEvent`, the wire format and event ordering are untouched.
Post-deploy behaviour confirms it: Alpaca authenticated, quiet-tier coverage transition selecting 150
symbols normally, ws listening on `0.0.0.0:8787`.

## 53. Sessions 001 and 002 unchanged — confirmed

Re-verified after all work in this task: **29 files across both sessions, every checksum identical**
to the baseline recorded before any edit. No modification, no pooling, no corrected copies, no
regenerated artifacts. `INSTRUMENT VALIDATION SESSION 001` (2026-09-10) and `002` (2026-09-11) remain
immutable.

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

Clean. Both untracked entries pre-date this work and were never staged.

---

## §6 post-deploy health — all checks pass

| Check | Result |
|---|---|
| Deployed SHA == intended | ✅ `df95d63` in both `.deployed-commit` and `HEAD` |
| Containers restarted on intended build | ✅ all five, `Up 16 seconds` |
| Alpaca authenticated | ✅ `alpaca ws: authenticated` |
| Market feed flowing | ✅ quiet-tier transition selected 150 symbols |
| Web healthy | ✅ public `/` → **200** |
| HTTP auth healthy | ✅ public `/api/health` without token → **401** |
| WebSocket auth healthy | ✅ public `/ws` HTTP/1.1 upgrade → **101** |
| Qualify healthy | ✅ passed the deploy gate and `check_health.py` |
| Auto-Trader connected | ✅ `Alpaca PAPER account connection verified` + `connected and welcomed by ws-server` |
| Measurement collector enabled | ✅ `measurement capture enabled path=data/research` |
| Discovery recorder enabled | ✅ writing `2026-09-12-*` segments |
| Measurement write_errors | ✅ **0** |
| Measurement dropped at startup | ✅ **0** |
| **pending_capacity reports 38,400** | ✅ verified from deployed source + recomputed |
| **capacity_evictions** | ✅ **0** |
| Discovery queue-loss telemetry parses | ✅ schema-2 records carry `queue_loss` (null when no loss) |
| Strategy changed | ✅ **none** |

### R5 restart accounting confirmed live

The new discovery segment is `2026-09-12-1-**1789183887236984**-4.jsonl` — a **new run id continuing
at sequence 4**, because `scan_disk_state` found the previous run's segments for the same UTC day and
resumed after the highest existing sequence instead of restarting at 1. That is restart-safe budget
accounting working in production, not just in a unit test.

### One log line worth explaining rather than glossing

A keyword scan for `write_error|queue full|audit has gaps|capacity` returned exactly **1** match. It
is benign: an `INFO` line reading *"quiet candidate coverage transition (five-minute grace, bounded
capacity)"* — the word "capacity" in prose, not an error. This is the third time in this programme a
naive keyword grep has produced a false positive, so I checked it rather than reporting a count.

---

## What happens next

1. **Production stays frozen (§7).** No threshold, detector, relative-volume, universe, cooldown,
   ranking, Auto-Trader, target/stop or measurement changes between now and the session. The deploy
   timer will no-op since `HEAD` and `.deployed-commit` both read `df95d63`.
2. **Do not restart `ws-server`.** Pending episodes are held in memory for 32 minutes before being
   written.
3. **Monday 2026-09-14**, capture 13:30–20:00 UTC unattended, then wait past ~20:32 for settlement.
4. **After ~20:35 UTC**, run §§9–14: the decisive capacity validation (`capacity_evictions = 0`,
   `pending_peak` comfortably below 38,400, flagging if it exceeds ~30,000), regular-session horizon
   coverage with the critical 1800s question, temporal/causal validation, discovery validation and the
   recall verdict, then export with `--expect episodes --expect discovery --expect trader` including
   both trader files, verify on production, transfer, verify locally, and decide the baseline name.

**Ping me after 20:35 UTC on Monday 2026-09-14.**

---

## Standing confirmations

- **No Alpha analysis performed.**
- **Strategy frozen** at `ba722698…`; eleven strategy sources explicitly verified unchanged.
- **Sessions 001 and 002 unchanged**, checksum-verified after all work.
- **Nothing fabricated** — every session-dependent item is marked PENDING with its reason.
- Production changes in this task: promote the checkout to the approved commit and deploy it. Nothing
  else; no `.env` change was needed this time.
