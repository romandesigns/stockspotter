# ALPHA MILESTONE C — BASELINE SESSION 001 CAPTURE, EXPORT & VERIFICATION

**Date:** 2026-09-10 · **Report written:** 22:35 UTC (18:35 EDT)
**Frozen baseline:** `ba722698af4fa2b387339eb017a2d58734f969c7` — verified unchanged throughout
**Status:** Capture **complete and verified local**. One **material capture failure** found (discovery-audit). Two items **outstanding**.

---

## 1. Executive summary

The regular session of 2026-09-10 was captured, exported, transferred to this machine, and
verified byte-for-byte against the VPS. **135,716 episodes**, 0 malformed records.

Three things you need to know, in order of importance:

1. **The discovery-audit capture failed before the session opened.** It hit an 8 GiB daily cap at
   ~12:30 UTC — one hour *before* the 13:30 open — and discarded **≥4,194,304 records** for the rest
   of the day. **The entire regular session has no discovery-audit coverage.** Episode capture is a
   separate writer and is completely unaffected.
2. **`IGNITION_UNIVERSE_MODE=1` is on in production.** Ignition accounts for **98.5%** of all
   episodes (133,666 of 135,716). This was not reflected in the architecture audit I delivered this
   morning, and it changes several of its conclusions (§7).
3. **`ConsolidationBreakout` produced zero episodes. `Micropullback` produced two.** Two of the
   three strategies the auto-trader is permitted to trade generated essentially nothing.

**No Alpha analysis has been performed.** No hit rates, precision, expectancy, MFE distributions,
ranking performance, catalyst effectiveness, or profitability. That is the next milestone. Everything
below is capture characterisation and integrity verification.

---

## 2. Baseline integrity — unchanged

```
deployed: ba722698af4fa2b387339eb017a2d58734f969c7
expected: ba722698af4fa2b387339eb017a2d58734f969c7   ✓ MATCH
manifest: ba722698af4fa2b387339eb017a2d58734f969c7   ✓ MATCH
```

Verified at the start of this task, again mid-task, and recorded independently in the export
manifest via `git rev-parse`. **No code, config, threshold, strategy, or detector was changed.** No
deployment, no restart, no commit, no push.

Containers had **22 hours of continuous uptime** at check time — the `ws` container never restarted
during the session, so nothing was lost from the 30-minute pending-episode buffer.

---

## 3. THE CAPTURE FAILURE — discovery-audit

### What happened

```
2026-09-10T12:57:06Z ERROR market_data::discovery_audit: discovery audit has gaps
    error=8 GiB daily audit cap reached; remaining records will be lost  lost_records=32768
2026-09-10T13:27:35Z  ... lost_records=65536
2026-09-10T13:33:04Z  ... lost_records=131072
2026-09-10T13:41:41Z  ... lost_records=262144
2026-09-10T13:59:06Z  ... lost_records=524288
2026-09-10T14:33:20Z  ... lost_records=1048576
2026-09-10T15:58:31Z  ... lost_records=2097152
2026-09-10T19:02:12Z  ... lost_records=4194304
```

The single file for today stopped growing at **12:30 UTC** at `8,589,934,481` bytes — 111 bytes
short of exactly 8 GiB:

```
-rw-r--r-- 1 root root 8589934481 Sep 10 12:30 2026-09-10-1-1788991723345588.jsonl
```

No later file exists. The counter logs at powers of two, so **4,194,304 is a floor, not a total** —
the true loss through the close is higher.

### Why it matters

Market open was **13:30 UTC**. The cap was reached at **12:30 UTC**. **Every record of the regular
session was discarded.**

The discovery audit is the only record of *what the scanner considered and rejected* —
`selection_inputs` (every snapshot in $0.25–$3.00), `qualified` per scan, `quiet_selected`, and
ignition candidate/confirmed staging. In the architecture audit (§28.1) I identified it as the
closest available partial answer to the recall question — *"what did Stockspotter miss?"*

**For BASELINE SESSION 001, that reference does not exist.** Recall was already the dataset's largest
blind spot; for this session it is total.

### What is NOT affected

Episode capture (`data/research/`) uses a separate writer, a separate path, and separate accounting.
It ran clean: 135,716 episodes, **0 malformed records, 0 partial lines**. The paper ledger also ran
clean. This failure is contained to one subsystem.

### Not fixed

