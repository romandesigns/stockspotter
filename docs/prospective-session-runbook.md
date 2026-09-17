# Prospective session runbook

One untouched regular session, captured and qualified without a human
changing anything in between. This is the operating procedure for that
session — not a description of what the system does, but the exact
sequence an operator (or the automation in `ops/qualify/`) performs.

**The whole point is that nothing is touched.** Every step below is a
read, an export, or a checksum. The only step that writes anything writes
into a new directory that did not previously exist. If a step fails, it is
**recorded and the session is abandoned** — it is never repaired mid-flight,
because a session repaired while it runs is no longer an untouched session
and cannot answer the question it was captured to answer.

---

## 0. What must already be true

Stage C established these. They are re-verified before open rather than
assumed, because the cost of assuming is a session that looks valid and
is not.

| | Expected |
|---|---|
| Deployed commit | `HEAD` == `ops/vps/.deployed-commit` == `completeness.commit` |
| OI config fingerprint | `oi-cfg-b4f21c8b311a1b99` |
| Qualification contract | `alpha-qualification-v3`, SHA `20c35d174da5897da2dc5b68921f2ab6c416ad36e23ea11ac32be61501972b0b` |

The contract SHA is recorded **before** the session opens and passed back
to `alpha_qualify --expected-spec-sha256` after the close. If anything in
the contract moved in between, the run refuses to start. That is what makes
the freeze mechanical instead of a promise.

---

## 1. Before open

Run `ops/qualify/session.sh preflight`. It performs, and fails loudly on
any of:

- **Exact deployed SHA.** `git -C /opt/apps/stockspotter rev-parse HEAD`
  equals `ops/vps/.deployed-commit`, and equals the `commit` field the live
  `/research/completeness` reports. Three independent sources; all three
  must agree. The third is the one that matters — it is the only one that
  proves the *running process* is the commit, rather than the checkout.
- **Exact OI fingerprint.** `oiConfigFingerprint` == `oi-cfg-b4f21c8b311a1b99`.
- **Qualification-spec SHA.** `alpha_qualify --print-spec` matches the
  recorded hash, and the hash is written into the session directory as
  `qualification-spec.sha256` before any data exists.
- **Health query.** `/research/completeness` returns 200.
- **All loss counters zero** — OI `dropped`, measurement `dropped`,
  discovery `queueLost`, `writeErrors`, `budgetDropped`, and every
  writer's `lossSpans`.
- **Capacity healthy** — `capacityEvictions == 0`, `cohortTruncations == 0`,
  and every queue's `queuePeak` comfortably below its bound.
- **Disk headroom** — free space on the research volume exceeds one
  session's worst observed footprint with margin. September 16 wrote
  19.3 GB; the floor is 40 GB free.
- **Retention not pending** — `retentionPending` is false. A retention
  sweep that is waiting to run may delete during the session.
- **No unapproved deploy pending** — `git -C /opt/apps/stockspotter status
  --porcelain` is empty and `git rev-list HEAD..origin/<branch>` is empty.
  A deploy landing mid-session would change the instrument under the
  measurement.

If any check fails: **do not open the session.** Fix it on another day.

---

## 2. During the session

**Do not modify production.** No deploy, no restart, no configuration
change, no threshold edit, no container action. The deploy timer should be
left alone; if it would fire, it fires against an unchanged commit and is
a no-op.

Read-only health checks are permitted and encouraged:

```sh
ops/qualify/session.sh health        # one read, prints the counters
```

Any loss, capacity error, or writer error observed during the session is
**recorded, not repaired**. Write it down, let the session finish, and let
the completeness gate decide. Repairing mid-session produces a session that
is half one instrument and half another, which no amount of later analysis
can separate.

---

## 3. After close

### 3.1 Wait for the settlement frontier, not the clock

The longest measurement horizon is 900 seconds. An episode that opened at
15:59:50 is not settled until 16:14:50, and one that opened later still is
not settled at all. **Do not guess a wall-clock delay.** Poll:

```sh
ops/qualify/session.sh settle       # blocks until pending == 0
```

It reads `measurementPending.pending` and returns only when it reaches
zero — or fails after its ceiling, which is itself a finding: episodes that
never settled are episodes the session cannot speak about.

### 3.2 Verify and export

```sh
ops/qualify/session.sh export /srv/research-export/session-NNN-YYYY-MM-DD
```

which, in order:

1. re-queries `/research/completeness` and writes it verbatim as
   `research/completeness-YYYY-MM-DD.json` — verbatim, because a
   transcription step is a place for a session to be described by a
   document that does not match it;
2. asserts `unsettled == 0`;
3. copies the session's capture files and marker files;
4. checksums **both sides** — source and destination — and refuses if any
   digest differs;
5. writes `.hold` so the retention sweep cannot reclaim it;
6. makes the export read-only.

### 3.3 Qualify, exactly once

```sh
ops/qualify/session.sh qualify \
    /srv/research-export/session-NNN-YYYY-MM-DD \
    YYYY-MM-DD \
    reports/qualification-YYYY-MM-DD
```

The output directory must not already exist; the tool refuses to overwrite
a result, because a qualification result is evidence and silently replacing
one destroys the record of what was concluded before. Running it twice
against the same output is an error, not an update.

---

## 4. Reading the answer

The run prints two lines:

```
SESSION STATUS: VALID | INVALID | INDETERMINATE
OI V1 EVIDENCE STATUS: QUALIFIES | DOES_NOT_QUALIFY | INSUFFICIENT_EVIDENCE | NOT_EVALUATED
```

**If SESSION STATUS is INVALID or INDETERMINATE: STOP.** The evidence
status will read `NOT_EVALUATED`, and that is not a hedge — no Alpha claim
was computed at all. There is nothing in the report to interpret about V1,
and interpreting it anyway is the specific failure this gate exists to
prevent. Capture another session.

**If SESSION STATUS is VALID**, the evidence decision stands on its own and
goes to human review. Read both interpretations at the top of
`FINAL-ALPHA-QUALIFICATION.md`; they answer different questions and either
one alone is misleading:

- **OI layer qualification** — given what Stockspotter detected, did V1
  prioritize usefully, early, and without unacceptable risk degradation?
- **End-to-end scanner coverage** — how much of the independent
  meaningful-opportunity population did Stockspotter detect at all?

A `QUALIFIES` verdict authorizes **nothing** on its own. It does not
promote Auto-Trader consumption of EarlyQuality or ContinuationConfidence,
does not permit orders from OI rank, does not replace production detector
gates, and does not suppress existing alerts. Human review sits between the
evidence and any production change, and no part of this pipeline can move
that boundary.

---

## 5. What the automation will not do

`ops/qualify/session.sh` captures, preserves, checksums, validates,
analyses and reports. It has no code path that tunes a threshold, fits a
model, enables a strategy, deploys anything, or modifies Auto-Trader. That
is a property of the script, not a convention — there is nothing in it to
disable.
