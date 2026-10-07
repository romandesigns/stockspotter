# Consumer-received observation: operations

Observation is **off** unless `OPPORTUNITY_OBSERVATION=1` is set. Nothing in this directory enables it.

## Persistent root

| | |
|---|---|
| Container path | `/app/data/observation` (`OPPORTUNITY_OBSERVATION_ROOT`, set in `ops/vps/docker-compose.yml`) |
| Host path | `/opt/apps/stockspotter/data/observation`, on the same persistent `../../data` mount as research and discovery |
| Retention | **Its own.** Research retention scans only `data/research`; discovery retention scans only `data/discovery-audit`; neither recurses. The observer refuses a root inside either tree (`RETENTION_MANAGED_DIRS`) |

**Provisioning** is one-time and done by an operator, never by the observer:

```sh
mkdir -p /opt/apps/stockspotter/data/observation
echo "provisioned $(date -u +%FT%TZ)" > /opt/apps/stockspotter/data/observation/.observation-root
```

The observer **never creates the root or the marker**. If either is missing, it logs and stays off, and it re-checks at every session rollover. An unmounted volume therefore cannot silently put a capture on the container's ephemeral layer.

**Capacity** has two layers:
- **In-band:** the capture-level budget (`OPPORTUNITY_OBSERVATION_MAX_BYTES`, spanning every rotated file). It warns at 60% (`run_end.captureWarning` plus a log line) and fails closed at 100%.
- **Out-of-band:** monitor the root like the other data trees, for example `du -sb data/observation` and `df -B1 /` in the existing health checks. The budget bounds one session; the export policy below bounds retained sessions.

## Runs and sessions

- **One run per market day.** Runs roll over at **20:10 America/New_York** (`next_rollover_after`), off the consumer thread, without restarting Stockspotter. A run therefore opens before the 04:00 ET open of the day it covers and closes after the 20:00 ET extended close.
- **Layout:** each run is a directory `<namespace>-<pid>-<start>-<n>/` holding `observations-0.ndjson`, `observations-1.ndjson`, … rotated at 256 MiB on the writer thread.
- **Evidence:** a run is evidence only once closed. An active run reads INDETERMINATE.

## Preregistration binding

- **What to set:** `OPPORTUNITY_OBSERVATION_PREREG_PATH` points at the frozen `step4-preregistration-v1.json`.
- **Startup check:** the observer refuses to start if the artifact's operational constants differ from the build's. Otherwise it records the artifact's RFC 8785 SHA-256 and the build commit (`STOCKSPOTTER_COMMIT`) in every `run_start`.
- **Computing the identity:**
  ```sh
  python3 ops/observation/observation_archive.py prereg-sha step4-preregistration-v1.json
  ```
- **Test fixture:** `step4-preregistration-v1.fixture.json` is a **test fixture**, not the frozen artifact.

## Post-close compression (`observation_archive.py`)

```sh
python3 ops/observation/observation_archive.py compress <run_dir>   # closed files only
python3 ops/observation/observation_archive.py verify   <run_dir>   # exit 1 on any failure
```

Per closed file:
1. SHA-256 the source;
2. record it in `archive-manifest.json` **before** compressing;
3. gzip to `.gz.part` and fsync it;
4. decompress and re-hash;
5. rename to `.gz` and mark `verified` only if the hashes match.

Active files are never compressed, and **the uncompressed source is never deleted by this tool**.

## Export and retention (PROPOSED, not implemented: nothing deletes anything)

**VPS holds at most:**
- the active uncompressed session;
- plus the **two most recent verified, compressed, closed sessions**.

