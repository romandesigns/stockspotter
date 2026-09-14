# MEASUREMENT BASELINE VERIFICATION — 2026-09-10

**Read-only verification. No source, config, credential, or dataset was modified. No staging, commit,
push, deploy, SSH, or production access occurred. This report is the only file created.**

**Evidence labels used throughout:**
`[CODE]` verified from code · `[DATA]` verified from local data · `[REPORTED]` reported but not
independently verified · `[INFER]` inference

---

## 1. Executive verdict

**The artifacts are intact. The measurement is partially defective. Do not begin outcome analysis at
long horizons until the collector is repaired.**

Integrity is clean: every checksum, gzip archive, and decompressed byte stream matches, 135,716
episode records parse with **zero** malformed lines, and the export manifest records the correct
baseline commit. Checksum validity is not measurement validity, and the two diverge here.

Three defects found, in severity order:

1. **The 1800-second horizon is structurally unobservable.** `settle_due` finalises a pending episode
   `OUTCOME_WINDOW_SECS` (1800 s) after it *opened*, and the longest horizon needs a price at or after
   `signal_at + 1800 s`. The settle boundary and the horizon target are the same instant. Observed:
   **76 of 135,716 (0.056%)**. This is a boundary collision, not data scarcity. `[CODE]``[DATA]`
2. **Forward paths can contain prices unknowable at their recorded timestamp.** `BarUpdate`
   contributes `(bar-open timestamp, bar-close price)`, so a price is stamped up to 60 s before it
   existed. Live sub-minute updates broadcast every 500 ms all share one `bucket_start` timestamp.
   This contaminates horizon sampling and MFE/MAE. `[CODE]`
3. **Discovery capture has no rotation on cap.** The 8 GiB limit is scoped per
   *(UTC day × process run)* file; on reaching it the writer drops records silently until the UTC day
   changes. `[CODE]`

The previously reported explanation for long-horizon censoring — that it reflects horizon
reachability within an episode's observed span — is **incorrect**, and so is the implication that the
2,048-point path cap is responsible. Both are corrected in §5.

**Session 001 remains usable** for short-horizon work (30–300 s), with the causality caveat in §4
Finding F4 applied. It is not usable for 1800 s outcomes at all.

---

## 2. Verified Git baseline

| Property | Value | Label |
|---|---|---|
| Repository | `/Users/wavystack-ios/Desktop/wavystack/stockspotter` | `[DATA]` |
| Current branch | `release/operating-run-20260907` | `[DATA]` |
| HEAD | `ba722698af4fa2b387339eb017a2d58734f969c7` | `[DATA]` |
| Expected baseline | `ba722698af4fa2b387339eb017a2d58734f969c7` | — |
| **HEAD vs baseline** | **IDENTICAL** | `[DATA]` |
| Baseline object exists locally | yes (`git cat-file -t` → `commit`) | `[DATA]` |
| Tracked-file diff vs HEAD | none | `[DATA]` |
| Staged changes | none | `[DATA]` |
| Untracked | `.claude/`, `AUDIT-2026-09-09.md` | `[DATA]` |
| Remotes | `origin` → `https://github.com/romandesigns/stockspotter.git` | `[DATA]` |
| Credentials in remote URL | **none present** | `[DATA]` |

### Checkpoint ancestry

```
git merge-base --is-ancestor 725179b ba722698…   → ANCESTOR ✓
git merge-base --is-ancestor 0544790 ba722698…   → ANCESTOR ✓
```

```
ba72269 (HEAD -> release/operating-run-20260907, origin/release/operating-run-20260907,
         origin/gpt/audit-remediation-20260909, gpt/audit-remediation-20260909)
        Build causal alpha measurement pipeline
0544790 Gate release deployments on remote CI lineage
725179b Harden authentication and realtime stream resilience
bafae88 Sign in once per device instead of re-entering the key every launch
54313cc Operate discovery coverage recording with reconciled Alpaca paper trades
```

**No branch switch or working-tree change was made.** HEAD already equalled the baseline, so all code
inspection below reads the baseline directly.

---

## 3. Artifact integrity results

Artifacts: `~/Desktop/wavystack/stockspotter-research/sessions/2026-09-10/`

| Check | Result | Label |
|---|---|---|
| `shasum -c SHA256SUMS` | both archives **OK** | `[DATA]` |
| `gunzip -t` | both archives **OK** | `[DATA]` |
| Decompressed vs raw (`gunzip -c … \| cmp -`) | **byte-identical**, both | `[DATA]` |
| Raw episodes SHA-256 | `9b63027bb6752c46affbf6409505c1647a2c9acd562f96f5b8b0558bbe084204` — matches manifest `sourceSha256` | `[DATA]` |
| Raw ledger SHA-256 | `f70299130079bc57eac9479f8cfe4bc3cdd324b3b6897aeb552505d3d831c23e` — matches manifest `sourceSha256` | `[DATA]` |
| Manifest `gitCommit` | `ba722698…` — matches verified baseline | `[DATA]` |
| Episode records parsed | **135,716**, **0 malformed**, final newline present | `[DATA]` |
| Ledger lines parsed | **757**, **0 malformed** | `[DATA]` |
| Manifest `partialLines` | 0 — consistent with independent parse | `[DATA]` |

