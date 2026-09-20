# Measurement validation session 001: read-only observer handoff

## Current evidence, not a prediction

At 2026-09-20 17:31 and 17:36 UTC the live observer returned **PENDING**, zero
errors. No Monday anchors or artifacts exist yet; no live smoke PASS is claimed.
SHA `7e365866ce5ae18ea77cce05d6d8f45bc69c7faa`, fingerprint
`oi-cfg-b4f21c8b311a1b99`, collector 297000, outcome writer 16384/64MiB,
all loss/error/eviction counters zero, no pending retention, ~69GiB free.
Protected container identities/configuration match the saved baseline.

Capture is continuously event-driven in `ws-server/main.rs`: market event →
`OutcomeDriver.observe_price` → shadow ranking → `anchor_and_settle` → durable
writer. It does not wait for cron or 09:30. The NY session date is computed with
`America/New_York`. September 21 regular trading is 13:30–20:00 UTC; first
meaningful premarket traffic can precede that. The deployed outcome close is
hardcoded 20:00 UTC, correct for this September session, not winter DST.

The durable source is **/opt/apps/stockspotter/data/research**, mounted as
`/app/data/research`, not the generic runbook's `/srv/stockspotter-research`.
The initial request for full-market subscription is logged; actual next-session
delivery and durable schema-2 rows remain to be proven. Capture and smoke/report
orchestration are separate. No external observer schedule was created here.
Claude was not contacted; requirements were recovered through the coordinating
task and independently checked against code and current diagnostics.

## Ready-to-run observer

Run in `H:/wavystack/stockspotter-chart-audit`:

```powershell
python -m unittest discover -s tools/chart-audit -p test_observe_session.py
python tools/chart-audit/observe-session.py --date 2026-09-21 --baseline data/chart-observer/baseline-20260920.json --out data/chart-observer/smoke-20260921-unique-time.json
```

Six deterministic tests cover valid sample/settlement distinction, idle pending,
restart/config/loss failures, schema/ID/horizon mismatches, partial live writes,
and offline missing/duplicate anchors. The exact probe has run against production
twice. `--out` refuses to overwrite existing evidence; choose a new timestamp.
Exit codes: 0 sampled smoke passed, 2 pending, 1 failed (some shell wrappers map
nonzero codes to 1; the JSON `status` is authoritative). Operational failures
such as SSH outage terminate without a PASS.

Requirements: this Windows host awake, working Python/SSH, tailnet/network and
existing `stockspotter-vps` SSH authorization. Production Python and Docker access
are already available. The script is piped to Python over SSH, installs nothing,
reads existing credentials only inside the already-running qualify process via
its normal environment, and never prints or copies them. It reads a bounded tail
of each target-date artifact and the existing authenticated health endpoint.
It does not restart, change configuration, edit strategy, or schedule production.
All snapshots and temporary analysis databases stay local in the workspace/temp.

A caller should first run before expected traffic, then at first meaningful
traffic and repeat while PENDING, retaining timestamped evidence. 60-second
checks are feasible: each current read took about 1 second remotely. This is a
proposed external observer cadence, **not an installed task**. A laptop asleep,
closed Codex session or unavailable network cannot provide an unattended check.
An already-authorized always-on observer must own the trigger before promising
autonomous verification. Any freeze identity/loss failure is reported, never
repaired during the session.

`PASS_SAMPLED_SMOKE` requires a saved unchanged baseline, actual new target-date
ranking/outcome/episode rows, correct measurement/schema/horizons, time-derived
opportunity identity, canonical episode UID and RiskQuality fields. It does not
prove every row or every anchor, statistical validity, or tomorrow's uptime.
Partial appended lines are PENDING, not silent corruption. Long-horizon outcomes
and episode rows can legitimately be unavailable at the first trade; inspect
anchors immediately and wait for actual rows rather than claiming early PASS.

## Post-settlement reconciliation

Keep collecting health snapshots without a restart. Require state-based
`anchorsSettled == anchorsCreated`, `outstanding == 0`, zero writer queue and
balanced `attempted == written + dropped + writeErrors`, zero loss/eviction,
and stable counters in repeat observations. Do not substitute a fixed 20-minute
sleep. Process-lifetime counters must be compared with the saved pre-session
baseline before comparing them to day-scoped file counts.

After the existing immutable-export/checksum procedure, run locally against its
`research` directory (not a file still being appended):

```powershell
python tools/chart-audit/reconcile-outcomes.py --directory H:/wavystack/session-001-20260921/research --date 2026-09-21
```

This scans every ranking/outcome/episode row, records hashes, detects file changes,
malformed rows, duplicate keys, missing/unmatched outcome anchors, invalid schema,
identity, provenance, horizon grid and absent causal fields. SQLite scratch joins
bound memory rather than keeping millions of keys in RAM. It reports fully
observed and censored totals, without generating replacement outcomes.

It **does not complete research interpretation**: score-decile and
resolved/partially_ambiguous/no_episodes strata need the recorded temporal
containment membership procedure (never a join on legacy episodeId). RiskQuality
formula replayability and the existing consumedFromLow plus dmfs300 OR dcons300
hypothesis versus Continuation still need a reviewed offline research step.
No fitting, new weights, thresholds, windows or V2.1 preregistration are authorized.
The existing `ops/qualify/session.sh` is an operator-invoked workflow, not proof
these additional outcome-native checks have been scheduled. Its base URL and
research path must match deployment when used; this observer uses the known
internal endpoint and actual mounted path directly.
