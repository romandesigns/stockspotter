# Qualification v4 machine gates (P3, 2026-09-25)

The next designated prospective session is evaluated by `alpha_qualify`
under `alpha-qualification-v4`. Before this change, the only machine gate was
`completeness::check`, which answers "did this capture lose evidence". It did
not ask whether the capture came from the pinned instrument, whether it had
observed the whole market day, or whether the session was chosen before it
was seen. This document and `crates/backtest-metrics/src/completeness.rs`
(`GATE_TABLE`, `qualification_gates`) close that gap.

## The rule

- **The session verdict is the AND of every gate.** A gate passes iff every
  one of its checks passes. `GateReport::passed()` recomputes this from the
  results each time. It is false unless every `GATE_TABLE` row has exactly one
  passing, non-absent result, so a check that was skipped cannot pass by
  omission.
- **No narrative override.** There is no flag, environment variable, CLI
  option or "accepted with caveats" state. `alpha_qualify` accepts exactly
  eight options, none of which touches the verdict.
  `qualification_gates_tests::nothing_can_force_a_pass` enforces this by
  reading the sources. A failed gate is recorded, and the session is
  abandoned.
- **Fail closed.** The gates read the captured `/research/completeness`
  document **raw, by field name**. They do not go through
  `CompletenessReport`, so a field this build does not know about is
  *absent*, not a serde default of `0`. An absent value is never a pass. It
  folds as *missing* (the session is INDETERMINATE). A violated predicate
  folds as *blocking* (the session is INVALID). Either way the session cannot
  reach VALID.
- **Nothing is dropped from the contract silently.**
  `QualificationSpec.qualificationGates` lists every gate, so dropping one
  changes the contract SHA. `QualificationSpec::validate` also refuses a spec
  that omits a gate the build evaluates, and `alpha_qualify` will not start
  under an invalid spec.
- **Output.** Every result is written as
  `{gate, check, pass, absent, observed, expected}` in
  `qualification.json` under `gates.results[]`, and as a table in
  `FINAL-ALPHA-QUALIFICATION.md`. Passing checks are included.

Paths are envelope paths (`report.…`, `measurementPending.…`). A bare
report resolves only `report.` paths. A `report.X` path that is absent under
`report` is also looked up at the envelope's top level. This is only so the
D7b branch can place `premarketVolume` beside `retention`; a field absent
from both locations is still absent.

## Gate table

`rows.*` comes from a streaming scan of the OI snapshot file
(`completeness::scan_baseline`). `artifacts.*` comes from the integrity
scan. `designation.*` and `protection.*` come from the exported session's
retention registry (see "Designation").

