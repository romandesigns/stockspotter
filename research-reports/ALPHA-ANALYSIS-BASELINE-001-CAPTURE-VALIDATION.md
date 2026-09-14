# ALPHA ANALYSIS BASELINE 001 — CAPTURE & VALIDATION REPORT

**Written:** 2026-09-11 00:50 UTC (2026-09-10 20:50 EDT)
**Strategy baseline:** `ba722698af4fa2b387339eb017a2d58734f969c7` — **FROZEN, unchanged**
**Measurement repair:** `5b5da9b74a1769ea5462637372e2fc499db47a74` — **committed, pushed, CI-validated, DEPLOYED**

---

## STATUS: §§1–6 COMPLETE. §§7–13 PENDING THE SESSION.

**Sections 1–6 are done and verified.** The repair is live in production and the repaired discovery
capture has been inspected ahead of the open, exactly as §6 requires.

**Sections 7–13 describe a session that has not happened yet.** At the time of writing it is
**00:50 UTC on 2026-09-11**. The next complete U.S. regular session is:

| Milestone | Time (UTC) | Time (ET) |
|---|---|---|
| Regular session opens | 2026-09-11 **13:30** | 09:30 |
| Regular session closes | 2026-09-11 **20:00** | 16:00 |
| Settlement completes | 2026-09-11 **~20:32** | ~16:32 |
| Earliest honest export | 2026-09-11 **~20:35** | ~16:35 |

**The finalization time is derived, not assumed.** §7 explicitly warns against assuming +30 minutes.
The deployed constant is `SETTLE_AFTER_SECS = longest_horizon_secs() + OBSERVATION_MARGIN_SECS =
1800 + 120 = **1920 s (32 minutes)**`, verified in the deployed checkout at
`crates/backtest-metrics/src/horizon.rs:65,70`. An episode opening at 19:59:59 settles at 20:31:59.

Every item below that depends on the session is marked **PENDING** with the reason. **Nothing has
been estimated, extrapolated, or filled in speculatively.** Per §13 the session is not yet named at
all — it cannot be `CANDIDATE ANALYSIS BASELINE 001` before it is captured.

---

## 1. Measurement-repair commit SHA

```
5b5da9b74a1769ea5462637372e2fc499db47a74
```

Parent: `ba722698af4fa2b387339eb017a2d58734f969c7` — the frozen strategy baseline, unchanged.

Staged by explicit pathname (six files); `git add .` / `git add -A` were not used; `.claude/` and
`AUDIT-2026-09-09.md` remain untracked and unstaged. `git diff --check` and
`git diff --cached --check` both clean. One commit, no amendment of prior checkpoints.

Pushed without force. Verified `local HEAD == origin/release/operating-run-20260907 HEAD ==
5b5da9b…`.

## 2. CI results

Run **`34546969225`**, workflow `Validate`, event `push`, head SHA `5b5da9b…` — CI ran against the
exact repair commit.

| Job | Result |
|---|---|
| **Tests, lint and build** | ✅ **SUCCESS** |
| Dependency advisories | ❌ failure — **JavaScript advisories step only** |

Rust dependency audit (RustSec) passed. The Bun advisory failure is the **previously documented and
accepted exception** on transitive JS advisories. It was **not** weakened: no threshold lowered, no
advisory ignores added, no dependencies upgraded in this task.

**No new functional, test, or build failure appeared**, so §3's stop-condition was not triggered and
deployment proceeded.

## 3. Exact deployed SHA

```
deployed-commit: 5b5da9b74a1769ea5462637372e2fc499db47a74
VPS HEAD:        5b5da9b74a1769ea5462637372e2fc499db47a74
```

