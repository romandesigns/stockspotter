# Discovery coverage measurement: implementation and first capture

The scanner now has an opt-in prospective evidence recorder and an independent candidate report. This closes a measurement gap: the old fixed-symbol history cannot tell us which intended candidates were absent from coverage. It does not yet establish an improved catch rate.

The primary label is a $0.25–$3 five-minute sampled flat base followed by a 10% rise within twenty minutes. It does not require a funnel pass, quiet-watch selection, float or an alert. This is the existing specification's quiet-to-running cohort, chosen before examining outcomes; the precise diagnostic definition and limitations are in [the protocol](discovery-coverage-protocol-2026-09-07.md). It is not a new trade-entry rule.

## First real capture

The read-only Alpaca SIP census completed at 17:31:54 UTC on September 7. It made asset/snapshot requests only: no FMP requests, subscriptions, orders, or deployment changes.

| Check | Result |
| --- | ---: |
| Requested universe symbols | 12,934 |
| Symbols with a returned raw snapshot | 12,628 |
| Missing snapshot responses | 306 |
| Snapshots usable by the existing price/previous-bar converter | 12,618 |
| Raw snapshots rejected for stale trade timestamps | 12,627 |
| Raw snapshots lacking a valid price/timestamp | 1 |
| Audit records | 67 |
| Recorded losses / incomplete snapshot scans | 0 / 0 |
| Captured bytes | 2,678,962 |

The analyzer returned **insufficient evidence** with whole-market recall left null. All underlying latest trades failed freshness/validity checks; a one-shot capture also provides neither the required time window nor actual scanner coverage. Zero labeled candidates here is not evidence that the scanner missed none. The [machine-readable report](discovery-smoke-analysis-2026-09-07.json) includes capture and analysis-source SHA-256 hashes.

## What the recorder measures

- Original snapshot prices and market timestamps across the requested universe, including symbols never selected by the scanner.
- Completed quiet-watch and funnel decisions, low-price selection inputs, and float-budget status.
- Fifteen-second snapshots of configured ignition/momentum monitors and their funnel, mover, quiet and confirmed ownership.
- The first actual trade receipt per symbol per heartbeat interval, with receipt time, market time and whether ignition processing was enabled.
- Candidate, confirmed and rejected ignition events, with separate receipt and market timestamps.

Configured monitors do not prove broker subscription acknowledgement. An observed fresh, monitored trade does prove receipt at that instant; absence remains unknown. Heartbeat changes expose tier transitions but cannot precisely timestamp every intervening subscription change. The analyzer reports first confirmed alert receipt before the observed +10% crossing, not a backdated alert or a simulated fill. It never claims an executable opportunity from snapshots.

## Running it

From the repository root, a one-shot collection check is:

```powershell
$env:DISCOVERY_AUDIT_DIR='data/discovery-check'
cargo run --offline --locked -p market-data --bin discovery_snapshot
python python/analyze_discovery.py data/discovery-check --output data/discovery-check/report.json
```

For actual discovery evidence, set `DISCOVERY_AUDIT_DIR=data/discovery-audit` in the scanner process environment and run the updated `ws-server` through the existing procedure during market sessions. In the VPS container, use `/app/data/discovery-audit`, which is under its existing persistent data mount. The recorder needs the updated scanner binary; setting the variable on an old deployment has no effect. Keep paper-trader activation separate; collecting scanner evidence requires no trader process.

After a capture:

```powershell
python python/analyze_discovery.py data/discovery-audit --output data/discovery-report.json
```

Python's pinned `tzdata` dependency supplies America/New_York rules on Windows. Audit files are separate per process and UTC day. The writer's 32-record queue avoids disk waits in tick dispatch. The 8 GiB per-process daily cap prevents unbounded single-day writes; old files are retained. Plan disk capacity: raw snapshots alone at this capture size and a fifteen-second cadence approach 4 GB per regular session. Queue saturation, disk failures and truncation invalidate completeness claims and are surfaced as audit gaps. The one-shot utility drains and synchronizes its output before reporting success.

## Validation and remaining evidence

The affected auto-trader, market-data and ws-server suites passed 139 tests; a subsequent timestamp-preservation regression brought market-data to 74 tests, making 140 passing tests across those three packages. Eight Python tests passed for independent candidate inclusion, freshness, censorship of missing paths, late alerts, unknown coverage, crossing the open, unsupported schema, and damaged capture files. The actual network capture also verified serialized timestamp preservation, writer draining, full requested-universe accounting and source hashing.

No local or remote capture service was activated, and trading settings are unchanged. The next required evidence is a market-session recording from the updated scanner. That will support candidate-level coverage and lead-time findings. Whole-market discovery accuracy and profitability remain unestablished; the report deliberately leaves recall null rather than assuming unobserved candidates or coverage gaps away.