| Gate | Check | Predicate | Why |
|---|---|---|---|
| `completeness-check` | `completeness.verdict` | == VALID | check(): writer reconciliation, artifacts, provenance, settlement -- the contract every session already had |
| `writer-loss` | `report.opportunityIntelligence.dropped` | present and == 0 | a record that was never written leaves no trace; any loss turns the population into a sample of unknown bias |
| `writer-loss` | `report.opportunityIntelligence.writeErrors` | present and == 0 | a record that was never written leaves no trace; any loss turns the population into a sample of unknown bias |
| `writer-loss` | `report.opportunityIntelligence.lossSpans` | present and == 0 | a record that was never written leaves no trace; any loss turns the population into a sample of unknown bias |
| `writer-loss` | `report.measurement.dropped` | present and == 0 | a record that was never written leaves no trace; any loss turns the population into a sample of unknown bias |
| `writer-loss` | `report.measurement.writeErrors` | present and == 0 | a record that was never written leaves no trace; any loss turns the population into a sample of unknown bias |
| `writer-loss` | `report.measurement.lossSpans` | present and == 0 | a record that was never written leaves no trace; any loss turns the population into a sample of unknown bias |
| `writer-loss` | `report.opportunityOutcomes.dropped` | present and == 0 | a record that was never written leaves no trace; any loss turns the population into a sample of unknown bias |
| `writer-loss` | `report.opportunityOutcomes.writeErrors` | present and == 0 | a record that was never written leaves no trace; any loss turns the population into a sample of unknown bias |
| `writer-loss` | `report.opportunityOutcomes.lossSpans` | present and == 0 | a record that was never written leaves no trace; any loss turns the population into a sample of unknown bias |
| `writer-loss` | `report.discovery.queueLost` | present and == 0 | a record that was never written leaves no trace; any loss turns the population into a sample of unknown bias |
| `writer-loss` | `report.discovery.writeErrors` | present and == 0 | a record that was never written leaves no trace; any loss turns the population into a sample of unknown bias |
| `writer-loss` | `report.discovery.budgetDropped` | present and == 0 | discovery is the independent reference population; a budget refusal truncates it |
| `capacity-eviction` | `report.opportunityEngine.capacityEvictions` | present and == 0 | an eviction removes an opportunity or anchor before it could be ranked or measured |
| `capacity-eviction` | `report.opportunityEngine.evictionMarkersDropped` | present and == 0 | a lost eviction marker means the artifact cannot describe its own truncation |
| `capacity-eviction` | `report.opportunityOutcomeEngine.capacityEvictions` | present and == 0 | an eviction removes an opportunity or anchor before it could be ranked or measured |
| `capacity-eviction` | `measurementPending.capacityEvictions` | present and == 0 | an eviction removes an opportunity or anchor before it could be ranked or measured |
| `ranking-truncation` | `report.opportunityEngine.cohortTruncations` | present and == 0 | a cut cohort reports scored opportunities as unranked and every cohort size as wrong (the 09-21..09-24 defect, D6) |
| `ranking-truncation` | `report.opportunityEngine.earlyCohortTruncations` | present and == 0 | a cut cohort reports scored opportunities as unranked and every cohort size as wrong (the 09-21..09-24 defect, D6) |
| `ranking-truncation` | `report.opportunityEngine.continuationCohortTruncations` | present and == 0 | a cut cohort reports scored opportunities as unranked and every cohort size as wrong (the 09-21..09-24 defect, D6) |
| `ranking-truncation` | `report.opportunityEngine.truncationMarkersDropped` | present and == 0 | a cut cohort reports scored opportunities as unranked and every cohort size as wrong (the 09-21..09-24 defect, D6) |
| `ranking-truncation` | `report.opportunityEngine.rankCohortCapacity` | >= report.opportunityEngine.capacity, and capacity > 0 | D6: the ranked cohort must be bound to open capacity so truncation is structurally impossible |
| `malformed-output` | `artifacts.malformedRecords` | sum over present artifacts == 0 | a malformed record means the file cannot be read as a whole |
| `malformed-output` | `artifacts.truncated` | no artifact ends mid-record | a partial final record is a capture cut mid-write |
| `malformed-output` | `artifacts.required` | every required artifact present with records > 0 | the session means nothing without its primary captures |
| `duplicate-identity` | `report.opportunityEngine.duplicateIdentityRefused` | present and == 0 | move-v1 refuses to open a colliding opportunityId and counts it; a refusal is an opportunity that was never captured |
| `lifecycle-contract` | `report.opportunityEngine.lifecycle` | == spec.expectedLifecycle | the evaluation's analytical unit is the opportunity; its key must be the move |
| `lifecycle-contract` | `report.oiVersions.lifecycle` | == spec.expectedLifecycle | the evaluation's analytical unit is the opportunity; its key must be the move |
| `baseline-truncation` | `rows.baselineTruncated` | 0 OI rows of the market day carry preDetection.baselineTruncated == true | a baseline that started after 04:00 ET is not this market day's baseline (D3); true on every deploy or restart day |
| `baseline-truncation` | `rows.baselineComplete` | > 0 OI rows of the market day carry baselineTruncated == false | absence of a truncated row proves nothing unless complete rows were positively observed |
| `deployed-before-open` | `designation.processStartedAt` | < market_day_open(marketDay) | a process started after 04:00 ET cannot have observed the whole market day |
| `deployed-before-open` | `designation.deployMarkerAt` | < market_day_open(marketDay) | the authoritative deploy marker must predate the observation boundary |
| `premarket-volume-init` | `report.premarketVolume.fetchFailures` | present and == 0 | D7b: a failed premarket-volume fetch leaves funnel qualification on a stale daily bar |
| `premarket-volume-init` | `report.premarketVolume.marketDay` | == marketDay | the state must belong to the designated market day, not a carried-over one |
| `premarket-volume-init` | `report.premarketVolume.initializedAt` | RFC 3339 and market_day(t) == marketDay | initialisation must have happened inside the designated market day |
| `schema-fingerprint` | `report.commit` | == expected commit (request, else designation) | a capture from a different instrument describes a different engine; comparing it under this contract would attribute one configuration's behaviour to another |
| `schema-fingerprint` | `report.oiConfigFingerprint` | == spec.expectedOiConfigFingerprint | a capture from a different instrument describes a different engine; comparing it under this contract would attribute one configuration's behaviour to another |
| `schema-fingerprint` | `report.oiVersions.configFingerprint` | == spec.expectedOiConfigFingerprint | a capture from a different instrument describes a different engine; comparing it under this contract would attribute one configuration's behaviour to another |
| `schema-fingerprint` | `report.oiVersions.opportunitySchema` | == spec.expectedOpportunitySchema | a capture from a different instrument describes a different engine; comparing it under this contract would attribute one configuration's behaviour to another |
| `schema-fingerprint` | `report.oiVersions.featureSchema` | == spec.expectedFeatureSchema | a capture from a different instrument describes a different engine; comparing it under this contract would attribute one configuration's behaviour to another |
| `schema-fingerprint` | `report.oiVersions.regimeClassifier` | == spec.expectedRegimeClassifier | a capture from a different instrument describes a different engine; comparing it under this contract would attribute one configuration's behaviour to another |
| `schema-fingerprint` | `report.oiVersions.priceRegime` | == spec.expectedPriceRegime | a capture from a different instrument describes a different engine; comparing it under this contract would attribute one configuration's behaviour to another |
| `schema-fingerprint` | `report.oiVersions.earlyQualityModel` | == spec.expectedEarlyQualityModel | a capture from a different instrument describes a different engine; comparing it under this contract would attribute one configuration's behaviour to another |
| `schema-fingerprint` | `report.oiVersions.continuationModel` | == spec.expectedContinuationModel | a capture from a different instrument describes a different engine; comparing it under this contract would attribute one configuration's behaviour to another |
| `schema-fingerprint` | `report.oiVersions.ranking` | == spec.expectedRanking | a capture from a different instrument describes a different engine; comparing it under this contract would attribute one configuration's behaviour to another |
| `schema-fingerprint` | `report.oiVersions.scorePolicy` | == spec.expectedScorePolicy | a capture from a different instrument describes a different engine; comparing it under this contract would attribute one configuration's behaviour to another |
| `schema-fingerprint` | `report.oiVersions.baselinePolicy` | == spec.expectedBaselinePolicy | a capture from a different instrument describes a different engine; comparing it under this contract would attribute one configuration's behaviour to another |
| `schema-fingerprint` | `report.signalContextSchema` | == spec.expectedSignalContextSchema | a capture from a different instrument describes a different engine; comparing it under this contract would attribute one configuration's behaviour to another |
| `schema-fingerprint` | `report.episodeSchema` | == spec.expectedEpisodeSchema | a capture from a different instrument describes a different engine; comparing it under this contract would attribute one configuration's behaviour to another |
| `schema-fingerprint` | `report.outcomeMeasurementVersion` | == spec.expectedOutcomeMeasurementVersion | a capture from a different instrument describes a different engine; comparing it under this contract would attribute one configuration's behaviour to another |
| `disposition-consistency` | `report.opportunityOutcomeEngine.dispositionCounts` | sum over every token == anchorsSettled | every settled row carries exactly one disposition; a mismatch means rows were counted twice or not at all |
| `disposition-consistency` | `report.opportunityOutcomeEngine.closureAnchorsMarked` | settledNonStillOpen <= marked <= settledNonStillOpen + outstanding | each marked anchor is either settled with its reason or still outstanding; outside that range closes and rows do not reconcile |
| `disposition-consistency` | `report.opportunityOutcomeEngine.dispositionCounts.inactivity` | absent or == 0 (v1 token) | move-v1 closes are setup_inactivity/invalidated; a v1 `inactivity` means the old lifecycle produced these rows |
| `disposition-consistency` | `report.opportunityEngine.closedByReason.inactivity` | absent or == 0 (v1 token) | same, on the engine side |
| `disposition-consistency` | `report.opportunityOutcomeEngine.dispositionCounts.setupInactivity` | present (move-v1 token) | a build that cannot count the move-v1 tokens cannot prove it wrote them |
| `disposition-consistency` | `report.opportunityOutcomeEngine.dispositionCounts.invalidated` | present (move-v1 token) | same |
| `designation` | `designation.record` | present, parses, schemaVersion 1, marketDay == session market day, preflight == PASS | a session chosen after it was seen is not a prospective session |
| `designation` | `designation.designatedAt` | < market_day_open(marketDay) | a session chosen after it was seen is not a prospective session |
| `designation` | `designation.specSha256` | == sha256 of the contract this build carries | a session chosen after it was seen is not a prospective session |
| `designation` | `designation.specVersion` | == spec.version | a session chosen after it was seen is not a prospective session |
| `designation` | `designation.commit` | == report.commit | a session chosen after it was seen is not a prospective session |
| `designation` | `designation.oiConfigFingerprint` | == report.oiConfigFingerprint | a session chosen after it was seen is not a prospective session |
| `designation` | `protection.research` | research/.retention/protected/<day>.json is a valid `designated` record | retention must be unable to delete the session before it is exported |
| `designation` | `protection.discovery` | discovery-audit/.retention/protected/<day>.json is a valid `designated` record | the reference population lives in discovery; same reason |

