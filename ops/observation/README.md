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