**Expected counts confirmed:** 135,716 episodes ✓ · 757 ledger lines ✓ — but see Correction C4: the
757 are *account snapshots*, not 757 trades.

### Timestamp ranges derived from records `[DATA]`

| Field | Min | Max | Coverage |
|---|---|---|---|
| `openedAt` | `2026-09-10T00:00:00.269978570Z` | `2026-09-10T21:56:00Z` | 135,716 / 135,716 |
| `closedAt` | `2026-09-09T23:59:00Z` | `2026-09-10T22:19:25.842020039Z` | 135,716 / 135,716 |
| `openingContext.capturedAt` | `2026-09-10T00:00:00.045764099Z` | `2026-09-10T21:56:00.017906766Z` | **135,716 / 135,716** |
| `openingContext.detectedAt` | — | — | **135,716 / 135,716** |

### Session-window inclusion, by `openedAt` (UTC; 2026-09-10 is EDT, so regular = 13:30–20:00 UTC) `[DATA]`

| Window | Episodes | Share of 135,716 |
|---|---:|---:|
| Overnight (00:00–08:00) | 16 | 0.01% |
| Premarket (08:00–13:30) | 5,123 | 3.78% |
| **Regular (13:30–20:00)** | **128,452** | **94.65%** |
| After-hours (20:00–24:00) | 2,125 | 1.57% |
| **Total** | **135,716** | 100% |

All three requested windows are present. The capture is a partial UTC day: it ends at
`openedAt` 21:56 UTC, so after-hours is truncated (after-hours runs to 00:00 UTC).

---

## 4. Findings

### F1 — The 1800 s horizon is structurally unobservable `[CODE]` `[DATA]` — **CRITICAL**

**Source:** `crates/ws-server/src/measurement.rs:56`, `:301–310`, `:343`;
`crates/backtest-metrics/src/horizon.rs:31–33`, `:186–200`

```rust
// measurement.rs:56
const OUTCOME_WINDOW_SECS: i64 = 1800;

// measurement.rs:301–310  (settle_due)
let due = (now - self.pending[index].episode.opened_at).num_seconds() >= OUTCOME_WINDOW_SECS;
```

```rust
// horizon.rs:186–200  (sample_at — the causal sampling rule)
let target_time = signal_at + Duration::seconds(horizon_secs);
if let Some((_, price)) = path.iter().find(|(t, _)| *t >= target_time) { … }
```

`HORIZON_SECS` includes `1800`. `signal_at` is the episode's `opened_at`. Settlement fires when
`now - opened_at >= 1800`, which is **the same instant** the 1800 s horizon requires a price at or
after. Forward accumulation therefore stops exactly at the boundary the longest horizon needs.

The only way to observe it is a price arriving in the same `observe()` call *before* `settle_due`
runs — `extend_paths` executes first (`measurement.rs:337` region), `settle_due` at `:343`. That race
window explains the tiny non-zero count.

**Reproduce:**
```sh
cd ~/Desktop/wavystack/stockspotter-research/sessions/2026-09-10/raw
python3 -c "$(cat <<'P'
import json,collections
h=collections.defaultdict(collections.Counter)
for l in open('episodes-2026-09-10.ndjson','rb'):
    if not l.strip(): continue
    for e in (json.loads(l).get('outcome') or {}).get('returns',[]):
        h[e['horizonSecs']]['observed' if 'observed' in e['outcome'] else 'censored']+=1
[print(k,dict(h[k])) for k in sorted(h)]
P
)"
```

**Result:** 1800 s observed **76 / 135,716 = 0.056%**, against 900 s at 51.48%. A 900× drop across one
grid step is not decay.

### F2 — The 2,048-point cap is NOT the cause `[CODE]` `[DATA]`

**Source:** `crates/ws-server/src/measurement.rs:65`, `:270–284`

The cap *can* truncate a path (`entry.path.len() < MAX_PATH_POINTS` guard), so in principle it can
prevent a later horizon being reached. Empirically it does not matter here:

| Population | Episodes | 1800 s observed | 900 s observed |
|---|---:|---:|---:|
| At path cap (`observationCount` ≥ 2049) | 17 | 0 | 5 |
| At pending cap (`observationCount` = 2048) | 950 | — | — |
| **Not capped** | **134,749** | **76** | **69,859** |