### Brief §16 mapping

| Brief condition | Gate(s) |
|---|---|
| known writer loss | `writer-loss` (plus `completeness-check`: writer reconciliation) |
| capacity eviction | `capacity-eviction` |
| ranking truncation | `ranking-truncation` |
| malformed output | `malformed-output` |
| duplicate durable identity | `duplicate-identity` |
| opportunity lifecycle contract violation | `lifecycle-contract`, and the v1/move-v1 token checks in `disposition-consistency` |
| baseline truncation / incomplete initialization | `baseline-truncation` (rows), `deployed-before-open` (process start and deploy marker) |
| required premarket-volume initialization failure | `premarket-volume-init` |
| schema / fingerprint mismatch | `schema-fingerprint`, plus the designation cross-checks |
| disposition inconsistency | `disposition-consistency` |

The brief's `closureAnchorsMarked > settled` condition is implemented in its
exact form: `settledNonStillOpen <= closureAnchorsMarked <= settledNonStillOpen
+ outstanding`. A marked anchor is either settled carrying its reason, or
still outstanding at report time. With `outstanding == 0`, which is the
normal case after `session.sh settle`, this reduces to
`marked == settled non-still_open`, and in particular `marked <= settled`.

## Deploy-day `baselineTruncated` (brief §17)

