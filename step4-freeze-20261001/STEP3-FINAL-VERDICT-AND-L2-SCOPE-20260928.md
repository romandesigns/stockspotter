# Step 3 final verdict — retractions, L1 acceptance, frozen protocol, L2 scope

**Reviewer:** Claude, independent reviewer. Codex sole writer/executor. **No project edits, no production, no capture, no designation, no trades, no deletions, no settings, no remote actions.**

**Read:** `observation-design/RECONCILIATION.md` (8,203 B, sha `dc01af8eb2bb5aba17b6…`) and `20260928-L1-RESULTS.md` (4,860 B, `19b770d004c42a63ab36…`).

**The reconciliation is correct on every contested point. I verified each against source and retract five of my defaults (§1).** L1 accepted with limits (§2). Step-3 policy resolved as a single route (§3). L2 scope given (§4).

---

## 1. Retractions — verified against source at `60c04ca`

### 1.1 My end-marker persistence certificate — **FULLY RETRACTED.** It fails on both mechanisms.

I claimed a `window_end` marker written "through the same writer" would give a persistence prefix property. Verified, and it is wrong twice over:

- **`research_writer.rs:524-539`** — `marker()` writes to `self.naming.markers(now.date_naive())`, a **separate sibling file**, and calls `self.offer(..., false)` with the comment *"Markers are how loss becomes discoverable, so they are never themselves filed as data loss when the queue rejects them."* So **a marker can be dropped with no counter incrementing** — marker absence is invisible.
- **`research_writer.rs:306`** — `HashMap<PathBuf, BufWriter<File>>`: **independent buffers per path.** There is no ordering relation between the data file and the markers file at all, so "same channel FIFO" never implied a shared byte prefix.
- **No `sync_all`, `sync_data` or `fsync` anywhere in the writer** — so even within one file, buffered bytes are not proven durable.
- **`:360`** `let Some(file) = entry else { continue };` — an open failure continues the loop, and `write_errors` is incremented separately at `:348`, `:374` and `:391` against the documented identity `written + dropped + write_errors == attempted`. **A later marker can therefore exist despite earlier missing data**, exactly as stated.
- And the in-band variant is forbidden outright: **`opportunity_shadow.rs:158-164`** — *"The data file is read line-by-line as `OpportunityScoreSnapshot` by the alpha dataset reader and the integrity check, and a line of any other shape counts as malformed -- which is blocking."*

So: in-band breaks existing readers, out-of-band gives no ordering. **The replacement is correct and stronger** — a separately named observation stream, actual closed files, exact reconciliation of counts, loss, closure and file-set completeness, with **end marker alone never certifying** and global `written` counters never replacing row reconciliation.

### 1.2 My "canonical digest" — **RETRACTED** in favour of exact canonical sorted-tuple equality

Right call. A home-grown digest adds an unreviewed primitive with no dependency available, for no gain at fixture scale. Exact sorted-tuple equality over `(run, window, opportunity, eligibility provenance)` is authoritative and catches the case a count-only check misses — identical counts with changed content. Optional offline file SHA with existing tools only; no runtime dependency, no historical rehash.

### 1.3 My `RandomState` run ID — **RETRACTED**

`RandomState` is not a portable entropy API and exposes no fallible initialization contract, so the "fail loudly if unavailable" check I specified was **unimplementable as written**, and I should not have implied a uniqueness guarantee from it. Their replacement is correct: **exclusive `create_dir` allocation under an explicit observer root**, name carrying host/root namespace, PID, start timestamp and a collision counter; bounded retries on `AlreadyExists`, fail startup on other errors or exhaustion; directories retained, never reused or deleted to free a name. And the guarantee must be stated for what it is — **allocation uniqueness within that retained local root, not global or cross-host.** Export manifests must reject duplicate or conflicting namespaces, cloned roots and missing provenance. A real OS-random dependency is a separate review if cross-host uniqueness ever becomes necessary.

### 1.4 My 30 s freshness rationale — **RETRACTED as a structural argument**

I argued a price older than the cadence implies the engine "had a newer opportunity and didn't take it". That is invalid: ranking runs only when elapsed ≥ cadence **and the open set is non-empty**, so **30 s is minimum spacing, not guaranteed frequency** — ranking may not run for an arbitrarily long stretch, and a price can be far older than 30 s with no newer ranking opportunity having occurred. Withdrawn. Complete provenance plus computable non-negative ages are **necessary, not sufficient**, and both market age and receipt age must be recorded with neither substituting for the other.

### 1.5 My test target and budget numbers — **RETRACTED**