Identical. Deployed via the project's normal mechanism — `git fetch` + `git merge --ff-only
FETCH_HEAD` to promote the release branch (deploy.sh deliberately does not auto-advance release
branches), then `ops/vps/deploy.sh`. No gate bypassed: dirty-checkout protection, branch validation,
remote-presence validation, token-length assertion, `--wait` health gates, post-deploy health probe
and `.deployed-commit` recording all active.

**One honest note on how the deploy actually ran.** My manual `deploy.sh` invocation returned exit 0
with no output — the `flock -n 9 || exit 0` path, because the systemd deploy timer held the lock. I
verified rather than assumed, found `.deployed-commit` still at `ba722698` and containers at 27
hours uptime, and confirmed the timer was mid-build (`docker compose build`, PID 816117). The
deployment was then completed by the project's own timer. Had I trusted the exit code, I would have
reported a deployment that had not happened.

## 4. Discovery storage configuration selected

**Measured before choosing:**

| Metric | Value |
|---|---|
| Disk total / used / **available** | 387 GiB / 130 GiB / **258 GiB** (34% used) |
| `data/` total | 25 GiB |
| `data/discovery-audit/` | **25 GiB**, 10 files |
| `data/research/` | 312 MiB |

**Selected:**

```
DISCOVERY_AUDIT_DAILY_BYTES = 25769803776   # 24 GiB
DISCOVERY_AUDIT_MAX_BYTES   = 85899345920   # 80 GiB
DISCOVERY_AUDIT_PER_FILE_BYTES = (default)  #  1 GiB
```

Appended to `/opt/apps/stockspotter/.env` after backing it up to `.env.bak-pre-r5`. No credential
value was read or printed.

**Rationale — daily budget.** I raised this as well as the ceiling, which was not explicitly asked
for and therefore needs justifying. R5 reserves half the daily allowance for the regular session. At
the 8 GiB default that leaves the regular session only **4 GiB**. Session 001's own evidence puts
overnight/premarket burn at ~0.64 GiB/hour and the regular session is busier, so 4 GiB would very
likely have degraded `coverage` records *during* the session — defeating the recall analysis this
capture exists to enable. At 24 GiB/day the reserve leaves ~12 GiB for the session against an
estimated 7–10 GiB need.

**Rationale — ceiling, deliberately not maximised.** 80 GiB is roughly 3.3 days of full-rate history
(more on quieter days), enough to re-run recall analysis and compare sessions. Discovery grows from
25 GiB to at most 80 GiB, consuming ≤55 GiB more and leaving **~203 GiB free — about 79% of current
free space** — for Docker layers, application data, exports, logs and normal operation. 258 GiB was
available; using it all would have defeated the purpose of R5.

## 5. Session date

**PENDING — target 2026-09-11.** Not yet captured.

## 6. Exact capture interval

**PENDING.** Capture is running continuously since the deployment restart at **2026-09-11 00:43:05
UTC**; the session interval cannot be stated until the session completes.

## 7. Exact regular-session interval

**2026-09-11 13:30:00 – 20:00:00 UTC** (09:30–16:00 ET). Scheduled, not yet observed.

## 8. Exact settlement/finalization interval

**20:00:00 – ~20:31:59 UTC**, derived from the deployed `SETTLE_AFTER_SECS = 1920 s`. Earliest
honest export ~20:35 UTC.

## 9. Any service/data interruption

**Recorded so far — one, planned and expected:**

| Event | Time (UTC) | Nature |
|---|---|---|
| Deployment restart of all five services | 2026-09-11 00:43:05 | **Planned** — this deployment |

This restart occurred **~12.8 hours before the target session opens**, so it does not interrupt the
session being captured. Pre-restart pending episodes were lost as designed (the documented
30-minute-window property, now 32); those belong to 2026-09-10 after-hours, not to the target
session.

Ongoing interruption recording continues through the session per §7.

## 10–36, 38–46. Session measurements

**ALL PENDING — the session has not occurred.**

This covers: total OpportunityEpisodes · distinct symbols · episodes by `openedBy` · episodes by
`closeReason` · feature-context coverage · research-rank coverage · all seven horizon
observed/censored counts (30s/60s/180s/300s/600s/900s/1800s) · MFE/MAE coverage · inverted episode
count · measurement dropped · measurement write-errors · pending-cap incidence · path-cap incidence ·
discovery record count · segment count · rotation count · sampled-out count · lost/drop count ·
degradation intervals · drop intervals · retention events · Auto-Trader journal records · skip
records and SkipReason availability · entry records · exit records and ExitReason availability · raw
size · compressed size · manifest schema-v2 summary · production checksum verification · local
checksum verification.

None of these exist. Reporting any of them now would be fabrication.

## 37. Does regular-session discovery support recall analysis?

**PENDING — cannot be answered before the session.**

What *can* be said now: the mechanism that made Session 001's answer NO is repaired and verified
live (§6 below), and the budget is configured so the regular session cannot be starved. Whether the
answer is YES depends on observed behaviour during the session.

## 47. Local immutable artifact path

**PENDING.** Target: `~/Desktop/wavystack/stockspotter-research/sessions/2026-09-11/`

## 48. Any remaining measurement defect

**None found in deployment verification.** Session-level validation (§8A–8E) is pending.

## 49. Does the session qualify as ANALYSIS BASELINE 001?

**NOT YET DETERMINED — the session has not been captured.**

Per §13 it is not even `CANDIDATE ANALYSIS BASELINE 001` yet; that name applies once captured, and
`ANALYSIS BASELINE 001` only if validation passes.

## 50. Analyses the artifact supports

**PENDING** — determined by §8 validation.

## 51. Analyses still prohibited

Regardless of outcome, and already fixed:

- **No Alpha analysis in this task or the capture task** — no hit rates, precision, feature
  correlations, momentum/rank/catalyst effectiveness, threshold optimisation, MFE/MAE by feature,
  detector combinations, Auto-Trader profitability, missed-runner characteristics, or new
  scoring/ranking formulas. None was performed.
- **No pooling with Session 001.** R1 and R2 changed measurement semantics; pre- and post-repair
  data are not directly comparable.
- **Recall analysis remains prohibited** unless §8E returns YES on the captured session.

## 52. Strategy behaviour frozen — confirmed

The deployed diff is six files, all measurement, capture or export:

```
crates/backtest-metrics/src/episode.rs
crates/backtest-metrics/src/horizon.rs
crates/market-data/src/discovery_audit.rs
crates/ws-server/src/measurement.rs
python/export_session.py
python/test_export_session.py
```

**No strategy file appears.** Untouched: universe construction · time-of-day behaviour ·
relative-volume formula · Fast Funnel · float threshold and priority · Ignition · ignition cooldown ·
flat-base gate · Momentum Scorer, weights, thresholds · Consolidation · Micropullback · Halt
Detector · catalyst behaviour · Auto-Trader gates, sizing, exits · targets · stops · strategy
enablement.

`live.rs` is not in the diff, so `ScanEvent`, the wire format and event ordering are unchanged — the
R2 causality correction lives entirely in the measurement consumer. Post-deployment logs confirm
identical strategy behaviour: `IGNITION_UNIVERSE_MODE` on with `max_monitors=6000`, movers scan
`gainers=25 most_active=25 universe=12636`, universe rescan applying normally.

Architecture findings from the audit were deliberately ignored, as instructed.

## 53. Session 001 unchanged — confirmed

Re-verified after all work in this task:

```
d365bd5bcf70c357c1378a023c8538238af97e0c48f334ccd1c979027f615c3c  export/session-2026-09-10/SHA256SUMS
888ae168cd3a36b232f04f3ad716576c1e64a6c1b95ea51b01eb4c783ea59802  export/session-2026-09-10/alpaca_paper_ledger.ndjson.gz
34245f0d011bd65d8c04b6422cdc319b3a1ee118bbe1a7b4bdcfd0660a42f16e  export/session-2026-09-10/episodes-2026-09-10.ndjson.gz
a0440188d5e63fcda4a599ad256d07ed099393b75c99a3ba08bdac2d567f4014  export/session-2026-09-10/manifest.json
f70299130079bc57eac9479f8cfe4bc3cdd324b3b6897aeb552505d3d831c23e  raw/alpaca_paper_ledger.jsonl
9b63027bb6752c46affbf6409505c1647a2c9acd562f96f5b8b0558bbe084204  raw/episodes-2026-09-10.ndjson
```

**Identical, all six.** `INSTRUMENT VALIDATION SESSION 001` remains immutable — not modified,
regenerated, overwritten, pooled, or silently compared against post-repair semantics.

## 54. `git log --oneline --decorate -5`

```
5b5da9b (HEAD -> release/operating-run-20260907, origin/release/operating-run-20260907) Repair causal alpha measurement and discovery capture
ba72269 (origin/gpt/audit-remediation-20260909, gpt/audit-remediation-20260909) Build causal alpha measurement pipeline
0544790 Gate release deployments on remote CI lineage
725179b Harden authentication and realtime stream resilience
bafae88 Sign in once per device instead of re-entering the key every launch
```

## 55. `git status --short`

```
?? .claude/
?? AUDIT-2026-09-09.md
```

Clean. Both untracked entries pre-date this work and were never staged.

---

## §5 — Post-deployment health verification

| Check | Result |
|---|---|
| Deployed SHA == repair commit | ✅ `5b5da9b…` |
| All five services restarted and up | ✅ web, ws, qualify, auto-trader, discovery-review |
| Alpaca authenticated | ✅ `alpaca ws: authenticated` |
| Market feed flowing | ✅ full-market subscription; universe **12,636**; movers `gainers=25 most_active=25` |
| Web healthy | ✅ public `/` → **200** |
| HTTP authentication healthy | ✅ public `/api/health` without token → **401** |
| WebSocket authentication healthy | ✅ public `/ws` HTTP/1.1 upgrade → **101** |
| Qualify healthy | ✅ passed deploy gate + `check_health.py` |
| Auto-Trader connected | ✅ `Alpaca PAPER account connection verified`; `connected and welcomed by ws-server` |
| Measurement collector enabled | ✅ `measurement capture enabled path=data/research` |
| Discovery recorder enabled | ✅ writing `2026-09-11-1-1789087386026894-1.jsonl` |
| Measurement dropped at startup | ✅ **0** |
| Measurement write_errors at startup | ✅ **0** |
| Strategy behaviour changed | ✅ **none** |

---

## §6 — Early discovery-capture verification (before the open)

Performed deliberately now rather than at close, which is the whole point of §6.

| Check | Result |
|---|---|
| Sequence rotation naming active | ✅ `<day>-<run>-<seq>.jsonl` — new segment ends `-1` |
| Segment under configured per-file size | ✅ 17 MB against a 1 GiB budget |
| Record schema | ✅ `schema: 2` |
| In-band accounting fields parse | ✅ `lost_records: 0`, `sampled_out: 0` on every record |
| Record kinds present | ✅ `stream_started`, `coverage`, `scan_started`, `snapshot_batch`, `snapshot_complete`, `scan_completed` |
| Day-level accounting sane | ✅ 2026-09-11 total ~498 MB against a 24 GiB daily budget |
| Storage ceiling accounting sane | ✅ directory ~25 GiB against an 80 GiB ceiling; no retention triggered |
| Regular-session reserved budget available | ✅ ~498 MB spent; pre-session cap 12 GiB; full 24 GiB unlocks at 13:30 UTC |
| `capture_degraded_start` | **absent** |
| `capture_degraded_end` | **absent** |
| **`capture_drop_start`** | **ABSENT — §6 blocker condition not triggered** |
| `capture_drop_end` | **absent** |
| `capture_rotated` | absent (not yet due at 17 MB) |
| `capture_retention_removed` | absent (not yet due) |
| Measurement/discovery error lines since restart | ✅ **0** |

**Restart accounting.** The new writer's `scan_disk_state` sums every file prefixed `2026-09-11-`,
which includes the 487 MB segment the pre-repair recorder wrote earlier today — so the day's spend
carried across the restart rather than resetting. Stated precisely: this is **verified by
construction and by local unit test** (`a_restart_cannot_grant_the_same_day_a_fresh_budget`), and the
deployed code contains it, but `day_bytes` is not exposed as a metric so I could not observe the
counter directly in production. The old file's run-suffix (`1788991723345588`) exceeds `u32::MAX` and
correctly fails sequence parsing, so the new segment began at `-1` as intended.

**Projection to the open, stated as an estimate.** From 00:50 to 13:30 UTC at Session 001's observed
~0.64 GiB/hour is ~8.1 GiB, plus 0.5 GiB already ≈ 8.6 GiB — below the 9.6 GiB degradation trigger
(80% of the 12 GiB pre-session cap). It is close enough that premarket degradation is possible. Per
§6 that is **not** failure provided it affects only the downsampled low-value classes with a recorded
sampling rate, and the regular-session reserve is protected by construction.

---

## What happens next

1. **Do nothing to production.** No strategy change, no restart, no threshold tuning between now and
   the session, per §7.
2. **Capture 2026-09-11 13:30–20:00 UTC** unattended.
3. **After ~20:35 UTC**, run §§8–13: horizon reachability (8A), temporal consistency (8B), causal
   timestamps verified against deployed data (8C), measurement integrity (8D), discovery integrity
   and the recall question (8E); then export with `--expect episodes --expect discovery --expect
   trader`, verify on production, transfer, verify locally, and name the session.
4. **Export must include the Auto-Trader skip journal**, not only `alpaca_paper_ledger.jsonl` —
   Session 001's omission. Note that in Session 001 `auto_trader_journal.jsonl` was stale since
   2026-09-04 because production runs the paper-runtime path; whether the repaired capture yields
   reconstructable `considered → skipped/entered → SkipReason → exit → ExitReason` evidence is an
   open question I will answer with evidence at §10, not assume.

**One item for you, outside this task.** The push surfaced that `credential.helper` in your global
git config is set to a GitHub PAT *as its value*, so git echoed the token to the terminal. It is in
scrollback and in plaintext in `~/.gitconfig`. Revoke that token and run
`git config --global --unset credential.helper`.

---

## Standing confirmations

- **No Alpha analysis performed.** No hit rates, precision, correlations, effectiveness measures,
  threshold optimisation, profitability, or missed-runner characteristics were computed or read.
- **Strategy remains frozen** at `ba722698…`; the deployed diff touches no strategy file.
- **Session 001 unchanged**, verified by checksum after all work.
- **No fabricated session data.** Every session-dependent item is marked PENDING with its reason.
- Production changes made: promote the checkout to the approved commit, append two storage keys to
  `.env` (backed up first), and run the standard deployment. Nothing else.