**H:\ durable archive** (`H:\wavystack\stockspotter-research\observation-archive\<run_id>\`), per completed session:
- the compressed files;
- `archive-manifest.json`, which records the source SHA-256 of every uncompressed file;
- the certificate (`assess` output);
- a copy of the preregistration artifact and its SHA;
- an **export receipt** `export-receipt.json`: destination path, destination SHA-256 of every copied file, copy time.

**A VPS session becomes deletion-eligible only when ALL hold:**
1. the off-box copy is complete (every manifest entry present at the destination);
2. the destination hash of every compressed file equals the manifest's `compressedSha256`;
3. `verify` passes **at the destination**: decompression reproduces every `sourceSha256`;
4. the certificate was produced (PASS, FAIL or INDETERMINATE: the verdict is evidence either way) and archived;
5. the export receipt exists at the destination and names each of the above.

Eligibility is computed by tooling. **Deletion stays a manual, separately authorized act** until automatic deletion is reviewed and approved.

## Outcome evidence (Step 4B-main.1; offline, behind the firewall)

Nothing here has fetched or evaluated a real outcome.

- **Policies are data, bound by SHA.**
  - `trade-condition-policy-v2` classifies conditions per tape.
  - `trading-status-policy-v1` classifies statuses per tape family.
  - The real candidates are the `*.proposed.json` files; tests use the
    `*.fixture.json` files. The raw metadata they were derived from is
    under `metadata/`, byte-exact.
- **Status evidence is stored raw** (every SIP status message). The policy
  assigns meaning at evaluation time:
  - HALT, PAUSE and NON_TRADABLE open an interruption; RESUME ends it;
    INFORMATIONAL changes nothing.
  - An unclassified status opens an *unclassified* interval.
  - Any interruption overlapping `(T0, T0+300s]` censors, unless the target
    was already reached.
- **Adapter** (`observation::outcome`):
  - Builds Alpaca v2 historical trades requests: `feed=sip`, `sort=asc`,
    paginated.
  - Refuses a request before its session's close, and refuses without an
    `OutcomeAccess`.
  - Transport is a trait. No HTTP implementation ships; it is added only at
    the gated fetch step.
- **Archive** (`outcome-evidence-v1`):
  - Every HTTP exchange body is kept with its SHA-256, and the page chain is
    checked.
  - Normalised trades are re-derived from the raw pages on every load.
  - Directories are never overwritten. An incomplete fetch carries
    `INCOMPLETE` and loads as incomplete evidence.
- **Campaign** (`observation::campaign`): an event-sourced ledger with states
  COLLECTING, CAPTURE_SET_CLOSED, OUTCOME_FETCH_AUTHORIZED, ANALYSIS_READY
  and MEASUREMENT_INSUFFICIENT.
  - `OutcomeAccess` exists only after closure *and* an explicit
    authorization event.
  - Fetch, load and evaluate all require it.
- **Freeze tooling** (`prereg_freeze.py`, prepared and unused):
  - `generate` refuses every unresolved placeholder, and any freeze that is
    not FINAL.
  - `manifest` re-derives and binds every identity: preregistration,
    implementation, protocol/gate, condition table, status policy, fetch
    contract, certificate semantics and campaign rules.

## Session-bounded OI reconciliation (Step 4B-main.3)

The OI research writer now closes each **Step-4 session**, in-band, without
the process exiting. A Step-4 session `d` is the observation run
`[20:10 ET d-1, 20:10 ET d)` (`observation::step4_session_of`); it is never
the UTC date. The engine's `sessionDate` is a UTC date and is left unchanged.

- **Tagging.** Every OI row is tagged with the session of its own ranking
  timestamp. The bytes on disk are unchanged.
- **Per-session tally.** Counts `attempted`/`dropped`/`lossSpans` at the
  producer and `written`/`writeErrors`/`flushErrors` on the writer thread.
  The tally is kept apart from the process-cumulative health.
- **Barrier.** At the boundary (driven by the 1 s tick), a barrier travels
  the same FIFO as the rows. On the writer thread it:
  1. flushes every buffer;
  2. `sync_data`s each file holding the session's rows;
  3. writes and syncs `oi_session_finished`, which carries the tally, each
     byte range with its SHA-256 and row count, the barrier result, and the
     engine deltas (scores, windows, truncations, evictions, dropped markers)
     over the session;
  4. records process, implementation, config and schema identity, and how
     accounting began and ended.

  The producer never blocks: a full queue defers the barrier, never drops it.
- **Certification.** `oi_extract::extract` (`d6-oi-extract-v2`) certifies a
  session from its marker alone. The rules are in the doc comment, and the
  proof suite is `observation_main3_tests.rs`. A restart inside a session,
  or a session with no durable marker, fails closed.
- **Legacy mode.** Process-close extraction remains
  `extract_legacy_process_close`, labelled `d6-oi-extract-v1-legacy-process-close`.
  It is for historical captures (before this change) only. The Step-4 join
  refuses it.