D3 defines `baselineTruncated = observationStartedAt > market_day_open(marketDay)`.
`observationStartedAt` is the **data timestamp of the first event the
FeatureCache ever folded**. It is not the process start time.

| Case | `baselineTruncated` |
|---|---|
| first event before 04:00:00 ET of the market day (for example a process running since the previous afternoon) | false |
| first event **exactly** 04:00:00 ET | **false.** The comparison is strict, and a cache whose first event is the open has observed the whole day. |
| first event 04:00:01, 04:01, 08:00 ET | true |
| process restarted intraday | true for every row after the restart, false for the rows before it |
| feed reconnect without a process restart | false: the cache is untouched, and a gap is only a period without events |
| process started at 03:00 ET but the feed is silent until 04:00:30 | **true.** The flag follows observation, not uptime. |
| the previous market day's 20:00–04:00 ET tail in the same UTC file | not counted: the row scan filters on `preDetection.marketDay` |

In EST the open is 09:00Z, not 08:00Z. The rule is DST-aware and tested in
both seasons.

**Operational consequence.** Deploying "before 04:00" is not enough. Between
20:00 and 04:00 ET there may be nothing to observe, so a process that starts
overnight can still record its first event after the open. The designated
session must be deployed while the **previous market day's feed is still
live** (before 20:00 ET). The `deployed-before-open` gate checks the process
start and the deploy marker. The row scan checks what was actually
observed. Both must pass.