Capped episodes are **967 of 135,716 = 0.71%**. The 1800 s failure occurs overwhelmingly in the
*uncapped* population. **The cap is a real mechanism with negligible effect on this dataset.**

### F3 — Forward observation *does* continue after episode close `[CODE]`

**Source:** `crates/ws-server/src/measurement.rs:270–284`, `:337–345`

```rust
// extend_paths — pushes into every pending episode, not just open ones
for entry in self.pending.iter_mut() {
    if entry.episode.id.symbol == symbol && entry.path.len() < MAX_PATH_POINTS {
        entry.path.push((at, price));
    }
}
```
The collector's own comment states it: *"A closed episode is not finished being measured — it moves to
the pending set and keeps collecting forward prices."* **Episode closure is therefore not a cause of
horizon censoring.** This eliminates one of the candidate explanations the task asked me to test.

### F4 — Outcomes can include prices unavailable at the recorded time `[CODE]` — **CAUSALITY**

**Source:** `crates/ws-server/src/measurement.rs:188–202`; `crates/market-data/src/live.rs` Bar and
Trade handlers; `packages/shared-types/src/index.ts` `BarUpdate`

```rust
// measurement.rs:197–199
ScanEvent::BarUpdate { symbol, timestamp, close, .. } => Some((symbol.clone(), *timestamp, *close)),
```

Two distinct problems:

1. **Final 60 s bars.** `BarUpdate.timestamp` is the bar **open**; `close` is the price at bar **end**.
   The path therefore records a price stamped up to **60 s before it was knowable**. By contrast the
   bar-derived *detection* events (`FunnelSignal`, `MomentumUpdate`, `ConsolidationEvent`) are stamped
   `bar.timestamp + 1 minute` — bar close. The two conventions disagree by one bar.
2. **Live sub-minute updates.** Non-final `BarUpdate` is broadcast every 500 ms with
   `timestamp = state.bucket_start`, so **many path points share a single timestamp** while carrying
   prices from progressively later moments in the bucket.

`sample_at` takes the *first* point with `t >= target_time`, and `compute_excursion` scans all points.
Both can therefore consume a price that did not exist at the timestamp attached to it. Bound on the
error: ≤60 s for 1-minute buckets, ≤30 s for sub-minute.

**Not quantifiable from this dataset** `[INFER]`: episode records store derived outcomes, not the
paths, so the proportion of sampled points sourced from `BarUpdate` cannot be recovered. Episodes
opened by `IgnitionDetector` (98.5%) take their *opening* price from a trade-stamped event, but their
forward paths may still mix in `BarUpdate` points.

### F5 — `data_gap` censoring applies only to excursion, never to horizons `[CODE]` `[DATA]`

**Source:** `crates/backtest-metrics/src/horizon.rs:130`, `:186–213`, `:215–232`

`sample_at` performs **no gap check** — its censor reasons are `SessionEnded`,
`InsufficientForwardData`, `CaptureEnded`. Only `compute_excursion` tests `MAX_GAP_SECS = 120`.

Confirmed in data: horizon outcomes show **zero** `data_gap`; all 33,564 `data_gap` censors are
excursion. **MFE/MAE is unavailable for 33,564 / 135,716 = 24.73% of episodes**, while the horizon
returns for those same episodes remain observed.

### F6 — `market.minuteOfDayUtc` is present on 1.45% of episodes, not universally `[DATA]`

| Field | Present | Denominator | Share |
|---|---:|---:|---:|
| `openingContext.market` | 1,970 | 135,716 | 1.45% |
| `market.minuteOfDayUtc` | 1,970 | 135,716 | 1.45% |
| `openedAt` | 135,716 | 135,716 | **100%** |
| `openingContext.capturedAt` | 135,716 | 135,716 | **100%** |

`minuteOfDayUtc` exists exactly when `market` context exists. **Regular-session filtering must use
`openedAt`**, which is universal. This directly corrects a claim in the prior report (C3).

### F7 — Six episodes close before they open `[DATA]`

All six are `closeReason: session_boundary` at the UTC day rollover, with
`openedAt = 2026-09-10T00:00:00Z` and `closedAt = 2026-09-09T23:59:00Z` — closed one minute before
opening. Symbols: FTFT, YMAT, SUNE, CULP, UFG, MGN.

**Disclosure of my own method error:** my first detection pass compared ISO timestamps
lexicographically, which is invalid across mixed precision (`…00Z` vs `…00.995…Z`) and produced two
false positives (AVAV, AAPL) that are chronologically fine. Only the six above are genuine. Any
re-run must parse timestamps rather than string-compare them.

### F8 — Discovery cap scope, rotation, and loss accounting `[CODE]`

**Source:** `crates/market-data/src/discovery_audit.rs:23`, `:34`, `:39–41`, `:55–66`, `:68–72`,
`:76–82`