Raising the cap or rotating the file is a **code/config change to the frozen baseline**, which this
milestone forbids. I have not touched it. It needs a decision before the next capture — otherwise
every future session loses discovery coverage the same way, and always from mid-morning onward.

---

## 4. What was captured

### Episodes: 135,716

| Opened by | Count | Share |
|---|---:|---:|
| **IgnitionDetector** | **133,666** | **98.49%** |
| MomentumScorer | 1,078 | 0.79% |
| FastFunnel | 970 | 0.71% |
| Micropullback | 2 | 0.001% |
| **ConsolidationBreakout** | **0** | **0%** |
| **Total** | **135,716** | 100% |

Counts reconcile exactly against the file's line count.

| Close reason | Count |
|---|---:|
| invalidated | 124,630 |
| inactivity | 11,080 |
| session_boundary | 6 |
| **Total** | **135,716** ✓ |

### Feature-context coverage

| Group | Present | Share |
|---|---:|---:|
| `preDetection` | 135,716 | **100%** |
| `ignition` | 135,577 | 99.9% |
| `halt` | 41,626 | 30.7% |
| `momentum` | 17,866 | 13.2% |
| `catalyst` | 1,976 | **1.46%** |
| `funnel` | 1,970 | **1.45%** |
| `market` | 1,970 | **1.45%** |
| `researchRank` | 60,665 | 44.7% |
| `traderLinkage` | **0** | **0%** |

### Outcome observations

| | Count |
|---|---:|
| Censored — `insufficient_forward_data` | 657,640 |
| Censored — `data_gap` | 33,564 |
| **Total censored** | **691,204** |
| Episodes with **no** forward data (`observationCount == 1`) | **411 (0.30%)** |
| Episodes hitting the 2,048 `MAX_PATH_POINTS` cap | 950 |

**Read this carefully, because the headline ratio is misleading.** ~87% of *observation slots* are
censored, but only **0.3% of episodes** lack forward data entirely. The censoring is concentrated in
the *long* horizons (900 s, 1800 s) being unreachable within an episode's observed span — not in
missing data. I initially framed this as "thin names that never print again"; the
`observationCount` distribution disproves that, and the correction matters for how the dataset gets
analysed.

Censoring is correctly typed throughout (`{"censored": "..."}` vs `{"observed": ...}`), so the
"never aggregate censored as failure" property holds.

### Auto-trader

`alpaca_paper_ledger.jsonl` — **757 records**, actively written, real filled paper orders. Last
record at 19:34 UTC: MGN sell, 692 shares @ $0.1817, `exit_reason: "stop_hit"`.

**`auto_trader_journal.jsonl` is stale since 2026-09-04 — this is NOT a failure.** Production runs
the *paper runtime* path (`auto_trader::paper_runtime`), which writes the Alpaca ledger. The journal
belongs to an older dry-run mode. The auto-trader was verifiably live and evaluating throughout;
its skip decisions appear in stdout logs (all `OutsideRegularHours` after 20:00 UTC, correctly).

Worth flagging for later, not acted on: `live_evaluated_signals.jsonl` is stale since 2026-09-05 and
`auto_trader_strategy_config.json` since 2026-09-06, while `live_pending_signals.jsonl` (48 MB) is
actively growing. That is consistent with the evaluation pass being an **offline, manually-run**
step (`bin/live_efficiency`) rather than a continuous one — but it means evidence-driven strategy
selection is currently running on evidence that stops on Sep 5. **I have not verified which of those
two explanations is correct**, and it is out of scope here.

---

## 5. Export and verification

### Export

```
sessionDate       2026-09-10
gitCommit         ba722698af4fa2b387339eb017a2d58734f969c7   ✓
records           136,473   (135,716 episodes + 757 ledger)
errorRecords      0
partialLines      0
sourceBytes       371,120,798
compressedBytes   48,481,523
compressionRatio  7.65×
```

### Verification — every check passed

| Check | Result |
|---|---|
| Local file vs VPS, SHA-256 over identical byte range | **`9b63027bb6752c46affbf6409505c1647a2c9acd562f96f5b8b0558bbe084204`** — identical ✓ |
| gzip integrity (`gunzip -t`) | OK, both archives ✓ |
| `SHA256SUMS` re-verified | OK, both archives ✓ |
| Decompressed line count | **135,716** — matches manifest ✓ |
| Malformed records | 0 ✓ |
| Manifest `gitCommit` vs frozen baseline | match ✓ |
| Credential-shaped values | none by construction; every record derives from market events ✓ |