- **`cargo test -p ws-server --lib` is unsuitable**: verified there is no `crates/ws-server/src/lib.rs`.
- **10 % overhead, 300 ms, 10 % sidecar/base are proposals, not evidence-backed necessities**, and I should not have framed them as gates. P3's VPS p99 is **historical context, not an oracle for local hardware.** Correct approach accepted: paired same-host observer-off/on over identical synthetic streams, with warmup, repetitions and hardware/build/workload disclosure, measuring receive→processing-completion, observation/lifecycle traversal, rank, serialization/enqueue, queue lag/drain and memory/bytes. **Rank-only timing understates overhead** — that is a good catch, since the hook cost lands outside `rank`. **No timing pass until budgets are explicitly adopted for the intended host and workload.**

### 1.6 What of mine stands

The consumer-local cohort as sufficient with the near-key rejected as primary; the **ranking-completion anchor** with processing-start retained as a recorded secondary; `scored ≠ complete eligible` (the engine emits a row per traversed **open** opportunity, unscored included); and the `#[serde(flatten)]` constraint — because `EventFrame` flattens `ScanEvent`, **any added `ScanEvent` field is automatically client-visible**, so a wrapper is mandatory if the stamped-envelope fallback is ever chosen.

---

## 2. L1 — ACCEPTED, with limits recorded

`cargo test --offline -p ws-server observation`: **10 passed, 0 failed, 188 filtered, exit 0**, and the disclosure that **nine are new and one is a pre-existing test that happens to match the filter** is the right kind of precision. Offline, existing toolchain and cache, no installs, no network, no production hook, `target-linux` preserved, source `ss-p3` unchanged but for the pre-existing `?? target-linux/`.

The test set covers what matters, and specifically the cases that killed my §3: an actual write failure via a read-only handle **followed by a successfully written end marker**, with a missing persisted row still failing; injected flush failure unable to close successfully; missing end, partial line, changed/omitted rotation file, duplicate row, run mismatch, unknown/lost source evidence, ambiguous mapping, counter overflow, exclusive-name collision, cap exhaustion, missing price provenance. And an unscored complete row passing **without** conflating ranked with open set.

**Limits, to travel with any later claim:** L1 is **typed pure validation with caller-supplied inputs — not a raw NDJSON loader**, and file rows, loss and closure evidence are supplied rather than acquired. **Independent acquisition, parsing and evidence authentication are unimplemented** and are the largest L2 item. File size and mtime are **not** byte-integrity proof. Faults are simulated at the trait boundary and **not injected into the real `ResearchWriter`**, so **no crash-durability property is established** — which matters precisely because §1.1 showed there is no `fsync`. No disk index, export-namespace merge, lifecycle-hook mapping, performance benchmark or price-path collector. No enabled observer, no schema/writer/event change.

**One deviation worth recording:** H: candidate creation was permission-denied despite a scoped grant, so the candidate lives in an authorized writable workspace outside the project evidence root. That is correctly disclosed; note only that **later provenance aggregation must resolve that root explicitly**, since §1.3's namespace guarantee is scoped to a retained root, and the candidate's root is not the evidence root. The shared clone also depends on the source object store and is not a standalone artifact.

---

## 3. Final step-3 policy — **Route A, with one outcome-free safeguard**

Both routes are legitimate. I choose **Route A: fix both market age and receipt age ≤ 30 s at rank completion, frozen now, before any outcomes**, and I will not mix it with Route B.

**Rationale.** The cardinal protection is that no threshold is ever chosen after seeing outcomes, and Route A has that outright and immediately. Route B buys precision at the cost of an **additional gated development capture** — which needs its own designation, budget, authorization and headroom, none of which exist — plus a permanent contamination surface between development and untouched confirmation intervals. **And Route A's only real weakness is detectable without touching outcomes:** if 30 s is too tight, the *eligibility rate* collapses, and eligibility rate is an outcome-free statistic exactly like the pool-size and fraction>5 gates already in the protocol. So Route A obtains most of Route B's safety without Route B's extra interval and extra leakage risk.

**The 30 s figure is an arbitrary declared hypothesis.** It is not implied by cadence (§1.4), not optimal, and not derived. It is chosen for transparency and because it is the same order as the minimum ranking spacing, which makes it easy to reason about — nothing more.

**Safeguard, pre-committed now:** step 4 must report the **eligibility rate and the full market-age and receipt-age distributions before any outcome access**. If eligibility falls below a floor declared at freeze time, the protocol is **re-frozen with a new threshold before outcomes** — never loosened after seeing them, and never run knowingly underpowered. That keeps the correction path inside the outcome-free region.

