# Consolidation index, 2026-10-08

`master` of this repository is the canonical source for Stockspotter. This
index records what happened to each line of work that the 2026-10-08 census
inventoried outside `master`: what landed, what was superseded, what is held
and why, and what the research found, including negative and withdrawn
results. Its scope is that inventory; work the census did not find is not
covered.

It is an index, not an archive. Raw data, research ledgers, operational reports
and anything describing hosts or accounts are kept in a private archive and are
referred to here only by a logical identifier. Nothing in this file is a claim
that a strategy is profitable, and nothing here authorizes a deployment.

## 1. How to read the status words

| Status | Meaning |
|---|---|
| Landed | The change is in `master`, either as the same commit or as an equivalent one. |
| Superseded | `master` solved the same problem differently; the older work is kept for history only. |
| Held | Not in `master`, with a stated reason. A held item needs new evidence, not just a merge. |
| Archive only | Research code, data or reports that do not belong in the product source. |
| Rejected | An earlier review decided against it; the reason is given. |

## 2. Engineering work

### Landed during this consolidation

| Change | Pull request | Evidence |
|---|---|---|
| Merging to `master` no longer publishes a desktop release or queues a mobile production build; both are manual dispatches from `master` | #29 | Tests pin the new workflow contract and fail against the old one. After the merge only validation ran. |
| Desktop shell lockfile audited before a release can build; rustls 0.23.43 -> 0.23.45; shared-types link step in the release job | #30 | `cargo audit` on the desktop lockfile failed before and passes after; the frontend type-check failed without the link and passes with it. |
| Ross five-pillar rule tests run in CI, with every numeric threshold pinned from both sides | #31 | 48 mutations of the rule's 18 numeric comparisons each fail at least one test; 26 are caught only by the added boundary tests. |
| The chart lifecycle harness (`tools/chart-recovery`) runs in the required `Tests, lint and build` check, and its own lockfile is audited by the existing advisory gate through a small adapter | #36 | 13 of 13 scenarios pass on the hosted runner. Reverting each of the three chart fixes it guards makes it fail: the mount dependencies (3 of 13 pass), the per-symbol history guard (12 of 13), the per-symbol memo keys (11 of 13). |
| Step 4 offline evaluator, narrowed to read-only: verifies and reads archived observer runs, with no deletion commands, receipt-supplied names validated and confined to the evidence directory, and bounded streaming reads | #35 | Compiled only with the `offline-eval` feature, which no deployed build enables; its tests run in CI with the feature on. The default build leaves the feature off and runs the same test set as before; the source tree is not byte-identical, since the evaluator's files and one optional dependency were added. No frozen file changes. Limits are stated below the table. |

What the evaluator's presence in `master` does and does not mean:

- It is a correctness tool for reading evidence. It is not evidence that any strategy works, and it changes no capture, ledger or
  frozen artifact.
- Its OI check accepts a document that matches its extractor's schema and whose counters agree with one another. That is a
  consistency check, not proof of origin: it does not show the document was produced by a trusted run of the extractor, and a
  forged document that is self-consistent would pass. "Certifies" means capture and accounting completeness: every row offered
  was written and accounted for. The frozen extractor keeps conflicting duplicates and counts them without holding them against
  completeness, and the evaluator reports exactly that.
- One command reads whole files because the frozen extractor takes whole slices. Its total input is budgeted at 1 GiB by default;
  a session whose input exceeds that is refused until an operator has sized memory for it and set the budget explicitly. The
  budget counts input bytes and is not a memory limit.
- Merging it changes the implementation commit that any future freeze would bind to; existing frozen artifacts are not altered.

### Already in `master` before this consolidation

These branches were checked commit by commit against `master`. Every commit has
an equivalent there (same patch, or a cherry-pick naming its source), so the
branches carry nothing further.

| Branch family | Subject |
|---|---|
| `p1/*` | Paper ledger rewrite, never-created order intents, retention registry, bounded container logs |
| `p2/*` | Market-day baselines, percentile thresholds per cohort, outcome disposition and rank-cohort capacity |
| `p3/*` | Session close on the New York clock, idle-feed deadline, premarket volume, move-v1 lifecycle, qualification gates |
| `topic/halt-status-correctness-20260928` | Trading status classified by tape and code, not by one status letter |
| `topic/observation-hotpath-perf-20260929` | Observer hot-path optimisation with a byte-identical golden test |
| `topic/mobile-advisories-20261006` | shell-quote update |