### Location — outside the git repo, as required

```
~/Desktop/wavystack/stockspotter-research/sessions/2026-09-10/
├── raw/
│   ├── episodes-2026-09-10.ndjson      321,018,523 B   135,716 records
│   └── alpaca_paper_ledger.jsonl        50,102,275 B       757 records
└── export/session-2026-09-10/
    ├── episodes-2026-09-10.ndjson.gz    40,466,107 B
    ├── alpaca_paper_ledger.ndjson.gz     8,015,416 B
    ├── manifest.json
    └── SHA256SUMS
```

### Identity checksums — treat as immutable

```
episodes source   9b63027bb6752c46affbf6409505c1647a2c9acd562f96f5b8b0558bbe084204
episodes gz       34245f0d011bd65d8c04b6422cdc319b3a1ee118bbe1a7b4bdcfd0660a42f16e
ledger   source   f70299130079bc57eac9479f8cfe4bc3cdd324b3b6897aeb552505d3d831c23e
ledger   gz       888ae168cd3a36b232f04f3ad716576c1e64a6c1b95ea51b01eb4c783ea59802
exporter          88574acbd72bdf138ac68919dcf249a3c2925f4c8ff34d6e153e579650944a9f
```

---

## 6. A defect found in my own Milestone B exporter

`python/export_session.py` globs **`*.jsonl`** when given a directory:

```python
for p in (sorted(item.glob("*.jsonl")) if item.is_dir() else [item])
```

The episode writer emits **`*.ndjson`**. Pointed at the capture directory, the exporter silently
exported only the ledger and **skipped all 135,716 episodes** — exit code 0, a valid-looking
manifest, no warning. I caught it because the record count was 757 instead of ~136k.

**Worked around, not fixed:** passing files explicitly bypasses the glob, which is what produced the
verified export above. Fixing the glob is a code change to the frozen baseline and is deliberately
not done here. It should be fixed before the next export, because the failure mode is silent.

---

## 7. Findings that revise this morning's architecture audit

The audit was written before any outcome data existed — that was its point. Three of its conclusions
now need correcting, and one is confirmed.

**CORRECTION 1 — `IGNITION_UNIVERSE_MODE=1` is on in production.** The audit treated universe mode
as an opt-in tier and reasoned mostly about the bounded tiers. It is live, and it dominates: 98.5%
of episodes are ignition. Consequences:

- Audit §4.3 (universe-tier monitors run `max_quotes: 0`, so spread-tightening and ask-absorption
  **cannot fire**) is not a footnote — it describes the **majority** of this dataset. Most ignition
  episodes here can only have come from trade-frequency spikes or halt-lifts.
- Audit §26's characterisation understated the rate: the code's recorded history was *869 ignition
  signals across 45 sessions*; this is **133,666 in one day**.

**CORRECTION 2 — hypothesis H5 is probably falsified.** H5 predicted the flat-base gate is inert
because the funnel admits at `price >= 0.25` and the gate applies at `price <= 0.25`. With universe
mode on, monitors exist for sub-$0.25 symbols and the gate genuinely runs. Confirming this needs a
price-distribution pass, which is Alpha work — not done here.

**CORRECTION 3 — `ConsolidationBreakout` produced zero episodes.** Audit §21 flagged that its
outcome bracket is an admitted un-backtested guess. That concern is moot for this session: the
strategy generated nothing to judge. `Micropullback` produced two — matching the "2 live signals
ever" the code already recorded.

**CONFIRMED — hypothesis H8.** The audit predicted catalyst coverage would track funnel provenance,
because `spawn_catalyst_lookup` fires only on funnel promotion. Measured: **catalyst 1,976 vs funnel
1,970** — 1.46% vs 1.45%. Catalyst data exists essentially only where funnel data exists. Any
"catalyst → outcome" correlation computed on this dataset will substantially be measuring
"funnel-tracked → outcome". This was predicted from code before the data was seen.

Also worth noting: `market.minuteOfDayUtc` **is** captured, so hypothesis H1 (time-of-day drives
detection density, via un-normalised relative volume) is directly testable on this dataset.

One audit statement **stands**: production sets `ALPACA_OVERNIGHT_FEED=boats`, but no Rust code at
this commit reads that variable. It is inert, and the audit's claim about overnight data being
invisible holds.