**Frozen protocol, versioned as `consumer-received-protocol-v1`:**

1. **Cohort:** consumer-received, `(observerRunId, receiveSequence)` assigned at successful shadow receive. Historical emission-associated producer-state idealization remains separately named and is not this.
2. **Anchor:** **ranking completion.** Processing-start recorded as a secondary, with `t_end − t_start` measured.
3. **Price:** the same **last-incorporated** price for both selectors, with complete provenance — originating received-event identity, market time, receipt time, revision/order status. Unknown provenance ⇒ ineligible.
4. **Freshness:** market age **and** receipt age both ≤ 30 s at rank completion. Neither substitutes for the other.
5. **Confirmation:** exactly one unique confirmation receipt per lifecycle at the anchor. Multiplicity ⇒ excluded from the strict primary and counted separately.
6. **Window:** first eligible **complete actual** window. Partial or uncertified windows are ineligible; left-censored lifecycles stay censored, never assumed first-eligible.
7. **Pool and budget:** common eligible pool, `min(5, pool)`; candidates **retired after comparison whether selected or not**; **no refill** and no score substitution.
8. **Primary:** difference in **+2 % reach within 300 s** of the anchor, same anchor denominator for both arms.
9. **Missingness:** censoring explicit; missing is unknown, never success or failure; **cluster dependence reported** by symbol and session.
10. **Selection:** the confirmation interval is fixed **prospectively** and is untouched — never chosen retroactively by outcomes; designation rules apply.

**Step 3 closes on acceptance of the above.** It does **not** wait on performance results or data success. Step 4 then establishes IDs, complete persisted windows, price provenance and 300 s paths; step 5 evaluates. Unknown price or path evidence are **step-4 gates, not an open protocol question.**

---

## 4. Exact L2 scope — appropriate to proceed

**Files:** hook sites in `crates/ws-server/src/main.rs` (receive bracket, run/sequence assignment, lag marker) and `crates/ws-server/src/opportunity_shadow.rs` (pre/post lifecycle mapping, window accounting around the existing `observe` → `rank`); extend `observation.rs`/`observation_tests.rs`. **Prefer the existing read-only `open_opportunities` iterator at `opportunity.rs:1978`;** a read-only accessor in `backtest-metrics` only if that proves insufficient, separately justified, with lifecycle semantics tested rather than assumed.

**Largest L2 item, per §2:** the **independent acquisition, parsing and evidence authentication** that L1 deliberately omits — the closed-file reader that produces the row/loss/closure evidence L1 currently receives as typed input. Until that exists, no certificate describes a real capture.

**Invariants:** observer **disabled by default** behind a flag; **existing dependencies only**; `ScanEvent` unchanged, therefore `EventFrame` bytes unchanged; **separate observation stream with its own path and ownership** — never route new shapes into OI data files, never change `research_writer` semantics implicitly; scoring, ranks, selection thresholds, notification and paper-order behaviour unchanged; event order and time-sampling semantics preserved; no retention change; no disk-index or capture-lifecycle behaviour beyond explicit new observation ownership.

**Commands:**

```
cargo test --offline -p ws-server observation
cargo test -p ws-server
cargo test -p backtest-metrics --lib
cargo test --workspace          # only once hook changes justify full compatibility validation
```

**Fixtures to add:** `EventFrame` serialization byte-identical observer-off vs observer-on; a fixture that **fails if any field is ever added to `ScanEvent`** (§1.6); and — given §1.1 — a case asserting that a certificate cannot be issued from counters alone when the observation file set is incomplete.

**Not in L2:** any timing or capacity pass until budgets are explicitly adopted for the intended host and workload (§1.5); production hooks enabled; capture designation; the 300 s path collector; export-namespace merging.

---

## Verdict

**Step 3: COMPLETE on my side** — `consumer-received-protocol-v1` as frozen in §3, Route A with the §3 safeguard. **L1: ACCEPTED** with §2 limits. **Five of my defaults retracted** in §1, each verified against source; the reconciliation was right and I was wrong on all five. **L2 as scoped in §4 is appropriate** under existing reversible-local-work authorization.

Empirical readiness remains **NOT_READY** downstream; no efficacy, receipt, delivery, executable or fill claim is accepted or implied. No user decision identified.

**Artifact:** `H:/wavystack/stockspotter-research/STEP3-FINAL-VERDICT-AND-L2-SCOPE-20260928.md`. Prior reviews and capture directories preserved unchanged.
