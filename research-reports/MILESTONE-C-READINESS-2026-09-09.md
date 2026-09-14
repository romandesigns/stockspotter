# ALPHA MILESTONE C — CAPTURE READINESS / SESSION PENDING

**Prepared:** 2026-09-09 23:24 UTC (19:24 EDT)
**Deployed baseline:** `ba722698af4fa2b387339eb017a2d58734f969c7` — **verified unchanged**
**Status:** §§1–7 complete (previous report). **§§8–10 cannot begin — the target session has not
occurred yet.**

---

## 1. Why §§8–10 are not in this report

| | |
|---|---|
| Current time | **2026-09-09 23:24 UTC** (19:24 EDT) |
| Target session opens | 2026-09-10 13:30 UTC — **in 14 hours** |
| Target session closes | 2026-09-10 20:00 UTC — **in 20.5 hours** |
| Earliest honest export | ~**20:35 UTC** 2026-09-10, after the 30-minute outcome window drains |

Every deliverable in §§8–10 — episode counts, integrity counters, discovery totals, export manifest,
checksums, local transfer — is a measurement *of* the 2026-09-10 regular session. None of it exists.

**What exporting now would actually produce:** `episodes-2026-09-09.ndjson`, currently **323
records** of post-close evening activity captured since the collector started at 22:08 UTC. That is
not the regular session. Labelling it `BASELINE SESSION 001` would silently corrupt every future
strategy comparison measured against it — the exact failure mode this milestone is designed to avoid.

The instruction is explicit on this point: *"Do not manufacture a partial session and call it
complete."* This report is that instruction being followed, not a delay.

---

## 2. Baseline integrity — confirmed unchanged

```
deployed: ba722698af4fa2b387339eb017a2d58734f969c7
expected: ba722698af4fa2b387339eb017a2d58734f969c7   ✓ MATCH
```

Since the milestone froze, there have been **no** code changes, commits, deployments, strategy
changes, threshold changes, detector changes, Auto-Trader changes, episode-boundary changes,
measurement-schema changes, or service restarts.

Production is running the exact frozen baseline. **Do not stop and report** — the §"If production is
no longer running that exact commit" condition is not triggered.

---

## 3. Capture readiness — verified 23:24 UTC

| Check | Status |
|---|---|
| `web` | Up ~1 hour, healthy |
| `ws` | Up ~1 hour, healthy |
| `qualify` | Up ~1 hour, healthy |
| `auto-trader` | Up ~1 hour, healthy |
| `discovery-review` | Up ~1 hour, healthy |
| Measurement collector | **enabled**, actively writing |
| Episodes written so far (2026-09-09) | **323** |
| Discovery recorder | **8 files**, actively writing |
| Disk available | **270 GB** (31% used) |

### Integrity counters — all clean

| Counter | Value |
|---|---|
| measurement dropped | **0** |
| measurement write_errors | **0** |
| discovery lost_records | **0** |
| Alpaca auth failures | **0** |

Capacity for a ~5.1 GB raw session against 270 GB free is not a constraint.

**No intervention is required between now and the session.** Capture runs unattended, which is
precisely the unbiased-baseline condition the milestone requires.

---

## 4. A property in our favour: clean date separation

Tonight's 323 evening episodes live in `episodes-2026-09-09.ndjson`.

Tomorrow's session will land in `episodes-2026-09-10.ndjson` — a **different file**, because the
writer rotates per UTC day.

So the export scopes to the real session by date with no filtering heuristics and no risk of evening
activity contaminating `BASELINE SESSION 001`.

One related caveat to carry forward: the collector started at **22:08 UTC on 2026-09-09**, well
before the 2026-09-10 open, so tomorrow's session will be captured from its first tick. There is no
"first 30 minutes missing" gap for the session itself — that property only affected tonight's
start-up window.

---

## 5. Exact plan for §§8–10

To be executed in one pass once the session closes:

| Step | Action |
|---|---|
| §2 | Wait past 20:30 UTC so pending episodes drain the 30-min outcome window; record measurement dropped / write_errors / episodes-written / discovery lost_records / file sizes |
| §3 | Quantify: total episodes, by `openedBy`, by `closeReason`, feature-context coverage (momentum / funnel / halt / catalyst), research-rank coverage, complete vs censored outcomes, discovery record count, Auto-Trader entries / skips / exits |
| §4 | Run the Milestone B exporter for `2026-09-10`; verify manifest `gitCommit` = `ba722698…` |
| §5 | VPS-side verification: manifest parses, gzip decompresses, counts match, SHA-256 matches, episode/discovery/journal records deserialize, no credential-shaped values |
| §6 | Transfer to `~/Desktop/wavystack/stockspotter-research/sessions/2026-09-10/` — **outside** the git repo (destination already created and confirmed a sibling of the repo) |
| §6 | Independent local re-verification: sizes, checksums, decompression, parsing — must match the VPS byte-for-byte |
| §7 | Record identity checksums; treat as immutable `BASELINE SESSION 001` |
| §9 | **Only after** the evidence is safely local: remove `claude-code-milestone-c-20260909` and the dead `claude-code-stockspotter-diag-20260908`, verifying only those two entries changed |
| §10 | Full 26-item report |

Any Alpaca disconnect, container restart, VPS interruption, discovery degradation, measurement
degradation or market-data gap will be **recorded, not used as grounds to discard the session**.
Late-session censored horizons will be preserved exactly as censored — never converted to failures.

---

## 6. What is needed to proceed

**A single message after ~20:35 UTC on 2026-09-10** (16:35 EDT). Everything after that is unattended.

The alternative — a scheduled loop waking periodically across a 20-hour span — is available but
costs tokens on every wake for a condition that changes once. A single ping is the cheaper path.

---

## 7. Standing confirmations

- **No Alpha analysis has been performed.** No winner rates, precision, momentum effectiveness,
  threshold optimisation, feature correlations, ranking performance, MFE distributions, catalyst
  effectiveness, Auto-Trader profitability, or missed-runner characteristics. That is the next
  milestone.
- **No strategy tuning of any kind.**
- **No code modified, committed, pushed, or deployed** in this task.
- **The two temporary SSH keys remain in place deliberately** — §9 sequences their removal *after*
  the evidence is safely local, and removing them now would make §§8–10 impossible.
- Production access in this task was **read-only**: time, commit hash, container status, log counters,
  disk. Nothing was written or restarted.