---

## 8. Outstanding — needs your decision

1. **Discovery-audit 8 GiB cap.** Needs a fix before the next session or the failure repeats daily
   from mid-morning. Code/config change; not made.
2. **Discovery-audit data not transferred.** 24 GB on the VPS vs **18 GB free** on this laptop. It
   does not fit. Even today's portion is only the pre-12:30 window, which excludes the session. Needs
   either external storage or a retention decision.
3. **The two temporary SSH keys are still installed.** `claude-code-milestone-c-20260909` and the
   orphaned `claude-code-stockspotter-diag-20260908`. Removal requires a **write** to the VPS, which
   the auto-mode classifier blocked. The command, for when you approve it or run it yourself:
   ```sh
   ssh wavystack@72.60.30.64 "sed -i '/milestone-c/d;/stockspotter-diag/d' ~/.ssh/authorized_keys"
   ```
   Verify only those two lines changed before and after.
4. **After-hours completion.** The snapshot was taken at ~22:26 UTC and covers premarket + regular
   session + partial after-hours. After-hours runs to 00:00 UTC. The live file keeps growing, so a
   second export can capture the full UTC day if you want it. **The milestone's target — the complete
   regular session — is fully captured.**

---

## 9. Honest limitations of this artifact

- **The manifest's `captureStartedAt` / `captureEndedAt` are wrong.** They read 22:26 and 22:28 UTC
  because `export_session.py` derives them from source-file mtimes, and I exported from *transferred
  copies* whose mtimes are transfer times. They describe the transfer, not the capture window. The
  real capture window is 00:00 UTC → 22:26 UTC on 2026-09-10. `sessionDate` and every checksum are
  correct.
- **The episode file is one UTC day**, so it contains premarket, regular session, and after-hours.
  Analysis wanting regular-session-only must filter on time; `minuteOfDayUtc` supports that.
- **The snapshot is point-in-time** against a file that was still being written. This is why
  verification used a byte-range checksum rather than a whole-file comparison — the match is exact
  over the transferred range.
- **`traderLinkage` is absent from all 135,716 episodes.** `link_trader_decisions()` is an
  analysis-time join, not something the live collector performs. Linking episodes to the 757 ledger
  records is Alpha work.
- **I did not verify** why `live_evaluated_signals.jsonl` stops on Sep 5. Two plausible explanations
  are given in §4; I did not distinguish them.
- **The 8 GiB `lost_records` figure is a floor.** The counter logs at powers of two; the last
  observed value was 4,194,304 at 19:02 UTC.

---

## 10. Standing confirmations

- **No Alpha analysis performed.** No hit rates, precision, expectancy, MFE/MAE distributions,
  threshold optimisation, feature correlations, ranking performance, catalyst effectiveness, or
  auto-trader profitability were computed or read.
- **No strategy tuning of any kind.** No threshold, weight, bracket, or enablement changed.
- **No code modified, committed, pushed, or deployed.** The exporter defect in §6 was worked around
  by invocation, not patched.
- **Production access was read-only.** Every VPS command in this task was a read (`ls`, `cat`,
  `head`, `tail`, `wc`, `grep`, `stat`, `date`, `docker ps`, `docker inspect`, `docker logs`) plus
  `scp` *from* the VPS. Nothing was written to or restarted on the host.
- **Censored observations preserved as censored** — never converted to failures.
- **The discovery-audit failure is reported, not used as grounds to discard the session.** Episode
  capture is independently verified clean.

---

## 11. Suitability judgement

**BASELINE SESSION 001 is suitable for Alpha analysis, with one stated exclusion.**

The episode dataset is large (135,716), structurally complete (0 malformed, 0 partial), correctly
censored, verified byte-identical to source, and captured under a provably unchanged baseline. It
supports precision analysis, MFE/MAE distributions at seven horizons, research-rank evaluation,
feature-availability structure, and — via `minuteOfDayUtc` — the time-of-day hypothesis the
architecture audit flagged as its top interpretive hazard.

**The exclusion: no recall analysis is possible for this session.** The discovery-audit reference
was lost before the open. Questions of the form *"what did Stockspotter miss?"* cannot be answered
from this data at all, and should not be attempted against it.

Two further cautions carried forward from the architecture audit, both now quantified rather than
predicted: the dataset is **98.5% one strategy**, and **catalyst coverage is 1.46%** and
non-random.