```rust
let run = format!("{}-{}", std::process::id(), Utc::now().timestamp_micros());   // :34
…
let today = record["recorded_at"].as_str().unwrap_or_default()[..10].to_string(); // :55
if today != day {
    file = Some(OpenOptions::new().create_new(true).write(true)
        .open(dir.join(format!("{today}-{run}.jsonl")))?);
    day = today;
    bytes = 0;                                                                    // :65
}
anyhow::ensure!(bytes + encoded.len() <= 8usize * 1024 * 1024 * 1024,
    "8 GiB daily audit cap reached; remaining records will be lost");             // :68–72
```

| Question asked | Answer | Label |
|---|---|---|
| Scope of the limit | Per **file**, and a file is per *(UTC day × process run)* — the filename embeds both | `[CODE]` |
| Reset | `bytes = 0` only on **UTC-day change** (derived from the record's own `recorded_at`, not wall clock) | `[CODE]` |
| Rotation on cap | **None.** On reaching the cap the writer drops every subsequent record until the UTC day changes | `[CODE]` |
| Does restarting change accounting | **Yes.** A restart yields a new `run`, hence a new filename, `create_new` succeeds and `bytes` resets — a fresh 8 GiB for the same day. The `lost` counter also resets, since `RECORDER` is a per-process `OnceLock` | `[CODE]` |
| Loss accounting | `lost: AtomicU64`, incremented per dropped record; logged **only when the count is a power of two** (`:79`) | `[CODE]` |

The power-of-two logging explains the doubling sequence in the previously reported log excerpt
(32,768 → … → 4,194,304) and confirms **those figures are lower bounds on a monotone counter, not
totals**.

**Is the reported premarket exhaustion consistent with the code?** Yes — a single 8 GiB file per day
with no rotation, filled by universe-wide recording, is fully consistent with exhaustion before the
open. `[CODE]` But consistency is not verification.

**Local evidence that regular-session discovery coverage is absent: there is none.** The
discovery-audit files were never transferred; the local session directory contains only
`episodes-2026-09-10.ndjson` and `alpaca_paper_ledger.jsonl`. The 12:30 UTC exhaustion time, the file
size, and the `lost_records` values all come from VPS reads recorded in a prior session and are
**`[REPORTED]`, not independently verified here.** SSH is prohibited in this task, so I did not and
cannot confirm them.

#### Proposed bounded capture design (not implemented, and not "remove the cap")

The requirement is to protect disk while guaranteeing the intended session survives. Four elements:

1. **Rotate instead of stop.** On reaching a per-file budget, close and open the next sequence file
   (`{day}-{run}-{seq}.jsonl`). Enforce the *disk* limit by a retention sweep over the directory, not
   by refusing to write. Loss then becomes a retention decision, not a silent mid-session cliff.
2. **Reserve a session budget.** Split the daily allowance into window sub-budgets — e.g. overnight
   and premarket capped at a fraction, with the remainder reserved and only unlockable at 13:30 UTC.
   The regular session can then never be starved by premarket volume, which is the exact failure
   observed.
3. **Degrade by record class before dropping.** Tier records by analytic value —
   `qualified` / ignition staging (never dropped) above `selection_inputs` full-universe snapshots
   (downsampled first, at a recorded sampling rate). Losing resolution on the bulkiest low-value class
   preserves the reference set that recall analysis depends on.
4. **Make loss in-band and self-describing.** Write an explicit gap marker record into the stream at
   every drop-onset and drop-resume, carrying the counter and the class dropped. A downstream reader
   currently cannot tell a quiet market from a truncated file without reading container logs.

Acceptance criteria for such a design are in §8.

### F9 — Exporter silently excludes `.ndjson` from directory inputs `[CODE]` `[DATA]`

**Source:** `python/export_session.py:142–149`, `:160`

```python
paths = sorted({
    p.resolve()
    for item in args.inputs
    for p in (sorted(item.glob("*.jsonl")) if item.is_dir() else [item])   # :145
    if p.is_file()
})
if not paths:
    raise SystemExit("no capture files found")
```

Confirmed behaviour:

- **Directory input globs `*.jsonl` only.** `episodes-2026-09-10.ndjson` is excluded. `[CODE]`
- **Explicit file input bypasses the glob** (`else [item]`), which is why the verified export exists.
- **The guard only fires when *nothing* matches.** A partial match — the ledger alone — proceeds to a
  clean exit code, a valid manifest, and a plausible-looking artifact. This is the dangerous property:
  the failure is silent, not loud. `[DATA]` (observed: a directory-input run produced 757 records)
- **`:160` names every output `{stem}.ndjson.gz`** regardless of source extension, so
  `alpaca_paper_ledger.jsonl` → `alpaca_paper_ledger.ndjson.gz`. Provenance survives only via the
  manifest's `sourceName`/`sourceSha256`.

Smallest repair and its tests are in §8. **Not patched.**

### F10 — Manifest capture bounds are file mtimes `[CODE]` `[DATA]`

**Source:** `python/export_session.py:162–167`, `:188–189`

```python
stat = path.stat()
modified = datetime.fromtimestamp(stat.st_mtime, timezone.utc)
earliest = min(earliest, modified) if earliest else modified
latest   = max(latest, modified) if latest else modified
…
"captureStartedAt": earliest.isoformat(),
"captureEndedAt":   latest.isoformat(),
```

| Field | Manifest value | What it actually is |
|---|---|---|
| `captureStartedAt` | `2026-09-10T22:26:09.827809+00:00` | mtime of the transferred copy |
| `captureEndedAt` | `2026-09-10T22:28:09.990345+00:00` | mtime of the transferred copy |

The manifest asserts a **2-minute** capture window for a **~22-hour** dataset. Because this export ran
from files produced by `scp`, the mtimes are transfer times and carry no information about capture.

The five distinct time concepts present in this system, and where each actually lives:

| Concept | Source of truth | Coverage |
|---|---|---|
| Market / event time | `BarUpdate.timestamp`, trade timestamps | on the wire |
| Episode opening / closing | `openedAt` / `closedAt` | 100% |
| **Capture / receipt time** | **`openingContext.capturedAt`** | **100%** |
| Export time | `manifest.exportedAt` | correct today |
| File modification / transfer | `st_mtime` | **currently mislabelled as capture** |

**Proposed honest semantics** (no relabelling of event bounds as capture bounds):

- Keep `exportedAt` unchanged — it is already correct.
- Rename the mtime pair to `sourceFileModifiedRange`, described as filesystem metadata.
- Add `eventTimeRange` = min/max of `openedAt`/`closedAt`, derived by streaming the records.
- Add `captureTimeRange` = min/max of `openingContext.capturedAt` — a genuine receipt window that is
  present on every record in this dataset.
- Where a record type carries no capture time, emit `null` rather than substituting a proxy.

### F11 — The ledger cannot reconstruct skip decisions `[DATA]` `[CODE]`

Ledger structure: 757 lines, each `{account_id, trades[]}` — **per-account snapshots, not trade
events.** Trade objects carry `proposal`, `buy`, `sells`, `adjustments`, `exit_reason`.

| Measure | Value | Note |
|---|---:|---|
| Snapshot lines | 757 | verified |
| Trades in final snapshot | 73 | verified |
| Distinct trade signatures across all snapshots | ~188 | heuristic key; treat as approximate `[INFER]` |
| Records containing any skip field | **0** | verified |

`exit_reason` over distinct signatures: `timeout` 31, `stop_hit` 23, `target_hit` 10,
`momentum_deteriorated` 7, absent/open 117.

**No `SkipReason` appears anywhere in the exported artifacts.** In the deployed configuration skips are
emitted to process stdout (`auto_trader::paper_runtime`), which was not captured or exported. The
`Skipped` journal variant writes to `auto_trader_journal.jsonl`, which was **not** part of this export.
Skip-composition analysis is therefore impossible from Session 001 as exported. `[DATA]`

### F12 — Trader linkage is uniformly absent `[DATA]`

`trader.considered == false` on **135,716 / 135,716 (100%)**; the object carries no other key. No
episode carries a `traderLinkage` object. `link_trader_decisions()` is an analysis-time join that the
live collector never performs, so linkage must be reconstructed offline — and, per F11, the exported
ledger lacks the skip side entirely.

### F13 — Strong within-symbol dependence `[DATA]`

| Measure | Value |
|---|---:|
| Episodes | 135,716 |
| Distinct symbols | 5,990 |
| Median episodes per symbol | 11 |
| Max episodes for one symbol | 244 (`AEON`) |
| Symbols with exactly one episode | 729 |

Top symbols: AEON 244, AEO 234, TLT 223, PATH 211, IONQ 210, SMR 209, BAC 204, CIFR 203.

Episodes are **not** independent observations. Any interval or significance treatment that assumes
independence across 135,716 rows will be badly overconfident; the effective sample is nearer the
symbol count, and clustered at that.

---

## 5. Corrections to previous reports

| # | Prior claim | Source report | Correction | Label |
|---|---|---|---|---|
| **C1** | Long-horizon censoring reflects "the *long* horizons (900 s, 1800 s) being unreachable within an episode's observed span — not missing data" | `MILESTONE-C-BASELINE-SESSION-001.md` §4 | **Incorrect for 1800 s.** It is a boundary collision between `OUTCOME_WINDOW_SECS` and the longest horizon (F1). 900 s at 51.5% is consistent with reachability; 1800 s at 0.056% is not. | `[CODE]``[DATA]` |
| **C2** | "950 episodes hit the 2,048 `MAX_PATH_POINTS` cap", implying material impact | same | Cap population is 967 total (950 at 2048, 17 at 2049) = **0.71%**, and the 1800 s failure is overwhelmingly in the uncapped population. The cap is not a cause. | `[DATA]` |
| **C3** | "`minuteOfDayUtc` supports regular-session filtering" | same, §11 + chat | Present on **1.45%** of episodes. Filtering is possible, but via `openedAt` (100%). | `[DATA]` |
| **C4** | "757 paper-ledger records", implying 757 trades | same | 757 **account snapshots**. Final snapshot holds 73 trades. | `[DATA]` |
| **C5** | Censoring reported as a combined 691,204 with 33,564 `data_gap` alongside horizon censoring | same | `data_gap` is **excursion-only**; horizons never carry it. The two must be reported separately (F5). | `[CODE]``[DATA]` |
| **C6** | Discovery cap described as a "daily cap" | same | Per *(UTC day × process run)* **file**; a restart grants a fresh 8 GiB for the same day. | `[CODE]` |
| **C7** | `lost_records = 4,194,304` presented as the loss figure | same | A power-of-two log sample of a monotone counter — a **lower bound**. Also `[REPORTED]` only; not verifiable locally. | `[CODE]` |

Corrections C1, C2, C3, C5 supersede the earlier report because they are supported by code traces and
local record counts reproduced in this document. C6 and C7 supersede on code evidence alone.

**Confirmed, not corrected:** episode count 135,716; `openedBy` distribution; `closeReason`
distribution; zero malformed records; manifest `gitCommit`; all checksums; ignition dominance; catalyst
coverage tracking funnel coverage.

---

## 6. Data-quality tables with explicit denominators

**Denominator for every row below: 135,716 episodes.**

### 6.1 Horizon returns

| Horizon (s) | Observed | Censored `InsufficientForwardData` | Observed % |
|---:|---:|---:|---:|
| 30 | 133,960 | 1,756 | 98.71% |
| 60 | 133,347 | 2,369 | 98.25% |
| 180 | 129,128 | 6,588 | 95.15% |
| 300 | 123,608 | 12,108 | 91.08% |
| 600 | 102,424 | 33,292 | 75.47% |
| 900 | 69,864 | 65,852 | 51.48% |
| **1800** | **76** | **135,640** | **0.06%** |

No horizon carries `SessionEnded`, `CaptureEnded`, or `DataGap` in this dataset.

### 6.2 Time-to-target

| Target % | Observed | Censored | Observed % |
|---:|---:|---:|---:|
| 2.0 | 5,284 | 130,432 | 3.89% |
| 5.0 | 1,384 | 134,332 | 1.02% |
| 10.0 | 445 | 135,271 | 0.33% |

Censoring here is expected — most episodes never reach the target — but it is **confounded with F1**:
a target unreached within the truncated window is indistinguishable from one never reached.

### 6.3 Excursion (MFE / MAE)

| Outcome | Count | Share |
|---|---:|---:|
| Observed | 102,152 | 75.27% |
| Censored `DataGap` | 33,564 | 24.73% |

### 6.4 Feature-context coverage

| Group | Present | Share |
|---|---:|---:|
| `preDetection` | 135,716 | 100% |
| `capturedAt` / `detectedAt` | 135,716 | 100% |
| `ignition` | 135,577 | 99.90% |
| `halt` | 41,626 | 30.67% |
| `momentum` | 17,866 | 13.16% |
| `catalyst` | 1,976 | 1.46% |
| `funnel` | 1,970 | 1.45% |
| `market` / `minuteOfDayUtc` | 1,970 | 1.45% |
| `researchRank` | 60,665 | 44.70% |
| `traderLinkage` | 0 | 0% |

### 6.5 Episode composition

| `openedBy` | Count | Share | | `closeReason` | Count |
|---|---:|---:|---|---|---:|
| IgnitionDetector | 133,666 | 98.49% | | invalidated | 124,630 |
| MomentumScorer | 1,078 | 0.79% | | inactivity | 11,080 |
| FastFunnel | 970 | 0.71% | | session_boundary | 6 |
| Micropullback | 2 | 0.001% | | | |
| **ConsolidationBreakout** | **0** | **0%** | | | |

### 6.6 Forward observation counts

| Statistic | Value |
|---|---:|
| Minimum | 1 |
| Median | 87 |
| Maximum | 2,049 |
| Episodes with exactly 1 (no forward data) | 411 (0.30%) |
| Episodes at or above cap (2048 or 2049) | 967 (0.71%) |

---

## 7. Analysis-readiness assessment

### Reliable

- **Short-horizon forward returns (30 s – 300 s)** — ≥91% observed at every step, with correct
  censoring types. Subject to the F4 causality caveat.
- **Episode composition and lifecycle** — `openedBy`, `closeReason`, timing, session windows. Complete
  and internally consistent.
- **Feature-availability structure** — a finding in its own right, fully measurable.
- **Session filtering** — via `openedAt` (100% coverage), not `minuteOfDayUtc`.

### Conditional

- **600 s / 900 s returns** — 75% / 51% observed. Usable only with censoring modelled explicitly;
  the censored half at 900 s is not missing-at-random (it correlates with symbol activity, and thereby
  with tier).
- **MFE / MAE** — unavailable for 24.73% of episodes, and `DataGap` correlates with thin, inactive
  symbols. Conditioning on availability selects a more-active subpopulation.
- **Cross-strategy comparison** — `ConsolidationBreakout` has zero episodes and `Micropullback` two.
  Only `IgnitionDetector` has usable volume; `MomentumScorer` (1,078) and `FastFunnel` (970) are thin.
- **Any inferential statistic** — requires clustering by symbol (F13). 135,716 rows over 5,990 symbols,
  median 11 and max 244 per symbol.

### Not supported

- **1800 s outcomes** — 0.056% observed. Do not analyse; do not aggregate; do not treat the 76
  observations as a sample, since surviving the race window is itself a selection mechanism.
- **Recall / missed-runner analysis** — regular-session discovery coverage is `[REPORTED]` absent and,
  regardless, no discovery data exists locally. Nothing in Session 001 supports "what did the scanner
  miss".
- **Auto-trader skip composition** — no skip record exists in the exported artifacts (F11).
- **Episode → trade attribution** — `trader.considered` is false on 100% of episodes and no linkage
  object exists (F12). The ledger's 73–188 trades cannot be joined to episodes without an offline
  reconstruction whose skip side is missing.
- **Anything requiring independence across episodes** — see F13.

### Cross-cutting cautions

**Ignition dominance:** 98.49% of episodes come from one detector, which in the deployed configuration
runs universe-wide with `max_quotes: 0` for universe-tier symbols. Any dataset-level statistic is
essentially a statement about that one detector in that one mode.

**Non-random feature availability:** catalyst (1.46%) tracks funnel (1.45%) almost exactly. Any
"catalyst effect" measured here is substantially a funnel-membership effect. Halt (30.67%) and
momentum (13.16%) are similarly tier-dependent.

**No inference of missing events, trades, or outcomes** has been made anywhere in this report.

---

## 8. Minimal proposed repair batch

Ordered by severity. **Nothing below has been implemented.**

### R1 — Decouple the settle window from the horizon grid (fixes F1)

**Change:** make the pending-settle deadline strictly exceed the longest horizon, and measure it from
a defined anchor. Minimum viable: `OUTCOME_WINDOW_SECS = HORIZON_SECS.max() + margin`, with the margin
large enough to admit a real observation past the final target.

**Acceptance criteria**
- 1800 s observed share on a replayed capture rises from ~0% to the same order as 900 s.
- No horizon's target time equals the settle boundary for any episode.
- Short horizons (30–300 s) are unchanged, byte-for-byte, on a fixed replay fixture.

**Tests**
- Unit: an episode whose path extends to `signal_at + longest + margin` observes *every* horizon.
- Unit: an episode settled at exactly `signal_at + longest` censors the longest horizon — pinning the
  old bug as a regression guard.
- Property: for any path, `observed(h)` is monotone non-increasing in `h`.

### R2 — Fix path-point time semantics (fixes F4)

**Change:** contribute forward path points at the time the price was *knowable*. For final bars, stamp
the close at bar end (`timestamp + interval`); for live buckets, stamp at broadcast time rather than
`bucket_start`. Alternatively exclude `BarUpdate` from paths where trade-stamped prices exist.

**Acceptance criteria**
- No path contains two points with an identical timestamp and different prices.
- For every sampled point, `point.t` ≥ the instant that price could first be observed.
- Detection timestamps and path timestamps use one documented convention.

**Tests**
- Unit: a 60 s bar produces a path point at bar close, not bar open.
- Unit: N live updates within one bucket produce N strictly increasing timestamps.
- Regression: a fixture asserting no horizon return is computed from a price stamped before it existed.

### R3 — Exporter: accept `.ndjson` and fail loudly on omission (fixes F9)

**Smallest repair:** glob both extensions for directory inputs.

**Acceptance criteria**
- Directory input containing both `.jsonl` and `.ndjson` exports both.
- Explicit-file input continues to work for both extensions.
- Empty/no-match input exits non-zero with a clear message (existing behaviour preserved).
- An expected-but-absent dataset causes a **loud failure**, not a clean exit — e.g. an `--expect`
  flag naming required groups, validated against `classify()` output.
- Manifest `records`, `sourceName`, `sourceSha256` remain accurate per file.

**Tests**
- Directory with `.jsonl` only → exported.
- Directory with `.ndjson` only → exported *(fails today)*.
- Directory with both → both exported, counts correct.
- Explicit file, each extension → exported.
- Empty directory → non-zero exit.
- `--expect episodes` with no episode file → non-zero exit *(new)*.
- Provenance: `sourceSha256` matches an independent `shasum` of the input.

### R4 — Honest manifest time semantics (fixes F10)

**Change:** as specified in F10 — keep `exportedAt`; rename mtimes to `sourceFileModifiedRange`; add
record-derived `eventTimeRange` and `captureTimeRange`; `null` where unavailable.

**Acceptance criteria**
- No field labelled "capture" is derived from filesystem metadata.
- `captureTimeRange` on this dataset reproduces `2026-09-10T00:00:00.045764099Z` →
  `2026-09-10T21:56:00.017906766Z`.
- Manifest `schemaVersion` increments; readers of v1 are not silently mis-fed v2.

**Tests**
- Export from files whose mtimes differ from record times → the three ranges differ and each is correct.
- A record type without `capturedAt` yields `null`, not a substituted proxy.

### R5 — Bounded discovery capture (fixes F8)

**Change:** the four-element design in F8 — rotate-with-retention, reserved session budget, class-based
degradation, in-band gap markers.

**Acceptance criteria**
- A simulated premarket flood does **not** prevent regular-session recording.
- Total directory size stays under a configured ceiling across a multi-day run.
- Every dropped span is discoverable from the data files alone, without container logs.
- Sampling rate, when degradation engages, is recorded in-band.
- A process restart does not silently grant a fresh budget for a day already at its limit.

**Tests**
- Unit: byte accounting resets on UTC-day change and **not** on restart within the same day.
- Unit: reaching a file budget rotates rather than drops.
- Unit: retention sweep removes oldest files, never the current one.
- Unit: gap markers bracket every dropped span with onset/resume and counts.

**Sequencing note.** R1 and R2 change measurement semantics, so any data captured before them is not
comparable to data captured after. If Session 001 is to remain the baseline, that discontinuity must be
recorded deliberately — or the baseline re-captured after repair.

---

## 9. Remaining uncertainties and unavailable evidence

**Unavailable in this task (SSH and production access prohibited):**

- VPS discovery-audit files, sizes, and modification times.
- The container logs containing the 8 GiB cap errors and their `lost_records` values.
- The 12:30 UTC exhaustion time and the claim that regular-session discovery coverage is absent.
- Live process state, environment variables, and container uptime.

All of these are `[REPORTED]` from a prior session's reads. **I did not re-verify any of them, and
this report does not treat them as established.**

**Genuine uncertainties:**

1. **F4's magnitude on this dataset.** Paths are not stored in episode records, so the fraction of
   sampled points originating from `BarUpdate` is unrecoverable. The defect is certain; its effect
   size here is not. `[INFER]`
2. **Authoritative distinct-trade count.** The ledger's snapshot structure and my heuristic signature
   give 73 (final snapshot) versus ~188 (distinct signatures). These are not reconcilable without
   knowing whether snapshots are cumulative or windowed, which I did not establish.
3. **Whether `live_evaluated_signals.jsonl` staleness is by design.** Two explanations were offered in
   the prior report; neither was tested, and the file is not part of this export.
4. **The six inverted-timestamp episodes.** The mechanism at the UTC rollover was not traced; only the
   occurrence is verified.
5. **Whether the 76 observed 1800 s outcomes are usable.** They survive a race condition, so they are
   a biased sample of unknown character. I recommend discarding rather than modelling them.
6. **Whether after-hours truncation matters.** The capture ends at `openedAt` 21:56 UTC; after-hours
   runs to 00:00 UTC. Whether the missing window matters depends on analyses not yet specified.

---

## 10. Confirmation of scope

- **Only one file was created:**
  `~/Desktop/wavystack/stockspotter-research/reports/MEASUREMENT-BASELINE-VERIFICATION-2026-09-10.md`
  It did not previously exist; no suffix was required; no existing report or dataset was modified,
  moved, or deleted.
- **No source code, configuration, credential, or dataset was modified.** No file in the repository was
  written. `git status` is unchanged from the start of this task.
- **No staging, commit, push, deploy, SSH, or production access occurred.** Every command was a local
  read: `git` inspection, `shasum`, `gunzip -t`, `cmp`, `sed`, `grep`, and streaming `python3` readers
  over stdin heredocs. No temporary or decompressed copies were written — archive verification used
  `gunzip -c | cmp -` streaming.
- **No credentials were printed.** The remote URL was redaction-filtered before display and was found
  to contain none.
- **No strategy tuning, profitability, hit-rate, expectancy, ranking-versus-return, or recommendation
  analysis was performed.**
- **No missing events, trades, or outcomes were inferred.** Absences are reported as absences.

**Stopping here, pending explicit repair instructions.**