One commit on a `p2` branch was a branch-local hash re-pin whose own message
said to drop it at merge; `master` carries the final pin.

### Superseded

| Work | Superseded by |
|---|---|
| An earlier "attention-worthy Ignition events only" client change | The attention filter that is in `master` (`qualifiesForIgnitionAttention`) |
| An earlier desktop release gate design based on a `skip_mobile` input | `validate-server.yml` plus the manual-dispatch release job |
| A September audit-follow-up line (isolated local stack, feed heartbeat, halt push channel, auto-trader order expiry) | Later independent fixes in `master`. That line was written against a much older base and no longer applies; anything still wanted from it would be a rewrite. |
| Chart-recovery working changes | The chart fixes that are in `master`. Compared change by change: each behavioural change is present in `master` or replaced by a later fix there, and the harness in `master` covers a superset of the scenarios (now run in CI, #36). |

### Held

| Work | Why it is held | What would change that |
|---|---|---|
| Step 4 offline evaluator as first proposed (pull request #32, closed unmerged; its branch is kept) | Its tests pass, but it adds file-deletion commands with no code-level authorization gate, joins a file name taken from a receipt onto a directory without validating it, and decompresses archives fully into memory with no size cap. | Nothing is pending: the narrowed, read-only port landed as #35 (see above). The deletion commands remain out of `master`; adding them would need a code-level authorization gate and its own review. |
| Order flow (new crate, endpoints, recorder, web and mobile UI) | Tests pass, but nothing shows its trade classification is correct: its confidence values are described in the code as untuned defaults, and its validation tool has not been run. The recorder is bounded by age only, and a bar-cache warmer is on by default. | A validation run on recorded sessions, a size bound on the recorder, and an explicit decision on the default-on warmer. It must not feed ranking or the auto-trader without preregistration. |
| In-place patch for the `braces` advisory | The patch reproduces and its differential tests pass, but the mobile advisory gate stays red: the advisory is matched by registry package name and version, and a second, unrelated advisory has no fixed release. | A fixed release in the registry that the lockfile can resolve to. The gate is not to be satisfied by a waiver, a renamed or vendored copy, or a version that merely escapes the match. |

### Rejected earlier

| Work | Reason recorded at the time |
|---|---|
| Backend chart port and candle-coverage continuity | It changes the provisional bar stream that feeds opportunity scoring, so it could not be merged while measurement was frozen. |
| Per-symbol freshness client change | It marked every historical bar as final, including the still-forming one. |
| 50 ms publication cadence | Little upside: almost all streams are bound by trade arrival, not by cadence. |

## 3. Research findings

Each row gives the method and its limit, because most of these results hold
only for the sample they were measured on. A negative result is a result.
"Sound but narrow" means the method supports the stated conclusion for that
sample and no further.

| # | Finding | Method and sample | Limit | Status |
|---|---|---|---|---|
| R1 | Ignition alerts are not distinguishable from random minutes | Alerts compared with seeded random minutes drawn from the same symbol-day and session; roughly 96 trading days | Nine hand-picked symbols, all chosen because they had run. No preregistration. Lift measured between 0.9x and 1.1x depending on the outcome definition | Sound but narrow |
| R2 | No detector showed an executable edge after costs | Chronological development, validation and holdout splits over about 255 sessions; a causal quote-based fill simulator with a base and a stress cost model; matched random-minute and random-name controls | Holdout confidence intervals include zero, so the result is "not shown positive", not "shown negative". Freeze order rests on a self-written ledger | Sound as stated |
| R3 | Informed pre-open rankers concentrate large intraday movers far better than a random draw | Eight frozen, parameter-free rankers on 378 sessions in four periods | The multiple (about 25x) is against the whole listed universe, most of which is inactive. Against active names it is about 3x. The label is "ran at some point that day", not a return from the ranking time, and it is not directional | Sound but narrow |
| R4 | A redesigned onset detector did not improve on the existing one | Preregistered challenge: 30 sessions, 120 hash-selected symbols per block across price and liquidity strata, with frozen source hashes | Only about a quarter to a third of decision minutes had complete outcomes. A "top-5 ranking looks promising" side result was not distinguishable from an alphabetical ordering | Sound but narrow |
| R5 | A fitted ranking model beat the existing in-play ranking and random, but was not shown better than premarket range or gap alone | Development, validation and final months of 20 sessions each; 600 names per block; paired session bootstrap | The model was fitted on about 1,600 rows; its probability calibration was contradicted on the final set | Sound but narrow; the simpler rankers are the finding |
| R6 | No strategy from an independent study qualified for promotion | 96 configurations over the same nine-symbol cache with base and stress costs | The holdout was declared contaminated by the study itself | Sound but narrow |
| R7 | Opportunity scoring V2.0 ranked the day's runners worse than the incumbent | Two development sessions with matched controls | No holdout, no confidence intervals, and the preregistration was not committed before the analysis outputs. A later variant looked better on the same two days and partly failed to replicate | Development evidence only |
| R8 | Six September sessions are unusable for prospective qualification | Completeness counters against the frozen capture contract | For four of them no machine verdict was produced; they are unqualified by contract, not by measurement | Sound |
| R9 | Suppressing repeat notifications removes most volume without losing first alerts; naive global caps destroy recall | Three sessions, one detector configuration | First-alert recall is preserved by construction, and the caps were applied in arbitrary order, so the result says nothing about caps with a priority signal | Sound but narrow |
| R10 | The preregistered two-arm observation experiment ("Step 4") has demonstrated no efficacy result | Designed and frozen; the reviewed campaign ledger is empty and the reviewed evidence contains no eligible capture and no evaluation | This statement covers the ledger and evidence that were reviewed. It is not an independent check of every machine the observer could run on | No result |
| R11 | Paper-trading profit and loss was negative | Operational ledger over 13 trading dates | Paper fills, no control arm, no preregistration. Declared at the time to be operational parity data, not research evidence | Not evidence |

### Withdrawn or contradicted, and not to be cited

| Earlier claim | What replaced it |
|---|---|
| "The new ranking is confirmed on the final set" | Contradicted the same day by its own follow-up: the effect was an artefact of evaluating inside an already-enriched subset. On the full market the new ranking was worse than the existing one at the top of the list. |
| "Ignition has about 1.1x lift" as a positive | The committed tool with matched controls gives 0.92x to 0.93x. See R1. |
| "The first half hour is where the detector adds value" | Reversed: that window has the highest base rate and the most negative detector edge on every split. |
| "Quote-based triggers are below random and were removed for that reason" | Reclassified as an attribution artefact. |
| An early simulated loss figure | Understated several times over once fills were priced from quotes instead of bars. |

## 4. Reusable tooling identified, not yet proposed

These exist only in the private archive today. Each would need its own review
before entering `master`.

- A seeded matched-random-minute baseline with a lift table.
- A causal quote-based fill simulator with base and stress cost models.
- An append-only experiment ledger.
- A freeze, seal, verify and reproduce harness. Its once-guard records that a
  run started but does not refuse a second run, which must be fixed first.

## 5. Archive references

Logical identifiers only. Each maps to a subset of rows in the private
consolidation register, which also records locations; neither is reproduced
here. The preservation state is as of 2026-10-08 and is stated per identifier:
"verified" means every file was hashed at the source and at two copies and a
restore was exercised; "in progress" means copying or verification had not
finished, so it must not be read as a complete verified archive.

| Identifier | Contents | Preservation state |
|---|---|---|
| `ARCH-GIT` | Copies of the inventoried local repository stores, including branches never published | Verified, with a relocated restore test |
| `ARCH-RESEARCH-A` | Reports, ledgers and scripts of the September falsification, ranking and benchmark studies behind R1 to R3 and the withdrawn claims | Reports and scripts verified; input data in progress |
| `ARCH-RESEARCH-B` | The independent detection, ranking-replication and strategy studies behind R4 to R6, with their reproduction runs | Reports verified; input data in progress |
| `ARCH-EVIDENCE` | Preserved session captures, completeness records and retention receipts behind R7 to R9 | Records and receipts verified; large captures in progress |
| `ARCH-STEP4` | The frozen preregistration generations and the campaign ledger behind R10 | Verified |
| `ARCH-OPS` | Operational and audit reports, including R11's ledger evidence | Reports verified; ledger files in progress |

## 6. What this consolidation did not do

- It did not deploy anything. Production runs from a release branch and was not
  touched.
- It did not publish a desktop release or queue a mobile build.
- It did not change, re-run or re-interpret any frozen experiment.
- It did not delete the original working copies of anything listed here.