Tests in `qualification_gates_tests.rs`:

- `deploy_day_boundary_in_edt`
- `deploy_day_boundary_in_est`
- `a_process_started_before_0400_on_a_silent_feed_is_still_truncated`
- `an_intraday_restart_truncates_only_the_rows_after_it`
- `a_feed_reconnect_without_a_restart_does_not_truncate`
- `the_previous_market_days_truncated_tail_does_not_fail_the_designated_day`
- `the_gates_reject_a_deploy_day_session` (end to end: rows, then scan, then gates)

## Designation

`ops/qualify/session.sh designate <market-day> <by> <reason>` runs the full
preflight and refuses unless it passes. It also refuses unless it is run
before `market_day_open(<market-day>)`. It then writes:

- `data/research/.retention/protected/<day>.json` and
  `data/discovery-audit/.retention/protected/<day>.json`: retention
  `designated` records (`market_data::retention_registry`). Once these
  exist, retention cannot delete the session without a verified receipt.
- `data/research/.retention/designations/<day>.json`: the designation
  record, with `schemaVersion`, `marketDay`, `designatedAt`, `designatedBy`,
  `commit`, `oiConfigFingerprint`, `specVersion`, `specSha256`,
  `processStartedAt`, `deployMarkerAt`, `containerRestartCounts`, and
  `preflight: "PASS"`. It sits beside the registry's `protected/` and
  `exports/` directories and is never read by the registry.

`session.sh export` copies `research/` whole, including `.retention/`, and
the `designation` gate reads these files from the export. `session.sh
qualify` takes `--expected-commit` from the designation record. Before this
change it took it from the capture's own health document, which made the
commit comparison circular.

## Provisional pins (the integrator must resolve these)

| Pin | Value here | Final value / owner |
|---|---|---|
| `spec.expectedLifecycle` / `EXPECTED_OPPORTUNITY_LIFECYCLE`, `session.sh EXPECTED_LIFECYCLE` | literal `opportunity-lifecycle-move-v1` | must reference the D5 branch's own constant (`OiVersions.lifecycle`) |
| `spec.expectedOpportunitySchema` / `session.sh EXPECTED_OPPORTUNITY_SCHEMA` | `2` (this branch's `OPPORTUNITY_SCHEMA_VERSION`) | `3` once p3/d5-move-v1 merges. The spec follows the constant automatically; the build fails until `session.sh` is updated. |
| `report.opportunityEngine.duplicateIdentityRefused`, `.lifecycle`, `report.oiVersions.lifecycle` | read by name, absent on this branch | added by p3/d5-move-v1 |
| `report.premarketVolume.{fetchFailures, marketDay, initializedAt}` | read by name, under `report` or top level | added by p3/d7b-premarket-volume |
| `dispositionCounts.{setupInactivity, invalidated}`, `closedByReason.inactivity` legacy | read by name | camelCase of the move-v1 tokens (`setup_inactivity`, `invalidated`); confirm against D5 |
| `report.opportunityEngine.marketDayId` (preflight timezone check) | read by name; `None` on this branch (TODO in `EngineCapture`) | populated by the D3/D7a follow-up |
| `EXPECTED_SPEC_SHA` in `session.sh` and the runbook | the SHA of this branch's spec | recomputed once over the merged tree. `runbook_contract_tests` refuses a mismatch. |

Until every row above is resolved, **no session can pass**. This is
intended: the gates fail closed.
